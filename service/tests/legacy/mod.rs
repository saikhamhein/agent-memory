//! Shared helper for workstream M: a real v1 database with history, built with the real v1 DDL.
//! Used by `migrate.rs` and `migrate_store.rs`. Temporary databases only.
#![allow(dead_code)]

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};
use tempfile::TempDir;

/// Verbatim from the v1 `Store::init` (and `memory.py`), kept here because `Store::init` creates v2 after the rework.
pub const V1_DDL: &str = "CREATE TABLE IF NOT EXISTS revisions (
    id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
    reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS observations (id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS dreams (id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
        input TEXT NOT NULL, promoted INTEGER, proposal TEXT);";

pub const LEGACY_TABLES: [&str; 4] = ["revisions", "state", "observations", "dreams"];

/// Python `json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2)`. Numbers keep the text they were parsed
/// from (serde_json `arbitrary_precision`), so literals written in Python's repr form round-trip exactly.
pub fn encode(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap()
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Parse JSON text keeping number literals as written (use for 2^64+ integers and floats).
pub fn json_text(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[derive(Clone)]
pub struct Revision {
    pub created: String,
    pub reason: String,
    pub memories: Vec<Value>,
}
/// The memories of one v1 revision, numbered 1.. by position in the `v1_db` call.
pub fn revision(created: &str, memories: Vec<Value>) -> Revision {
    Revision { created: created.into(), reason: format!("Revision at {created}"), memories }
}
#[derive(Clone)]
pub struct Observation {
    pub id: String,
    pub created: String,
    pub body: Value,
}
pub fn observation(id: &str, created: &str) -> Observation {
    Observation {
        id: id.into(),
        created: created.into(),
        body: mem(id, global(), &format!("Observed {id}")),
    }
}
#[derive(Clone)]
pub struct Dream {
    pub id: String,
    pub created: String,
    pub base: i64,
    pub input: Value,
    pub promoted: Option<i64>,
    pub proposal: Option<Value>,
}
/// A v1 dream whose input freezes `consumed` observations (the shape `Store::dream` wrote).
pub fn dream(id: &str, base: i64, observations: &[&Observation], promoted: Option<i64>) -> Dream {
    let seen: Vec<Value> = observations
        .iter()
        .map(|o| json!({"id": o.id, "created": o.created, "body": o.body}))
        .collect();
    Dream {
        id: id.into(),
        created: "2026-10-20T00:00:00+00:00".into(),
        base,
        input: json!({"base": base, "memories": [], "observations": seen}),
        promoted,
        proposal: promoted.map(|_| json!({"dream_id": id, "base_revision": base, "memories": [],
            "summary": "s", "coverage": "c", "conflicts": [], "reviewed": true})),
    }
}

pub struct V1 {
    pub dir: TempDir,
    pub db: PathBuf,
}
impl V1 {
    pub fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }
    pub fn mnt(&self) -> PathBuf {
        self.dir.path().join("mnt")
    }
    pub fn mirror(&self) -> PathBuf {
        self.mnt().join("shared-agent-memory")
    }
}

/// A v1 database as the v1 service left it: revision 0 empty, then `revisions` as 1..n each with parent = id − 1,
/// `state.active` = n. Rollbacks are just later revisions repeating an earlier body.
pub fn v1_db(revisions: &[Revision], observations: &[Observation], dreams: &[Dream]) -> V1 {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("memory.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
    conn.execute_batch(V1_DDL).unwrap();
    conn.execute(
        "INSERT INTO revisions VALUES (0,NULL,?1,?2,?3,?4)",
        params!["2026-10-01T00:00:00+00:00", "Empty initial store", "[]", sha256(b"[]")],
    )
    .unwrap();
    for (i, r) in revisions.iter().enumerate() {
        let id = i as i64 + 1;
        let body = encode(&Value::Array(r.memories.clone()));
        conn.execute(
            "INSERT INTO revisions VALUES (?1,?2,?3,?4,?5,?6)",
            params![id, id - 1, r.created, r.reason, body, sha256(body.as_bytes())],
        )
        .unwrap();
    }
    conn.execute("INSERT INTO state VALUES ('active',?1)", [revisions.len() as i64]).unwrap();
    for o in observations {
        conn.execute("INSERT INTO observations VALUES (?1,?2,?3)", params![o.id, o.created, encode(&o.body)]).unwrap();
    }
    for d in dreams {
        conn.execute(
            "INSERT INTO dreams VALUES (?1,?2,?3,?4,?5,?6)",
            params![d.id, d.created, d.base, encode(&d.input), d.promoted, d.proposal.as_ref().map(encode)],
        )
        .unwrap();
    }
    drop(conn);
    V1 { dir, db }
}

// ---- v1 memory builders (valid per v1 `validate`) ----
pub fn global() -> Value {
    json!({"kind": "global"})
}
pub fn project(root: &str) -> Value {
    json!({"kind": "project", "value": root})
}
pub fn topic(name: &str) -> Value {
    json!({"kind": "topic", "value": name})
}
pub fn role(name: &str) -> Value {
    json!({"kind": "role", "value": name})
}
pub fn mem(id: &str, scope: Value, body: &str) -> Value {
    json!({"id": id, "title": format!("Title of {id}"), "body": body, "scope": scope,
        "confidence": "explicit", "tags": ["fixture"],
        "evidence": [{"source": "test", "locator": "fixture:1", "note": "Test evidence"}]})
}
/// Same memory with other fields replaced.
pub fn with(memory: &Value, field: &str, value: Value) -> Value {
    let mut m = memory.clone();
    m[field] = value;
    m
}

// ---- inspection ----
pub fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}
/// Every row of the four legacy tables, in key order — the `iterdump` equivalent that ignores new tables.
pub fn legacy_dump(db: &Path) -> String {
    let conn = Connection::open(db).unwrap();
    let mut out = String::new();
    for (table, order) in [("revisions", "id"), ("state", "key"), ("observations", "id"), ("dreams", "id")] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY {order}")).unwrap();
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            out.push_str(table);
            for i in 0..columns {
                out.push_str(&format!("|{:?}", row.get_ref(i).unwrap()));
            }
            out.push('\n');
        }
    }
    out
}

