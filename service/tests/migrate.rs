//! Migration acceptance tests (design §3.6, §4.3 M) against the real CLI.
mod legacy;

use legacy::{Dream, created, dump, memory, v1_db};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Output};

fn migrate(db: &Path, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_agent-memory-server"))
        .args(["migrate", "--database"])
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

fn report(db: &Path, args: &[&str]) -> Value {
    let out = migrate(db, args);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn failure(db: &Path, args: &[&str]) -> String {
    let out = migrate(db, args);
    assert!(!out.status.success());
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// (memory_id, operation, path, created_at, created_by) in version order.
fn versions(db: &Path) -> Vec<(String, String, Option<String>, String, String)> {
    let db = Connection::open(db).unwrap();
    let mut stmt = db
        .prepare("SELECT memory_id,operation,path,created_at,created_by FROM memory_versions ORDER BY seq")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn global() -> Value {
    json!({"kind": "global"})
}

#[test]
fn replays_history() {
    let a1 = memory("a", global(), "first");
    let b1 = memory("b", global(), "bee");
    let a2 = memory("a", global(), "second");
    let b2 = memory("b", json!({"kind": "topic", "value": "hardware"}), "bee");
    let v1 = v1_db(
        &[json!([a1, b1]), json!([a2, b2]), json!([b2]), json!([a2, b2])],
        &[],
        &[],
    );
    let r = report(&v1.path, &[]);
    assert_eq!(r["versions"], json!({"created": 3, "modified": 2, "deleted": 1}));
    let v = versions(&v1.path);
    let ops: Vec<_> = v
        .iter()
        .map(|(_, op, path, at, by)| (op.as_str(), path.clone().unwrap(), at.clone(), by.as_str()))
        .collect();
    let by = |n| format!("migration:revision-{n}");
    assert_eq!(
        ops,
        [
            ("created", "/global/a.md".into(), created(1), by(1).as_str()),
            ("created", "/global/b.md".into(), created(1), by(1).as_str()),
            ("modified", "/global/a.md".into(), created(2), by(2).as_str()),
            ("modified", "/topics/hardware/b.md".into(), created(2), by(2).as_str()),
            ("deleted", "/global/a.md".into(), created(3), by(3).as_str()),
            ("created", "/global/a.md".into(), created(4), by(4).as_str()),
        ]
    );
    // b keeps its id across the rename; a starts a new lineage at r4.
    assert_eq!(v[1].0, v[3].0);
    assert_eq!(v[0].0, v[4].0);
    assert_ne!(v[0].0, v[5].0);
    let db = Connection::open(&v1.path).unwrap();
    let (n, version): (i64, i64) = db
        .query_row("SELECT COUNT(*), (SELECT user_version FROM pragma_user_version) FROM memories", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((n, version), (2, 2));
}

#[test]
fn round_trip_exact() {
    let mut m = memory(
        "unicode",
        json!({"kind": "project", "value": "/home/sai/Projects/agent-memory"}),
        "မြန်မာ text — and \"quotes\"\nline two",
    );
    m["evidence"][0]["extra"] = json!({"nested": [1, 2.5]});
    m["big"] = serde_json::from_str("123456789012345678901234567890").unwrap();
    m["float"] = json!(0.1);
    m["expires"] = Value::Null;
    m["priority"] = json!(7);
    m["updated"] = json!("2026-10-30");
    let v1 = v1_db(&[json!([m])], &[], &[]);
    report(&v1.path, &[]);
    let db = Connection::open(&v1.path).unwrap();
    let (path, content): (String, String) = db
        .query_row("SELECT path, content FROM memories", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(path, "/projects/home/sai/Projects/agent-memory/unicode.md");
    let back = agent_memory_server::model::parse_legacy(&path, &content).unwrap();
    let encode = agent_memory_server::model::encode;
    assert_eq!(encode(&back).unwrap(), encode(&m).unwrap());
}

#[test]
fn legacy_preserved_and_frozen() {
    let v1 = v1_db(
        &[json!([memory("a", global(), "x")])],
        &[memory("o", global(), "obs")],
        &[Dream { observations: vec![0], promoted: Some(1) }],
    );
    let before = dump(&v1.path);
    report(&v1.path, &[]);
    assert_eq!(dump(&v1.path), before);
    let db = Connection::open(&v1.path).unwrap();
    for sql in [
        "INSERT INTO revisions VALUES (9,1,'t','r','[]','x')",
        "INSERT INTO dreams VALUES ('d','t',0,'{}',NULL,NULL)",
        "UPDATE state SET value=0",
    ] {
        let err = db.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("store migrated to v2"), "{sql}: {err}");
    }
}

#[test]
fn atomic_on_offender() {
    let v1 = v1_db(
        &[
            json!([memory("a", global(), "x")]),
            json!([memory("a", json!({"kind": "topic", "value": "machine learning"}), "x")]),
        ],
        &[],
        &[],
    );
    let bytes = fs::read(&v1.path).unwrap();
    let err = failure(&v1.path, &[]);
    assert!(err.contains("machine learning"), "{err}");
    assert_eq!(fs::read(&v1.path).unwrap(), bytes);
    assert!(!v1.dir.path().join("mnt").exists());
}

#[test]
fn dry_run_changes_nothing_and_second_run_refuses() {
    let v1 = v1_db(&[json!([memory("a", global(), "x")])], &[], &[]);
    let bytes = fs::read(&v1.path).unwrap();
    let r = report(&v1.path, &["--dry-run"]);
    assert_eq!(r["dry_run"], true);
    assert_eq!(r["memories"][0]["path"], "/global/a.md");
    assert_eq!(fs::read(&v1.path).unwrap(), bytes);
    report(&v1.path, &[]);
    let bytes = fs::read(&v1.path).unwrap();
    assert!(failure(&v1.path, &[]).contains("already migrated"));
    assert_eq!(fs::read(&v1.path).unwrap(), bytes);
}

#[test]
fn refuses_nonlinear_chain() {
    let v1 = v1_db(&[json!([]), json!([])], &[], &[]);
    Connection::open(&v1.path)
        .unwrap()
        .execute("UPDATE revisions SET parent=0 WHERE id=2", [])
        .unwrap();
    assert!(failure(&v1.path, &[]).contains("linear"));
}

#[test]
fn pin() {
    let v1 = v1_db(
        &[json!([memory("keep-it-simple", global(), "Simple wins."), memory("t", json!({"kind": "topic", "value": "x"}), "y")])],
        &[],
        &[],
    );
    let r = report(&v1.path, &["--pin", "keep-it-simple"]);
    assert_eq!(r["pinned_chars"], 12);
    let mirrored = fs::read_to_string(v1.dir.path().join("mnt/shared-agent-memory/global/keep-it-simple.md"));
    if let Ok(text) = mirrored {
        assert!(text.contains("\"pinned\": true"));
    }
    let last = versions(&v1.path).pop().unwrap();
    assert_eq!(last.4, "migration:pin");

    let over = v1_db(&[json!([memory("big", global(), &"x".repeat(6001))])], &[], &[]);
    let bytes = fs::read(&over.path).unwrap();
    assert!(failure(&over.path, &["--pin", "big"]).contains("pinned_budget_exceeded"));
    assert_eq!(fs::read(&over.path).unwrap(), bytes);
    // Pinning a topic memory breaks the v2 write rules.
    let topic = v1_db(&[json!([memory("t", json!({"kind": "topic", "value": "x"}), "y")])], &[], &[]);
    failure(&topic.path, &["--pin", "t"]);
    failure(&topic.path, &["--pin", "missing"]);
}
