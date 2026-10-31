mod common;

use agent_memory_server::{
    Error,
    model::{self, hash},
    recall,
    store::Store,
};
use common::{TempStore, content, race, seed};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn kind(e: &Error) -> &str {
    match e {
        Error::Conflict { kind, .. } => kind,
        Error::Invalid(_) => "invalid",
        Error::NotFound => "not_found",
        Error::Internal(_) => "internal",
    }
}
fn conflict_paths(e: &Error) -> Vec<String> {
    match e {
        Error::Conflict { paths, .. } => paths.clone(),
        _ => panic!("not a conflict: {e}"),
    }
}
fn create(t: &mut TempStore, path: &str, body: &str) -> Value {
    t.create("live", path, &content(path, body, json!({})), "token:test")
        .unwrap()
}
fn id(v: &Value) -> &str {
    v["id"].as_str().unwrap()
}
fn revision(t: &TempStore) -> i64 {
    t.status().unwrap()["revision"].as_i64().unwrap()
}
fn version_count(t: &TempStore) -> usize {
    t.sql()
        .query_row("SELECT COUNT(*) FROM memory_versions", [], |r| r.get(0))
        .unwrap()
}
fn marker(t: &TempStore) -> Value {
    serde_json::from_str(&fs::read_to_string(t.mirror().join(".agent-memory-store")).unwrap())
        .unwrap()
}
fn review() -> Value {
    json!({"summary": "Test", "coverage": "Fixtures", "conflicts": [], "reviewed": true})
}