pub struct Ran {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}
impl Ran {
    pub fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
    pub fn assert_ok(&self) -> &Self {
        assert!(self.status.success(), "expected success, got {:?}\n{}", self.status, self.text());
        self
    }
    pub fn assert_failed(&self) -> &Self {
        assert!(!self.status.success(), "expected failure, got success\n{}", self.text());
        self
    }
}
pub fn server(args: &[&str]) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-memory-server")).args(args).output().unwrap();
    Ran {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}
/// `agent-memory-server migrate --database DB …extra`. Fails loudly if the subcommand does not exist, so a
/// "refuses" test cannot pass just because clap rejected the command line.
pub fn migrate(db: &Path, extra: &[&str]) -> Ran {
    let mut args = vec!["migrate", "--database", db.to_str().unwrap()];
    args.extend_from_slice(extra);
    let ran = server(&args);
    assert!(!ran.stderr.contains("unrecognized subcommand"), "no `migrate` subcommand:\n{}", ran.stderr);
    assert!(!ran.stderr.contains("unexpected argument"), "migrate rejected its arguments:\n{}", ran.stderr);
    ran
}

#[derive(Debug)]
pub struct Row {
    pub id: String,
    pub path: String,
    pub content: String,
    pub sha: String,
    pub version_id: String,
    pub created_at: String,
    pub updated_at: String,
}
pub fn live(conn: &Connection) -> Vec<Row> {
    let mut stmt = conn
        .prepare(
            "SELECT m.id,m.path,m.content,m.content_sha256,m.memory_version_id,m.created_at,m.updated_at
             FROM memories m JOIN stores s ON s.id=m.store_id WHERE s.kind='live' ORDER BY m.path",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok(Row {
            id: r.get(0)?,
            path: r.get(1)?,
            content: r.get(2)?,
            sha: r.get(3)?,
            version_id: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
#[derive(Debug, Clone)]
pub struct Version {
    pub seq: i64,
    pub id: String,
    pub memory_id: String,
    pub operation: String,
    pub path: Option<String>,
    pub content: Option<String>,
    pub sha: Option<String>,
    pub created_at: String,
    pub created_by: String,
}
/// All versions of the live store, oldest first.
pub fn versions(conn: &Connection) -> Vec<Version> {
    let mut stmt = conn
        .prepare(
            "SELECT v.seq,v.id,v.memory_id,v.operation,v.path,v.content,v.content_sha256,v.created_at,v.created_by
             FROM memory_versions v JOIN stores s ON s.id=v.store_id WHERE s.kind='live' ORDER BY v.seq",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok(Version {
            seq: r.get(0)?,
            id: r.get(1)?,
            memory_id: r.get(2)?,
            operation: r.get(3)?,
            path: r.get(4)?,
            content: r.get(5)?,
            sha: r.get(6)?,
            created_at: r.get(7)?,
            created_by: r.get(8)?,
        })
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// Inverse of §3.6 for one stored file: `(v1 object, pinned)`. Independent of the implementation under test.
pub fn to_v1(path: &str, content: &str) -> (Value, bool) {
    let rest = content.strip_prefix("---\n").expect("content starts with frontmatter");
    let end = rest.find("\n---\n").expect("frontmatter is closed");
    let mut object = json_text(&rest[..end]);
    let body = &rest[end + "\n---\n".len()..];
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let (scope, slug) = match segments[0] {
        "global" => (json!({"kind": "global"}), segments[1]),
        "projects" => (
            json!({"kind": "project", "value": format!("/{}", segments[1..segments.len() - 1].join("/"))}),
            segments[segments.len() - 1],
        ),
        "topics" => (json!({"kind": "topic", "value": segments[1]}), segments[2]),
        "roles" => (json!({"kind": "role", "value": segments[1]}), segments[2]),
        other => panic!("unknown top-level directory {other}"),
    };
    let pinned = object.as_object_mut().unwrap().remove("pinned") == Some(Value::Bool(true));
    for forbidden in ["id", "scope", "body"] {
        assert!(object.get(forbidden).is_none(), "frontmatter must not carry {forbidden}");
    }
    object["id"] = json!(slug.strip_suffix(".md").unwrap());
    object["scope"] = scope;
    object["body"] = json!(body);
    (object, pinned)
}
/// The §3.6 stored form of a v1 memory: `"---\n" + encode(object minus id/scope/body) + "\n---\n" + body`.
pub fn expected_content(v1: &Value) -> String {
    let mut meta = v1.clone();
    let object = meta.as_object_mut().unwrap();
    for key in ["id", "scope", "body"] {
        object.remove(key);
    }
    format!("---\n{}\n---\n{}", encode(&meta), v1["body"].as_str().unwrap())
}

/// What the pinned budget counts for one memory (design §3.1): the characters of the block recall prints for it,
/// `### {title} [{path}]\n{body}\n\n` with the body's trailing whitespace trimmed. For a `mem()` memory in `/global/`.
pub fn block_len(id: &str, body: &str) -> usize {
    format!("### Title of {id} [/global/{id}.md]\n{}\n\n", body.trim_end()).chars().count()
}
