//! Workstream A acceptance tests (design §4.3 "A"): the v2 store, its write rules,
//! dreams and the read-only mirror. Written against the frozen §4.1 interface.
//! Temporary stores only; nothing here touches a live database.
mod common;

use agent_memory_server::{model, recall, store::Store};
use common::*;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::io::AsRawFd,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

/// `model::PINNED_BUDGET`, design §3.1.
const PINNED_BUDGET: usize = 4000;
const MAX_CONTENT: usize = 102_400;
const STORE_LIMIT: usize = 10_000;

fn pinned() -> Value {
    json!({"pinned": true})
}
fn no_meta() -> Value {
    json!({})
}
fn text(title: &str, body: &str) -> String {
    content(title, body, &no_meta())
}
/// Content for a memory seeded by `seed(path, body)` (its title is the slug).
fn seeded(path: &str, body: &str) -> String {
    text(
        path.rsplit('/').next().unwrap().trim_end_matches(".md"),
        body,
    )
}
fn get(ts: &TempStore, id: &str) -> Value {
    ts.store.get(&ts.live(), id).unwrap()
}
fn live_list(ts: &TempStore) -> Value {
    ts.store.list(&ts.live(), "/").unwrap()
}
fn versions_of(ts: &TempStore, store: &str, memory_id: &str) -> Vec<Value> {
    data(&ts.store.versions(store, Some(memory_id), None).unwrap()).clone()
}
fn status_u64(ts: &TempStore, key: &str) -> u64 {
    ts.store.status().unwrap()[key]
        .as_u64()
        .unwrap_or_else(|| panic!("status.{key} is not a number"))
}
fn start(ts: &mut TempStore, sessions: Value) -> Value {
    ts.store.dream_start(&sessions, "token:review").unwrap()
}
fn review(summary: &str) -> Value {
    json!({"summary": summary, "coverage": "fixtures only", "conflicts": [], "reviewed": true})
}
fn adopt(ts: &mut TempStore, dream: &str) -> agent_memory_server::Result<Value> {
    ts.store.dream_adopt(dream, &review("test"), "token:admin")
}
fn dream_status(ts: &TempStore, dream: &str) -> String {
    str_of(&ts.store.dream(dream, false).unwrap(), "status").to_owned()
}
/// Two live memories, `/global/x.md` and `/global/y.md`.
fn xy() -> (TempStore, String, Value, Value) {
    let mut ts = TempStore::new();
    let live = ts.live();
    let x = seed(&mut ts.store, "/global/x.md", "x original");
    let y = seed(&mut ts.store, "/global/y.md", "y original");
    (ts, live, x, y)
}
fn set_content(ts: &mut TempStore, store: &str, memory: &Value, body: &str, by: &str) -> Value {
    let title = str_of(memory, "path").to_owned();
    ts.store
        .update(
            store,
            &id(memory),
            Some(&seeded(&title, body)),
            None,
            Some(str_of(memory, "content_sha256")),
            by,
        )
        .unwrap()
}

// ---- mirror assertions ---------------------------------------------------

/// The mirror equals the live store: bytes, modes, marker, INDEX.md, generations.
fn assert_mirror(ts: &TempStore) {
    let status = ts.store.status().unwrap();
    let revision = status["revision"].as_i64().unwrap();
    let link = ts.link();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "mnt/shared-agent-memory must be a symlink"
    );
    assert_eq!(
        fs::read_link(&link).unwrap().file_name().unwrap().to_str(),
        Some(revision.to_string().as_str()),
        "the symlink must point at .generations/<revision>"
    );
    let generations: Vec<_> = fs::read_dir(ts.mnt.join(".generations"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        generations.len() <= 2 && generations.contains(&revision.to_string()),
        "keep only the current and previous generation, found {generations:?}"
    );
    let mut top: Vec<_> = fs::read_dir(&ts.mnt)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(
        top,
        [".generations", "shared-agent-memory"],
        "only the live store is mirrored"
    );

    let (live_revision, memories) = ts.store.live_memories().unwrap();
    assert_eq!(live_revision, revision);
    let live = ts.live();
    let mut expected: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for m in &memories {
        let stored = ts.store.get(&live, &m.id).unwrap();
        expected.insert(
            m.path.trim_start_matches('/').to_owned(),
            str_of(&stored, "content").as_bytes().to_vec(),
        );
    }
    let index = recall::index_md(&memories, revision, str_of(&status, "mirror_dir"));
    expected.insert("INDEX.md".into(), index.into_bytes());

    let mut actual = BTreeMap::new();
    for (name, (mode, bytes)) in tree(&link) {
        if name == "/" || name.ends_with('/') {
            assert_eq!(mode, 0o555, "directory {name} must be 0555");
        } else {
            assert_eq!(mode, 0o444, "file {name} must be 0444");
            if name == ".agent-memory-store" {
                let marker: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(marker["store_id"], status["store_id"]);
                assert_eq!(marker["revision"].as_i64(), Some(revision));
            } else {
                actual.insert(name, bytes);
            }
        }
    }
    assert_eq!(actual, expected, "mirror files differ from the live store");
}

// ---- 1. init -------------------------------------------------------------

#[test]
fn init_v2() {
    let ts = TempStore::new();
    let conn = Connection::open(&ts.db).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let stores: Vec<(String, Option<String>)> = conn
        .prepare("SELECT kind, archived_at FROM stores")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        stores,
        [("live".to_owned(), None)],
        "exactly one live store"
    );
    // The legacy tables exist and are frozen.
    let err = conn
        .execute(
            "INSERT INTO revisions(parent,created,reason,body,sha256) VALUES (0,'t','r','[]','x')",
            [],
        )
        .unwrap_err();
    assert!(err.to_string().contains("migrated to v2"), "{err}");

    let status = ts.store.status().unwrap();
    assert_eq!(status["revision"], 0);
    assert_eq!(status["memories"], 0);
    assert_eq!(status["pinned_chars"], 0);
    assert_eq!(status["observations_pending"], 0);
    assert_eq!(status["dreams_running"], 0);
    assert_eq!(status["mirror_stale"], false);
    assert!(str_of(&status, "store_id").starts_with("memstore_"));
    assert!(str_of(&status, "mirror_dir").ends_with("shared-agent-memory"));
    assert!(ts.store.is_live(&ts.live()).unwrap());
    assert_mirror(&ts);

    // A v1 database is refused, and the message tells the operator what to run.
    let tmp = tempfile::TempDir::new().unwrap();
    let v1 = tmp.path().join("v1.sqlite3");
    Connection::open(&v1)
        .unwrap()
        .execute_batch(
            "CREATE TABLE revisions (id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
                reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
             CREATE TABLE state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE observations (id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
             CREATE TABLE dreams (id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
                input TEXT NOT NULL, promoted INTEGER, proposal TEXT);
             INSERT INTO revisions VALUES (0,NULL,'t','Empty initial store','[]','x');
             INSERT INTO state VALUES ('active',0);",
        )
        .unwrap();
    let mnt = tmp.path().join("mnt");
    let refused = Store::open(&v1, &mnt)
        .err()
        .expect("a v1 database must be refused");
    assert!(refused.to_string().contains("migrate"), "{refused}");
    assert!(!mnt.exists(), "a refused open must not create a mirror");
}

// ---- 2. paths ------------------------------------------------------------

fn deep(segments: usize) -> String {
    format!("/projects{}/s.md", "/d".repeat(segments - 2))
}

