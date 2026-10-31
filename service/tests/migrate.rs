//! Workstream M acceptance tests (design §3.6, §4.3 "M"): `agent-memory-server migrate` on real v1 databases.
//! Black box: the real binary, plus direct SQLite and file inspection. Temporary databases only.
mod legacy;

use legacy::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

const T1: &str = "2026-10-02T10:00:00+00:00";
const T2: &str = "2026-10-03T11:30:00+00:00";
const T3: &str = "2026-10-04T12:45:00+00:00";
const T4: &str = "2026-10-05T13:15:30+00:00";

/// Three-revision history used by several tests; nothing exotic.
fn plain() -> V1 {
    let a = mem("alpha", global(), "Alpha one");
    let b = mem("beta", topic("hardware"), "Beta one");
    let c = mem("gamma", project("/home/sai/Projects/agent-memory"), "Gamma one");
    v1_db(
        &[
            revision(T1, vec![a.clone(), b.clone()]),
            revision(T2, vec![a.clone(), b.clone(), c.clone()]),
            revision(T3, vec![with(&a, "body", json!("Alpha two")), b, c]),
        ],
        &[observation("obs-1", T1)],
        &[],
    )
}

/// Every stored version, rebuilt as a v1 object, equals the object of the revision that created it.
fn assert_round_trips(db: &V1, revisions: &[Revision]) {
    let by_time: BTreeMap<&str, &Revision> = revisions.iter().map(|r| (r.created.as_str(), r)).collect();
    let versions = versions(&db.conn());
    assert!(!versions.is_empty());
    for v in versions.iter().filter(|v| v.operation != "deleted") {
        let (path, content) = (v.path.as_deref().unwrap(), v.content.as_deref().unwrap());
        assert_eq!(v.sha.as_deref(), Some(sha256(content.as_bytes()).as_str()), "sha of {path}");
        let (rebuilt, _) = to_v1(path, content);
        let revision = by_time[v.created_at.as_str()];
        let original = revision.memories.iter().find(|m| m["id"] == rebuilt["id"]).unwrap();
        assert_eq!(encode(&rebuilt), encode(original), "{path} at {}", v.created_at);
        assert_eq!(content, expected_content(original), "{path} stored form");
    }
}

