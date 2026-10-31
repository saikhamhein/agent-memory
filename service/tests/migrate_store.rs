//! Workstream M acceptance tests that need the v2 library API (design §3.6 step 6, §3.5 "pending", §3.3 start view):
//! what the migrated database looks like to `Store` and to recall.
mod legacy;

use agent_memory_server::{
    recall::{self, Request},
    store::Store,
};
use legacy::*;
use serde_json::{Value, json};

const T1: &str = "2026-10-02T10:00:00+00:00";
const T2: &str = "2026-10-03T11:30:00+00:00";

fn request(project: &str, role: &str, topics: &[&str]) -> Request {
    Request {
        project: project.into(),
        role: role.into(),
        topics: topics.iter().map(|t| t.to_string()).collect(),
        query: String::new(),
    }
}

#[test]
fn pending_observations_carry_over() {
    let consumed = observation("obs-consumed", T1);
    let pending = observation("obs-pending", T1);
    let in_unpromoted = observation("obs-unpromoted", T2);
    let db = v1_db(
        &[revision(T1, vec![mem("alpha", global(), "Alpha")])],
        &[consumed.clone(), pending.clone(), in_unpromoted.clone()],
        &[dream("dream-promoted", 0, &[&consumed], Some(1)), dream("dream-open", 1, &[&in_unpromoted], None)],
    );
    migrate(&db.db, &[]).assert_ok();
    let mut store = Store::open(&db.db, &db.mnt()).unwrap();
    let started: Value = store.dream_start(&json!([]), "test").unwrap();
    // Pending = not in the input of any promoted legacy dream (an unpromoted legacy dream consumed nothing).
    let listed = started["observations"].as_array().expect("dream lists its observations");
    let text = serde_json::to_string(listed).unwrap();
    assert_eq!(listed.len(), 2, "{text}");
    assert!(text.contains("obs-pending") && text.contains("obs-unpromoted"), "{text}");
    assert!(!text.contains("obs-consumed"), "{text}");
    // The rows themselves are untouched, consumed or not.
    let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 3);
}

#[test]
fn pinned_memories_appear_in_every_start_view() {
    let db = v1_db(
        &[revision(T1, vec![
            mem("keep-it-simple", global(), "Keep it simple"),
            mem("proj-rule", project("/home/sai/Projects/x"), "Project rule"),
            mem("plain", global(), "Not pinned"),
        ])],
        &[],
        &[],
    );
    migrate(&db.db, &["--pin", "keep-it-simple", "--pin", "proj-rule"]).assert_ok();
    let store = Store::open(&db.db, &db.mnt()).unwrap();
    let (revision, memories) = store.live_memories().unwrap();
    let mirror = db.mirror().display().to_string();
    for (label, req) in [
        ("no project", request("", "", &[])),
        ("other project", request("/home/sai/Projects/other", "", &[])),
        ("a role", request("", "coordinator", &[])),
        ("topics", request("", "", &["hardware", "ui"])),
    ] {
        let recalled = recall::recall(&memories, revision, &mirror, &req);
        assert!(recalled.pinned.contains(&"/global/keep-it-simple.md".to_string()), "{label}: {:?}", recalled.pinned);
        assert!(recalled.text.contains("Keep it simple"), "{label}");
        assert!(!recalled.pinned.iter().any(|p| p.contains("plain")), "{label}: unpinned memory listed as pinned");
        assert!(!recalled.pinned.iter().any(|p| p.contains("proj-rule")), "{label}: other project's memory");
    }
    for project in ["/home/sai/Projects/x", "/home/sai/Projects/x/sub/dir"] {
        let recalled = recall::recall(&memories, revision, &mirror, &request(project, "", &[]));
        assert!(recalled.pinned.contains(&"/projects/home/sai/Projects/x/proj-rule.md".to_string()), "{project}");
    }
}
