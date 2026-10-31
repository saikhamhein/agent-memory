//! One-shot migration of a v1 store (whole-store revisions) to schema v2 (design §3.6).
//! Everything happens in one exclusive transaction; any failure rolls it back,
//! which leaves the database file byte-identical.
use agent_memory_server::{
    Result,
    model::{
        PINNED_BUDGET, block_len, decode, encode, hash, invalid, now, parse, parse_legacy,
        render_legacy,
    },
    store::Store,
};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};
use uuid::Uuid;

const LEGACY_TABLES: [&str; 4] = ["revisions", "state", "observations", "dreams"];

struct Head {
    id: String,
    v1: Value,
    path: String,
    content: String,
    versions: usize,
}

pub fn migrate(database: &Path, mnt: &Path, pins: &[String], dry_run: bool) -> Result<Value> {
    // A missing database is an error, as for `serve`.
    let mut db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(10))?;
    // EXCLUSIVE also keeps readers out in rollback-journal mode.
    let tx = db.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    let report = replay(&tx, pins, dry_run)?;
    if dry_run {
        return Ok(report); // dropping the transaction rolls it back
    }
    tx.commit()?;
    drop(db);
    // Opening the migrated store builds its missing mirror.
    Store::open(database, mnt)?;
    Ok(report)
}