#[test]
fn replays_history() {
    // r1 creates a and b; r2 changes a and rescopes b; r3 deletes a; r4 restores r2 (a v1 rollback).
    let a1 = mem("a", global(), "a one");
    let b1 = mem("b", global(), "b one");
    let a2 = with(&a1, "body", json!("a two"));
    let b2 = with(&b1, "scope", topic("hardware"));
    let revisions = [
        revision(T1, vec![a1.clone(), b1.clone()]),
        revision(T2, vec![a2.clone(), b2.clone()]),
        revision(T3, vec![b2.clone()]),
        revision(T4, vec![a2.clone(), b2.clone()]),
    ];
    let db = v1_db(&revisions, &[], &[]);
    migrate(&db.db, &[]).assert_ok();
    let conn = db.conn();

    let mut lineages: BTreeMap<String, Vec<Version>> = BTreeMap::new();
    for v in versions(&conn) {
        lineages.entry(v.memory_id.clone()).or_default().push(v);
    }
    assert_eq!(lineages.len(), 3, "a (first lineage), b, and a again as a new lineage");
    let summary = |l: &Vec<Version>| -> Vec<(String, String, String, Option<String>)> {
        l.iter().map(|v| (v.operation.clone(), v.created_at.clone(), v.created_by.clone(), v.path.clone())).collect()
    };
    let step = |op: &str, at: &str, n: u32, path: Option<&str>| {
        (op.to_string(), at.to_string(), format!("migration:revision-{n}"), path.map(str::to_string))
    };
    let mut found: Vec<_> = lineages.values().map(summary).collect();
    found.sort();
    let mut expected = vec![
        vec![step("created", T1, 1, Some("/global/a.md")), step("modified", T2, 2, Some("/global/a.md")),
             step("deleted", T3, 3, found_deleted_path(&lineages))],
        vec![step("created", T1, 1, Some("/global/b.md")), step("modified", T2, 2, Some("/topics/hardware/b.md"))],
        vec![step("created", T4, 4, Some("/global/a.md"))],
    ];
    expected.sort();
    assert_eq!(found, expected);

    // Live store: b (renamed, same id as before the rename) and the new a. The deleted lineage has no row.
    let rows = live(&conn);
    assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["/global/a.md", "/topics/hardware/b.md"]);
    let first_a = lineages.values().find(|l| l.len() == 3).unwrap()[0].memory_id.clone();
    let b_lineage = lineages.values().find(|l| l.len() == 2 && l[1].path.as_deref() == Some("/topics/hardware/b.md")).unwrap();
    assert_ne!(rows[0].id, first_a, "a reappearing after deletion starts a new lineage");
    assert_eq!(rows[1].id, b_lineage[0].memory_id);
    assert_eq!(rows[0].content, expected_content(&a2));
    assert_eq!(rows[1].content, expected_content(&b2));
    assert_round_trips(&db, &revisions);

    // Row bookkeeping follows the lineage.
    for row in &rows {
        let own: Vec<_> = versions(&conn).into_iter().filter(|v| v.memory_id == row.id).collect();
        assert_eq!(row.version_id, own.last().unwrap().id);
        assert_eq!(row.created_at, own[0].created_at);
        assert_eq!(row.updated_at, own.last().unwrap().created_at);
        assert_eq!(row.sha, sha256(row.content.as_bytes()));
        assert!(row.id.starts_with("mem_") && row.id.len() == 36, "{}", row.id);
        assert!(row.version_id.starts_with("memver_") && row.version_id.len() == 39);
    }
    // Revision of the live store = max(seq) of its versions; revision 0 (empty) made no version.
    assert_eq!(versions(&conn).len(), 7);
}

/// The deleted version's path is not specified (it may be kept or nulled); take whatever was stored.
fn found_deleted_path(lineages: &BTreeMap<String, Vec<Version>>) -> Option<&str> {
    lineages.values().find(|l| l.len() == 3).and_then(|l| l[2].path.as_deref())
}

#[test]
fn round_trip_exact() {
    let exotic = json_text(
        r#"{
        "id": "exotic",
        "title": "Ünïcödé — မြန်မာစာ 日本語 🧠",
        "body": "Line one\r\n---\nBody with a rule above, trailing spaces   \n\n  indented\n\nမြန်မာ ယူနီကုဒ် ✓\n",
        "scope": {"kind": "global"},
        "confidence": "corroborated",
        "tags": ["ünï", "မြန်မာ", "plain"],
        "evidence": [
            {"source": "codex", "locator": "a.jsonl:12", "note": "n1", "message_sha256": "ab12", "evidence_type": "user_message", "extra": {"nested": [1, 2, {"k": null}]}},
            {"source": "claude", "locator": "b.jsonl:7", "note": "n2", "date": "2026-10-23T12:38:05.624Z"}
        ],
        "expires": null,
        "priority": 7,
        "updated": "2026-10-30",
        "big": 18446744073709551617,
        "neg": -123456789012345678901234567890,
        "f1": 0.1, "f2": 1e+22, "f3": 1.5e-07, "f4": -0.0, "f5": 100.0,
        "empty_list": [], "empty_obj": {}, "flag": true, "nothing": null,
        "ключ": "значение"
    }"#,
    );
    let empty_expires = with(&mem("second", topic("burmese"), "Second"), "expires", json!(""));
    let dated = with(&mem("third", role("coordinator"), "Third"), "expires", json!("2099-12-31"));
    let revisions = [
        revision(T1, vec![exotic.clone(), empty_expires.clone(), dated.clone()]),
        revision(T2, vec![with(&exotic, "body", json!("changed body")), empty_expires, dated]),
    ];
    let db = v1_db(&revisions, &[], &[]);
    migrate(&db.db, &[]).assert_ok();
    assert_round_trips(&db, &revisions);
    // The exact text, not just equal values: integers and floats keep their literal form in the stored bytes.
    let row = live(&db.conn()).into_iter().find(|r| r.path == "/global/exotic.md").unwrap();
    for literal in ["18446744073709551617", "-123456789012345678901234567890", "1e+22", "1.5e-07", "-0.0", "100.0"] {
        assert!(row.content.contains(literal), "{literal} lost in\n{}", row.content);
    }
    assert!(row.content.contains("မြန်မာစာ 日本語 🧠"), "non-ASCII must be stored as UTF-8, not \\u escapes");
}