#[test]
fn init_v2() {
    let t = TempStore::new();
    let sql = t.sql();
    let version: i64 = sql
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let live: i64 = sql
        .query_row("SELECT COUNT(*) FROM stores WHERE kind='live'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!((version, live, revision(&t)), (2, 1, 0));
    // Init is idempotent on v2.
    Store::init(&t.db).unwrap();

    let v1 = t.dir.path().join("v1.sqlite3");
    let db = rusqlite::Connection::open(&v1).unwrap();
    db.execute_batch(
        "CREATE TABLE revisions (id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
            reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
         INSERT INTO revisions VALUES (0,NULL,'now','Empty initial store','[]','x');",
    )
    .unwrap();
    let err = Store::open(&v1, &t.mnt).err().unwrap();
    assert!(err.to_string().contains("migrate"), "{err}");
    assert!(
        Store::init(&v1)
            .unwrap_err()
            .to_string()
            .contains("migrate")
    );
    assert!(Store::open(&t.dir.path().join("missing.sqlite3"), &t.mnt).is_err());
}

#[test]
fn path_rules() {
    for (path, kind, value) in [
        ("/global/a.md", model::Kind::Global, ""),
        (
            "/projects/home/sai/x/a-1.md",
            model::Kind::Project,
            "/home/sai/x",
        ),
        ("/topics/hardware/a.md", model::Kind::Topic, "hardware"),
        ("/roles/coordinator/a.md", model::Kind::Role, "coordinator"),
    ] {
        let scope = model::validate_path(path).unwrap();
        assert_eq!(
            (scope.kind, scope.value.as_str(), scope.slug.as_str()),
            (
                kind,
                value,
                path.rsplit('/').next().unwrap().trim_end_matches(".md")
            )
        );
    }
    let deep = format!("/projects/{}/a.md", vec!["d"; 18].join("/"));
    assert!(model::validate_path(&deep).is_ok());
    let too_deep = format!("/projects/{}/a.md", vec!["d"; 19].join("/"));
    let too_long = format!(
        "/projects/{}/a.md",
        "d".repeat(1025 - "/projects//a.md".len())
    );
    assert_eq!(too_long.len(), 1025);
    for bad in [
        "/projects/../a.md",
        "/global//a.md",
        "/projects/home//a.md",
        too_deep.as_str(),
        too_long.as_str(),
        "/topics/မြန်မာ/a.md",
        "/other/a.md",
        "/global/a.txt",
        "/global/A.md",
        "/global/-a.md",
        "global/a.md",
        "/global/x/a.md",
        "/topics/a.md",
        "/projects/a.md",
    ] {
        assert!(model::validate_path(bad).is_err(), "{bad}");
    }
}

#[test]
fn content_rules() {
    let ok = content("T", "Body", json!({}));
    assert!(model::parse("/global/a.md", &ok).is_ok());
    let evidence = json!([{"source": "s", "locator": "l", "note": "n"}]);
    for (path, bad) in [
        ("/global/a.md", "No frontmatter".to_owned()),
        (
            "/global/a.md",
            content("T", "Body", json!({"evidence": []})),
        ),
        (
            "/global/a.md",
            content("T", "Body", json!({"confidence": "sure"})),
        ),
        ("/global/a.md", content("T", "Body", json!({"id": "a"}))),
        (
            "/global/a.md",
            content("T", "Body", json!({"scope": {"kind": "global"}})),
        ),
        ("/global/a.md", content("T", "Body", json!({"body": "b"}))),
        ("/global/a.md", content(&"t".repeat(121), "Body", json!({}))),
        (
            "/global/a.md",
            content("T", &"b".repeat(102_400), json!({})),
        ),
        (
            "/global/a.md",
            content("T", "see ?api_key=abcdef", json!({})),
        ),
        ("/global/a.md", content("T", "  \n", json!({}))),
        (
            "/topics/x/a.md",
            content("T", "Body", json!({"pinned": true})),
        ),
        (
            "/global/a.md",
            content("T", "Body", json!({"evidence": evidence, "tags": "x"})),
        ),
    ] {
        assert!(model::parse(path, &bad).is_err(), "{bad:.200}");
    }
    assert!(
        model::parse(
            "/global/a.md",
            &content(&"t".repeat(120), "Body", json!({}))
        )
        .is_ok()
    );
    assert!(
        model::parse(
            "/projects/p/a.md",
            &content("T", "B", json!({"pinned": true}))
        )
        .is_ok()
    );
}

#[test]
fn sha_is_raw_bytes() {
    let mut t = TempStore::new();
    let raw = "---\n{ \"title\":\"မြန်မာ\" ,\"confidence\" : \"explicit\",\"tags\":[],\n\"evidence\":[{\"source\":\"s\",\"locator\":\"l\",\"note\":\"n\"}]}\n---\nမြန်မာ body  \n";
    let m = t
        .create("live", "/topics/burmese/a.md", raw, "token:test")
        .unwrap();
    assert_eq!(m["content"], raw);
    assert_eq!(m["content_sha256"], hash(raw.as_bytes()));
    assert_eq!(t.get("live", id(&m)).unwrap()["content"], raw);
    assert_eq!(
        fs::read(t.mirror().join("topics/burmese/a.md")).unwrap(),
        raw.as_bytes()
    );
}

#[test]
fn create_conflicts() {
    let mut t = TempStore::new();
    create(&mut t, "/global/a.md", "One");
    let c = |t: &mut TempStore, path: &str| {
        t.create("live", path, &content("T", "B", json!({})), "x")
            .unwrap_err()
    };
    let e = c(&mut t, "/global/a.md");
    assert_eq!(
        (kind(&e), conflict_paths(&e)),
        ("path_exists", vec!["/global/a.md".to_owned()])
    );
    let e = c(&mut t, "/topics/x/a.md");
    assert_eq!(
        (kind(&e), conflict_paths(&e)),
        ("slug_exists", vec!["/global/a.md".to_owned()])
    );
}

#[test]
fn update_precondition() {
    let mut t = TempStore::new();
    let m = create(&mut t, "/global/a.md", "One");
    let before = version_count(&t);
    let e = t
        .update(
            "live",
            id(&m),
            Some(&content("T", "Two", json!({}))),
            None,
            Some(&hash(b"stale")),
            "x",
        )
        .unwrap_err();
    assert_eq!(kind(&e), "precondition_failed");
    assert_eq!(version_count(&t), before);
    let sha = m["content_sha256"].as_str().unwrap();
    let u = t
        .update(
            "live",
            id(&m),
            Some(&content("T", "Two", json!({}))),
            None,
            Some(sha),
            "x",
        )
        .unwrap();
    assert_ne!(u["memory_version_id"], m["memory_version_id"]);
    let r = t
        .update("live", id(&m), None, Some("/topics/y/a.md"), None, "x")
        .unwrap();
    assert_eq!(
        (r["id"].clone(), r["path"].clone()),
        (m["id"].clone(), json!("/topics/y/a.md"))
    );
    let history = t.versions("live", Some(id(&m)), None).unwrap();
    let paths: Vec<_> = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["/topics/y/a.md", "/global/a.md", "/global/a.md"]);
    assert!(history["data"][0].get("content").is_none());
}

