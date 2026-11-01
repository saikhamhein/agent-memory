//! Shared helpers for the v2 store tests (workstream A owns this file).
//!
//! - `SealedTempDir`: a temporary directory that makes a sealed (0555/0444) tree
//!   deletable on drop. Use it instead of a bare `TempDir` for anything that holds
//!   a mirror; `assert_no_leftovers` proves it left nothing behind.
//! - `TempStore`: a v2 database plus a temporary `mnt`, removed on drop.
//! - `content` / `frontmatter` / `render`: valid memory bytes (§3.1 file format).
//! - `seed`: create a memory through the public API; `seed_raw` bulk-inserts rows
//!   straight into the v2 tables for tests that need thousands of memories (the
//!   mirror is rewritten on every API write, so seeding 10,000 through the API
//!   would be quadratic).
//! - `race(n, f)`: the precondition harness. `n` threads each open their own
//!   `Store` on the same database, are released together on a barrier, and the
//!   per-thread results come back in thread order.
#![allow(dead_code)]

use agent_memory_server::{Error, store::Store};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt::Debug,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Barrier,
    thread,
};
use tempfile::TempDir;

pub const ACTOR: &str = "token:test";

/// A `TempDir` whose tree is made writable again before it is deleted. The mirror is
/// read-only (0555 directories), and `TempDir` alone cannot remove it: it would leak
/// the directory silently on every passing run.
pub struct SealedTempDir(Option<TempDir>);

impl SealedTempDir {
    pub fn new() -> Self {
        Self(Some(TempDir::new().unwrap()))
    }
    pub fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

impl Drop for SealedTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            make_writable(dir.path());
            // `TempDir::drop` ignores errors; report a failed removal instead.
            let path = dir.path().to_owned();
            if let Err(e) = dir.close() {
                eprintln!("could not remove {}: {e}", path.display());
            }
        }
    }
}

/// Fail if `dir` exists and has anything in it. Use it with the path of a dropped
/// `SealedTempDir`, or with a fresh `TMPDIR` at the end of a suite run: a leak is a failure.
pub fn assert_no_leftovers(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return; // gone
    };
    let left: Vec<_> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(left.is_empty(), "{} was left with {left:?}", dir.display());
}

pub struct TempStore {
    pub store: Store,
    pub db: PathBuf,
    pub mnt: PathBuf,
    dir: SealedTempDir,
}

