//! A real v1 database with history, built with the v1 DDL that memory.py and the
//! v1 service used. Shared by the migration and install tests.
#![allow(dead_code)]
use agent_memory_server::model::{encode, hash};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::PathBuf;
use tempfile::TempDir;

const V1_DDL: &str = "CREATE TABLE IF NOT EXISTS revisions (
    id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
    reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS observations (
    id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS dreams (
    id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
    input TEXT NOT NULL, promoted INTEGER, proposal TEXT);";

pub struct V1 {
    pub dir: TempDir,
    pub path: PathBuf,
}

/// A legacy dream that consumed these observation indexes, promoted as that revision or not at all.
pub struct Dream {
    pub observations: Vec<usize>,
    pub promoted: Option<i64>,
}

/// The `created` timestamp of revision `n` (Python isoformat, as v1 wrote it).
pub fn created(n: i64) -> String {
    format!("2026-10-{:02}T08:{:02}:00.123456+00:00", 1 + n / 60, n % 60)
}

/// A valid v1 memory object.
pub fn memory(id: &str, scope: Value, body: &str) -> Value {
    json!({
        "id": id, "scope": scope, "title": format!("Title of {id}"), "body": body,
        "confidence": "explicit", "tags": ["test"],
        "evidence": [{"source": "test", "locator": "fixture", "note": "synthetic"}],
    })
}

/// Revision n (1-based) holds `revisions[n-1]`; revision 0 is v1's empty initial store.
pub fn v1_db(revisions: &[Value], observations: &[Value], dreams: &[Dream]) -> V1 {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "journal_mode", "DELETE").unwrap();
    db.execute_batch(V1_DDL).unwrap();
    let mut bodies = vec![json!([])];
    bodies.extend(revisions.iter().cloned());
    for (n, memories) in bodies.iter().enumerate() {
        let n = n as i64;
        let body = encode(memories).unwrap();
        db.execute(
            "INSERT INTO revisions VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                n,
                (n > 0).then(|| n - 1),
                created(n),
                if n == 0 {
                    "Empty initial store".into()
                } else {
                    format!("Revision {n}")
                },
                body,
                hash(body.as_bytes())
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO state VALUES ('active',?1)",
        [revisions.len() as i64],
    )
    .unwrap();
    let observation = |i: usize| json!({"id": format!("obs-{i}"), "created": created(i as i64), "body": observations[i]});
    for (i, body) in observations.iter().enumerate() {
        db.execute(
            "INSERT INTO observations VALUES (?1,?2,?3)",
            params![format!("obs-{i}"), created(i as i64), encode(body).unwrap()],
        )
        .unwrap();
    }
    for (i, dream) in dreams.iter().enumerate() {
        let base = dream.promoted.map_or(revisions.len() as i64, |p| p - 1);
        let input = json!({
            "base": base,
            "memories": bodies[base as usize],
            "observations": dream.observations.iter().map(|&o| observation(o)).collect::<Vec<_>>(),
        });
        db.execute(
            "INSERT INTO dreams VALUES (?1,?2,?3,?4,?5,NULL)",
            params![
                format!("dream-{i}"),
                created(base),
                base,
                encode(&input).unwrap(),
                dream.promoted
            ],
        )
        .unwrap();
    }
    drop(db);
    V1 { dir, path }
}

/// Schema and every row of the four legacy tables, in rowid order, like Python's iterdump.
pub fn dump(path: &std::path::Path) -> Vec<String> {
    let db = Connection::open(path).unwrap();
    let mut out = Vec::new();
    for table in ["revisions", "state", "observations", "dreams"] {
        out.push(
            db.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap(),
        );
        let mut stmt = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = stmt.column_count();
        let rows = stmt
            .query_map([], |row| {
                (0..columns)
                    .map(|i| {
                        row.get::<_, rusqlite::types::Value>(i)
                            .map(|v| format!("{v:?}"))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        out.extend(rows.map(|r| format!("{table}: {}", r.unwrap().join("|"))));
    }
    out
}