#[test]
fn concurrent_writers_one_winner() {
    let mut t = TempStore::new();
    let m = create(&mut t, "/global/a.md", "One");
    let before = version_count(&t);
    let (memory, sha) = (
        id(&m).to_owned(),
        m["content_sha256"].as_str().unwrap().to_owned(),
    );
    let results = race(&t, 8, move |i, s| {
        s.update(
            "live",
            &memory,
            Some(&content("T", &format!("Writer {i}"), json!({}))),
            None,
            Some(&sha),
            "x",
        )
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r.as_ref().is_err_and(|e| kind(e) == "precondition_failed"))
            .count(),
        7
    );
    assert_eq!(version_count(&t), before + 1);
}

#[test]
fn delete_and_redact() {
    let mut t = TempStore::new();
    let m = create(&mut t, "/global/a.md", "One");
    let e = t
        .delete("live", id(&m), Some(&hash(b"stale")), "x")
        .unwrap_err();
    assert_eq!(kind(&e), "precondition_failed");
    let u = t
        .update(
            "live",
            id(&m),
            Some(&content("T", "Two", json!({}))),
            None,
            None,
            "x",
        )
        .unwrap();
    let head = u["memory_version_id"].as_str().unwrap();
    assert_eq!(
        kind(&t.redact("live", head, "token:admin").unwrap_err()),
        "invalid"
    );
    let first = m["memory_version_id"].as_str().unwrap();
    let r = t.redact("live", first, "token:admin").unwrap();
    assert_eq!(
        (&r["content"], &r["path"], &r["content_sha256"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    assert_eq!(
        (r["created_by"].as_str(), r["redacted_by"].as_str()),
        (Some("token:test"), Some("token:admin"))
    );
    assert_eq!(
        t.delete(
            "live",
            id(&m),
            Some(u["content_sha256"].as_str().unwrap()),
            "x"
        )
        .unwrap(),
        json!({"type": "memory_deleted", "id": id(&m)})
    );
    assert!(matches!(t.get("live", id(&m)), Err(Error::NotFound)));
    let history = t.versions("live", Some(id(&m)), None).unwrap();
    let ops: Vec<_> = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["operation"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["deleted", "modified", "created"]);
    assert_eq!(
        (
            &history["data"][0]["path"],
            &history["data"][0]["content_sha256"]
        ),
        (&json!("/global/a.md"), &Value::Null)
    );
    // After deletion the old head is no longer current, so it can be redacted.
    t.redact("live", head, "token:admin").unwrap();
}

#[test]
fn store_full_and_pinned_budget() {
    let mut t = TempStore::new();
    let many: Vec<_> = (0..10_000)
        .map(|i| {
            (
                format!("/topics/bulk/m{i}.md"),
                content("T", "B", json!({})),
            )
        })
        .collect();
    seed(&mut t, &many);
    let e = t
        .create(
            "live",
            "/global/one-more.md",
            &content("T", "B", json!({})),
            "x",
        )
        .unwrap_err();
    assert!(e.to_string().contains("store_full"), "{e}");

    let mut t = TempStore::new();
    let pin = |body: &str| content("T", body, json!({"pinned": true}));
    t.create("live", "/global/a.md", &pin(&"a".repeat(3_000)), "x")
        .unwrap();
    let b = t
        .create("live", "/projects/p/b.md", &pin(&"ဗ".repeat(1_000)), "x")
        .unwrap();
    assert_eq!(t.status().unwrap()["pinned_chars"], 4_000);
    let before = version_count(&t);
    let e = t
        .create("live", "/global/c.md", &pin("x"), "x")
        .unwrap_err();
    assert_eq!(kind(&e), "pinned_budget_exceeded");
    let e = t
        .update(
            "live",
            id(&b),
            Some(&pin(&"b".repeat(1_001))),
            None,
            None,
            "x",
        )
        .unwrap_err();
    assert_eq!(kind(&e), "pinned_budget_exceeded");
    assert_eq!(version_count(&t), before);
    assert!(
        t.list("live", "/global/c").unwrap()["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dream_start_freezes_and_clones() {
    let mut t = TempStore::new();
    create(&mut t, "/global/a.md", "One");
    create(&mut t, "/topics/x/b.md", "Two");
    let obs = t
        .observe(
            "/global/c.md",
            &content("T", "Observed", json!({})),
            "token:obs",
        )
        .unwrap();
    let rev = revision(&t);
    let live = t.list("live", "/").unwrap();
    let d = t
        .dream_start(
            &json!([{"source": "claude", "locator": "s1", "sha256": "ab"}]),
            "token:r",
        )
        .unwrap();
    assert_eq!(d["status"], "running");
    assert_eq!(d["observations"][0]["id"], obs["id"]);
    assert_eq!(d["sessions"][0]["locator"], "s1");
    let out = d["output_store_id"].as_str().unwrap();
    let copy = t.list(out, "/").unwrap();
    let key = |v: &Value| {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["id"].clone(),
                    m["path"].clone(),
                    m["content_sha256"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&copy), key(&live));
    assert_eq!((revision(&t), t.list("live", "/").unwrap()), (rev, live));
    assert!(!t.is_live(out).unwrap());
    let dream_versions = t
        .versions(
            out,
            None,
            Some(&format!("dream:{}", d["id"].as_str().unwrap())),
        )
        .unwrap();
    assert_eq!(dream_versions["data"].as_array().unwrap().len(), 2);

    let sessions: Vec<_> = (0..101)
        .map(|i| json!({"source": "s", "locator": format!("l{i}")}))
        .collect();
    assert!(t.dream_start(&json!(sessions), "x").is_err());
    assert!(
        t.dream_start(
            &json!([{"source": "s", "locator": "l"}, {"source": "t", "locator": "l"}]),
            "x"
        )
        .is_err()
    );
    assert!(t.dream_start(&json!(sessions[..100]), "x").is_ok());
}

#[test]
fn dream_never_modifies_input() {
    let mut t = TempStore::new();
    let a = create(&mut t, "/global/a.md", "One");
    let live = t.list("live", "/").unwrap();
    let rev = revision(&t);
    let d = t.dream_start(&json!([]), "x").unwrap();
    let out = d["output_store_id"].as_str().unwrap().to_owned();
    t.update(
        &out,
        id(&a),
        Some(&content("T", "Changed", json!({}))),
        None,
        None,
        "x",
    )
    .unwrap();
    t.create(&out, "/global/new.md", &content("T", "New", json!({})), "x")
        .unwrap();
    t.delete(&out, id(&a), None, "x").unwrap();
    t.dream_cancel(d["id"].as_str().unwrap(), "x").unwrap();
    assert_eq!((t.list("live", "/").unwrap(), revision(&t)), (live, rev));
    assert_eq!(marker(&t)["revision"], rev);
}

#[test]
fn adopt_merges() {
    let mut t = TempStore::new();
    let keep = create(&mut t, "/global/keep.md", "Keep");
    let change = create(&mut t, "/global/change.md", "Change");
    let gone = create(&mut t, "/global/gone.md", "Gone");
    let d = t.dream_start(&json!([]), "x").unwrap();
    let (did, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    t.update(
        &out,
        id(&change),
        Some(&content("T", "Changed", json!({}))),
        Some("/topics/t/change.md"),
        None,
        "x",
    )
    .unwrap();
    t.delete(&out, id(&gone), None, "x").unwrap();
    let added = t
        .create(
            &out,
            "/global/added.md",
            &content("T", "Added", json!({})),
            "x",
        )
        .unwrap();
    let diff = t.dream(&did, true).unwrap()["diff"].clone();
    assert_eq!(diff["added"][0]["memory_id"], added["id"]);
    assert_eq!(diff["removed"][0]["path"], "/global/gone.md");
    assert_eq!(diff["changed"][0]["before"]["path"], "/global/change.md");
    assert_eq!(diff["changed"][0]["after"]["path"], "/topics/t/change.md");

    let done = t.dream_adopt(&did, &review(), "token:admin").unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["adoption"]["removed"], json!(["/global/gone.md"]));
    let paths: Vec<_> = t.list("live", "/").unwrap()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        paths,
        ["/global/added.md", "/global/keep.md", "/topics/t/change.md"]
    );
    assert_eq!(
        t.get("live", id(&change)).unwrap()["content"],
        content("T", "Changed", json!({}))
    );
    assert_eq!(
        t.get("live", id(&keep)).unwrap()["memory_version_id"],
        keep["memory_version_id"]
    );
    let by_dream = t
        .versions("live", None, Some(&format!("dream:{did}")))
        .unwrap();
    assert_eq!(by_dream["data"].as_array().unwrap().len(), 3);
    let e = t
        .create(
            &out,
            "/global/late.md",
            &content("T", "Late", json!({})),
            "x",
        )
        .unwrap_err();
    assert_eq!(kind(&e), "store_archived");
    assert_eq!(
        kind(&t.dream_adopt(&did, &review(), "x").unwrap_err()),
        "invalid"
    );
    assert!(fs::read(t.mirror().join("global/added.md")).is_ok());
}

#[test]
fn adopt_conflict_per_memory() {
    let mut t = TempStore::new();
    let x = create(&mut t, "/global/x.md", "X");
    let y = create(&mut t, "/global/y.md", "Y");
    let d = t.dream_start(&json!([]), "x").unwrap();
    let (did, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    t.update(
        &out,
        id(&x),
        Some(&content("T", "Dream X", json!({}))),
        None,
        None,
        "x",
    )
    .unwrap();
    t.update(
        "live",
        id(&x),
        Some(&content("T", "Live X", json!({}))),
        None,
        None,
        "x",
    )
    .unwrap();
    let live = t.list("live", "/").unwrap();
    let e = t.dream_adopt(&did, &review(), "x").unwrap_err();
    assert_eq!(
        (kind(&e), conflict_paths(&e)),
        ("adopt_conflict", vec!["/global/x.md".to_owned()])
    );
    assert_eq!(t.list("live", "/").unwrap(), live);
    assert_eq!(t.dream(&did, false).unwrap()["status"], "running");

    // Live changes X, the dream changes Y: both survive.
    let d = t.dream_start(&json!([]), "x").unwrap();
    let (did, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    t.update(
        &out,
        id(&y),
        Some(&content("T", "Dream Y", json!({}))),
        None,
        None,
        "x",
    )
    .unwrap();
    t.update(
        "live",
        id(&x),
        Some(&content("T", "Live X2", json!({}))),
        None,
        None,
        "x",
    )
    .unwrap();
    t.dream_adopt(&did, &review(), "x").unwrap();
    assert_eq!(
        t.get("live", id(&x)).unwrap()["content"],
        content("T", "Live X2", json!({}))
    );
    assert_eq!(
        t.get("live", id(&y)).unwrap()["content"],
        content("T", "Dream Y", json!({}))
    );

    // Both add the same path.
    let d = t.dream_start(&json!([]), "x").unwrap();
    let (did, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    t.create(
        &out,
        "/global/z.md",
        &content("T", "Dream Z", json!({})),
        "x",
    )
    .unwrap();
    create(&mut t, "/global/z.md", "Live Z");
    let e = t.dream_adopt(&did, &review(), "x").unwrap_err();
    assert_eq!(
        (kind(&e), conflict_paths(&e)),
        ("adopt_conflict", vec!["/global/z.md".to_owned()])
    );
}

#[test]
fn adopt_swaps_paths() {
    let mut t = TempStore::new();
    let a = create(&mut t, "/global/a.md", "A");
    let b = create(&mut t, "/topics/x/b.md", "B");
    let d = t.dream_start(&json!([]), "x").unwrap();
    let (did, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    t.update(&out, id(&a), None, Some("/topics/x/a.md"), None, "x")
        .unwrap();
    t.update(&out, id(&b), None, Some("/global/b.md"), None, "x")
        .unwrap();
    t.dream_adopt(&did, &review(), "x").unwrap();
    assert_eq!(t.get("live", id(&a)).unwrap()["path"], "/topics/x/a.md");
    assert_eq!(t.get("live", id(&b)).unwrap()["path"], "/global/b.md");
}

#[test]
fn adopt_gates_and_observations() {
    let mut t = TempStore::new();
    t.observe("/global/o.md", &content("T", "Obs", json!({})), "x")
        .unwrap();
    let first = t.dream_start(&json!([]), "x").unwrap();
    let second = t.dream_start(&json!([]), "x").unwrap();
    let id1 = first["id"].as_str().unwrap();
    for gate in [
        json!({"reviewed": false}),
        json!({"summary": ""}),
        json!({"coverage": ""}),
        json!({"conflicts": [{"status": "open"}]}),
    ] {
        let mut r = review();
        for (k, v) in gate.as_object().unwrap() {
            r[k] = v.clone();
        }
        assert_eq!(
            kind(&t.dream_adopt(id1, &r, "x").unwrap_err()),
            "invalid",
            "{gate}"
        );
    }
    t.dream_adopt(id1, &review(), "x").unwrap();
    assert_eq!(t.status().unwrap()["observations_pending"], 0);
    let e = t
        .dream_adopt(second["id"].as_str().unwrap(), &review(), "x")
        .unwrap_err();
    assert_eq!(kind(&e), "observations_consumed");

    let pin = |body: &str| content("T", body, json!({"pinned": true}));
    t.create("live", "/global/p.md", &pin(&"p".repeat(3_000)), "x")
        .unwrap();
    let d = t.dream_start(&json!([]), "x").unwrap();
    let out = d["output_store_id"].as_str().unwrap();
    t.create(out, "/global/q.md", &pin(&"q".repeat(1_000)), "x")
        .unwrap();
    t.create("live", "/projects/p/r.md", &pin("r"), "x")
        .unwrap();
    let e = t
        .dream_adopt(d["id"].as_str().unwrap(), &review(), "x")
        .unwrap_err();
    assert_eq!(kind(&e), "pinned_budget_exceeded");
    assert!(
        t.list("live", "/global/q").unwrap()["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancel_semantics() {
    let mut t = TempStore::new();
    let d = t.dream_start(&json!([]), "x").unwrap();
    let did = d["id"].as_str().unwrap();
    let c = t.dream_cancel(did, "x").unwrap();
    assert_eq!(c["status"], "canceled");
    assert!(c["ended_at"].is_string());
    assert_eq!(t.dream_cancel(did, "x").unwrap(), c);
    assert_eq!(
        kind(&t.dream_adopt(did, &review(), "x").unwrap_err()),
        "invalid"
    );
    let e = t
        .create(
            d["output_store_id"].as_str().unwrap(),
            "/global/a.md",
            &content("T", "B", json!({})),
            "x",
        )
        .unwrap_err();
    assert_eq!(kind(&e), "store_archived");
    let d = t.dream_start(&json!([]), "x").unwrap();
    t.dream_adopt(d["id"].as_str().unwrap(), &review(), "x")
        .unwrap();
    assert_eq!(
        kind(&t.dream_cancel(d["id"].as_str().unwrap(), "x").unwrap_err()),
        "invalid"
    );
    assert!(matches!(
        t.dream_cancel("drm_missing", "x"),
        Err(Error::NotFound)
    ));
    let list = t.dreams().unwrap();
    assert_eq!(list["data"][0]["id"], d["id"]);
}

fn assert_mirror(t: &TempStore) {
    let mirror = t.mirror();
    assert_eq!(
        marker(t),
        json!({"store_id": t.live_id(), "revision": revision(t)})
    );
    let (rev, memories) = t.live_memories().unwrap();
    let index = recall::index_md(&memories, rev, &mirror.display().to_string());
    assert_eq!(fs::read_to_string(mirror.join("INDEX.md")).unwrap(), index);
    let mut files = 0;
    fn walk(dir: &Path, files: &mut usize) {
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o555,
            "{dir:?}"
        );
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, files);
            } else {
                assert_eq!(
                    fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                    0o444,
                    "{p:?}"
                );
                *files += 1;
            }
        }
    }
    walk(&mirror, &mut files);
    assert_eq!(files, memories.len() + 2);
    for m in t.list("live", "/").unwrap()["data"].as_array().unwrap() {
        let stored = t.get("live", id(m)).unwrap();
        let file = mirror.join(m["path"].as_str().unwrap().trim_start_matches('/'));
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            stored["content"].as_str().unwrap()
        );
    }
}

#[test]
fn mirror_tracks_live() {
    let mut t = TempStore::new();
    assert_mirror(&t);
    let a = create(&mut t, "/projects/home/u/p/a.md", "A");
    assert_mirror(&t);
    let a = t
        .update(
            "live",
            id(&a),
            Some(&content("T", "A2", json!({}))),
            Some("/global/a.md"),
            None,
            "x",
        )
        .unwrap();
    assert_mirror(&t);
    let old = t.versions("live", Some(id(&a)), None).unwrap()["data"][1]["id"].clone();
    t.redact("live", old.as_str().unwrap(), "x").unwrap();
    assert_mirror(&t);
    let d = t.dream_start(&json!([]), "x").unwrap();
    let out = d["output_store_id"].as_str().unwrap().to_owned();
    t.create(
        &out,
        "/topics/t/dream-only.md",
        &content("T", "D", json!({})),
        "x",
    )
    .unwrap();
    assert!(!t.mirror().join("topics/t/dream-only.md").exists());
    t.dream_adopt(d["id"].as_str().unwrap(), &review(), "x")
        .unwrap();
    assert!(t.mirror().join("topics/t/dream-only.md").exists());
    assert_mirror(&t);
    t.delete("live", id(&a), None, "x").unwrap();
    assert_mirror(&t);
    // Only the current and the previous generation are kept.
    assert!(fs::read_dir(t.mnt.join(".generations")).unwrap().count() <= 2);
}

#[test]
fn mirror_swap_atomic() {
    let mut t = TempStore::new();
    let a = create(&mut t, "/global/a.md", "A");
    let mirror = t.mirror();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let done = done.clone();
        std::thread::spawn(move || {
            let (mut last, mut reads) = (0, 0);
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                // Resolve once, then read the whole generation through that path.
                let Ok(target) = fs::canonicalize(&mirror) else {
                    panic!("mirror link missing")
                };
                let Ok(m) = fs::read_to_string(target.join(".agent-memory-store")) else {
                    continue;
                };
                let m: Value = serde_json::from_str(&m).unwrap();
                let rev = m["revision"].as_i64().unwrap();
                let Ok(body) = fs::read_to_string(target.join("global/a.md")) else {
                    continue;
                };
                assert!(
                    body.ends_with(&format!("r{rev}")) || rev == 1,
                    "{rev}: {body}"
                );
                assert!(rev >= last, "revision went backwards: {last} -> {rev}");
                last = rev;
                reads += 1;
            }
            reads
        })
    };
    for _ in 0..200 {
        let next = revision(&t) + 1;
        t.update(
            "live",
            id(&a),
            Some(&content("T", &format!("r{next}"), json!({}))),
            None,
            None,
            "x",
        )
        .unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
}

#[test]
fn mirror_failure_keeps_commit() {
    let mut t = TempStore::new();
    fs::set_permissions(&t.mnt, fs::Permissions::from_mode(0o555)).unwrap();
    let a = create(&mut t, "/global/a.md", "A");
    assert_eq!(t.status().unwrap()["mirror_stale"], true);
    assert_eq!(t.get("live", id(&a)).unwrap()["path"], "/global/a.md");
    assert_ne!(marker(&t)["revision"], revision(&t));
    fs::set_permissions(&t.mnt, fs::Permissions::from_mode(0o755)).unwrap();
    t.reopen();
    assert_eq!(t.status().unwrap()["mirror_stale"], false);
    assert_mirror(&t);
}