impl TempStore {
    pub fn new() -> Self {
        let dir = SealedTempDir::new();
        let db = dir.path().join("memory.sqlite3");
        let mnt = dir.path().join("mnt");
        Store::init(&db).unwrap();
        let store = Store::open(&db, &mnt).unwrap();
        Self {
            store,
            db,
            mnt,
            dir,
        }
    }
    /// A second, independent `Store` on the same database and mirror.
    pub fn open(&self) -> Store {
        Store::open(&self.db, &self.mnt).unwrap()
    }
    pub fn reopen(&mut self) {
        self.store = self.open();
    }
    pub fn live(&self) -> String {
        self.store.status().unwrap()["store_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    /// The path agents read: a symlink to the current generation.
    pub fn link(&self) -> PathBuf {
        self.mnt.join("shared-agent-memory")
    }
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Recursively restore write permission so a mirror tree can be deleted.
pub fn make_writable(root: &Path) {
    let Ok(meta) = fs::symlink_metadata(root) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o755));
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

// ---- memory content ------------------------------------------------------

/// Default valid frontmatter with `meta`'s keys added or replacing the defaults.
pub fn frontmatter(title: &str, meta: &Value) -> Value {
    let mut fm = json!({
        "title": title,
        "confidence": "explicit",
        "tags": ["test"],
        "evidence": [{"source": "test", "locator": "store_v2", "note": "fixture"}],
    });
    for (key, value) in meta.as_object().expect("meta must be a JSON object") {
        fm[key] = value.clone();
    }
    fm
}

/// `"---\n" + <JSON object> + "\n---\n" + body`, exactly as §3.1 freezes it.
pub fn render(frontmatter: &Value, body: &str) -> String {
    format!("---\n{frontmatter}\n---\n{body}")
}

pub fn content(title: &str, body: &str, meta: &Value) -> String {
    render(&frontmatter(title, meta), body)
}

/// Create a valid memory at `path` and return the memory object.
pub fn seed(store: &mut Store, path: &str, body: &str) -> Value {
    seed_meta(store, path, body, &json!({}))
}

pub fn seed_meta(store: &mut Store, path: &str, body: &str, meta: &Value) -> Value {
    let live = store.status().unwrap()["store_id"]
        .as_str()
        .unwrap()
        .to_owned();
    store
        .create(&live, path, &content(&title_of(path), body, meta), ACTOR)
        .unwrap_or_else(|e| panic!("seed {path}: {e:?}"))
}

fn title_of(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches(".md")
        .to_owned()
}

/// Insert `count` valid memories (`/topics/bulk/bulk-NNNNN.md`) straight into the
/// live store's tables. Reopen the `Store` afterwards.
pub fn seed_raw(db: &Path, count: usize) {
    let mut conn = Connection::open(db).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let live: String = conn
        .query_row("SELECT id FROM stores WHERE kind='live'", [], |r| r.get(0))
        .unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut memory = tx
            .prepare(
                "INSERT INTO memories(store_id,id,path,content,content_sha256,memory_version_id,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
            )
            .unwrap();
        let mut version = tx
            .prepare(
                "INSERT INTO memory_versions(id,store_id,memory_id,operation,path,content,content_sha256,created_at,created_by)
                 VALUES (?1,?2,?3,'created',?4,?5,?6,?7,'test:seed_raw')",
            )
            .unwrap();
        for i in 0..count {
            let id = format!("mem_{:032x}", 0x5eed_0000_u64 + i as u64);
            let version_id = format!("memver_{:032x}", 0x5eed_0000_u64 + i as u64);
            let path = format!("/topics/bulk/bulk-{i:05}.md");
            let text = content(&format!("Bulk {i}"), "bulk body", &json!({}));
            let digest = sha256(text.as_bytes());
            let at = "2026-11-01T00:00:00+00:00";
            memory
                .execute(params![live, id, path, text, digest, version_id, at])
                .unwrap();
            version
                .execute(params![version_id, live, id, path, text, digest, at])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

// ---- small accessors -----------------------------------------------------

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("`{key}` is not a string in {v}"))
}
pub fn id(v: &Value) -> String {
    str_of(v, "id").to_owned()
}
pub fn data(v: &Value) -> &Vec<Value> {
    v["data"]
        .as_array()
        .unwrap_or_else(|| panic!("no `data` array in {v}"))
}
/// Sorted paths in a `list` result.
pub fn listed_paths(list: &Value) -> Vec<String> {
    let mut paths: Vec<String> = data(list)
        .iter()
        .map(|m| str_of(m, "path").to_owned())
        .collect();
    paths.sort();
    paths
}
/// `(id, path, content_sha256)` of every memory in a `list` result, sorted.
pub fn listed_identity(list: &Value) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = data(list)
        .iter()
        .map(|m| {
            (
                id(m),
                str_of(m, "path").to_owned(),
                str_of(m, "content_sha256").to_owned(),
            )
        })
        .collect();
    rows.sort();
    rows
}
pub fn find_by_path(list: &Value, path: &str) -> Value {
    data(list)
        .iter()
        .find(|m| m["path"] == path)
        .unwrap_or_else(|| panic!("{path} is not in {list}"))
        .clone()
}
pub fn version_count(store: &Store, store_id: &str) -> usize {
    data(&store.versions(store_id, None, None).unwrap()).len()
}
pub fn revision(store: &Store) -> i64 {
    store.status().unwrap()["revision"].as_i64().unwrap()
}

// ---- error expectations --------------------------------------------------

/// Expect a 409 of the given `type`; return its `paths`.
pub fn conflict<T: Debug>(result: agent_memory_server::Result<T>, kind: &str) -> Vec<String> {
    match result {
        Err(Error::Conflict {
            kind: actual,
            paths,
            ..
        }) if actual == kind => paths,
        other => panic!("expected conflict `{kind}`, got {other:?}"),
    }
}
/// Expect a 400; return its message.
pub fn invalid<T: Debug>(result: agent_memory_server::Result<T>) -> String {
    match result {
        Err(Error::Invalid(message)) => message,
        other => panic!("expected an invalid-input error, got {other:?}"),
    }
}
pub fn not_found<T: Debug>(result: agent_memory_server::Result<T>) {
    match result {
        Err(Error::NotFound) => (),
        other => panic!("expected not found, got {other:?}"),
    }
}

// ---- filesystem snapshots ------------------------------------------------

/// Every entry under `root` (symlinks followed) as `relative path -> (mode, bytes)`;
/// directories have empty bytes and a trailing `/` in their key.
pub fn tree(root: &Path) -> BTreeMap<String, (u32, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, (u32, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            let path = entry.path();
            let meta = fs::metadata(&path).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            if meta.is_dir() {
                out.insert(format!("{name}/"), (mode, Vec::new()));
                walk(&path, &format!("{name}/"), out);
            } else {
                out.insert(name, (mode, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    let meta = fs::metadata(root).unwrap();
    out.insert("/".into(), (meta.permissions().mode() & 0o777, Vec::new()));
    walk(root, "", &mut out);
    out
}

// ---- the precondition harness -------------------------------------------

/// Run `f(i, store)` on `n` threads. Each thread opens its own `Store` on the same
/// database and mirror, then all threads are released together on a barrier.
/// Results come back in thread order.
pub fn race<T: Send>(
    ts: &TempStore,
    n: usize,
    f: impl Fn(usize, &mut Store) -> T + Sync,
) -> Vec<T> {
    let barrier = Barrier::new(n);
    let (db, mnt) = (&ts.db, &ts.mnt);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let (barrier, f) = (&barrier, &f);
                scope.spawn(move || {
                    let mut store = Store::open(db, mnt).unwrap();
                    barrier.wait();
                    f(i, &mut store)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}
