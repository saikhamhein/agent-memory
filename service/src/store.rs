use crate::{
    Error, Result, mirror,
    model::{self, Memory, PINNED_BUDGET, block_len, decode, encode, hash, invalid, now, string},
    recall,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

/// Memories per store.
pub const MAX_MEMORIES: i64 = 10_000;
/// Sessions per dream.
pub const MAX_SESSIONS: usize = 100;

const V1_TABLES: &str = "
CREATE TABLE IF NOT EXISTS revisions (id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
    reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS observations (id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS dreams (id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
    input TEXT NOT NULL, promoted INTEGER, proposal TEXT);";

pub struct Store {
    db: Connection,
    mnt: PathBuf,
    live: String,
    mirror_stale: bool,
}

/// A stored memory row: id, path, content, content_sha256.
type Row = (String, String, String, String);

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn connect(path: &Path, flags: OpenFlags) -> Result<Connection> {
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(Duration::from_secs(10))?;
    db.pragma_update(None, "foreign_keys", true)?;
    Ok(db)
}

fn user_version(db: &Connection) -> Result<i64> {
    Ok(db.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Resolves `live` or a store id, failing for archived stores when `write`.
fn resolve(db: &Connection, store: &str, write: bool) -> Result<(String, bool)> {
    let (id, kind, archived): (String, String, Option<String>) = db
        .query_row(
            "SELECT id,kind,archived_at FROM stores WHERE id=?1 OR (?1='live' AND kind='live')",
            [store],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if write && archived.is_some() {
        return Err(Error::conflict(
            "store_archived",
            "This store is archived and read-only",
            vec![],
        ));
    }
    Ok((id, kind == "live"))
}

fn rows(db: &Connection, store_id: &str) -> Result<Vec<Row>> {
    let mut stmt = db.prepare(
        "SELECT id,path,content,content_sha256 FROM memories WHERE store_id=?1 ORDER BY path",
    )?;
    let rows = stmt
        .query_map([store_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn head(db: &Connection, store_id: &str, id: &str) -> Result<Row> {
    db.query_row(
        "SELECT id,path,content,content_sha256 FROM memories WHERE store_id=?1 AND id=?2",
        [store_id, id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .optional()?
    .ok_or(Error::NotFound)
}

fn revision(db: &Connection, store_id: &str) -> Result<i64> {
    Ok(db.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM memory_versions WHERE store_id=?1",
        [store_id],
        |r| r.get(0),
    )?)
}

fn slug(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".md"))
        .unwrap_or(path)
}

/// `path_exists` or `slug_exists` unless the path and slug are free for `id`.
fn check_free(db: &Connection, store_id: &str, path: &str, id: &str) -> Result<()> {
    let mut stmt = db.prepare("SELECT id,path FROM memories WHERE store_id=?1 AND id<>?2")?;
    let taken = stmt
        .query_map([store_id, id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if taken.iter().any(|(_, p)| p == path) {
        return Err(Error::conflict(
            "path_exists",
            "A memory already exists at this path",
            vec![path.into()],
        ));
    }
    if let Some((_, other)) = taken.iter().find(|(_, p)| slug(p) == slug(path)) {
        return Err(Error::conflict(
            "slug_exists",
            "The slug is already used by another memory in this store",
            vec![other.clone()],
        ));
    }
    Ok(())
}

/// The store's pinned `block_len` total and the pinned paths.
fn pinned(db: &Connection, store_id: &str) -> Result<(usize, Vec<String>)> {
    let mut stmt =
        db.prepare("SELECT path,content FROM memories WHERE store_id=?1 ORDER BY path")?;
    let mut total = 0;
    let mut paths = Vec::new();
    for row in stmt.query_map([store_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (path, content) = row?;
        // Decode every frontmatter: a raw-byte prefilter misses escaped keys.
        if model::split(&content)?.0["pinned"] == true {
            let memory = model::parse(&path, &content)?;
            total += block_len(&memory);
            paths.push(path);
        }
    }
    Ok((total, paths))
}

/// `pinned_budget_exceeded`, naming every pinned path, when the store's pinned
/// memories exceed the budget.
fn check_pinned(db: &Connection, store_id: &str) -> Result<()> {
    let (total, paths) = pinned(db, store_id)?;
    if total > PINNED_BUDGET {
        return Err(Error::conflict(
            "pinned_budget_exceeded",
            &format!(
                "Pinned memories would total {total} characters; the budget is {PINNED_BUDGET}"
            ),
            paths,
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_version(
    db: &Connection,
    store_id: &str,
    memory_id: &str,
    operation: &str,
    path: Option<&str>,
    content: Option<&str>,
    actor: &str,
    at: &str,
) -> Result<String> {
    let id = new_id("memver");
    db.execute(
        "INSERT INTO memory_versions(id,store_id,memory_id,operation,path,content,content_sha256,created_at,created_by)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![id, store_id, memory_id, operation, path, content,
            content.map(|c| hash(c.as_bytes())), at, actor],
    )?;
    Ok(id)
}

fn insert_memory(
    db: &Connection,
    store_id: &str,
    id: &str,
    path: &str,
    content: &str,
    actor: &str,
) -> Result<()> {
    let at = now();
    let version = add_version(
        db,
        store_id,
        id,
        "created",
        Some(path),
        Some(content),
        actor,
        &at,
    )?;
    db.execute(
        "INSERT INTO memories VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
        params![
            store_id,
            id,
            path,
            content,
            hash(content.as_bytes()),
            version,
            at
        ],
    )?;
    Ok(())
}

fn modify_memory(
    db: &Connection,
    store_id: &str,
    id: &str,
    path: &str,
    content: &str,
    actor: &str,
) -> Result<()> {
    let at = now();
    let version = add_version(
        db,
        store_id,
        id,
        "modified",
        Some(path),
        Some(content),
        actor,
        &at,
    )?;
    db.execute(
        "UPDATE memories SET path=?3,content=?4,content_sha256=?5,memory_version_id=?6,updated_at=?7
         WHERE store_id=?1 AND id=?2",
        params![store_id, id, path, content, hash(content.as_bytes()), version, at],
    )?;
    Ok(())
}

fn delete_memory(db: &Connection, store_id: &str, id: &str, path: &str, actor: &str) -> Result<()> {
    add_version(db, store_id, id, "deleted", Some(path), None, actor, &now())?;
    db.execute(
        "DELETE FROM memories WHERE store_id=?1 AND id=?2",
        [store_id, id],
    )?;
    Ok(())
}

fn version_json(r: &rusqlite::Row, with_content: bool) -> rusqlite::Result<Value> {
    let mut v = json!({
        "id": r.get::<_, String>("id")?,
        "memory_id": r.get::<_, String>("memory_id")?,
        "operation": r.get::<_, String>("operation")?,
        "path": r.get::<_, Option<String>>("path")?,
        "content_sha256": r.get::<_, Option<String>>("content_sha256")?,
        "created_at": r.get::<_, String>("created_at")?,
        "created_by": r.get::<_, String>("created_by")?,
        "redacted_at": r.get::<_, Option<String>>("redacted_at")?,
        "redacted_by": r.get::<_, Option<String>>("redacted_by")?,
    });
    if with_content {
        v["content"] = json!(r.get::<_, Option<String>>("content")?);
    }
    Ok(v)
}

/// Archives a dream's output store and deletes its memories and versions, so
/// a secret copied into it leaves with the dream.
fn end_output(db: &Connection, store_id: &str, at: &str) -> Result<()> {
    db.execute("DELETE FROM memories WHERE store_id=?1", [store_id])?;
    db.execute("DELETE FROM memory_versions WHERE store_id=?1", [store_id])?;
    db.execute(
        "UPDATE stores SET archived_at=?2 WHERE id=?1",
        [store_id, at],
    )?;
    Ok(())
}

/// Runs the credential check on every string inside `value`.
fn check_strings(value: &Value) -> Result<()> {
    match value {
        Value::String(s) => model::check_secrets(s),
        Value::Array(items) => items.iter().try_for_each(check_strings),
        Value::Object(fields) => fields
            .iter()
            .try_for_each(|(k, v)| model::check_secrets(k).and_then(|()| check_strings(v))),
        _ => Ok(()),
    }
}

/// Observation ids in the input of an adopted v2 dream or a promoted legacy dream.
fn consumed_observations(db: &Connection) -> Result<HashSet<String>> {
    let mut stmt = db.prepare(
        "SELECT input FROM dreams WHERE promoted IS NOT NULL
         UNION ALL SELECT input FROM dream_jobs WHERE status='completed'",
    )?;
    let mut consumed = HashSet::new();
    for input in stmt.query_map([], |r| r.get::<_, String>(0))? {
        for o in decode(&input?)?["observations"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(id) = o["id"].as_str() {
                consumed.insert(id.to_owned());
            }
        }
    }
    Ok(consumed)
}

fn pending_observations(db: &Connection) -> Result<Vec<Value>> {
    let consumed = consumed_observations(db)?;
    let mut stmt = db.prepare("SELECT id,created,body FROM observations ORDER BY created,id")?;
    let mut pending = Vec::new();
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (id, created, body) = row?;
        if !consumed.contains(&id) {
            pending.push(json!({"id": id, "created": created, "body": decode(&body)?}));
        }
    }
    Ok(pending)
}

struct Dream {
    id: String,
    status: String,
    output: String,
    input: Value,
    base: Map<String, Value>,
    adoption: Option<Value>,
    created_at: String,
    ended_at: Option<String>,
}

impl Dream {
    fn load(db: &Connection, id: &str) -> Result<Self> {
        db.query_row(
            "SELECT id,status,output_store_id,input,adoption,created_at,ended_at FROM dream_jobs WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?
        .ok_or(Error::NotFound)
        .and_then(|(id, status, output, input, adoption, created_at, ended_at)| {
            let input = decode(&input)?;
            Ok(Self {
                id,
                status,
                output,
                base: input["base"].as_object().cloned().unwrap_or_default(),
                input,
                adoption: adoption.as_deref().map(decode).transpose()?,
                created_at,
                ended_at,
            })
        })
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "status": self.status,
            "output_store_id": self.output,
            "sessions": self.input["sessions"],
            "observations": self.input["observations"],
            "created_at": self.created_at,
            "ended_at": self.ended_at,
            "adoption": self.adoption,
        })
    }

    fn actor(&self) -> String {
        format!("dream:{}", self.id)
    }

    /// The output store against the base: (added, removed, changed) rows, by path.
    fn changes(&self, db: &Connection) -> Result<(Vec<Row>, Vec<String>, Vec<Row>)> {
        let base = &self.base;
        let output = rows(db, &self.output)?;
        let ids: HashSet<&str> = output.iter().map(|r| r.0.as_str()).collect();
        let (mut added, mut changed) = (Vec::new(), Vec::new());
        for row in output.iter().cloned() {
            match base.get(&row.0) {
                None => added.push(row),
                Some(b) if b["path"] != row.1.as_str() || b["content_sha256"] != row.3.as_str() => {
                    changed.push(row)
                }
                _ => (),
            }
        }
        let removed = base
            .keys()
            .filter(|id| !ids.contains(id.as_str()))
            .cloned()
            .collect();
        Ok((added, removed, changed))
    }

    /// The base content of `memory_id`: the copy this dream made at start.
    fn base_content(&self, db: &Connection, memory_id: &str) -> Result<Option<String>> {
        Ok(db
            .query_row(
                "SELECT content FROM memory_versions WHERE store_id=?1 AND memory_id=?2
                 AND operation='created' AND created_by=?3 ORDER BY seq LIMIT 1",
                [&self.output, memory_id, &self.actor()],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    fn diff(&self, db: &Connection) -> Result<Value> {
        let (added, removed, changed) = self.changes(db)?;
        let base = &self.base;
        let before = |id: &str| -> Result<Value> {
            Ok(json!({"path": base[id]["path"], "content": self.base_content(db, id)?}))
        };
        Ok(json!({
            "added": added.iter().map(|r| json!({"memory_id": r.0, "path": r.1, "content": r.2})).collect::<Vec<_>>(),
            "removed": removed.iter().map(|id| Ok(json!({"memory_id": id, "path": base[id]["path"], "content": self.base_content(db, id)?}))).collect::<Result<Vec<_>>>()?,
            "changed": changed.iter().map(|r| Ok(json!({"memory_id": r.0, "before": before(&r.0)?, "after": {"path": r.1, "content": r.2}}))).collect::<Result<Vec<_>>>()?,
        }))
    }
}

impl Store {
    pub fn init(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut db = connect(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        db.pragma_update(None, "journal_mode", "DELETE")?;
        if user_version(&db)? >= 2 {
            return Ok(());
        }
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(V1_TABLES)?;
        let legacy: i64 = tx.query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))?;
        if legacy > 0 {
            return Err(invalid(&format!(
                "{} is a v1 store; run `agent-memory-server migrate --database {}`",
                path.display(),
                path.display()
            )));
        }
        tx.execute_batch(include_str!("schema_v2.sql"))?;
        tx.execute(
            "INSERT INTO stores VALUES (?1,'live',?2,NULL)",
            [new_id("memstore"), now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Opens a v2 store whose live mirror is written under `mnt`.
    pub fn open(path: &Path, mnt: &Path) -> Result<Self> {
        // Missing stores are an error; only the explicit init command creates one.
        let db = connect(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        if user_version(&db)? < 2 {
            return Err(invalid(&format!(
                "{} uses schema v1; run `agent-memory-server migrate --database {}` first",
                path.display(),
                path.display()
            )));
        }
        let live = db.query_row("SELECT id FROM stores WHERE kind='live'", [], |r| r.get(0))?;
        let mut store = Self {
            db,
            mnt: mnt.to_owned(),
            live,
            mirror_stale: false,
        };
        store.sync_mirror();
        Ok(store)
    }

    fn mirror_dir(&self) -> String {
        mirror::dir(&self.mnt).display().to_string()
    }

    /// Rewrites the mirror unless it already matches the store. Rebuilding the
    /// same revision (a crash or a new renderer) briefly removes that generation.
    /// A failure never undoes the commit; it marks the mirror stale.
    fn sync_mirror(&mut self) {
        let result = (|| -> Result<()> {
            let (revision, rows, memories) = self.live_rows()?;
            let index = recall::index_md(&memories, revision, &self.mirror_dir());
            if !mirror::is_current(&self.mnt, &self.live, revision, &index) {
                mirror::write(&self.mnt, &self.live, revision, &rows, &index)?;
            }
            Ok(())
        })();
        if let Err(e) = &result {
            eprintln!("Mirror write failed; it is stale until the next write: {e}");
        }
        self.mirror_stale = result.is_err();
    }

    fn live_rows(&self) -> Result<(i64, Vec<Row>, Vec<Memory>)> {
        let tx = self.db.unchecked_transaction()?;
        let revision = revision(&tx, &self.live)?;
        let rows = rows(&tx, &self.live)?;
        let memories = rows
            .iter()
            .map(|(id, path, content, _)| {
                let mut m = model::parse(path, content)?;
                m.id = id.clone();
                Ok(m)
            })
            .collect::<Result<_>>()?;
        Ok((revision, rows, memories))
    }

    pub fn is_live(&self, store: &str) -> Result<bool> {
        Ok(resolve(&self.db, store, false)?.1)
    }

    pub fn status(&self) -> Result<Value> {
        let count = |sql: &str| self.db.query_row(sql, [&self.live], |r| r.get::<_, i64>(0));
        let running: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM dream_jobs WHERE status='running'",
            [],
            |r| r.get(0),
        )?;
        Ok(json!({
            "store_id": self.live,
            "revision": revision(&self.db, &self.live)?,
            "memories": count("SELECT COUNT(*) FROM memories WHERE store_id=?1")?,
            "pinned_chars": pinned(&self.db, &self.live)?.0,
            "observations_pending": pending_observations(&self.db)?.len(),
            "dreams_running": running,
            "mirror_dir": self.mirror_dir(),
            "mirror_stale": self.mirror_stale,
        }))
    }

    /// The live revision and its memories, parsed, for recall.
    pub fn live_memories(&self) -> Result<(i64, Vec<Memory>)> {
        let (revision, _, memories) = self.live_rows()?;
        Ok((revision, memories))
    }

    pub fn list(&self, store: &str, path_prefix: &str) -> Result<Value> {
        if !path_prefix.starts_with('/') {
            return Err(invalid("path_prefix must start with /"));
        }
        let (store_id, _) = resolve(&self.db, store, false)?;
        let mut stmt = self.db.prepare(
            "SELECT id,path,content,content_sha256,memory_version_id,updated_at FROM memories
             WHERE store_id=?1 AND substr(path,1,length(?2))=?2 ORDER BY path",
        )?;
        let data = stmt
            .query_map([&store_id, path_prefix], |r| {
                let path: String = r.get(1)?;
                let content: String = r.get(2)?;
                let title = model::parse(&path, &content)
                    .map(|m| m.title().to_owned())
                    .unwrap_or_default();
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "path": path,
                    "title": title,
                    "content_sha256": r.get::<_, String>(3)?,
                    "memory_version_id": r.get::<_, String>(4)?,
                    "updated_at": r.get::<_, String>(5)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({ "data": data }))
    }

    pub fn get(&self, store: &str, id: &str) -> Result<Value> {
        let (store_id, _) = resolve(&self.db, store, false)?;
        self.db
            .query_row(
                "SELECT path,content,content_sha256,memory_version_id,updated_at FROM memories
                 WHERE store_id=?1 AND id=?2",
                [&store_id, id],
                |r| {
                    let path: String = r.get(0)?;
                    let content: String = r.get(1)?;
                    let title = model::parse(&path, &content)
                        .map(|m| m.title().to_owned())
                        .unwrap_or_default();
                    Ok(json!({
                        "id": id,
                        "path": path,
                        "title": title,
                        "content_sha256": r.get::<_, String>(2)?,
                        "memory_version_id": r.get::<_, String>(3)?,
                        "updated_at": r.get::<_, String>(4)?,
                        "content": content,
                    }))
                },
            )
            .optional()?
            .ok_or(Error::NotFound)
    }

    pub fn create(&mut self, store: &str, path: &str, content: &str, actor: &str) -> Result<Value> {
        let memory = model::parse(path, content)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (store_id, live) = resolve(&tx, store, true)?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM memories WHERE store_id=?1",
            [&store_id],
            |r| r.get(0),
        )?;
        if count >= MAX_MEMORIES {
            return Err(invalid(&format!(
                "store_full: a store holds at most {MAX_MEMORIES} memories"
            )));
        }
        check_free(&tx, &store_id, path, "")?;
        let id = new_id("mem");
        insert_memory(&tx, &store_id, &id, path, content, actor)?;
        if memory.pinned() {
            check_pinned(&tx, &store_id)?;
        }
        tx.commit()?;
        self.written(live);
        self.get(&store_id, &id)
    }

    pub fn update(
        &mut self,
        store: &str,
        id: &str,
        content: Option<&str>,
        path: Option<&str>,
        expected: Option<&str>,
        actor: &str,
    ) -> Result<Value> {
        if content.is_none() && path.is_none() {
            return Err(invalid("Give content, path or both"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (store_id, live) = resolve(&tx, store, true)?;
        let (_, old_path, old_content, old_sha) = head(&tx, &store_id, id)?;
        if expected.is_some_and(|e| e != old_sha) {
            return Err(Error::conflict(
                "precondition_failed",
                "The memory changed since it was read; re-read it and retry",
                vec![old_path],
            ));
        }
        let new_path = path.unwrap_or(&old_path);
        let new_content = content.unwrap_or(&old_content);
        let memory = model::parse(new_path, new_content)?;
        if new_path == old_path && new_content == old_content {
            drop(tx);
            return self.get(&store_id, id);
        }
        if new_path != old_path {
            check_free(&tx, &store_id, new_path, id)?;
        }
        modify_memory(&tx, &store_id, id, new_path, new_content, actor)?;
        if memory.pinned() {
            check_pinned(&tx, &store_id)?;
        }
        tx.commit()?;
        self.written(live);
        self.get(&store_id, id)
    }

    pub fn delete(
        &mut self,
        store: &str,
        id: &str,
        expected: Option<&str>,
        actor: &str,
    ) -> Result<Value> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (store_id, live) = resolve(&tx, store, true)?;
        let (_, path, _, sha) = head(&tx, &store_id, id)?;
        if expected.is_some_and(|e| e != sha) {
            return Err(Error::conflict(
                "precondition_failed",
                "The memory changed since it was read; re-read it and retry",
                vec![path],
            ));
        }
        delete_memory(&tx, &store_id, id, &path, actor)?;
        tx.commit()?;
        self.written(live);
        Ok(json!({"type": "memory_deleted", "id": id}))
    }

    fn written(&mut self, live: bool) {
        if live {
            self.sync_mirror();
        }
    }

    /// Versions, newest first, without content.
    pub fn versions(
        &self,
        store: &str,
        memory_id: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<Value> {
        let (store_id, _) = resolve(&self.db, store, false)?;
        let mut stmt = self.db.prepare(
            "SELECT * FROM memory_versions WHERE store_id=?1
             AND (?2 IS NULL OR memory_id=?2) AND (?3 IS NULL OR created_by=?3) ORDER BY seq DESC",
        )?;
        let data = stmt
            .query_map(params![store_id, memory_id, created_by], |r| {
                version_json(r, false)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({ "data": data }))
    }

    pub fn version(&self, store: &str, id: &str) -> Result<Value> {
        let (store_id, _) = resolve(&self.db, store, false)?;
        self.db
            .query_row(
                "SELECT * FROM memory_versions WHERE store_id=?1 AND id=?2",
                [&store_id, id],
                |r| version_json(r, true),
            )
            .optional()?
            .ok_or(Error::NotFound)
    }

    /// Removes a past version's content, path and hash. Archived stores can be
    /// redacted too: removing a leaked secret outranks read-only history.
    pub fn redact(&mut self, store: &str, id: &str, actor: &str) -> Result<Value> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (store_id, _) = resolve(&tx, store, false)?;
        let (redacted, is_head): (bool, bool) = tx
            .query_row(
                "SELECT redacted_at IS NOT NULL,
                        EXISTS(SELECT 1 FROM memories m WHERE m.store_id=v.store_id AND m.memory_version_id=v.id)
                 FROM memory_versions v WHERE store_id=?1 AND id=?2",
                [&store_id, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if is_head {
            return Err(invalid(
                "Cannot redact the current version; update or delete the memory first",
            ));
        }
        if !redacted {
            tx.execute(
                "UPDATE memory_versions SET content=NULL,path=NULL,content_sha256=NULL,redacted_at=?3,redacted_by=?4
                 WHERE store_id=?1 AND id=?2",
                params![store_id, id, now(), actor],
            )?;
        }
        tx.commit()?;
        self.version(&store_id, id)
    }

    pub fn observe(&self, path: &str, content: &str, actor: &str) -> Result<Value> {
        model::parse(path, content)?;
        let id = Uuid::new_v4().simple().to_string();
        let body = json!({"path": path, "content": content, "created_by": actor});
        self.db.execute(
            "INSERT INTO observations VALUES (?1,?2,?3)",
            params![id, now(), encode(&body)?],
        )?;
        Ok(json!({ "id": id }))
    }

    /// Replaces an observation's path and content with nulls, here and in every
    /// dream input that copied it. The id, time and `created_by` stay.
    pub fn redact_observation(&mut self, id: &str, actor: &str) -> Result<Value> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (created, body): (String, String) = tx
            .query_row(
                "SELECT created,body FROM observations WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let mut body = decode(&body)?;
        if body.get("redacted_at").is_none() {
            body = json!({
                "path": null,
                "content": null,
                "created_by": body.get("created_by").cloned().unwrap_or(Value::Null),
                "redacted_at": now(),
                "redacted_by": actor,
            });
            tx.execute(
                "UPDATE observations SET body=?2 WHERE id=?1",
                params![id, encode(&body)?],
            )?;
            let jobs = {
                let mut stmt = tx.prepare("SELECT id,input FROM dream_jobs")?;
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (job, input) in jobs {
                let mut input = decode(&input)?;
                let mut copied = false;
                for o in input["observations"].as_array_mut().into_iter().flatten() {
                    if o["id"] == id {
                        o["body"] = body.clone();
                        copied = true;
                    }
                }
                if copied {
                    tx.execute(
                        "UPDATE dream_jobs SET input=?2 WHERE id=?1",
                        params![job, encode(&input)?],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(json!({"id": id, "created": created, "body": body}))
    }

    /// Clones the live store into a new output store and freezes the input.
    /// Only one dream runs at a time.
    pub fn dream_start(&mut self, sessions: &Value, _actor: &str) -> Result<Value> {
        let list = sessions
            .as_array()
            .filter(|a| a.len() <= MAX_SESSIONS)
            .ok_or_else(|| invalid("sessions must be an array of at most 100 sessions"))?;
        let mut locators = HashSet::new();
        for s in list {
            let fields = s
                .as_object()
                .ok_or_else(|| invalid("Each session must be an object"))?;
            if let Some(key) = fields
                .keys()
                .find(|k| !matches!(k.as_str(), "source" | "locator" | "sha256"))
            {
                return Err(invalid(&format!(
                    "Unknown session key {key:?}; use source, locator and sha256"
                )));
            }
            string(s, "source")?;
            if !locators.insert(string(s, "locator")?) {
                return Err(invalid("Session locators must be unique"));
            }
            if s.get("sha256").is_some_and(|v| !v.is_string()) {
                return Err(invalid("sha256 must be a string"));
            }
            check_strings(s)?;
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: Option<String> = tx
            .query_row(
                "SELECT id FROM dream_jobs WHERE status='running'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(running) = running {
            return Err(Error::conflict(
                "dream_running",
                "A dream is already running; adopt or cancel it first",
                vec![running],
            ));
        }
        let id = new_id("drm");
        let actor = format!("dream:{id}");
        let output = new_id("memstore");
        let at = now();
        tx.execute(
            "INSERT INTO stores VALUES (?1,'dream_output',?2,NULL)",
            [&output, &at],
        )?;
        let mut base = Map::new();
        for (memory_id, path, content, sha) in rows(&tx, &self.live)? {
            add_version(
                &tx,
                &output,
                &memory_id,
                "created",
                Some(&path),
                Some(&content),
                &actor,
                &at,
            )?;
            tx.execute(
                "INSERT INTO memories(store_id,id,path,content,content_sha256,memory_version_id,created_at,updated_at)
                 SELECT ?1,id,path,content,content_sha256,
                        (SELECT id FROM memory_versions WHERE store_id=?1 AND memory_id=?3),created_at,updated_at
                 FROM memories WHERE store_id=?2 AND id=?3",
                [&output, &self.live, &memory_id],
            )?;
            base.insert(memory_id, json!({"path": path, "content_sha256": sha}));
        }
        let input = json!({
            "base": base,
            "observations": pending_observations(&tx)?,
            "sessions": sessions,
        });
        tx.execute(
            "INSERT INTO dream_jobs VALUES (?1,'running',?2,?3,NULL,?4,NULL)",
            params![id, output, encode(&input)?, at],
        )?;
        tx.commit()?;
        self.dream(&id, false)
    }

    /// Dreams, newest first.
    pub fn dreams(&self) -> Result<Value> {
        let mut stmt = self
            .db
            .prepare("SELECT id FROM dream_jobs ORDER BY created_at DESC, rowid DESC")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let data = ids
            .iter()
            .map(|id| Ok(Dream::load(&self.db, id)?.json()))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({ "data": data }))
    }

    pub fn dream(&self, id: &str, diff: bool) -> Result<Value> {
        let dream = Dream::load(&self.db, id)?;
        let mut value = dream.json();
        if diff {
            if dream.status != "running" {
                return Err(invalid(&format!(
                    "The dream is {}; a diff exists only while it runs",
                    dream.status
                )));
            }
            value["diff"] = dream.diff(&self.db)?;
        }
        Ok(value)
    }

    /// Applies the dream's changes to the live store, per memory, or changes nothing.
    pub fn dream_adopt(&mut self, id: &str, review: &Value, _actor: &str) -> Result<Value> {
        let fields = review
            .as_object()
            .ok_or_else(|| invalid("The review must be an object"))?;
        if let Some(key) = fields
            .keys()
            .find(|k| !matches!(k.as_str(), "summary" | "coverage" | "conflicts"))
        {
            return Err(invalid(&format!(
                "Unknown review key {key:?}; use summary, coverage and conflicts"
            )));
        }
        check_strings(review)?;
        let summary = string(review, "summary")?;
        let coverage = string(review, "coverage")?;
        let conflicts = review["conflicts"]
            .as_array()
            .ok_or_else(|| invalid("conflicts must be an array"))?;
        if conflicts.iter().any(|c| c["status"] != "resolved") {
            return Err(invalid("Resolve conflicts before adopting"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let dream = Dream::load(&tx, id)?;
        if dream.status != "running" {
            return Err(invalid(&format!(
                "The dream is {}; only a running dream can be adopted",
                dream.status
            )));
        }
        let (added, removed, changed) = dream.changes(&tx)?;
        let base = &dream.base;
        let live: HashMap<String, Row> = rows(&tx, &self.live)?
            .into_iter()
            .map(|r| (r.0.clone(), r))
            .collect();

        // Each changed or removed memory must still be as the dream found it.
        let mut stale = Vec::new();
        for id in changed.iter().map(|r| &r.0).chain(&removed) {
            let b = &base[id.as_str()];
            if !live
                .get(id)
                .is_some_and(|r| b["path"] == r.1.as_str() && b["content_sha256"] == r.3.as_str())
            {
                stale.push(b["path"].as_str().unwrap_or_default().to_owned());
            }
        }
        // Every path and slug the dream adds or moves to must be free in the result.
        let mut result: BTreeMap<&str, &str> = live
            .values()
            .map(|r| (r.0.as_str(), r.1.as_str()))
            .collect();
        for id in &removed {
            result.remove(id.as_str());
        }
        let mine: HashSet<&str> = added.iter().chain(&changed).map(|r| r.0.as_str()).collect();
        for r in added.iter().chain(&changed) {
            result.insert(&r.0, &r.1);
        }
        let mut owners: HashMap<&str, Vec<&str>> = HashMap::new();
        for (id, path) in &result {
            owners.entry(slug(path)).or_default().push(id);
        }
        for ids in owners.values().filter(|ids| ids.len() > 1) {
            stale.extend(
                ids.iter()
                    .filter(|id| mine.contains(*id))
                    .map(|id| result[id].to_owned()),
            );
        }
        if !stale.is_empty() {
            stale.sort();
            stale.dedup();
            return Err(Error::conflict(
                "adopt_conflict",
                "The live store changed these memories since the dream started; cancel, start a new dream and carry the edits over",
                stale,
            ));
        }
        let actor = dream.actor();
        for id in &removed {
            delete_memory(&tx, &self.live, id, &live[id].1, &actor)?;
        }
        // Move renamed memories aside first so swapped paths never collide.
        for r in &changed {
            tx.execute(
                "UPDATE memories SET path=?3 WHERE store_id=?1 AND id=?2",
                [&self.live, &r.0, &format!("//moving/{}", r.0)],
            )?;
        }
        for (id, path, content, _) in &changed {
            modify_memory(&tx, &self.live, id, path, content, &actor)?;
        }
        for (id, path, content, _) in &added {
            insert_memory(&tx, &self.live, id, path, content, &actor)?;
        }
        check_pinned(&tx, &self.live)?;
        let paths = |rows: &[Row]| rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>();
        let adoption = json!({
            "summary": summary,
            "coverage": coverage,
            "conflicts": conflicts,
            "added": paths(&added),
            "changed": paths(&changed),
            "removed": removed.iter().map(|id| &base[id.as_str()]["path"]).collect::<Vec<_>>(),
        });
        let at = now();
        tx.execute(
            "UPDATE dream_jobs SET status='completed',adoption=?2,ended_at=?3 WHERE id=?1",
            params![dream.id, encode(&adoption)?, at],
        )?;
        end_output(&tx, &dream.output, &at)?;
        tx.commit()?;
        self.sync_mirror();
        self.dream(id, false)
    }

    pub fn dream_cancel(&mut self, id: &str, _actor: &str) -> Result<Value> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let dream = Dream::load(&tx, id)?;
        match dream.status.as_str() {
            "running" => {
                let at = now();
                tx.execute(
                    "UPDATE dream_jobs SET status='canceled',ended_at=?2 WHERE id=?1",
                    [id, &at],
                )?;
                end_output(&tx, &dream.output, &at)?;
                tx.commit()?;
            }
            "canceled" => drop(tx),
            _ => return Err(invalid("A completed dream cannot be canceled")),
        }
        self.dream(id, false)
    }

    pub fn backup(source: &Path, destination: &Path) -> Result<()> {
        let db = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(10))?;
        // Reserve the output exclusively so a typo cannot overwrite another store.
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)?;
        let mut target = Connection::open(destination)?;
        rusqlite::backup::Backup::new(&db, &mut target)?.run_to_completion(
            128,
            Duration::from_millis(10),
            None,
        )?;
        let result: String = target.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if result != "ok" {
            return Err(invalid("Backup integrity check failed"));
        }
        Ok(())
    }
}