#[test]
fn path_rules() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let ok = text("ok", "body");

    // All four layouts are accepted, by the validator and by create.
    for (path, slug) in [
        ("/global/a.md", "a"),
        ("/projects/home/sai/x/b.md", "b"),
        ("/topics/hardware/c.md", "c"),
        ("/roles/coordinator/d.md", "d"),
    ] {
        assert_eq!(model::validate_path(path).unwrap().slug, slug, "{path}");
        ts.store
            .create(&live, path, &seeded(path, "body"), ACTOR)
            .unwrap_or_else(|e| panic!("{path}: {e:?}"));
    }
    // Boundaries that are still valid: 20 segments, 1,024 bytes, 80-character slug,
    // dotted and hyphenated segments.
    let mut bytes_1024 = String::from("/projects");
    for _ in 0..5 {
        bytes_1024 += &format!("/{}", "a".repeat(200));
    }
    bytes_1024 += "/a/slug.md";
    assert_eq!(bytes_1024.len(), 1024);
    for path in [
        deep(20),
        bytes_1024.clone(),
        format!("/global/{}.md", "a".repeat(80)),
        "/global/0-9-a.md".into(),
        "/projects/home/sai/.hidden-dir/v1.2_x/e.md".into(),
    ] {
        model::validate_path(&path).unwrap_or_else(|e| panic!("{path}: {e:?}"));
    }

    let mut bad: Vec<(String, &str)> = vec![
        ("/global/../a.md".into(), "dot-dot segment"),
        ("/projects/home/../etc/a.md".into(), "dot-dot segment"),
        ("/projects/./home/a.md".into(), "dot segment"),
        ("/global//a.md".into(), "empty segment"),
        ("/topics//a.md".into(), "empty segment"),
        ("/global/a.md/".into(), "trailing slash"),
        ("global/a.md".into(), "not absolute"),
        (deep(21), "21 segments"),
        (bytes_1024.replace("/slug.md", "/slugs.md"), "1,025 bytes"),
        ("/topics/café/a.md".into(), "non-ASCII segment"),
        ("/global/မြန်မာ.md".into(), "non-ASCII slug"),
        ("/topics/machine learning/a.md".into(), "space in segment"),
        ("/misc/a.md".into(), "unknown top-level directory"),
        ("/a.md".into(), "no top-level directory"),
        (
            "/Global/a.md".into(),
            "top-level directories are case-sensitive",
        ),
        ("/global/a.txt".into(), "not .md"),
        ("/global/a".into(), "no extension"),
        ("/global/.md".into(), "empty slug"),
        ("/global/A.md".into(), "uppercase slug"),
        ("/global/-a.md".into(), "slug starts with a hyphen"),
        ("/global/a_b.md".into(), "underscore in slug"),
        ("/global/a.b.md".into(), "dot in slug"),
        (
            format!("/global/{}.md", "a".repeat(81)),
            "81-character slug",
        ),
    ];
    bad.push(("".into(), "empty path"));
    for (path, why) in &bad {
        assert!(
            model::validate_path(path).is_err(),
            "{why}: {path:?} was accepted"
        );
        invalid(ts.store.create(&live, path, &ok, ACTOR));
    }
    assert_eq!(
        status_u64(&ts, "memories"),
        4,
        "rejected paths wrote nothing"
    );
    assert_eq!(status_u64(&ts, "revision"), 4);
}

// ---- 3. content ----------------------------------------------------------

#[test]
fn content_rules() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let fm = |meta: Value| frontmatter("Title", &meta);
    let without = |key: &str| {
        let mut f = fm(json!({}));
        f.as_object_mut().unwrap().remove(key);
        render(&f, "body")
    };
    let overhead = text("T", "").len();

    let bad: Vec<(String, String)> = vec![
        ("no frontmatter".into(), "just a body\n".into()),
        (
            "frontmatter not closed".into(),
            format!("---\n{}\nbody", fm(json!({}))),
        ),
        (
            "frontmatter not JSON".into(),
            "---\ntitle: x\n---\nbody".into(),
        ),
        (
            "frontmatter not an object".into(),
            "---\n[1]\n---\nbody".into(),
        ),
        ("missing title".into(), without("title")),
        ("missing confidence".into(), without("confidence")),
        ("missing tags".into(), without("tags")),
        ("missing evidence".into(), without("evidence")),
        (
            "empty evidence".into(),
            render(&fm(json!({"evidence": []})), "body"),
        ),
        (
            "evidence item without note".into(),
            render(
                &fm(json!({"evidence": [{"source": "s", "locator": "l"}]})),
                "body",
            ),
        ),
        (
            "evidence item with empty locator".into(),
            render(
                &fm(json!({"evidence": [{"source": "s", "locator": "", "note": "n"}]})),
                "body",
            ),
        ),
        (
            "bad confidence".into(),
            render(&fm(json!({"confidence": "maybe"})), "body"),
        ),
        (
            "tags not a list".into(),
            render(&fm(json!({"tags": "a"})), "body"),
        ),
        ("empty title".into(), text("", "body")),
        ("multi-line title".into(), text("a\nb", "body")),
        ("121-character title".into(), text(&"t".repeat(121), "body")),
        ("id key".into(), render(&fm(json!({"id": "x"})), "body")),
        (
            "scope key".into(),
            render(&fm(json!({"scope": {"kind": "global"}})), "body"),
        ),
        ("body key".into(), render(&fm(json!({"body": "x"})), "body")),
        (
            "bad expires".into(),
            render(&fm(json!({"expires": "tomorrow"})), "body"),
        ),
        (
            "non-integer priority".into(),
            render(&fm(json!({"priority": "high"})), "body"),
        ),
        (
            "102,401 bytes".into(),
            text("T", &"x".repeat(MAX_CONTENT + 1 - overhead)),
        ),
        (
            "api_key=".into(),
            text("T", "set api_key=abcd1234efgh5678 in the shell"),
        ),
        (
            "private key".into(),
            text("T", "-----BEGIN RSA PRIVATE KEY-----"),
        ),
        ("empty body".into(), text("T", "")),
    ];
    for (why, text) in &bad {
        invalid(ts.store.create(&live, "/global/bad.md", text, ACTOR));
        assert!(
            model::parse("/global/bad.md", text).is_err(),
            "{why} parsed"
        );
    }
    // `pinned` is only for /global/ and /projects/ paths.
    let pinned_topic = content("T", "body", &pinned());
    invalid(
        ts.store
            .create(&live, "/topics/t/p.md", &pinned_topic, ACTOR),
    );
    invalid(
        ts.store
            .create(&live, "/roles/r/p.md", &pinned_topic, ACTOR),
    );
    assert!(model::parse("/topics/t/p.md", &pinned_topic).is_err());
    assert_eq!(
        status_u64(&ts, "memories"),
        0,
        "rejected content wrote nothing"
    );
    assert_eq!(status_u64(&ts, "revision"), 0);

    // Accepted: both ends of the size limit, optional fields and unknown keys.
    let exactly = text("T", &"x".repeat(MAX_CONTENT - overhead));
    assert_eq!(exactly.len(), MAX_CONTENT);
    ts.store
        .create(&live, "/global/max.md", &exactly, ACTOR)
        .unwrap();
    let title_120 = text(&"t".repeat(120), "body");
    ts.store
        .create(&live, "/global/title.md", &title_120, ACTOR)
        .unwrap();
    for (n, meta) in [
        json!({"confidence": "tentative"}),
        json!({"confidence": "corroborated"}),
        json!({"expires": "2026-12-31"}),
        json!({"expires": null}),
        json!({"expires": ""}),
        json!({"priority": 7}),
        json!({"pinned": true}),
        json!({"updated": "2026-11-01", "custom": {"nested": [1, 2.5]}}),
    ]
    .iter()
    .enumerate()
    {
        let path = format!("/global/ok-{n}.md");
        let good = content("Title", "body", meta);
        ts.store
            .create(&live, &path, &good, ACTOR)
            .unwrap_or_else(|e| panic!("{meta}: {e:?}"));
        let parsed = model::parse(&path, &good).unwrap();
        assert_eq!(parsed.body, "body");
        assert_eq!(parsed.meta["title"], "Title");
    }
    ts.store
        .create(
            &live,
            "/projects/home/sai/x/pinned.md",
            &content("Title", "body", &pinned()),
            ACTOR,
        )
        .unwrap();
}