fn replay(tx: &Transaction, pins: &[String], dry_run: bool) -> Result<Value> {
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 2 {
        return Err(invalid("Store is already migrated to v2; nothing to do"));
    }
    let legacy = dump(tx)?;
    let revisions = {
        let mut stmt = tx.prepare("SELECT id,parent,created,body FROM revisions ORDER BY id")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (i, (id, parent, ..)) in revisions.iter().enumerate() {
        if *id != i as i64 || *parent != id.checked_sub(1).filter(|p| *p >= 0) {
            return Err(invalid(&format!(
                "Revisions are not a linear chain (parent = id - 1) at revision {id}; refusing to migrate"
            )));
        }
    }

    tx.execute_batch(include_str!("schema_v2.sql"))?;
    let store = new_id("memstore");
    tx.execute(
        "INSERT INTO stores(id,kind,created_at) VALUES (?1,'live',?2)",
        params![store, now()],
    )?;
    let mut heads: BTreeMap<String, Head> = BTreeMap::new();
    let mut offenders = BTreeSet::new();
    let mut counts = BTreeMap::from([("created", 0), ("modified", 0), ("deleted", 0)]);
    let mut write = |head: &Head, operation: &'static str, at: &str, actor: &str| -> Result<()> {
        let version = new_id("memver");
        let (path, content, sha) = match operation {
            "deleted" => (Some(&head.path), None, None),
            _ => (
                Some(&head.path),
                Some(&head.content),
                Some(hash(head.content.as_bytes())),
            ),
        };
        tx.execute(
            "INSERT INTO memory_versions(id,store_id,memory_id,operation,path,content,content_sha256,created_at,created_by)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![version, store, head.id, operation, path, content, sha, at, actor],
        )?;
        match operation {
            "created" => tx.execute(
                "INSERT INTO memories VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                params![store, head.id, head.path, head.content, sha, version, at],
            )?,
            "modified" => tx.execute(
                "UPDATE memories SET path=?3,content=?4,content_sha256=?5,memory_version_id=?6,updated_at=?7
                 WHERE store_id=?1 AND id=?2",
                params![store, head.id, head.path, head.content, sha, version, at],
            )?,
            _ => tx.execute(
                "DELETE FROM memories WHERE store_id=?1 AND id=?2",
                params![store, head.id],
            )?,
        };
        *counts.get_mut(operation).unwrap() += 1;
        Ok(())
    };

    for (n, _, created, body) in revisions.iter().skip(1) {
        let actor = format!("migration:revision-{n}");
        let objects = decode(body)?;
        let objects = objects
            .as_array()
            .ok_or_else(|| invalid(&format!("Revision {n} is not a list of memories")))?;
        let mut seen = BTreeSet::new();
        for v1 in objects {
            let slug = v1["id"].as_str().unwrap_or_default().to_owned();
            seen.insert(slug.clone());
            let (path, content) = match rendered(v1) {
                Ok(rendered) => rendered,
                Err(e) => {
                    offenders.insert(format!(
                        "{slug} at revision {n}: scope {}: {e}",
                        v1["scope"]
                    ));
                    continue;
                }
            };
            match heads.get_mut(&slug) {
                None => {
                    let head = Head {
                        id: new_id("mem"),
                        v1: v1.clone(),
                        path,
                        content,
                        versions: 1,
                    };
                    write(&head, "created", created, &actor)?;
                    heads.insert(slug, head);
                }
                Some(head) if head.path != path || head.content != content => {
                    (head.v1, head.path, head.content) = (v1.clone(), path, content);
                    head.versions += 1;
                    write(head, "modified", created, &actor)?;
                }
                Some(_) => (),
            }
        }
        let gone: Vec<String> = heads
            .keys()
            .filter(|s| !seen.contains(*s))
            .cloned()
            .collect();
        for slug in gone {
            // A slug that comes back later starts a new lineage with a new id.
            write(&heads.remove(&slug).unwrap(), "deleted", created, &actor)?;
        }
    }

    let pinned_at = now();
    for slug in pins.iter().collect::<BTreeSet<_>>() {
        let head = heads
            .get_mut(slug.as_str())
            .ok_or_else(|| invalid(&format!("--pin {slug}: no live memory has that slug")))?;
        if head.v1.get("pinned") == Some(&Value::Bool(true)) {
            continue;
        }
        let mut v1 = head.v1.clone();
        v1["pinned"] = json!(true);
        (head.path, head.content) = render_legacy(&v1)?;
        head.v1 = v1;
        head.versions += 1;
        write(head, "modified", &pinned_at, "migration:pin")?;
    }

    // The final set must equal the active revision and pass every v2 write rule.
    let active: String = tx.query_row(
        "SELECT r.body FROM revisions r JOIN state s ON r.id=s.value WHERE s.key='active'",
        [],
        |r| r.get(0),
    )?;
    let expected: BTreeSet<String> = decode(&active)?
        .as_array()
        .into_iter()
        .flatten()
        .map(without_pinned)
        .collect::<Result<_>>()?;
    let actual: BTreeSet<String> = heads
        .values()
        .map(|h| without_pinned(&h.v1))
        .collect::<Result<_>>()?;
    if offenders.is_empty() && expected != actual {
        return Err(invalid(
            "Replayed memories differ from the active revision; refusing to migrate",
        ));
    }
    let mut pinned_chars = 0;
    let mut memories = Vec::new();
    for (slug, head) in &heads {
        match parse(&head.path, &head.content) {
            Ok(memory) => {
                let pinned = memory.meta.get("pinned") == Some(&Value::Bool(true));
                if pinned {
                    pinned_chars += block_len(&memory);
                }
                memories.push(
                    json!({"slug":slug,"path":head.path,"versions":head.versions,"pinned":pinned}),
                );
            }
            Err(e) => {
                offenders.insert(format!("{slug} (live): {e}"));
            }
        }
    }
    if !offenders.is_empty() {
        let list: Vec<String> = offenders.into_iter().map(|o| format!("- {o}")).collect();
        return Err(invalid(&format!(
            "Migration aborted; nothing was changed. Offenders:\n{}",
            list.join("\n")
        )));
    }
    if pinned_chars > PINNED_BUDGET {
        return Err(invalid(&format!(
            "pinned_budget_exceeded: pinned memories total {pinned_chars} characters, over {PINNED_BUDGET}"
        )));
    }
    if dump(tx)? != legacy {
        return Err(invalid("Legacy tables changed during migration"));
    }
    memories.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let revision: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM memory_versions",
        [],
        |r| r.get(0),
    )?;
    Ok(json!({
        "dry_run": dry_run,
        "store_id": store,
        "legacy_revision": revisions.len() as i64 - 1,
        "revision": revision,
        "versions": counts,
        "pinned_chars": pinned_chars,
        "memories": memories,
    }))
}

/// render_legacy plus the round-trip check: parsing back must give the identical v1 object.
fn rendered(v1: &Value) -> Result<(String, String)> {
    let (path, content) = render_legacy(v1)?;
    if encode(&parse_legacy(&path, &content)?)? != encode(v1)? {
        return Err(invalid("does not round-trip exactly"));
    }
    Ok((path, content))
}

fn without_pinned(v1: &Value) -> Result<String> {
    let mut v1 = v1.clone();
    if let Some(fields) = v1.as_object_mut() {
        fields.remove("pinned");
    }
    encode(&v1)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

/// Schema and rows of the legacy tables, compared before and after.
fn dump(tx: &Transaction) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for table in LEGACY_TABLES {
        out.push(tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get(0),
        )?);
        let mut stmt = tx.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let values = (0..columns)
                .map(|i| {
                    row.get::<_, rusqlite::types::Value>(i)
                        .map(|v| format!("{v:?}"))
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out.push(values.join("|"));
        }
    }
    Ok(out)
}