#[test]
fn legacy_preserved_and_frozen() {
    let o1 = observation("obs-consumed", T1);
    let o2 = observation("obs-pending", T2);
    let a = mem("alpha", global(), "Alpha");
    let db = v1_db(
        &[revision(T1, vec![a.clone()]), revision(T2, vec![with(&a, "body", json!("Alpha 2"))])],
        &[o1.clone(), o2.clone()],
        &[dream("dream-promoted", 1, &[&o1], Some(2)), dream("dream-open", 2, &[&o2], None)],
    );
    let before = legacy_dump(&db.db);
    migrate(&db.db, &[]).assert_ok();
    assert_eq!(legacy_dump(&db.db), before, "legacy rows must be byte-identical");

    let conn = db.conn();
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 2);
    let names = |kind: &str| -> Vec<String> {
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type=?1 AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        stmt.query_map([kind], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    for table in ["stores", "memories", "memory_versions", "dream_jobs", "revisions", "state", "observations", "dreams"] {
        assert!(names("table").contains(&table.to_string()), "missing table {table}");
    }
    for index in ["one_live_store", "versions_by_memory"] {
        assert!(names("index").contains(&index.to_string()), "missing index {index}");
    }
    assert_eq!(names("trigger"), ["legacy_dreams_frozen", "legacy_revisions_frozen", "legacy_state_frozen"]);
    let (stores, live_stores): (i64, i64) = conn
        .query_row("SELECT COUNT(*),SUM(kind='live' AND archived_at IS NULL) FROM stores", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((stores, live_stores), (1, 1));

    let frozen = |sql: &str| conn.execute(sql, []).unwrap_err().to_string();
    for sql in [
        "INSERT INTO revisions VALUES (99,2,'x','x','[]','x')",
        "INSERT INTO dreams VALUES ('new','x',2,'{}',NULL,NULL)",
        "UPDATE state SET value=0 WHERE key='active'",
    ] {
        assert!(frozen(sql).contains("store migrated to v2"), "{sql}");
    }
    // Observations are still written after migration.
    conn.execute("INSERT INTO observations VALUES ('after','x','{}')", []).unwrap();
}

#[test]
fn atomic_on_offender() {
    // Offenders hide in history, not only in the active revision: r1 has a bad topic that r2 deletes.
    let good = mem("good", global(), "fine");
    let bad_topic = mem("bad-topic", topic("machine learning"), "x");
    let bad_role = mem("bad-role", role("qa lead"), "x");
    let bad_project = mem("bad-project", project("/srv/has space/x"), "x");
    let db = v1_db(
        &[revision(T1, vec![good.clone(), bad_topic]), revision(T2, vec![good, bad_role, bad_project])],
        &[observation("obs", T1)],
        &[],
    );
    let before = bytes(&db.db);
    let ran = migrate(&db.db, &[]);
    ran.assert_failed();
    for value in ["machine learning", "qa lead", "/srv/has space/x"] {
        assert!(ran.text().contains(value), "offender {value:?} not listed:\n{}", ran.text());
    }
    assert_eq!(bytes(&db.db), before, "database must be byte-identical after an aborted migration");
    assert!(!db.dir.path().join("memory.sqlite3-journal").exists());
    assert!(!db.mnt().exists(), "no mirror for an aborted migration");
}

#[test]
fn refuses_a_non_linear_revision_chain() {
    let db = plain();
    db.conn().execute("UPDATE revisions SET parent=0 WHERE id=3", []).unwrap();
    let before = bytes(&db.db);
    migrate(&db.db, &[]).assert_failed();
    assert_eq!(bytes(&db.db), before);
    assert!(!db.mnt().exists());
}

#[test]
fn dry_run_changes_nothing() {
    let keep = mem("keep-it-simple", global(), &"မ".repeat(137));
    let db = v1_db(&[revision(T1, vec![keep, mem("other", topic("hardware"), "Other")])], &[], &[]);
    let before = bytes(&db.db);
    let ran = migrate(&db.db, &["--dry-run", "--pin", "keep-it-simple"]);
    ran.assert_ok();
    for needle in ["keep-it-simple", "/global/keep-it-simple.md", "other", "/topics/hardware/other.md"] {
        assert!(ran.text().contains(needle), "report lacks {needle:?}:\n{}", ran.text());
    }
    assert!(ran.text().contains("137"), "report shows the 137 pinned characters:\n{}", ran.text());
    assert_eq!(bytes(&db.db), before);
    assert!(!db.mnt().exists(), "dry run builds no mirror");
    // Still migratable afterwards.
    migrate(&db.db, &["--pin", "keep-it-simple"]).assert_ok();
}

#[test]
fn dry_run_lists_offenders_and_changes_nothing() {
    let db = v1_db(&[revision(T1, vec![mem("x", topic("machine learning"), "x")])], &[], &[]);
    let before = bytes(&db.db);
    let ran = migrate(&db.db, &["--dry-run"]);
    assert!(ran.text().contains("machine learning"), "{}", ran.text());
    assert_eq!(bytes(&db.db), before);
    assert!(!db.mnt().exists());
}

#[test]
fn second_run_refuses() {
    let db = plain();
    migrate(&db.db, &[]).assert_ok();
    let after_first = bytes(&db.db);
    let ran = migrate(&db.db, &[]);
    ran.assert_failed();
    let text = ran.text().to_lowercase();
    assert!(text.contains("v2") || text.contains("already"), "{text}");
    assert_eq!(bytes(&db.db), after_first, "a refused second run changes nothing");
    assert_eq!(versions(&db.conn()).len(), 4, "no versions were replayed twice");
}

fn pinned_state(db: &V1) -> BTreeMap<String, bool> {
    live(&db.conn()).iter().map(|r| (r.path.clone(), to_v1(&r.path, &r.content).1)).collect()
}

#[test]
fn pin_marks_only_the_named_memories() {
    let keep = mem("keep-it-simple", global(), "Keep it simple. မြန်မာ");
    let revisions = [
        revision(T1, vec![keep.clone(), mem("other", global(), "Other"), mem("proj", project("/home/sai/Projects/x"), "P")]),
        revision(T2, vec![with(&keep, "body", json!("Keep it simple v2. မြန်မာ")), mem("other", global(), "Other"),
                          mem("proj", project("/home/sai/Projects/x"), "P")]),
    ];
    let db = v1_db(&revisions, &[], &[]);
    migrate(&db.db, &["--pin", "keep-it-simple", "--pin", "proj"]).assert_ok();
    assert_eq!(
        pinned_state(&db),
        BTreeMap::from([
            ("/global/keep-it-simple.md".to_string(), true),
            ("/global/other.md".to_string(), false),
            ("/projects/home/sai/Projects/x/proj.md".to_string(), true),
        ])
    );
    let conn = db.conn();
    let all = versions(&conn);
    for row in live(&conn) {
        let own: Vec<_> = all.iter().filter(|v| v.memory_id == row.id).collect();
        let pins: Vec<_> = own.iter().filter(|v| v.created_by == "migration:pin").collect();
        if pinned_state(&db)[&row.path] {
            assert_eq!(pins.len(), 1, "{}: one extra version by migration:pin", row.path);
            assert_eq!(pins[0].operation, "modified");
            assert_eq!(own.last().unwrap().id, pins[0].id, "the pin version is the head");
            assert_eq!(row.version_id, pins[0].id);
            assert!(row.content.contains("\"pinned\": true"));
        } else {
            assert!(pins.is_empty(), "{} must not get a pin version", row.path);
        }
    }
    // Pinning only adds the key: stripping it gives back the exact v1 objects.
    for row in live(&conn) {
        let (rebuilt, _) = to_v1(&row.path, &row.content);
        let original = revisions[1].memories.iter().find(|m| m["id"] == rebuilt["id"]).unwrap();
        assert_eq!(encode(&rebuilt), encode(original));
    }
    // The mirror holds the pinned bytes and the start view without a project lists the pinned memory first.
    let file = fs::read_to_string(db.mirror().join("global/keep-it-simple.md")).unwrap();
    assert!(file.contains("\"pinned\": true"));
    let index = fs::read_to_string(db.mirror().join("INDEX.md")).unwrap();
    assert!(index.contains("Title of keep-it-simple") && index.contains("Keep it simple v2."), "{index}");
}

#[test]
fn pin_budget_counts_characters_not_bytes() {
    let burmese = |n: usize| "မ".repeat(n); // 3 bytes per character
    for (second, ok) in [(2000, true), (2001, false)] {
        let db = v1_db(&[revision(T1, vec![mem("one", global(), &burmese(2000)), mem("two", global(), &burmese(second))])], &[], &[]);
        let before = bytes(&db.db);
        let ran = migrate(&db.db, &["--pin", "one", "--pin", "two"]);
        if ok {
            ran.assert_ok();
        } else {
            ran.assert_failed();
            assert_eq!(bytes(&db.db), before, "an over-budget pin set aborts everything");
            assert!(!db.mnt().exists());
        }
    }
}

#[test]
fn pin_aborts_on_unknown_or_unpinnable_slug() {
    let db = v1_db(&[revision(T1, vec![mem("a", global(), "a"), mem("t", topic("hardware"), "t"), mem("r", role("coordinator"), "r")])], &[], &[]);
    let before = bytes(&db.db);
    for slug in ["no-such-memory", "t", "r"] {
        let ran = migrate(&db.db, &["--pin", "a", "--pin", slug]);
        ran.assert_failed();
        assert!(ran.text().contains(slug), "{}", ran.text());
        assert_eq!(bytes(&db.db), before, "--pin {slug}: nothing changes");
    }
}

#[test]
fn mirror_is_built() {
    let db = plain();
    migrate(&db.db, &[]).assert_ok();
    let conn = db.conn();
    let mirror = db.mirror();
    assert!(fs::symlink_metadata(&mirror).unwrap().file_type().is_symlink(), "mirror is a symlink to a generation");
    let marker: Value = serde_json::from_str(&fs::read_to_string(mirror.join(".agent-memory-store")).unwrap()).unwrap();
    let (store_id, revision): (String, i64) = (
        conn.query_row("SELECT id FROM stores WHERE kind='live'", [], |r| r.get(0)).unwrap(),
        conn.query_row("SELECT MAX(seq) FROM memory_versions", [], |r| r.get(0)).unwrap(),
    );
    assert_eq!(marker["store_id"], json!(store_id));
    assert_eq!(marker["revision"], json!(revision));
    let rows = live(&conn);
    assert_eq!(rows.len(), 3);
    for row in rows {
        let file = mirror.join(row.path.trim_start_matches('/'));
        assert_eq!(fs::read_to_string(&file).unwrap(), row.content, "{}", row.path);
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o444, "{}", row.path);
        assert_eq!(fs::metadata(file.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o555);
    }
    assert!(mirror.join("INDEX.md").is_file());
}

#[test]
fn check_refuses_an_unmigrated_database() {
    let db = plain();
    let config = db.dir.path().join("server.toml");
    fs::write(&config, format!(
        "listen = \"127.0.0.1:0\"\ndatabase = {:?}\n[[tokens]]\nname = \"t\"\nsha256 = \"{}\"\ncapabilities = [\"recall\"]\n",
        db.db, sha256(b"t"))).unwrap();
    let ran = server(&["check", "--config", config.to_str().unwrap()]);
    ran.assert_failed();
    assert!(ran.text().contains("migrate"), "message must name `migrate`:\n{}", ran.text());
}
