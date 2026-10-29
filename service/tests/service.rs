use agent_memory_server::{
    api,
    config::{Capability, Config, Token},
    model::{self, Recall},
    store::Store,
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};
use tempfile::TempDir;
use tower::ServiceExt;

fn memory(id: &str) -> Value {
    json!({"id":id,"title":id,"body":"Check existing evidence before asking. မြန်မာ",
        "scope":{"kind":"global"},"confidence":"explicit","tags":["evidence"],
        "evidence":[{"source":"test","locator":"fixture:1","note":"Test evidence","extra":"preserved"}]})
}
fn fixture() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("memory.sqlite3");
    Store::init(&path).unwrap();
    let store = Store::open(&path).unwrap();
    (tmp, store)
}
fn reviewed(dream: &Value, memories: Value) -> Value {
    let mut p = dream["proposal"].clone();
    p["memories"] = memories;
    p["reviewed"] = json!(true);
    p["summary"] = json!("Test revision");
    p["coverage"] = json!("Fixtures only");
    p
}
fn config(path: &Path) -> Config {
    let mut projects = BTreeMap::new();
    projects.insert("example".into(), "/projects/a".into());
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: path.into(),
        projects,
        tokens: vec![
            Token {
                name: "reader".into(),
                sha256: model::hash(b"reader"),
                capabilities: HashSet::from([Capability::Recall]),
            },
            Token {
                name: "admin".into(),
                sha256: model::hash(b"admin"),
                capabilities: HashSet::from([
                    Capability::Recall,
                    Capability::Observe,
                    Capability::Review,
                    Capability::Promote,
                ]),
            },
        ],
    }
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(String::from_utf8_lossy(&bytes))),
    )
}

