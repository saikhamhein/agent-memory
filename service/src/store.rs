use crate::{
    Error, Result,
    model::{decode, encode, hash, invalid, now, string, validate},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{collections::HashSet, fs, os::unix::fs::OpenOptionsExt, path::Path, time::Duration};
use uuid::Uuid;

pub struct Store(pub Connection);
fn active(db: &Connection) -> Result<(i64, Value)> {
    let (revision, body): (i64, String) = db.query_row(
        "SELECT r.id,r.body FROM revisions r JOIN state s ON r.id=s.value WHERE s.key='active'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((revision, decode(&body)?))
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        // Missing stores are an error; only the explicit init command creates one.
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.busy_timeout(Duration::from_secs(10))?;
        active(&db)?;
        Ok(Self(db))
    }

    pub fn init(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(10))?;
        db.pragma_update(None, "journal_mode", "DELETE")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS revisions (
            id INTEGER PRIMARY KEY, parent INTEGER, created TEXT NOT NULL,
            reason TEXT NOT NULL, body TEXT NOT NULL, sha256 TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS observations (id TEXT PRIMARY KEY, created TEXT NOT NULL, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS dreams (id TEXT PRIMARY KEY, created TEXT NOT NULL, base INTEGER NOT NULL,
                input TEXT NOT NULL, promoted INTEGER, proposal TEXT);")?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO revisions VALUES (0,NULL,?1,?2,?3,?4)",
            params![now(), "Empty initial store", "[]", hash(b"[]")],
        )?;
        tx.execute("INSERT OR IGNORE INTO state VALUES ('active',0)", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn active(&self) -> Result<(i64, Value)> {
        active(&self.0)
    }
    pub fn status(&self) -> Result<Value> {
        let (revision, memories) = self.active()?;
        let observations: i64 = self
            .0
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))?;
        let dreams: i64 = self
            .0
            .query_row("SELECT COUNT(*) FROM dreams", [], |r| r.get(0))?;
        Ok(
            json!({"revision":revision,"memories":memories.as_array().map(Vec::len),"observations":observations,"dreams":dreams}),
        )
    }

    pub fn observe(&self, memory: Value) -> Result<Value> {
        validate(&json!([memory]))?;
        let id = Uuid::new_v4().simple().to_string();
        self.0.execute(
            "INSERT INTO observations VALUES (?1,?2,?3)",
            params![id, now(), encode(&memory)?],
        )?;
        Ok(json!({"id":id}))
    }

    pub fn dream(&mut self) -> Result<Value> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (base, memories) = active(&tx)?;
        let mut consumed = HashSet::new();
        {
            let mut stmt = tx.prepare("SELECT input FROM dreams WHERE promoted IS NOT NULL")?;
            for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                let input = decode(&row?)?;
                if let Some(observations) = input["observations"].as_array() {
                    for o in observations {
                        if let Some(id) = o["id"].as_str() {
                            consumed.insert(id.to_owned());
                        }
                    }
                }
            }
        }
        let mut observations = Vec::new();
        {
            let mut stmt =
                tx.prepare("SELECT id,created,body FROM observations ORDER BY created,id")?;
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (id, created, body) = row?;
                if !consumed.contains(&id) {
                    observations.push(json!({"id":id,"created":created,"body":decode(&body)?}));
                }
            }
        }
        let id = Uuid::new_v4().simple().to_string();
        let input = json!({"base":base,"memories":memories,"observations":observations});
        tx.execute(
            "INSERT INTO dreams(id,created,base,input) VALUES (?1,?2,?3,?4)",
            params![id, now(), base, encode(&input)?],
        )?;
        tx.commit()?;
        Ok(json!({"input":input,"proposal":initial_proposal(&id,base,memories),"promoted":null}))
    }

    pub fn get_dream(&self, id: &str) -> Result<Value> {
        let row = self
            .0
            .query_row(
                "SELECT base,input,promoted,proposal FROM dreams WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let input = decode(&row.1)?;
        let proposal = match row.3 {
            Some(body) => decode(&body)?,
            None => initial_proposal(id, row.0, input["memories"].clone()),
        };
        Ok(json!({"input":input,"proposal":proposal,"promoted":row.2}))
    }

    pub fn check_proposal(&self, proposal: &Value) -> Result<Value> {
        check_proposal(&self.0, proposal)
    }

    pub fn diff(&self, proposal: &Value) -> Result<Value> {
        let dream = self.check_proposal(proposal)?;
        let before = dream["input"]["memories"]
            .as_array()
            .ok_or_else(|| invalid("Invalid dream input"))?;
        let after = proposal["memories"]
            .as_array()
            .ok_or_else(|| invalid("Invalid proposal"))?;
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        for m in after {
            match before.iter().find(|old| old["id"] == m["id"]) {
                None => added.push(m.clone()),
                Some(old) if old != m => changed.push(json!({"before":old,"after":m})),
                _ => (),
            }
        }
        for old in before {
            if !after.iter().any(|m| m["id"] == old["id"]) {
                removed.push(old.clone());
            }
        }
        Ok(
            json!({"base_revision":proposal["base_revision"],"added":added,"removed":removed,"changed":changed}),
        )
    }

    pub fn promote(&mut self, proposal: Value) -> Result<Value> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let dream = check_proposal(&tx, &proposal)?;
        let (current, _) = active(&tx)?;
        if !dream["promoted"].is_null() || proposal["base_revision"] != current {
            return Err(Error::Conflict(
                "Stale or already promoted dream. Start a new dream and reconcile.".into(),
            ));
        }
        if proposal["reviewed"] != true {
            return Err(invalid("Review the proposal before promotion"));
        }
        let summary = string(&proposal, "summary")?;
        string(&proposal, "coverage")?;
        let conflicts = proposal["conflicts"]
            .as_array()
            .ok_or_else(|| invalid("conflicts must be an array"))?;
        if conflicts.iter().any(|c| c["status"] != "resolved") {
            return Err(invalid("Resolve conflicts before promotion"));
        }
        let body = encode(&proposal["memories"])?;
        tx.execute(
            "INSERT INTO revisions(parent,created,reason,body,sha256) VALUES (?1,?2,?3,?4,?5)",
            params![current, now(), summary, body, hash(body.as_bytes())],
        )?;
        let revision = tx.last_insert_rowid();
        tx.execute("UPDATE state SET value=?1 WHERE key='active'", [revision])?;
        tx.execute(
            "UPDATE dreams SET promoted=?1,proposal=?2 WHERE id=?3",
            params![revision, encode(&proposal)?, string(&proposal, "dream_id")?],
        )?;
        tx.commit()?;
        Ok(json!({"revision":revision}))
    }

    pub fn rollback(&mut self, request: Value) -> Result<Value> {
        let expected = request["expected"]
            .as_i64()
            .ok_or_else(|| invalid("expected must be a revision number"))?;
        let target = request["revision"]
            .as_i64()
            .ok_or_else(|| invalid("revision must be a revision number"))?;
        let reason = string(&request, "reason")?;
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, _) = active(&tx)?;
        if current != expected {
            return Err(Error::Conflict(
                "Active revision changed; inspect before rollback".into(),
            ));
        }
        let (body, digest): (String, String) = tx
            .query_row(
                "SELECT body,sha256 FROM revisions WHERE id=?1",
                [target],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        tx.execute(
            "INSERT INTO revisions(parent,created,reason,body,sha256) VALUES (?1,?2,?3,?4,?5)",
            params![
                current,
                now(),
                format!("Rollback to {target}: {reason}"),
                body,
                digest
            ],
        )?;
        let revision = tx.last_insert_rowid();
        tx.execute("UPDATE state SET value=?1 WHERE key='active'", [revision])?;
        tx.commit()?;
        Ok(json!({"revision":revision,"restored_from":target}))
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

fn initial_proposal(id: &str, base: i64, memories: Value) -> Value {
    json!({"dream_id":id,"base_revision":base,"memories":memories,"summary":"","coverage":"","conflicts":[],"reviewed":false})
}

fn check_proposal(db: &Connection, proposal: &Value) -> Result<Value> {
    validate(&proposal["memories"])?;
    let id = string(proposal, "dream_id")?;
    let row = db
        .query_row(
            "SELECT base,input,promoted FROM dreams WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if proposal["base_revision"] != row.0 {
        return Err(invalid("Altered base revision"));
    }
    Ok(json!({"input":decode(&row.1)?,"promoted":row.2}))
}