// ---- 4. raw bytes --------------------------------------------------------

#[test]
fn sha_is_raw_bytes() {
    let mut ts = TempStore::new();
    let live = ts.live();
    // Non-canonical JSON spacing and key order, a non-canonical float, Burmese text
    // and trailing whitespace: all must survive byte for byte.
    let first = "---\n{ \"title\" : \"မြန်မာ\" ,\n \"extra\": 1.50,\"confidence\":\"explicit\",\"tags\":[ \"a\" ],\
                 \"evidence\":[{\"source\":\"s\",\"locator\":\"l\",\"note\":\"မှတ်ချက်\"}] }\n---\nမြန်မာ body  \n\n";
    let created = ts
        .store
        .create(&live, "/global/raw.md", first, ACTOR)
        .unwrap();
    assert_eq!(str_of(&created, "content_sha256"), sha256(first.as_bytes()));
    let stored = get(&ts, &id(&created));
    assert_eq!(str_of(&stored, "content"), first);
    assert_eq!(str_of(&stored, "content_sha256"), sha256(first.as_bytes()));
    assert_eq!(
        fs::read(ts.link().join("global/raw.md")).unwrap(),
        first.as_bytes()
    );
    let version = ts
        .store
        .version(&live, str_of(&created, "memory_version_id"))
        .unwrap();
    assert_eq!(str_of(&version, "content"), first);
    assert_eq!(str_of(&version, "content_sha256"), sha256(first.as_bytes()));

    let second = first.replace("1.50", "1.5e0").replace("  \n\n", "\n");
    let updated = ts
        .store
        .update(
            &live,
            &id(&created),
            Some(&second),
            None,
            Some(&sha256(first.as_bytes())),
            ACTOR,
        )
        .unwrap();
    assert_eq!(
        str_of(&updated, "content_sha256"),
        sha256(second.as_bytes())
    );
    assert_eq!(str_of(&get(&ts, &id(&created)), "content"), second);
    assert_eq!(
        fs::read(ts.link().join("global/raw.md")).unwrap(),
        second.as_bytes()
    );
    assert_ne!(sha256(first.as_bytes()), sha256(second.as_bytes()));
}

// ---- 5. create conflicts -------------------------------------------------

