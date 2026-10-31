//! Shared helpers for v2 store tests: temporary stores, valid content, bulk
//! seeding and the precondition race harness.
#![allow(dead_code)]

use agent_memory_server::{model::hash, store::Store};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
};
use tempfile::TempDir;

/// An initialized v2 store with its own temporary database and `mnt`.
pub struct TempStore {
    pub dir: TempDir,
    pub db: PathBuf,
    pub mnt: PathBuf,
    pub store: Store,
}

impl TempStore {
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("memory.sqlite3");
        let mnt = dir.path().join("mnt");
        Store::init(&db).unwrap();
        let store = Store::open(&db, &mnt).unwrap();
        Self {
            dir,
            db,
            mnt,
            store,
        }
    }

    /// Another handle on the same database and `mnt`.
    pub fn open(&self) -> Store {
        Store::open(&self.db, &self.mnt).unwrap()
    }

    pub fn reopen(&mut self) {
        self.store = self.open();
    }

    /// The mirror directory, `mnt/shared-agent-memory`.
    pub fn mirror(&self) -> PathBuf {
        self.mnt.join("shared-agent-memory")
    }

    /// A raw connection, for checks the API does not expose.
    pub fn sql(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    pub fn live_id(&self) -> String {
        self.store.status().unwrap()["store_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

impl Deref for TempStore {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.store
    }
}
impl DerefMut for TempStore {
    fn deref_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

/// Valid content: `meta` keys override the defaults.
pub fn content(title: &str, body: &str, meta: Value) -> String {
    let mut front = json!({
        "title": title,
        "confidence": "explicit",
        "tags": ["test"],
        "evidence": [{"source": "test", "locator": "fixture:1", "note": "Test evidence"}],
    });
    for (k, v) in meta.as_object().cloned().unwrap_or_default() {
        front[k] = v;
    }
    format!(
        "---\n{}\n---\n{body}",
        serde_json::to_string(&front).unwrap()
    )
}

/// Inserts `(path, content)` memories into the live store in one transaction,
/// bypassing per-write mirror rebuilds, then reopens to rebuild the mirror once.
/// Content is not validated; pass valid content.
pub fn seed(t: &mut TempStore, memories: &[(String, String)]) {
    let live = t.live_id();
    let mut db = t.sql();
    let tx = db.transaction().unwrap();
    for (path, content) in memories {
        let id = format!("mem_{}", uuid::Uuid::new_v4().simple());
        let version = format!("memver_{}", uuid::Uuid::new_v4().simple());
        let sha = hash(content.as_bytes());
        tx.execute(
            "INSERT INTO memory_versions(id,store_id,memory_id,operation,path,content,content_sha256,created_at,created_by)
             VALUES (?1,?2,?3,'created',?4,?5,?6,'2026-11-01T00:00:00+00:00','seed')",
            params![version, live, id, path, content, sha],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO memories VALUES (?1,?2,?3,?4,?5,?6,'2026-11-01T00:00:00+00:00','2026-11-01T00:00:00+00:00')",
            params![live, id, path, content, sha, version],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    t.reopen();
}

/// Runs `f(i, store)` on `n` threads at once, each with its own `Store` on the
/// same database, released together by a barrier. Results are in thread order.
pub fn race<T, F>(t: &TempStore, n: usize, f: F) -> Vec<T>
where
    T: Send + 'static,
    F: Fn(usize, &mut Store) -> T + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let barrier = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let (f, barrier) = (f.clone(), barrier.clone());
            let (db, mnt) = (t.db.clone(), t.mnt.clone());
            std::thread::spawn(move || {
                let mut store = Store::open(&db, &mnt).unwrap();
                barrier.wait();
                f(i, &mut store)
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

pub fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}