#[tokio::test]
async fn authentication_and_capabilities() {
    let (tmp, store) = fixture();
    let app = api::router(config(&tmp.path().join("memory.sqlite3")), store);
    assert_eq!(
        request(&app, "GET", "/healthz", None, json!(null)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/v1/status", None, json!(null))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "GET", "/v1/status", Some("wrong"), json!(null))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "GET", "/v1/status", Some("reader"), json!(null))
            .await
            .0,
        StatusCode::OK
    );
    for (method, path) in [
        ("POST", "/v1/observations"),
        ("POST", "/v1/dreams"),
        ("POST", "/v1/proposals/promote"),
        ("POST", "/v1/rollback"),
        ("GET", "/v1/export"),
    ] {
        assert_eq!(
            request(&app, method, path, Some("reader"), json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
}

#[tokio::test]
async fn api_workflow_and_stale_revision_guard() {
    let (tmp, store) = fixture();
    let app = api::router(config(&tmp.path().join("memory.sqlite3")), store);
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/observations",
            Some("admin"),
            memory("first")
        )
        .await
        .0,
        StatusCode::OK
    );
    let (code, recall) = request(&app, "POST", "/v1/recall", Some("reader"), json!({})).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(recall["memories"], json!([]));
    let (code, dream) = request(&app, "POST", "/v1/dreams", Some("admin"), json!({})).await;
    assert_eq!(code, StatusCode::OK);
    let (_, second) = request(&app, "POST", "/v1/dreams", Some("admin"), json!({})).await;
    let proposal = reviewed(&dream, json!([memory("first")]));
    let (_, diff) = request(
        &app,
        "POST",
        "/v1/proposals/diff",
        Some("admin"),
        proposal.clone(),
    )
    .await;
    assert_eq!(diff["added"], json!([memory("first")]));
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/proposals/validate",
            Some("admin"),
            proposal.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/proposals/promote",
            Some("admin"),
            proposal.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/proposals/promote",
            Some("admin"),
            proposal
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/proposals/promote",
            Some("admin"),
            reviewed(&second, json!([]))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, recall) = request(&app, "POST", "/v1/recall", Some("reader"), json!({})).await;
    assert_eq!(recall["revision"], 1);
    assert_eq!(recall["memories"], json!([memory("first")]));
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/rollback",
            Some("admin"),
            json!({"revision":0,"expected":0,"reason":"test"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, rollback) = request(
        &app,
        "POST",
        "/v1/rollback",
        Some("admin"),
        json!({"revision":0,"expected":1,"reason":"test"}),
    )
    .await;
    assert_eq!(rollback["revision"], 2);
}

#[tokio::test]
async fn project_aliases_and_request_validation() {
    let (tmp, mut store) = fixture();
    let mut project = memory("project");
    project["scope"] = json!({"kind":"project","value":"/projects/a"});
    let dream = store.dream().unwrap();
    store
        .promote(reviewed(&dream, json!([project.clone()])))
        .unwrap();
    let app = api::router(config(&tmp.path().join("memory.sqlite3")), store);
    let (_, result) = request(
        &app,
        "POST",
        "/v1/recall",
        Some("reader"),
        json!({"project_id":"example"}),
    )
    .await;
    assert_eq!(result["memories"], json!([project]));
    let (_, result) = request(
        &app,
        "POST",
        "/v1/recall",
        Some("reader"),
        json!({"project":"/projects/ab"}),
    )
    .await;
    assert_eq!(result["memories"], json!([]));
    for body in [
        json!({"project_id":"unknown"}),
        json!({"project_id":"example","project":"/a"}),
        json!({"limit":0}),
        json!({"budget":1}),
    ] {
        assert_eq!(
            request(&app, "POST", "/v1/recall", Some("reader"), body)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let body = json!({"query":"x".repeat(4*1024*1024)});
    assert_eq!(
        request(&app, "POST", "/v1/recall", Some("reader"), body)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn snapshots_review_gates_and_late_observations() {
    let (_tmp, mut store) = fixture();
    let first = store.observe(memory("first")).unwrap();
    let dream = store.dream().unwrap();
    let late = store.observe(memory("late")).unwrap();
    let mut p = reviewed(&dream, json!([memory("first")]));
    p["reviewed"] = json!(false);
    assert!(store.promote(p.clone()).is_err());
    p["reviewed"] = json!(true);
    p["coverage"] = json!("");
    assert!(store.promote(p.clone()).is_err());
    p["coverage"] = json!("fixtures");
    p["conflicts"] = json!([{"status":"unresolved"}]);
    assert!(store.promote(p.clone()).is_err());
    p["conflicts"] = json!([]);
    store.promote(p.clone()).unwrap();
    let saved = store.get_dream(p["dream_id"].as_str().unwrap()).unwrap();
    assert_eq!(saved["input"], dream["input"]);
    assert_eq!(saved["proposal"], p);
    assert_eq!(dream["input"]["observations"][0]["id"], first["id"]);
    let next = store.dream().unwrap();
    assert_eq!(next["input"]["observations"].as_array().unwrap().len(), 1);
    assert_eq!(next["input"]["observations"][0]["id"], late["id"]);
}

#[test]
fn scope_expiry_budget_and_credentials() {
    let mut expired = memory("old");
    expired["expires"] = json!("2000-01-01");
    let mut tentative = memory("guess");
    tentative["confidence"] = json!("tentative");
    let mut topic = memory("topic");
    topic["scope"] = json!({"kind":"topic","value":"hardware"});
    let memories = json!([memory("global"), expired, tentative, topic.clone()]);
    let request = Recall {
        topics: vec!["hardware".into()],
        budget: Some(300),
        ..Default::default()
    };
    let output = model::recall(&memories, 3, &request).unwrap();
    assert_eq!(output["memories"], json!([topic, memory("global")]));
    assert!(output["text"].as_str().unwrap().chars().count() <= 300);
    let mut secret = memory("secret");
    secret["body"] = json!("api_key=abcdefghijklmnop");
    assert!(model::validate(&json!([secret])).is_err());
    let mut bad = memory("bad");
    bad["evidence"] = json!([]);
    assert!(model::validate(&json!([bad])).is_err());
}

#[test]
fn restart_backup_and_python_compatibility() {
    let (tmp, mut store) = fixture();
    let mut saved = memory("saved");
    saved["evidence"][0]["numbers"] =
        serde_json::from_str("[18446744073709551617, -18446744073709551617, 1e-07, 1e+20, -0.0]")
            .unwrap();
    let dream = store.dream().unwrap();
    store
        .promote(reviewed(&dream, json!([saved.clone()])))
        .unwrap();
    drop(store);
    let path = tmp.path().join("memory.sqlite3");
    let store = Store::open(&path).unwrap();
    assert_eq!(store.active().unwrap(), (1, json!([saved.clone()])));
    let backup = tmp.path().join("backup.sqlite3");
    Store::backup(&path, &backup).unwrap();
    assert!(Store::backup(&path, &backup).is_err());
    assert_eq!(
        Store::open(&backup).unwrap().active().unwrap(),
        store.active().unwrap()
    );
    let script = r#"
import json, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import memory
root=Path(sys.argv[2])
db=memory.connect(root)
n, items=memory.active(db)
assert n == 1 and items[0]['id'] == 'saved'
assert items[0]['evidence'][0]['numbers'] == [18446744073709551617, -18446744073709551617, 1e-7, 1e20, -0.0]
assert db.execute('SELECT sha256 FROM revisions WHERE id=1').fetchone()[0] == memory.digest(items)
path=memory.new_dream(db, root)
p=json.loads(path.read_text())
p.update(summary='Python roundtrip', coverage='Fixture', reviewed=True)
memory.atomic_json(path,p)
assert memory.promote(db,path) == 2
db.close()
"#;
    let output = std::process::Command::new("python3")
        .args([
            "-c",
            script,
            concat!(env!("CARGO_MANIFEST_DIR"), "/.."),
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(store.active().unwrap(), (2, json!([saved])));
}

#[test]
fn numeric_encoding_matches_python() {
    // Differential coverage across binary64 exponents, signed zero, subnormals,
    // formatting boundaries, integers larger than u64/i128 and nested values.
    let script = r#"
import json, math, random, struct, sys
sys.path.insert(0, sys.argv[1])
import memory
rng = random.Random(812)
values = [0.0, -0.0, 1e-7, 1e-5, 1e-4, 1e15, 1e16, 1e20, 1e23,
          5e-324, 1.7976931348623157e308, 18446744073709551617, -(2**130),
          {'nested': [1.2345678901234567, '\u0001 မြန်မာ 😀', {}, []]}]
for _ in range(5000):
    number = struct.unpack('!d', rng.getrandbits(64).to_bytes(8, 'big'))[0]
    if math.isfinite(number):
        values.append(number)
print(json.dumps({'value': values, 'encoded': memory.encode(values)}, ensure_ascii=False))
"#;
    let output = std::process::Command::new("python3")
        .args(["-c", script, concat!(env!("CARGO_MANIFEST_DIR"), "/..")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        model::encode(&data["value"]).unwrap(),
        data["encoded"].as_str().unwrap()
    );
    let overflowing: Value = serde_json::from_str("[1e999]").unwrap();
    assert!(model::encode(&overflowing).is_err());
}

#[test]
fn missing_store_and_bad_config_fail_closed() {
    let tmp = TempDir::new().unwrap();
    assert!(Store::open(&tmp.path().join("missing.sqlite3")).is_err());
    let mut config = config(&tmp.path().join("memory.sqlite3"));
    config.tokens.clear();
    assert!(config.validate().is_err());
}

#[test]
fn concurrent_promotions_cannot_overwrite_each_other() {
    let (tmp, mut store) = fixture();
    let first = reviewed(&store.dream().unwrap(), json!([memory("first")]));
    let second = reviewed(&store.dream().unwrap(), json!([memory("second")]));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .map(|proposal| {
            let barrier = barrier.clone();
            let path = tmp.path().join("memory.sqlite3");
            std::thread::spawn(move || {
                let mut writer = Store::open(&path).unwrap();
                barrier.wait();
                writer.promote(proposal)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(agent_memory_server::Error::Conflict(_))))
            .count(),
        1
    );
    assert_eq!(store.active().unwrap().0, 1);
}