#[test]
fn create_conflicts() {
    let mut ts = TempStore::new();
    let live = ts.live();
    seed(&mut ts.store, "/global/a.md", "first");
    let revision = status_u64(&ts, "revision");

    let paths = conflict(
        ts.store.create(
            &live,
            "/global/a.md",
            &seeded("/global/a.md", "other"),
            ACTOR,
        ),
        "path_exists",
    );
    assert_eq!(paths, ["/global/a.md"]);
    // The slug is unique in the store, whatever the directory.
    for other in [
        "/topics/t/a.md",
        "/roles/r/a.md",
        "/projects/home/sai/x/a.md",
    ] {
        let paths = conflict(
            ts.store
                .create(&live, other, &seeded(other, "other"), ACTOR),
            "slug_exists",
        );
        assert_eq!(
            paths,
            ["/global/a.md"],
            "slug_exists names the path using the slug"
        );
    }
    assert_eq!(
        status_u64(&ts, "revision"),
        revision,
        "failed creates add no version"
    );
    assert_eq!(status_u64(&ts, "memories"), 1);

    // Eight writers creating one path: exactly one wins.
    let results = race(&ts, 8, |i, store| {
        store.create(
            &live,
            "/global/race.md",
            &text("race", &format!("writer {i}")),
            ACTOR,
        )
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    for r in results.into_iter().filter(Result::is_err) {
        conflict(r, "path_exists");
    }
    assert_eq!(status_u64(&ts, "memories"), 2);
}

// ---- 6. update preconditions and rename ----------------------------------

#[test]
fn update_precondition() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let created = seed(&mut ts.store, "/topics/t/a.md", "one");
    let mid = id(&created);
    let h1 = str_of(&created, "content_sha256").to_owned();
    let v1 = str_of(&created, "memory_version_id").to_owned();
    let two = seeded("/topics/t/a.md", "two");

    // A stale hash fails and adds nothing.
    let before = version_count(&ts.store, &live);
    conflict(
        ts.store.update(
            &live,
            &mid,
            Some(&two),
            None,
            Some(&sha256(b"not the content")),
            ACTOR,
        ),
        "precondition_failed",
    );
    assert_eq!(version_count(&ts.store, &live), before);
    assert_eq!(str_of(&get(&ts, &mid), "content_sha256"), h1);

    // A matching hash creates a new version.
    let updated = ts
        .store
        .update(&live, &mid, Some(&two), None, Some(&h1), "token:writer")
        .unwrap();
    let v2 = str_of(&updated, "memory_version_id").to_owned();
    assert_ne!(v2, v1);
    assert_eq!(str_of(&updated, "content_sha256"), sha256(two.as_bytes()));
    assert_eq!(str_of(&get(&ts, &mid), "content"), two);
    assert_eq!(version_count(&ts.store, &live), before + 1);
    // The old hash is now stale.
    conflict(
        ts.store.update(
            &live,
            &mid,
            Some(&seeded("/topics/t/a.md", "three")),
            None,
            Some(&h1),
            ACTOR,
        ),
        "precondition_failed",
    );
    // No precondition means an unconditional update.
    let three = seeded("/topics/t/a.md", "three");
    ts.store
        .update(&live, &mid, Some(&three), None, None, ACTOR)
        .unwrap();
    assert_eq!(str_of(&get(&ts, &mid), "content"), three);

    // A rename keeps the id; the content is unchanged and the history shows both paths.
    let h3 = sha256(three.as_bytes());
    conflict(
        ts.store
            .update(&live, &mid, None, Some("/topics/t2/a.md"), Some(&h1), ACTOR),
        "precondition_failed",
    );
    let renamed = ts
        .store
        .update(&live, &mid, None, Some("/topics/t2/a.md"), Some(&h3), ACTOR)
        .unwrap();
    assert_eq!(id(&renamed), mid);
    assert_eq!(str_of(&renamed, "path"), "/topics/t2/a.md");
    assert_eq!(str_of(&renamed, "content_sha256"), h3);
    let moved = get(&ts, &mid);
    assert_eq!(str_of(&moved, "path"), "/topics/t2/a.md");
    assert_eq!(str_of(&moved, "content"), three);
    assert_eq!(listed_paths(&live_list(&ts)), ["/topics/t2/a.md"]);
    let history = versions_of(&ts, &live, &mid);
    assert_eq!(history.len(), 4);
    let paths: Vec<&str> = history.iter().map(|v| str_of(v, "path")).collect();
    assert_eq!(paths.first(), Some(&"/topics/t2/a.md"), "newest first");
    assert_eq!(paths.last(), Some(&"/topics/t/a.md"));
    let operations: Vec<&str> = history.iter().map(|v| str_of(v, "operation")).collect();
    assert_eq!(operations, ["modified", "modified", "modified", "created"]);
    assert_eq!(str_of(&history[2], "created_by"), "token:writer");
    assert!(
        history.iter().all(|v| v["content"].is_null()),
        "listings carry no content"
    );
    assert_eq!(str_of(&history[3], "id"), v1);
    assert_eq!(str_of(&history[2], "id"), v2);
}

// ---- 7. concurrent writers -----------------------------------------------

#[test]
fn concurrent_writers_one_winner() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let created = seed(&mut ts.store, "/global/a.md", "start");
    let mid = id(&created);
    let h = str_of(&created, "content_sha256").to_owned();
    let before = version_count(&ts.store, &live);
    let revision = status_u64(&ts, "revision");

    let results = race(&ts, 8, |i, store| {
        store.update(
            &live,
            &mid,
            Some(&seeded("/global/a.md", &format!("writer {i}"))),
            None,
            Some(&h),
            &format!("token:w{i}"),
        )
    });
    let winners: Vec<usize> = (0..8).filter(|&i| results[i].is_ok()).collect();
    assert_eq!(winners.len(), 1, "exactly one writer may win: {results:?}");
    let mut losers = 0;
    for r in results.into_iter().filter(Result::is_err) {
        conflict(r, "precondition_failed");
        losers += 1;
    }
    assert_eq!(losers, 7);
    assert_eq!(version_count(&ts.store, &live), before + 1);
    assert_eq!(status_u64(&ts, "revision"), revision + 1);
    assert_eq!(
        str_of(&get(&ts, &mid), "content"),
        seeded("/global/a.md", &format!("writer {}", winners[0]))
    );
    assert_mirror(&ts);

    // The same holds for deletes.
    let h = str_of(&get(&ts, &mid), "content_sha256").to_owned();
    let results = race(&ts, 8, |_, store| {
        store.delete(&live, &mid, Some(&h), ACTOR)
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(status_u64(&ts, "memories"), 0);
    assert_eq!(
        versions_of(&ts, &live, &mid)
            .iter()
            .filter(|v| v["operation"] == "deleted")
            .count(),
        1
    );
}

// ---- 8. delete and redact ------------------------------------------------

#[test]
fn delete_and_redact() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let created = seed(&mut ts.store, "/global/a.md", "one");
    let mid = id(&created);
    let v1 = str_of(&created, "memory_version_id").to_owned();
    let two = seeded("/global/a.md", "two");
    let updated = ts
        .store
        .update(
            &live,
            &mid,
            Some(&two),
            None,
            Some(str_of(&created, "content_sha256")),
            ACTOR,
        )
        .unwrap();
    let v2 = str_of(&updated, "memory_version_id").to_owned();
    let h2 = sha256(two.as_bytes());

    // Redacting the head version of a live memory is refused.
    invalid(ts.store.redact(&live, &v2, "token:admin"));
    assert_eq!(
        str_of(&ts.store.version(&live, &v2).unwrap(), "content"),
        two
    );

    // Redacting an older version nulls its fields and keeps who made it.
    let redacted = ts.store.redact(&live, &v1, "token:admin").unwrap();
    assert_eq!(str_of(&redacted, "id"), v1);
    for key in ["content", "path", "content_sha256"] {
        assert!(redacted[key].is_null(), "{key} must be null: {redacted}");
    }
    assert!(redacted["redacted_at"].is_string());
    assert_eq!(str_of(&redacted, "redacted_by"), "token:admin");
    assert_eq!(str_of(&redacted, "created_by"), ACTOR);
    assert_eq!(str_of(&redacted, "operation"), "created");
    let again = ts.store.version(&live, &v1).unwrap();
    assert!(again["content"].is_null() && again["path"].is_null());
    assert_eq!(str_of(&again, "redacted_by"), "token:admin");
    assert_eq!(
        str_of(&get(&ts, &mid), "content"),
        two,
        "the live memory is unaffected"
    );
    assert_mirror(&ts);

    // A wrong expected hash blocks the delete.
    conflict(
        ts.store.delete(&live, &mid, Some(&sha256(b"stale")), ACTOR),
        "precondition_failed",
    );
    assert_eq!(status_u64(&ts, "memories"), 1);

    // Delete adds a `deleted` version that keeps the lineage and the last path.
    let before = version_count(&ts.store, &live);
    let deleted = ts
        .store
        .delete(&live, &mid, Some(&h2), "token:admin")
        .unwrap();
    assert_eq!(str_of(&deleted, "type"), "memory_deleted");
    assert_eq!(str_of(&deleted, "id"), mid);
    not_found(ts.store.get(&live, &mid));
    assert!(listed_paths(&live_list(&ts)).is_empty());
    let history = versions_of(&ts, &live, &mid);
    assert_eq!(history.len(), 3);
    assert_eq!(version_count(&ts.store, &live), before + 1);
    assert_eq!(str_of(&history[0], "operation"), "deleted");
    assert_eq!(str_of(&history[0], "created_by"), "token:admin");
    assert_eq!(str_of(&history[0], "path"), "/global/a.md");
    assert!(history[0]["content"].is_null() && history[0]["content_sha256"].is_null());
    assert_eq!(str_of(&history[1], "operation"), "modified");
    assert_mirror(&ts);
    assert!(!ts.link().join("global/a.md").exists());

    // Once the memory is gone, its last content version is no longer a head.
    let redacted = ts.store.redact(&live, &v2, "token:admin").unwrap();
    assert!(redacted["content"].is_null());
    // The path can be used again, by a new memory.
    let reborn = seed(&mut ts.store, "/global/a.md", "again");
    assert_ne!(id(&reborn), mid);
    // Delete without a precondition is allowed.
    ts.store.delete(&live, &id(&reborn), None, ACTOR).unwrap();
    // Unknown objects.
    not_found(ts.store.delete(&live, &mid, None, ACTOR));
    not_found(
        ts.store
            .version(&live, "memver_00000000000000000000000000000000"),
    );
}

// ---- 9. limits -----------------------------------------------------------

#[test]
fn store_full() {
    let mut ts = TempStore::new();
    let live = ts.live();
    seed_raw(&ts.db, STORE_LIMIT - 2);
    ts.reopen();
    assert_eq!(status_u64(&ts, "memories") as usize, STORE_LIMIT - 2);
    let last = seed(&mut ts.store, "/global/last.md", "the 9,999th");
    seed(&mut ts.store, "/global/limit.md", "the 10,000th");
    assert_eq!(status_u64(&ts, "memories") as usize, STORE_LIMIT);
    let revision = status_u64(&ts, "revision");

    let message = invalid(
        ts.store
            .create(&live, "/global/over.md", &text("over", "x"), ACTOR),
    );
    assert!(message.contains("store_full"), "{message}");
    assert_eq!(status_u64(&ts, "memories") as usize, STORE_LIMIT);
    assert_eq!(status_u64(&ts, "revision"), revision, "nothing was written");
    assert!(!ts.link().join("global/over.md").exists());
    // At the limit, existing memories can still be updated and deleted.
    set_content(&mut ts, &live, &last, "changed", ACTOR);
    ts.store.delete(&live, &id(&last), None, ACTOR).unwrap();
    seed(&mut ts.store, "/global/over.md", "room again");
}

#[test]
fn pinned_budget() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let make = |ts: &mut TempStore, path: &str, body: &str| {
        let title = path.rsplit('/').next().unwrap().trim_end_matches(".md");
        ts.store
            .create(&live, path, &content(title, body, &pinned()), ACTOR)
    };
    // The budget counts characters, not bytes: 2,500 Burmese letters are 7,500 bytes.
    let burmese = "မ".repeat(2500);
    let p1 = make(&mut ts, "/global/p1.md", &burmese).unwrap();
    let p2 = make(&mut ts, "/global/p2.md", &"b".repeat(1500)).unwrap();
    assert_eq!(status_u64(&ts, "pinned_chars") as usize, PINNED_BUDGET);
    let revision = status_u64(&ts, "revision");

    // One more character is refused and nothing is written.
    conflict(
        make(&mut ts, "/global/p3.md", "c"),
        "pinned_budget_exceeded",
    );
    conflict(
        make(&mut ts, "/projects/home/sai/x/p3.md", "c"),
        "pinned_budget_exceeded",
    );
    assert_eq!(status_u64(&ts, "revision"), revision);
    assert_eq!(status_u64(&ts, "memories"), 2);
    assert!(!ts.link().join("global/p3.md").exists());
    // Growing a pinned memory past the budget is refused too.
    let grown = content("p2", &"b".repeat(1501), &pinned());
    conflict(
        ts.store.update(
            &live,
            &id(&p2),
            Some(&grown),
            None,
            Some(str_of(&p2, "content_sha256")),
            ACTOR,
        ),
        "pinned_budget_exceeded",
    );
    assert_eq!(status_u64(&ts, "revision"), revision);

    // Unpinned memories are not counted, however large.
    seed(&mut ts.store, "/global/big.md", &"u".repeat(20_000));
    assert_eq!(status_u64(&ts, "pinned_chars") as usize, PINNED_BUDGET);
    // Shrinking or unpinning frees budget.
    let shrunk = content("p2", &"b".repeat(1000), &pinned());
    ts.store
        .update(
            &live,
            &id(&p2),
            Some(&shrunk),
            None,
            Some(str_of(&p2, "content_sha256")),
            ACTOR,
        )
        .unwrap();
    assert_eq!(status_u64(&ts, "pinned_chars") as usize, 3500);
    make(&mut ts, "/projects/home/sai/x/p3.md", &"c".repeat(500)).unwrap();
    assert_eq!(status_u64(&ts, "pinned_chars") as usize, PINNED_BUDGET);
    ts.store
        .update(
            &live,
            &id(&p1),
            Some(&seeded("/global/p1.md", "plain")),
            None,
            None,
            ACTOR,
        )
        .unwrap();
    assert_eq!(status_u64(&ts, "pinned_chars") as usize, 1500);
}

// ---- 10-11. dreams: start and isolation -----------------------------------

/// Everything observable about the live store, for "left untouched" assertions:
/// memory identities, version count, revision and the mirror's files.
type Snapshot = (
    Vec<(String, String, String)>,
    usize,
    u64,
    Vec<(String, Vec<u8>)>,
);

fn live_snapshot(ts: &TempStore) -> Snapshot {
    let live = ts.live();
    let files = tree(&ts.link())
        .into_iter()
        .map(|(name, (_, bytes))| (name, bytes))
        .collect();
    (
        listed_identity(&live_list(ts)),
        version_count(&ts.store, &live),
        status_u64(ts, "revision"),
        files,
    )
}

#[test]
fn dream_start_freezes_and_clones() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let a = seed(&mut ts.store, "/global/a.md", "alpha");
    let b = seed(&mut ts.store, "/topics/t/b.md", "beta");
    seed_meta(&mut ts.store, "/global/c.md", "gamma", &pinned());
    let o1 = ts
        .store
        .observe("/global/obs-1.md", &text("obs-1", "one"), ACTOR)
        .unwrap();
    let o2 = ts
        .store
        .observe("/global/obs-2.md", &text("obs-2", "two"), ACTOR)
        .unwrap();
    assert_eq!(status_u64(&ts, "observations_pending"), 2);
    let before = live_snapshot(&ts);

    let sessions = json!([
        {"source": "claude", "locator": "/tmp/s1.jsonl", "sha256": sha256(b"s1")},
        {"source": "codex", "locator": "/tmp/s2.jsonl"},
    ]);
    let dream = start(&mut ts, sessions.clone());
    let did = id(&dream);
    let out = str_of(&dream, "output_store_id").to_owned();
    assert!(did.starts_with("drm_"));
    assert!(out.starts_with("memstore_") && out != live);
    assert_eq!(str_of(&dream, "status"), "running");
    assert!(dream["ended_at"].is_null());
    assert_eq!(dream["sessions"], sessions);
    assert_eq!(dream["observations"].as_array().unwrap().len(), 2);
    let observations = dream["observations"].to_string();
    assert!(observations.contains(str_of(&o1, "id")) && observations.contains(str_of(&o2, "id")));
    assert!(!ts.store.is_live(&out).unwrap());
    assert_eq!(status_u64(&ts, "dreams_running"), 1);
    assert_eq!(
        status_u64(&ts, "observations_pending"),
        2,
        "pending until a dream is adopted"
    );

    // The output store is a clone: same ids, paths and hashes, `created` by the dream.
    let cloned = ts.store.list(&out, "/").unwrap();
    assert_eq!(listed_identity(&cloned), listed_identity(&live_list(&ts)));
    assert_eq!(
        str_of(&ts.store.get(&out, &id(&a)).unwrap(), "content"),
        seeded("/global/a.md", "alpha")
    );
    assert_eq!(
        str_of(&ts.store.get(&out, &id(&b)).unwrap(), "path"),
        "/topics/t/b.md"
    );
    let output_versions = data(&ts.store.versions(&out, None, None).unwrap()).clone();
    assert_eq!(output_versions.len(), 3);
    for v in &output_versions {
        assert_eq!(str_of(v, "operation"), "created");
        assert_eq!(str_of(v, "created_by"), format!("dream:{did}"));
    }
    // The live store is untouched, mirror included.
    assert_eq!(live_snapshot(&ts), before);
    assert_eq!(
        status_u64(&ts, "memories"),
        3,
        "status counts the live store only"
    );

    // The input is frozen: later observations are not part of this dream.
    ts.store
        .observe("/global/obs-3.md", &text("obs-3", "three"), ACTOR)
        .unwrap();
    assert_eq!(
        ts.store.dream(&did, false).unwrap()["observations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(str_of(&ts.store.dreams().unwrap()["data"][0], "id"), did);

    // Limits on sessions; a rejected start creates nothing.
    let dreams = data(&ts.store.dreams().unwrap()).len();
    let numbered = |n: usize| -> Value {
        (0..n)
            .map(|i| json!({"source": "claude", "locator": format!("/tmp/{i}.jsonl")}))
            .collect()
    };
    invalid(ts.store.dream_start(&numbered(101), "token:review"));
    let duplicate = json!([
        {"source": "claude", "locator": "/tmp/same.jsonl"},
        {"source": "claude", "locator": "/tmp/same.jsonl"},
    ]);
    invalid(ts.store.dream_start(&duplicate, "token:review"));
    invalid(
        ts.store
            .dream_start(&json!([{"source": "claude"}]), "token:review"),
    );
    assert_eq!(data(&ts.store.dreams().unwrap()).len(), dreams);
    start(&mut ts, numbered(100));
    start(&mut ts, json!([]));
    assert_eq!(data(&ts.store.dreams().unwrap()).len(), dreams + 2);
}

#[test]
fn dream_never_modifies_input() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let a = seed(&mut ts.store, "/global/a.md", "alpha");
    let b = seed(&mut ts.store, "/topics/t/b.md", "beta");
    let c = seed(&mut ts.store, "/topics/t/c.md", "gamma");
    let before = live_snapshot(&ts);
    let dream = start(&mut ts, json!([]));
    let out = str_of(&dream, "output_store_id").to_owned();
    assert_eq!(live_snapshot(&ts), before);

    // Every kind of edit in the output store.
    set_content(&mut ts, &out, &a, "alpha edited", "dream:agent");
    ts.store
        .update(&out, &id(&b), None, Some("/topics/t2/b.md"), None, ACTOR)
        .unwrap();
    ts.store.delete(&out, &id(&c), None, ACTOR).unwrap();
    ts.store
        .create(&out, "/global/new.md", &text("new", "added"), ACTOR)
        .unwrap();
    // A path used in both stores is not a conflict; the stores are separate.
    ts.store
        .create(&out, "/global/live-twin.md", &text("live-twin", "x"), ACTOR)
        .unwrap();
    let created = &ts.store.versions(&out, Some(&id(&a)), None).unwrap();
    let old_version = str_of(&data(created)[1], "id").to_owned();
    ts.store.redact(&out, &old_version, "token:review").unwrap();
    assert_eq!(
        live_snapshot(&ts),
        before,
        "edits to the output store reach the live store"
    );
    assert_eq!(
        str_of(&get(&ts, &id(&a)), "content"),
        seeded("/global/a.md", "alpha")
    );
    assert!(ts.store.version(&live, &old_version).is_err());

    // Cancelling is no different.
    ts.store
        .dream_cancel(str_of(&dream, "id"), "token:review")
        .unwrap();
    assert_eq!(live_snapshot(&ts), before);
}

// ---- 12-14. adopt --------------------------------------------------------

#[test]
fn adopt_merges() {
    let mut ts = TempStore::new();
    let live = ts.live();
    let a = seed(&mut ts.store, "/global/a.md", "alpha");
    let b = seed(&mut ts.store, "/topics/t/b.md", "beta");
    let c = seed(&mut ts.store, "/topics/t/c.md", "gamma");
    let d = seed(&mut ts.store, "/global/d.md", "delta");
    let revision = status_u64(&ts, "revision");

    let dream = start(&mut ts, json!([]));
    let did = id(&dream);
    let out = str_of(&dream, "output_store_id").to_owned();
    let new_a = seeded("/global/a.md", "alpha edited by the dream");
    ts.store
        .update(
            &out,
            &id(&a),
            Some(&new_a),
            None,
            Some(str_of(&a, "content_sha256")),
            "token:review",
        )
        .unwrap();
    ts.store
        .update(
            &out,
            &id(&b),
            None,
            Some("/topics/t2/b.md"),
            None,
            "token:review",
        )
        .unwrap();
    ts.store
        .delete(&out, &id(&c), None, "token:review")
        .unwrap();
    let e = ts
        .store
        .create(&out, "/global/e.md", &text("e", "epsilon"), "token:review")
        .unwrap();

    // The diff is against the frozen base.
    let diff = ts.store.dream(&did, true).unwrap()["diff"].clone();
    let added = diff["added"].as_array().unwrap();
    assert_eq!(
        (
            added.len(),
            str_of(&added[0], "path"),
            str_of(&added[0], "memory_id")
        ),
        (1, "/global/e.md", id(&e).as_str())
    );
    let removed = diff["removed"].as_array().unwrap();
    assert_eq!(
        (removed.len(), str_of(&removed[0], "path")),
        (1, "/topics/t/c.md")
    );
    let changed = diff["changed"].as_array().unwrap();
    assert_eq!(changed.len(), 2);
    let a_change = changed.iter().find(|c| c["memory_id"] == a["id"]).unwrap();
    assert_eq!(str_of(&a_change["before"], "path"), "/global/a.md");
    assert_eq!(str_of(&a_change["after"], "content"), new_a);
    let b_change = changed.iter().find(|c| c["memory_id"] == b["id"]).unwrap();
    assert_eq!(str_of(&b_change["before"], "path"), "/topics/t/b.md");
    assert_eq!(str_of(&b_change["after"], "path"), "/topics/t2/b.md");
    assert!(ts.store.dream(&did, false).unwrap().get("diff").is_none());

    let done = ts
        .store
        .dream_adopt(&did, &review("Merged the test dream"), "token:admin")
        .unwrap();
    assert_eq!(str_of(&done, "status"), "completed");
    assert!(done["ended_at"].is_string());
    assert_eq!(
        str_of(&done["adoption"], "summary"),
        "Merged the test dream"
    );
    assert_eq!(str_of(&done["adoption"], "coverage"), "fixtures only");
    assert_eq!(done["adoption"]["added"], json!(["/global/e.md"]));
    assert_eq!(done["adoption"]["removed"], json!(["/topics/t/c.md"]));
    assert_eq!(done["adoption"]["changed"].as_array().unwrap().len(), 2);

    // The live store has the merge, and untouched memories got no version.
    assert_eq!(
        listed_paths(&live_list(&ts)),
        [
            "/global/a.md",
            "/global/d.md",
            "/global/e.md",
            "/topics/t2/b.md"
        ]
    );
    assert_eq!(str_of(&get(&ts, &id(&a)), "content"), new_a);
    let moved = get(&ts, &id(&b));
    assert_eq!(str_of(&moved, "path"), "/topics/t2/b.md");
    assert_eq!(str_of(&moved, "content"), seeded("/topics/t/b.md", "beta"));
    not_found(ts.store.get(&live, &id(&c)));
    assert_eq!(
        str_of(&get(&ts, &id(&d)), "memory_version_id"),
        str_of(&d, "memory_version_id")
    );
    assert_eq!(
        str_of(&find_by_path(&live_list(&ts), "/global/e.md"), "path"),
        "/global/e.md"
    );
    let by_dream = data(
        &ts.store
            .versions(&live, None, Some(&format!("dream:{did}")))
            .unwrap(),
    )
    .clone();
    let mut operations: Vec<&str> = by_dream.iter().map(|v| str_of(v, "operation")).collect();
    operations.sort();
    assert_eq!(operations, ["created", "deleted", "modified", "modified"]);
    // The revision is max(seq) of the live versions; seq is shared with the output
    // store, so the four applied versions raise it by at least four.
    assert!(status_u64(&ts, "revision") >= revision + 4);
    assert_eq!(status_u64(&ts, "dreams_running"), 0);
    assert_mirror(&ts);

    // The output store is archived: every write to it is refused.
    assert!(
        conflict(
            ts.store
                .create(&out, "/global/late.md", &text("late", "x"), ACTOR),
            "store_archived"
        )
        .is_empty()
    );
    conflict(
        ts.store
            .update(&out, &id(&a), Some(&new_a), None, None, ACTOR),
        "store_archived",
    );
    conflict(
        ts.store.delete(&out, &id(&d), None, ACTOR),
        "store_archived",
    );
    // Adopting with an empty diff is valid and writes nothing.
    let empty = start(&mut ts, json!([]));
    let revision = status_u64(&ts, "revision");
    let done = adopt(&mut ts, &id(&empty)).unwrap();
    assert_eq!(str_of(&done, "status"), "completed");
    assert_eq!(status_u64(&ts, "revision"), revision);
}

#[test]
fn adopt_conflict_per_memory() {
    // Both change X: conflict on X, nothing applied, the dream stays running.
    let (mut ts, live, x, y) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    set_content(&mut ts, &out, &x, "x by the dream", "dream:agent");
    set_content(&mut ts, &out, &y, "y by the dream", "dream:agent");
    set_content(&mut ts, &live, &x, "x by live", ACTOR);
    let revision = status_u64(&ts, "revision");
    assert_eq!(
        conflict(adopt(&mut ts, &did), "adopt_conflict"),
        ["/global/x.md"]
    );
    assert_eq!(status_u64(&ts, "revision"), revision);
    assert_eq!(
        str_of(&get(&ts, &id(&x)), "content"),
        seeded("/global/x.md", "x by live")
    );
    assert_eq!(
        str_of(&get(&ts, &id(&y)), "content"),
        seeded("/global/y.md", "y original"),
        "Y must not be half-applied"
    );
    assert_eq!(dream_status(&ts, &did), "running");
    assert_mirror(&ts);

    // Live changes X, the dream changes Y: merges, and live X is kept.
    let (mut ts, live, x, y) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    set_content(&mut ts, &out, &y, "y by the dream", "dream:agent");
    set_content(&mut ts, &live, &x, "x by live", ACTOR);
    adopt(&mut ts, &did).unwrap();
    assert_eq!(
        str_of(&get(&ts, &id(&x)), "content"),
        seeded("/global/x.md", "x by live")
    );
    assert_eq!(
        str_of(&get(&ts, &id(&y)), "content"),
        seeded("/global/y.md", "y by the dream")
    );
    assert_eq!(dream_status(&ts, &did), "completed");

    // Both add the same path.
    let (mut ts, live, _, _) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    ts.store
        .create(&out, "/global/new.md", &text("new", "dream"), ACTOR)
        .unwrap();
    ts.store
        .create(&live, "/global/new.md", &text("new", "live"), ACTOR)
        .unwrap();
    assert_eq!(
        conflict(adopt(&mut ts, &did), "adopt_conflict"),
        ["/global/new.md"]
    );
    assert_eq!(dream_status(&ts, &did), "running");

    // Added slugs must be free too, whatever the directory.
    let (mut ts, live, _, _) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    ts.store
        .create(&out, "/topics/z/dup.md", &text("dup", "dream"), ACTOR)
        .unwrap();
    ts.store
        .create(&live, "/global/dup.md", &text("dup", "live"), ACTOR)
        .unwrap();
    conflict(adopt(&mut ts, &did), "adopt_conflict");
    assert_eq!(status_u64(&ts, "memories"), 3);

    // Live deleted X, the dream changed X.
    let (mut ts, live, x, _) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    set_content(&mut ts, &out, &x, "x by the dream", "dream:agent");
    ts.store.delete(&live, &id(&x), None, ACTOR).unwrap();
    assert_eq!(
        conflict(adopt(&mut ts, &did), "adopt_conflict"),
        ["/global/x.md"]
    );

    // Live changed X, the dream deleted X.
    let (mut ts, live, x, _) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    ts.store.delete(&out, &id(&x), None, "dream:agent").unwrap();
    set_content(&mut ts, &live, &x, "x by live", ACTOR);
    assert_eq!(
        conflict(adopt(&mut ts, &did), "adopt_conflict"),
        ["/global/x.md"]
    );
    assert_eq!(
        str_of(&get(&ts, &id(&x)), "content"),
        seeded("/global/x.md", "x by live")
    );

    // Live renamed X (its path changed), the dream changed X's content.
    let (mut ts, live, x, _) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    set_content(&mut ts, &out, &x, "x by the dream", "dream:agent");
    ts.store
        .update(&live, &id(&x), None, Some("/topics/q/x.md"), None, ACTOR)
        .unwrap();
    conflict(adopt(&mut ts, &did), "adopt_conflict");
    assert_eq!(str_of(&get(&ts, &id(&x)), "path"), "/topics/q/x.md");
}

#[test]
fn adopt_gates_and_observations() {
    let (mut ts, _live, x, _y) = xy();
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    set_content(&mut ts, &out, &x, "x by the dream", "dream:agent");
    let revision = status_u64(&ts, "revision");

    // The review gates: reviewed, summary, coverage, resolved conflicts.
    let good = review("A summary");
    let broken = [
        json!({"summary": "s", "coverage": "c", "conflicts": []}),
        json!({"summary": "s", "coverage": "c", "conflicts": [], "reviewed": false}),
        json!({"coverage": "c", "conflicts": [], "reviewed": true}),
        json!({"summary": "  ", "coverage": "c", "conflicts": [], "reviewed": true}),
        json!({"summary": "s", "conflicts": [], "reviewed": true}),
        json!({"summary": "s", "coverage": "c", "reviewed": true}),
        json!({"summary": "s", "coverage": "c", "conflicts": "none", "reviewed": true}),
        json!({"summary": "s", "coverage": "c", "conflicts": [{"status": "open"}], "reviewed": true}),
        json!({"summary": "s", "coverage": "c", "conflicts": [{"status": "resolved"}, {"status": "open"}], "reviewed": true}),
    ];
    for r in &broken {
        invalid(ts.store.dream_adopt(&did, r, "token:admin"));
        assert_eq!(dream_status(&ts, &did), "running", "{r}");
        assert_eq!(status_u64(&ts, "revision"), revision, "{r}");
    }
    let mut resolved = good.clone();
    resolved["conflicts"] = json!([{"status": "resolved", "note": "kept the dream's wording"}]);
    ts.store
        .dream_adopt(&did, &resolved, "token:admin")
        .unwrap();
    // The dream must be running.
    invalid(ts.store.dream_adopt(&did, &good, "token:admin"));

    // Observations: two dreams over the same pending observation; only the first
    // adopted one may consume it.
    let (mut ts, _live, _x, _y) = xy();
    let o = ts
        .store
        .observe("/global/obs.md", &text("obs", "lesson"), ACTOR)
        .unwrap();
    let first = start(&mut ts, json!([]));
    let second = start(&mut ts, json!([]));
    for d in [&first, &second] {
        assert!(d["observations"].to_string().contains(str_of(&o, "id")));
    }
    assert_eq!(status_u64(&ts, "observations_pending"), 1);
    adopt(&mut ts, &id(&first)).unwrap();
    assert_eq!(status_u64(&ts, "observations_pending"), 0);
    let revision = status_u64(&ts, "revision");
    conflict(adopt(&mut ts, &id(&second)), "observations_consumed");
    assert_eq!(dream_status(&ts, &id(&second)), "running");
    assert_eq!(status_u64(&ts, "revision"), revision);
    // A dream started after the adoption no longer carries it.
    let third = start(&mut ts, json!([]));
    assert!(third["observations"].as_array().unwrap().is_empty());
    // A cancelled dream consumes nothing.
    let o2 = ts
        .store
        .observe("/global/obs2.md", &text("obs2", "lesson"), ACTOR)
        .unwrap();
    let doomed = start(&mut ts, json!([]));
    ts.store.dream_cancel(&id(&doomed), "token:review").unwrap();
    let fourth = start(&mut ts, json!([]));
    assert!(
        fourth["observations"]
            .to_string()
            .contains(str_of(&o2, "id"))
    );
    assert_eq!(status_u64(&ts, "observations_pending"), 1);
    adopt(&mut ts, &id(&fourth)).unwrap();

    // The pinned budget is checked again at adopt: each side fits alone, not together.
    let mut ts = TempStore::new();
    let live = ts.live();
    seed(&mut ts.store, "/global/base.md", "base");
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    let pin = |title: &str, n: usize| content(title, &"p".repeat(n), &pinned());
    ts.store
        .create(
            &out,
            "/global/dream-pin.md",
            &pin("dream-pin", 2500),
            "dream:agent",
        )
        .unwrap();
    ts.store
        .create(&live, "/global/live-pin.md", &pin("live-pin", 2500), ACTOR)
        .unwrap();
    let revision = status_u64(&ts, "revision");
    conflict(adopt(&mut ts, &did), "pinned_budget_exceeded");
    assert_eq!(status_u64(&ts, "revision"), revision);
    assert_eq!(status_u64(&ts, "pinned_chars"), 2500);
    assert_eq!(dream_status(&ts, &did), "running");
}

// ---- 15. cancel ----------------------------------------------------------

#[test]
fn cancel_semantics() {
    let (mut ts, live, x, _y) = xy();
    ts.store
        .observe("/global/obs.md", &text("obs", "lesson"), ACTOR)
        .unwrap();
    let before = live_snapshot(&ts);
    let dream = start(&mut ts, json!([]));
    let (did, out) = (id(&dream), str_of(&dream, "output_store_id").to_owned());
    set_content(&mut ts, &out, &x, "x by the dream", "dream:agent");

    let canceled = ts.store.dream_cancel(&did, "token:review").unwrap();
    assert_eq!(str_of(&canceled, "status"), "canceled");
    assert!(canceled["ended_at"].is_string());
    assert_eq!(status_u64(&ts, "dreams_running"), 0);
    assert_eq!(live_snapshot(&ts), before);
    // The output store is archived.
    assert!(
        conflict(
            ts.store
                .create(&out, "/global/late.md", &text("late", "x"), ACTOR),
            "store_archived"
        )
        .is_empty()
    );
    conflict(
        ts.store.delete(&out, &id(&x), None, ACTOR),
        "store_archived",
    );
    // Cancelling again is a no-op.
    let again = ts.store.dream_cancel(&did, "token:review").unwrap();
    assert_eq!(str_of(&again, "status"), "canceled");
    assert_eq!(again["ended_at"], canceled["ended_at"]);
    // A canceled dream cannot be adopted; its observations are still pending.
    invalid(adopt(&mut ts, &did));
    assert_eq!(dream_status(&ts, &did), "canceled");
    assert_eq!(status_u64(&ts, "observations_pending"), 1);

    // Cancelling a completed dream is a 400; unknown ids are 404.
    let completed = start(&mut ts, json!([]));
    adopt(&mut ts, &id(&completed)).unwrap();
    invalid(ts.store.dream_cancel(&id(&completed), "token:review"));
    assert_eq!(dream_status(&ts, &id(&completed)), "completed");
    not_found(
        ts.store
            .dream_cancel("drm_00000000000000000000000000000000", "token:review"),
    );
    not_found(
        ts.store
            .dream("drm_00000000000000000000000000000000", false),
    );
    not_found(adopt(&mut ts, "drm_00000000000000000000000000000000"));
    // The live store never saw the dream's edit.
    assert_eq!(
        str_of(&ts.store.get(&live, &id(&x)).unwrap(), "content"),
        seeded("/global/x.md", "x original")
    );
}

// ---- 16-18. mirror -------------------------------------------------------

#[test]
fn mirror_tracks_live() {
    let mut ts = TempStore::new();
    let live = ts.live();
    assert_mirror(&ts); // revision 0

    let a = seed(&mut ts.store, "/global/a.md", "alpha");
    assert_mirror(&ts);
    let b = seed(&mut ts.store, "/topics/hardware/b.md", "မြန်မာ beta\n");
    assert_mirror(&ts);
    seed_meta(
        &mut ts.store,
        "/projects/home/sai/x/p.md",
        "pinned",
        &pinned(),
    );
    assert_mirror(&ts);

    let a2 = set_content(&mut ts, &live, &a, "alpha two", ACTOR);
    assert_mirror(&ts);
    ts.store
        .update(
            &live,
            &id(&b),
            None,
            Some("/topics/software/b.md"),
            None,
            ACTOR,
        )
        .unwrap();
    assert_mirror(&ts);
    assert!(
        !ts.link().join("topics/hardware").exists(),
        "empty directories are not kept"
    );
    ts.store
        .delete(&live, &id(&a), Some(str_of(&a2, "content_sha256")), ACTOR)
        .unwrap();
    assert_mirror(&ts);
    ts.store
        .redact(&live, str_of(&a, "memory_version_id"), "token:admin")
        .unwrap();
    assert_mirror(&ts);
    // A rejected write changes nothing.
    conflict(
        ts.store
            .create(&live, "/topics/software/b.md", &text("b", "dup"), ACTOR),
        "path_exists",
    );
    assert_mirror(&ts);

    // Dream: starting and editing the output store never shows in the mirror; adopting does.
    let dream = start(&mut ts, json!([]));
    let out = str_of(&dream, "output_store_id").to_owned();
    assert_mirror(&ts);
    ts.store
        .create(
            &out,
            "/global/from-dream.md",
            &text("from-dream", "new"),
            ACTOR,
        )
        .unwrap();
    assert_mirror(&ts);
    assert!(!ts.link().join("global/from-dream.md").exists());
    adopt(&mut ts, &id(&dream)).unwrap();
    assert_mirror(&ts);
    assert_eq!(
        fs::read(ts.link().join("global/from-dream.md")).unwrap(),
        text("from-dream", "new").as_bytes()
    );

    // A reopened store sees the same mirror.
    ts.reopen();
    assert_mirror(&ts);
    assert_eq!(status_u64(&ts, "memories"), 3);
}

/// The generation behind an open directory descriptor, from /proc.
fn generation_of(dir: &fs::File) -> i64 {
    let target = fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd())).unwrap();
    let name = target
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .replace(" (deleted)", "");
    name.parse()
        .unwrap_or_else(|_| panic!("unexpected generation directory {target:?}"))
}

#[test]
fn mirror_swap_atomic() {
    // Linux only: the reader pins a generation with an open directory descriptor, so
    // the marker and the memory file it reads come from the same generation.
    let mut ts = TempStore::new();
    let live = ts.live();
    let k = seed(&mut ts.store, "/global/k.md", "tick-0");
    seed(&mut ts.store, "/global/c.md", "constant");
    let base = status_u64(&ts, "revision") as i64;
    let link = ts.link();
    let done = AtomicBool::new(false);
    let expected = |tick: i64| seeded("/global/k.md", &format!("tick-{tick}"));

    let reads = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let (mut reads, mut last) = (0u32, 0i64);
            while !done.load(Ordering::Acquire) || reads == 0 {
                let dir = fs::File::open(&link).expect("the symlink must always resolve");
                let generation = generation_of(&dir);
                let at = |name: &str| format!("/proc/self/fd/{}/{name}", dir.as_raw_fd());
                match (
                    fs::read(at(".agent-memory-store")),
                    fs::read(at("global/k.md")),
                    fs::read(at("global/c.md")),
                    fs::read(at("INDEX.md")),
                ) {
                    (Ok(marker), Ok(k), Ok(_), Ok(_)) => {
                        let marker: Value = serde_json::from_slice(&marker).unwrap();
                        assert_eq!(
                            marker["revision"].as_i64(),
                            Some(generation),
                            "marker and generation disagree"
                        );
                        assert!(
                            generation >= last,
                            "revision went backwards: {last} then {generation}"
                        );
                        assert_eq!(
                            String::from_utf8(k).unwrap(),
                            expected(generation - base),
                            "torn generation {generation}"
                        );
                        last = generation;
                        reads += 1;
                    }
                    // Only a generation older than the previous one may be retired.
                    _ => {
                        let now: i64 = fs::read_link(&link)
                            .unwrap()
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .parse()
                            .unwrap();
                        assert!(
                            now >= generation + 2,
                            "generation {generation} was incomplete while still current ({now})"
                        );
                    }
                }
            }
            reads
        });
        let mut current = k;
        for tick in 1..=200 {
            current = set_content(&mut ts, &live, &current, &format!("tick-{tick}"), ACTOR);
        }
        done.store(true, Ordering::Release);
        reader.join().unwrap()
    });
    assert!(reads > 0, "the reader never read a complete generation");
    assert_eq!(status_u64(&ts, "revision") as i64, base + 200);
    assert_mirror(&ts);
}

#[test]
fn mirror_failure_keeps_commit() {
    let mut ts = TempStore::new();
    let live = ts.live();
    seed(&mut ts.store, "/global/a.md", "alpha");
    assert_eq!(ts.store.status().unwrap()["mirror_stale"], false);

    // Make the mirror unwritable: a regular file where the generations directory
    // must be (a permission bit would not stop root).
    let generations = ts.mnt.join(".generations");
    make_writable(&ts.mnt);
    fs::remove_dir_all(&generations).unwrap();
    fs::write(&generations, "in the way").unwrap();

    let created = ts
        .store
        .create(
            &live,
            "/global/b.md",
            &seeded("/global/b.md", "beta"),
            ACTOR,
        )
        .expect("a mirror failure must not fail the write");
    let status = ts.store.status().unwrap();
    assert_eq!(status["mirror_stale"], true);
    assert_eq!(status["memories"], 2);
    assert_eq!(status["revision"], 2);
    assert_eq!(str_of(&get(&ts, &id(&created)), "path"), "/global/b.md");
    // Later writes still commit while the mirror is broken.
    seed(&mut ts.store, "/global/c.md", "gamma");
    assert_eq!(status_u64(&ts, "revision"), 3);

    // Reopening with the obstacle gone rebuilds the mirror.
    fs::remove_file(&generations).unwrap();
    ts.reopen();
    assert_eq!(ts.store.status().unwrap()["mirror_stale"], false);
    assert_eq!(
        listed_paths(&live_list(&ts)),
        ["/global/a.md", "/global/b.md", "/global/c.md"]
    );
    assert_mirror(&ts);
}
