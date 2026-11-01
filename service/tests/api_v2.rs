//! Acceptance tests for workstream H (design §4.1 HTTP API, §4.3 "H").
//!
//! Every test drives the real axum router in-process (`tower::ServiceExt::oneshot`)
//! on a temporary store and mirror directory; nothing touches the live store.
//! Run: `cargo test --locked --manifest-path service/Cargo.toml --test api_v2`.
//!
//! Assumptions that the §4.1 table leaves open are marked `ASSUMPTION` and were
//! sent to the architect; a ruling changes the test, not the engineer's code.

use agent_memory_server::{
    api,
    config::{Capability, Config, Token},
    recall,
    store::Store,
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tower::ServiceExt;

const MIB4: usize = 4 * 1024 * 1024;
const NO_ID: &str = "mem_00000000000000000000000000000000";
const NO_STORE: &str = "memstore_00000000000000000000000000000000";

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Valid memory content (§3.1 file format); `extra` keys are merged into the frontmatter.
fn content_with(title: &str, body: &str, extra: Value) -> String {
    let mut meta = json!({
        "title": title, "confidence": "explicit", "tags": ["http"],
        "evidence": [{"source": "test", "locator": "api_v2", "note": "fixture"}]
    });
    for (k, v) in extra.as_object().unwrap() {
        meta[k] = v.clone();
    }
    format!("---\n{meta}\n---\n{body}")
}
/// Pinned cost of a memory (design §3.1, `model::block_len`): the characters of the block recall prints for it,
/// `### {title} [{path}]\n{body}\n\n`, with the body's trailing whitespace trimmed. Written out here on purpose.
fn block_len(title: &str, path: &str, body: &str) -> usize {
    format!("### {title} [{path}]\n{}\n\n", body.trim_end())
        .chars()
        .count()
}

fn content(title: &str, body: &str) -> String {
    content_with(title, body, json!({}))
}

const TOKENS: [(&str, &[Capability]); 6] = [
    ("reader", &[Capability::Recall]),
    ("observer", &[Capability::Observe]),
    ("reviewonly", &[Capability::Review]),
    ("reviewer", &[Capability::Recall, Capability::Review]),
    ("maintainer", &[Capability::Recall, Capability::Promote]),
    (
        "admin",
        &[
            Capability::Recall,
            Capability::Observe,
            Capability::Review,
            Capability::Promote,
        ],
    ),
];

fn config(db: &Path, mnt: &Path) -> Config {
    // Fix round (X15, X16): the engineer deletes `projects` and `mount` from this literal.
    let mut projects = BTreeMap::new();
    projects.insert("example".into(), "/projects/a".into());
    Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        database: db.into(),
        projects,
        tokens: TOKENS
            .iter()
            .map(|(name, caps)| Token {
                name: (*name).into(),
                sha256: sha256(name.as_bytes()),
                capabilities: HashSet::from_iter(caps.iter().copied()),
            })
            .collect(),
        mount: Some(mnt.into()),
    }
}

struct Fx {
    tmp: TempDir,
    app: Router,
    db: PathBuf,
    mnt: PathBuf,
}

impl Fx {
    fn new() -> Self {
        Self::seeded(|_| {})
    }
    /// `prepare` may edit the initialised database (raw SQL) before the service opens it.
    fn seeded(prepare: impl FnOnce(&Path)) -> Self {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("memory.sqlite3");
        let mnt = tmp.path().join("mnt");
        Store::init(&db).unwrap();
        prepare(&db);
        let store = Store::open(&db, &mnt).unwrap();
        let app = api::router(config(&db, &mnt), store);
        Fx { tmp, app, db, mnt }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        call(&self.app, method, path, Some(token), body).await
    }
    /// Setup call by the admin token that must succeed with 200.
    /// POST with a raw (here: empty) body.
    async fn call_bytes_to(&self, path: &str, token: &str, body: Vec<u8>) -> (StatusCode, Value) {
        call_bytes(&self.app, "POST", path, Some(token), body).await
    }
    async fn ok(&self, method: &str, path: &str, body: Value) -> Value {
        let (code, value) = self.call(method, path, "admin", body).await;
        assert_eq!(code, StatusCode::OK, "{method} {path}: {value}");
        value
    }
    async fn create(&self, store: &str, path: &str, content: &str) -> Value {
        self.ok(
            "POST",
            &format!("/v1/memory_stores/{store}/memories"),
            json!({"path": path, "content": content}),
        )
        .await
    }
    async fn update(&self, store: &str, id: &str, content: &str) -> Value {
        self.ok(
            "POST",
            &format!("/v1/memory_stores/{store}/memories/{id}"),
            json!({"content": content}),
        )
        .await
    }
    async fn dream(&self) -> Value {
        self.ok("POST", "/v1/dreams", json!({"sessions": []})).await
    }
    async fn adopt(&self, token: &str, id: &str) -> (StatusCode, Value) {
        self.call("POST", &format!("/v1/dreams/{id}/adopt"), token, review())
            .await
    }
    async fn status(&self) -> Value {
        self.call("GET", "/v1/status", "reader", Value::Null)
            .await
            .1
    }
    /// An independent `Store` on the same database, as the oracle for what the service holds.
    fn oracle(&self) -> Store {
        Store::open(&self.db, &self.mnt).unwrap()
    }
}

fn review() -> Value {
    json!({"summary": "Test adoption", "coverage": "Fixtures only", "conflicts": []})
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    call_bytes(app, method, path, token, bytes).await
}

async fn call_bytes(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Vec<u8>,
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
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(String::from_utf8_lossy(&bytes))),
    )
}

fn assert_conflict(result: &(StatusCode, Value), kind: &str, paths: Option<&[&str]>) {
    let (code, body) = result;
    assert_eq!(*code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["type"], kind, "{body}");
    assert!(
        body["error"].as_str().is_some_and(|s| !s.is_empty()),
        "{body}"
    );
    // §4.1: every 409 carries `paths` except store_archived.
    if kind == "store_archived" {
        assert!(body.get("paths").is_none(), "{body}");
    } else if let Some(paths) = paths {
        let mut got: Vec<_> = body["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        got.sort_by_key(ToString::to_string);
        let mut want: Vec<_> = paths.iter().map(|p| json!(p)).collect();
        want.sort_by_key(ToString::to_string);
        assert_eq!(got, want, "{body}");
    } else {
        assert!(
            body["paths"].as_array().is_some_and(|p| !p.is_empty()),
            "{body}"
        );
    }
}

/// §4.1: every 400 has type `invalid`, 404 has type `not_found`; the message names the reason.
fn assert_invalid(result: &(StatusCode, Value), reason: &str) {
    let (code, body) = result;
    assert_eq!(*code, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["type"], "invalid", "{body}");
    let message = body["error"].as_str().unwrap_or_default();
    assert!(!message.is_empty() && message.contains(reason), "{body}");
}

fn versions_of(list: &Value) -> Vec<Value> {
    list["data"].as_array().unwrap().clone()
}

// ---------------------------------------------------------------------------
// 1. capability_matrix
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Need {
    Recall,
    Observe,
    Review,
    Promote,
}
impl Need {
    fn cap(self) -> Capability {
        match self {
            Need::Recall => Capability::Recall,
            Need::Observe => Capability::Observe,
            Need::Review => Capability::Review,
            Need::Promote => Capability::Promote,
        }
    }
}

/// §4.1: live reads need `recall`, live writes and redact/adopt need `promote`,
/// reads and writes of any other store and all dream calls need `review`,
/// observations need `observe`. Each token gets a fresh store so that every
/// operation is attempted in the same state; an allowed operation must succeed
/// (200) and a denied one must be 403.
#[tokio::test]
async fn capability_matrix() {
    for (token, caps) in TOKENS {
        let fx = Fx::new();
        let a = fx
            .create("live", "/global/a.md", &content("Alpha", "alpha body"))
            .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let b = fx
            .create("live", "/global/b.md", &content("Beta", "beta body"))
            .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        fx.update("live", &a, &content("Alpha two", "alpha body two"))
            .await;
        let old = versions_of(
            &fx.ok(
                "GET",
                &format!("/v1/memory_stores/live/memory_versions?memory_id={a}"),
                Value::Null,
            )
            .await,
        )
        .last()
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let d2 = fx.dream().await;
        let (d2_id, out) = (
            d2["id"].as_str().unwrap().to_owned(),
            d2["output_store_id"].as_str().unwrap().to_owned(),
        );

        let live = "/v1/memory_stores/live";
        let o = format!("/v1/memory_stores/{out}");
        let ops: Vec<(&str, &str, String, Value, Need)> = vec![
            (
                "status",
                "GET",
                "/v1/status".into(),
                Value::Null,
                Need::Recall,
            ),
            (
                "recall",
                "POST",
                "/v1/recall".into(),
                json!({}),
                Need::Recall,
            ),
            (
                "live list",
                "GET",
                format!("{live}/memories"),
                Value::Null,
                Need::Recall,
            ),
            (
                "live get",
                "GET",
                format!("{live}/memories/{a}"),
                Value::Null,
                Need::Recall,
            ),
            (
                "live versions",
                "GET",
                format!("{live}/memory_versions"),
                Value::Null,
                Need::Review,
            ),
            (
                "live version",
                "GET",
                format!("{live}/memory_versions/{old}"),
                Value::Null,
                Need::Review,
            ),
            (
                "live create",
                "POST",
                format!("{live}/memories"),
                json!({"path": "/global/c.md", "content": content("Gamma", "gamma body")}),
                Need::Promote,
            ),
            (
                "live update",
                "POST",
                format!("{live}/memories/{b}"),
                json!({"content": content("Beta two", "beta body two")}),
                Need::Promote,
            ),
            (
                "live delete",
                "DELETE",
                format!("{live}/memories/{b}"),
                Value::Null,
                Need::Promote,
            ),
            (
                "redact",
                "POST",
                format!("{live}/memory_versions/{old}/redact"),
                json!({}),
                Need::Promote,
            ),
            (
                "observe",
                "POST",
                "/v1/observations".into(),
                json!({"path": "/global/obs-matrix.md", "content": content("Observed", "seen once")}),
                Need::Observe,
            ),
            (
                "dream list",
                "GET",
                "/v1/dreams".into(),
                Value::Null,
                Need::Review,
            ),
            (
                "dream get",
                "GET",
                format!("/v1/dreams/{d2_id}?diff=true"),
                Value::Null,
                Need::Review,
            ),
            (
                "output list",
                "GET",
                format!("{o}/memories"),
                Value::Null,
                Need::Review,
            ),
            (
                "output get",
                "GET",
                format!("{o}/memories/{a}"),
                Value::Null,
                Need::Review,
            ),
            (
                "output versions",
                "GET",
                format!("{o}/memory_versions"),
                Value::Null,
                Need::Review,
            ),
            (
                "output create",
                "POST",
                format!("{o}/memories"),
                json!({"path": "/global/o.md", "content": content("Omega", "omega body")}),
                Need::Review,
            ),
            (
                "output update",
                "POST",
                format!("{o}/memories/{a}"),
                json!({"content": content("Alpha three", "alpha body three")}),
                Need::Review,
            ),
            (
                "output delete",
                "DELETE",
                format!("{o}/memories/{b}"),
                Value::Null,
                Need::Review,
            ),
            (
                "dream cancel",
                "POST",
                format!("/v1/dreams/{d2_id}/cancel"),
                json!({}),
                Need::Review,
            ),
            // One dream runs at a time (X10): start follows cancel, and adopt
            // takes whichever dream is then running (d2, or the one just started).
            (
                "dream start",
                "POST",
                "/v1/dreams".into(),
                json!({"sessions": []}),
                Need::Review,
            ),
            (
                "dream adopt",
                "POST",
                "/v1/dreams/{running}/adopt".into(),
                review(),
                Need::Promote,
            ),
        ];
        for (name, method, path, body, need) in ops {
            let path = if path.contains("{running}") {
                let dreams = fx.ok("GET", "/v1/dreams", Value::Null).await;
                let running = dreams["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|d| d["status"] == "running")
                    .expect("one dream is running")["id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                path.replace("{running}", &running)
            } else {
                path
            };
            let (code, response) = fx.call(method, &path, token, body).await;
            if caps.contains(&need.cap()) {
                assert_eq!(
                    code,
                    StatusCode::OK,
                    "{token} should be allowed {name}: {response}"
                );
            } else {
                assert_eq!(
                    code,
                    StatusCode::FORBIDDEN,
                    "{token} should be denied {name}: {response}"
                );
            }
        }
    }
}

#[tokio::test]
async fn authentication_applies_to_every_route() {
    let fx = Fx::new();
    let store = "/v1/memory_stores/live";
    let routes = [
        ("GET", "/v1/status".to_string()),
        ("POST", "/v1/recall".into()),
        ("GET", format!("{store}/memories")),
        ("GET", format!("{store}/memories/{NO_ID}")),
        ("POST", format!("{store}/memories")),
        ("POST", format!("{store}/memories/{NO_ID}")),
        ("DELETE", format!("{store}/memories/{NO_ID}")),
        ("GET", format!("{store}/memory_versions")),
        ("GET", format!("{store}/memory_versions/memver_x")),
        ("POST", format!("{store}/memory_versions/memver_x/redact")),
        ("POST", "/v1/observations".into()),
        ("POST", "/v1/dreams".into()),
        ("GET", "/v1/dreams".into()),
        ("GET", "/v1/dreams/drm_x".into()),
        ("POST", "/v1/dreams/drm_x/adopt".into()),
        ("POST", "/v1/dreams/drm_x/cancel".into()),
    ];
    for (method, path) in routes {
        for token in [None, Some("wrong")] {
            let (code, _) = call(&fx.app, method, &path, token, json!({})).await;
            assert_eq!(code, StatusCode::UNAUTHORIZED, "{method} {path} {token:?}");
        }
    }
    assert_eq!(
        call(&fx.app, "GET", "/healthz", None, Value::Null).await.0,
        StatusCode::OK
    );
}

// ---------------------------------------------------------------------------
// 2. live_alias
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_alias() {
    let fx = Fx::new();
    let created = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let id = created["id"].as_str().unwrap();
    let store_id = fx.status().await["store_id"].as_str().unwrap().to_owned();
    assert!(store_id.starts_with("memstore_") && store_id.len() == "memstore_".len() + 32);
    // Reads agree through both names (`reviewer` holds recall and review, which version reads need).
    for suffix in [
        "memories".to_string(),
        format!("memories/{id}"),
        "memory_versions".to_string(),
    ] {
        let alias = fx
            .call(
                "GET",
                &format!("/v1/memory_stores/live/{suffix}"),
                "reviewer",
                Value::Null,
            )
            .await;
        let direct = fx
            .call(
                "GET",
                &format!("/v1/memory_stores/{store_id}/{suffix}"),
                "reviewer",
                Value::Null,
            )
            .await;
        assert_eq!(alias.0, StatusCode::OK, "{suffix}");
        assert_eq!(alias, direct, "{suffix}");
    }
    // A write through the real id is visible through the alias, and the reverse.
    let via_id = fx
        .create(&store_id, "/global/b.md", &content("Beta", "beta body"))
        .await;
    let alias_get = fx
        .ok(
            "GET",
            &format!(
                "/v1/memory_stores/live/memories/{}",
                via_id["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert_eq!(alias_get["path"], "/global/b.md");
    fx.update("live", id, &content("Alpha two", "alpha body two"))
        .await;
    let direct = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/{store_id}/memories/{id}"),
            Value::Null,
        )
        .await;
    assert_eq!(direct["content"], content("Alpha two", "alpha body two"));
    // The real id is live for capability purposes: reader may not write through it.
    let denied = fx
        .call(
            "POST",
            &format!("/v1/memory_stores/{store_id}/memories"),
            "reader",
            json!({"path": "/global/c.md", "content": content("Gamma", "gamma body")}),
        )
        .await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    // ... nor read its versions, through either name.
    for name in ["live", store_id.as_str()] {
        let path = format!("/v1/memory_stores/{name}/memory_versions");
        assert_eq!(
            fx.call("GET", &path, "reader", Value::Null).await.0,
            StatusCode::FORBIDDEN,
            "{name}"
        );
    }
}

// ---------------------------------------------------------------------------
// Object shapes: status, list, get, create/update/delete, versions, redact
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_shape_and_counters() {
    let fx = Fx::new();
    let s = fx.status().await;
    assert_eq!(s["revision"], 0);
    assert_eq!(s["memories"], 0);
    assert_eq!(s["pinned_chars"], 0);
    assert_eq!(s["observations_pending"], 0);
    assert_eq!(s["dreams_running"], 0);
    assert_eq!(s["mirror_stale"], false);
    let mirror_dir = s["mirror_dir"].as_str().unwrap();
    assert!(
        mirror_dir.starts_with(fx.mnt.to_str().unwrap()),
        "{mirror_dir}"
    );
    assert!(mirror_dir.ends_with("shared-agent-memory"), "{mirror_dir}");
    let keys: HashSet<_> = s.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        HashSet::from_iter(
            [
                "store_id",
                "revision",
                "memories",
                "pinned_chars",
                "observations_pending",
                "dreams_running",
                "mirror_dir",
                "mirror_stale"
            ]
            .map(String::from)
        )
    );

    fx.create(
        "live",
        "/global/a.md",
        &content_with("Pinned rule", "twelve chars", json!({"pinned": true})),
    )
    .await;
    fx.create("live", "/global/b.md", &content("Plain", "not counted"))
        .await;
    fx.ok(
        "POST",
        "/v1/observations",
        json!({"path": "/global/seen.md", "content": content("Seen", "once")}),
    )
    .await;
    fx.dream().await;
    let s = fx.status().await;
    assert_eq!(
        s["revision"], 2,
        "revision is the live store's version count"
    );
    assert_eq!(s["memories"], 2);
    assert_eq!(
        s["pinned_chars"],
        block_len("Pinned rule", "/global/a.md", "twelve chars")
    );
    assert_eq!(s["observations_pending"], 1);
    assert_eq!(s["dreams_running"], 1);
}

#[tokio::test]
async fn read_shapes_and_byte_exact_content() {
    let fx = Fx::new();
    // Non-canonical JSON spacing and Burmese text must come back byte-exact (§4.3 A4 over HTTP).
    let raw = "---\n{\"title\" :  \"Spaced\",\"confidence\":\"explicit\",\"tags\":[\"x\"],\
        \"evidence\":[{\"source\":\"t\",\"locator\":\"l\",\"note\":\"n\"}]}\n---\nမြန်မာ body\n";
    let created = fx.create("live", "/global/spaced.md", raw).await;
    let id = created["id"].as_str().unwrap();
    let hash = sha256(raw.as_bytes());
    assert!(id.starts_with("mem_") && id.len() == 4 + 32, "{id}");
    assert_eq!(created["path"], "/global/spaced.md");
    assert_eq!(created["content_sha256"], hash);
    assert!(
        created["memory_version_id"]
            .as_str()
            .unwrap()
            .starts_with("memver_")
    );
    assert!(created["updated_at"].as_str().is_some());

    fx.create(
        "live",
        "/topics/hardware/usb.md",
        &content("USB power", "usb body"),
    )
    .await;
    fx.create(
        "live",
        "/projects/p/q/proj.md",
        &content("Project", "project body"),
    )
    .await;

    let all = fx
        .ok("GET", "/v1/memory_stores/live/memories", Value::Null)
        .await;
    let rows = all["data"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let row = rows.iter().find(|r| r["id"] == id).unwrap();
    let keys: HashSet<_> = row.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        HashSet::from_iter(
            [
                "id",
                "path",
                "title",
                "content_sha256",
                "memory_version_id",
                "updated_at"
            ]
            .map(String::from)
        ),
        "list rows carry metadata and no content"
    );
    assert_eq!(row["title"], "Spaced");
    // path_prefix narrows by whole prefix.
    for (prefix, expect) in [
        ("/global/", 1),
        ("/topics/", 1),
        ("/projects/p/", 1),
        ("/", 3),
        ("/roles/", 0),
    ] {
        let r = fx
            .ok(
                "GET",
                &format!("/v1/memory_stores/live/memories?path_prefix={prefix}"),
                Value::Null,
            )
            .await;
        assert_eq!(r["data"].as_array().unwrap().len(), expect, "{prefix}");
    }

    let got = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/live/memories/{id}"),
            Value::Null,
        )
        .await;
    assert_eq!(got["content"], raw);
    assert_eq!(got["content_sha256"], hash);
    assert_eq!(got["path"], "/global/spaced.md");
}

#[tokio::test]
async fn versions_and_redact() {
    let fx = Fx::new();
    let v1_text = content("First", "secret-ish first body");
    let m = fx.create("live", "/global/a.md", &v1_text).await;
    let id = m["id"].as_str().unwrap().to_owned();
    fx.update("live", &id, &content("Second", "second body"))
        .await;
    fx.create("live", "/global/other.md", &content("Other", "other body"))
        .await;

    let list = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
            Value::Null,
        )
        .await;
    let versions = versions_of(&list);
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["operation"], "modified", "newest first");
    assert_eq!(versions[1]["operation"], "created");
    for v in &versions {
        assert!(v["id"].as_str().unwrap().starts_with("memver_"));
        assert_eq!(v["memory_id"], id.as_str());
        assert_eq!(v["created_by"], "token:admin");
        assert!(v.get("content").is_none(), "list omits content: {v}");
        assert!(v["redacted_at"].is_null() && v["redacted_by"].is_null());
    }
    assert_eq!(
        fx.ok("GET", "/v1/memory_stores/live/memory_versions", Value::Null)
            .await["data"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    let old_id = versions[1]["id"].as_str().unwrap();
    let old = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions/{old_id}"),
            Value::Null,
        )
        .await;
    assert_eq!(old["content"], v1_text);
    assert_eq!(old["content_sha256"], sha256(v1_text.as_bytes()));
    assert_eq!(old["path"], "/global/a.md");

    // The head of a live memory cannot be redacted (§3.2).
    let head_id = versions[0]["id"].as_str().unwrap();
    let refused = fx
        .call(
            "POST",
            &format!("/v1/memory_stores/live/memory_versions/{head_id}/redact"),
            "admin",
            json!({}),
        )
        .await;
    assert_invalid(&refused, "");
    // An older version can: its fields are nulled, its history stays.
    let redacted = fx
        .ok(
            "POST",
            &format!("/v1/memory_stores/live/memory_versions/{old_id}/redact"),
            json!({}),
        )
        .await;
    assert_eq!(redacted["id"], old_id);
    assert!(
        redacted["content"].is_null()
            && redacted["path"].is_null()
            && redacted["content_sha256"].is_null()
    );
    assert!(redacted["redacted_at"].as_str().is_some());
    assert_eq!(redacted["redacted_by"], "token:admin");
    assert_eq!(redacted["created_by"], "token:admin");
    assert_eq!(redacted["operation"], "created");
    let again = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions/{old_id}"),
            Value::Null,
        )
        .await;
    assert!(again["content"].is_null() && again["redacted_by"] == "token:admin");
    // The head stays readable.
    let got = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/live/memories/{id}"),
            Value::Null,
        )
        .await;
    assert_eq!(got["content"], content("Second", "second body"));
}

// ---------------------------------------------------------------------------
// Preconditions over HTTP
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preconditions_over_http() {
    let fx = Fx::new();
    let url = "/v1/memory_stores/live/memories";
    let m = fx
        .create("live", "/global/a.md", &content("One", "one body"))
        .await;
    let id = m["id"].as_str().unwrap().to_owned();
    let sha = m["content_sha256"].as_str().unwrap().to_owned();
    let count_versions = || async {
        versions_of(
            &fx.ok(
                "GET",
                &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
                Value::Null,
            )
            .await,
        )
        .len()
    };
    assert_eq!(count_versions().await, 1);

    // A stale hash is refused and leaves no version behind.
    let stale = fx.call("POST", &format!("{url}/{id}"), "admin", json!({
        "content": content("Two", "two body"),
        "precondition": {"type": "content_sha256", "content_sha256": sha256(b"something else")}
    })).await;
    assert_conflict(&stale, "precondition_failed", Some(&["/global/a.md"]));
    assert_eq!(count_versions().await, 1);
    assert_eq!(
        fx.ok("GET", &format!("{url}/{id}"), Value::Null).await["content_sha256"],
        sha.as_str()
    );

    // A matching hash creates a new version id and the memory keeps its id.
    let two = content("Two", "two body");
    let updated = fx
        .ok(
            "POST",
            &format!("{url}/{id}"),
            json!({
                "content": two, "precondition": {"type": "content_sha256", "content_sha256": sha}
            }),
        )
        .await;
    assert_eq!(updated["id"], id.as_str());
    assert_ne!(updated["memory_version_id"], m["memory_version_id"]);
    assert_eq!(updated["content_sha256"], sha256(two.as_bytes()));
    assert_eq!(count_versions().await, 2);
    // Replaying the same precondition now fails: the stored hash moved on.
    let replay = fx
        .call(
            "POST",
            &format!("{url}/{id}"),
            "admin",
            json!({
                "content": content("Three", "three body"),
                "precondition": {"type": "content_sha256", "content_sha256": sha}
            }),
        )
        .await;
    assert_conflict(&replay, "precondition_failed", Some(&["/global/a.md"]));

    // Without a precondition an update applies unconditionally.
    fx.update("live", &id, &content("Three", "three body"))
        .await;
    assert_eq!(count_versions().await, 3);

    // Rename keeps the id; history shows both paths.
    let renamed = fx
        .ok(
            "POST",
            &format!("{url}/{id}"),
            json!({"path": "/topics/misc/a.md"}),
        )
        .await;
    assert_eq!(renamed["id"], id.as_str());
    assert_eq!(renamed["path"], "/topics/misc/a.md");
    let paths: Vec<_> = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
            Value::Null,
        )
        .await,
    )
    .iter()
    .map(|v| v["path"].clone())
    .collect();
    assert_eq!(paths.first().unwrap(), "/topics/misc/a.md");
    assert!(paths.contains(&json!("/global/a.md")));

    // Delete with a stale hash is refused; with the current hash it succeeds and the lineage stays.
    let now = fx.ok("GET", &format!("{url}/{id}"), Value::Null).await["content_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused = fx
        .call(
            "DELETE",
            &format!("{url}/{id}?expected_content_sha256={}", sha256(b"nope")),
            "admin",
            Value::Null,
        )
        .await;
    assert_conflict(&refused, "precondition_failed", None);
    let deleted = fx
        .ok(
            "DELETE",
            &format!("{url}/{id}?expected_content_sha256={now}"),
            Value::Null,
        )
        .await;
    assert_eq!(deleted, json!({"type": "memory_deleted", "id": id}));
    assert_eq!(
        fx.call("GET", &format!("{url}/{id}"), "admin", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let history = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(history[0]["operation"], "deleted");
    assert_eq!(
        history[0]["path"], "/topics/misc/a.md",
        "a deleted version keeps the last path"
    );
    let tomb = fx
        .ok(
            "GET",
            &format!(
                "/v1/memory_stores/live/memory_versions/{}",
                history[0]["id"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
    assert!(
        tomb["content"].is_null() && tomb["content_sha256"].is_null(),
        "{tomb}"
    );
    assert!(history.len() >= 5);
    // Deleting twice is a 404, not a second tombstone.
    assert_eq!(
        fx.call("DELETE", &format!("{url}/{id}"), "admin", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// §4.3 A7 through HTTP: eight concurrent updates carrying the same hash, one winner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_http_writers_one_winner() {
    let fx = Fx::new();
    let m = fx
        .create("live", "/global/a.md", &content("Start", "start body"))
        .await;
    let (id, sha) = (
        m["id"].as_str().unwrap().to_owned(),
        m["content_sha256"].as_str().unwrap().to_owned(),
    );
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let app = fx.app.clone();
            let (url, sha) = (format!("/v1/memory_stores/live/memories/{id}"), sha.clone());
            tokio::spawn(async move {
                call(
                    &app,
                    "POST",
                    &url,
                    Some("admin"),
                    json!({
                        "content": content(&format!("Racer {i}"), "racer body"),
                        "precondition": {"type": "content_sha256", "content_sha256": sha}
                    }),
                )
                .await
            })
        })
        .collect();
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    assert_eq!(
        results.iter().filter(|r| r.0 == StatusCode::OK).count(),
        1,
        "{results:?}"
    );
    for r in results.iter().filter(|r| r.0 != StatusCode::OK) {
        assert_conflict(r, "precondition_failed", None);
    }
    let versions = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(versions.len(), 2);
}

// ---------------------------------------------------------------------------
// 3. error_mapping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn error_mapping_conflicts() {
    let fx = Fx::new();
    let url = "/v1/memory_stores/live/memories";
    let a = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;

    // path_exists: create on a taken path, and a rename onto one.
    let again = fx
        .call(
            "POST",
            url,
            "admin",
            json!({"path": "/global/a.md", "content": content("Other", "other body")}),
        )
        .await;
    assert_conflict(&again, "path_exists", Some(&["/global/a.md"]));
    let b = fx
        .create("live", "/global/b.md", &content("Beta", "beta body"))
        .await;
    let rename = fx
        .call(
            "POST",
            &format!("{url}/{}", b["id"].as_str().unwrap()),
            "admin",
            json!({"path": "/global/a.md"}),
        )
        .await;
    assert_conflict(&rename, "path_exists", Some(&["/global/a.md"]));

    // slug_exists: the slug a.md is used elsewhere in the store.
    let slug = fx
        .call(
            "POST",
            url,
            "admin",
            json!({"path": "/topics/hardware/a.md", "content": content("Other", "other body")}),
        )
        .await;
    assert_conflict(&slug, "slug_exists", Some(&["/global/a.md"]));

    // precondition_failed (update and delete) is covered in `preconditions_over_http`.
    let stale = fx
        .call(
            "POST",
            &format!("{url}/{}", a["id"].as_str().unwrap()),
            "admin",
            json!({
                "content": content("Alpha two", "x"),
                "precondition": {"type": "content_sha256", "content_sha256": sha256(b"stale")}
            }),
        )
        .await;
    assert_conflict(&stale, "precondition_failed", Some(&["/global/a.md"]));

    // pinned_budget_exceeded: `paths` = every pinned path the store would hold after the rejected write (architect ruling).
    // 4,000 characters fit exactly, one more does not, and nothing is written.
    let fx2 = Fx::new();
    // The budget counts rendered blocks, heading included: exactly 4,000 is accepted, 4,001 is refused.
    let overhead = block_len("Core", "/global/core.md", "");
    let refused = fx2.call("POST", url, "admin", json!({
        "path": "/global/core.md",
        "content": content_with("Core", &"p".repeat(4001 - overhead), json!({"pinned": true}))
    })).await;
    assert_conflict(
        &refused,
        "pinned_budget_exceeded",
        Some(&["/global/core.md"]),
    );
    assert_eq!(
        fx2.ok("GET", url, Value::Null).await["data"],
        json!([]),
        "nothing written"
    );
    let four_k = "p".repeat(4000 - overhead);
    fx2.create(
        "live",
        "/global/core.md",
        &content_with("Core", &four_k, json!({"pinned": true})),
    )
    .await;
    let over = fx2.call("POST", url, "admin", json!({
        "path": "/global/more.md", "content": content_with("More", "q", json!({"pinned": true}))
    })).await;
    assert_conflict(
        &over,
        "pinned_budget_exceeded",
        Some(&["/global/core.md", "/global/more.md"]),
    );
    let grow = fx2.call("POST", url, "admin", json!({
        "path": "/projects/p/more.md", "content": content_with("More", "q", json!({"pinned": true}))
    })).await;
    assert_conflict(
        &grow,
        "pinned_budget_exceeded",
        Some(&["/global/core.md", "/projects/p/more.md"]),
    );
    assert_eq!(
        fx2.ok("GET", url, Value::Null).await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fx2.status().await["pinned_chars"], 4000);
    // Unpinned memories are not limited by the pinned budget.
    fx2.create(
        "live",
        "/global/free.md",
        &content("Free", &"u".repeat(7000)),
    )
    .await;
}

#[tokio::test]
async fn error_mapping_dream_conflicts() {
    let fx = Fx::new();
    let a = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let b = fx
        .create("live", "/global/b.md", &content("Beta", "beta body"))
        .await;
    let (a_id, b_id) = (
        a["id"].as_str().unwrap().to_owned(),
        b["id"].as_str().unwrap().to_owned(),
    );

    // adopt_conflict: live and dream both change a -> 409 naming a; nothing changes; dream stays running.
    let d = fx.dream().await;
    let (d_id, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    fx.update(&out, &a_id, &content("Alpha dream", "dream edit"))
        .await;
    fx.update("live", &a_id, &content("Alpha live", "live edit"))
        .await;
    let before = fx
        .ok("GET", "/v1/memory_stores/live/memories", Value::Null)
        .await;
    let conflict = fx.adopt("admin", &d_id).await;
    assert_conflict(&conflict, "adopt_conflict", Some(&["/global/a.md"]));
    assert_eq!(
        fx.ok("GET", "/v1/memory_stores/live/memories", Value::Null)
            .await,
        before
    );
    let got = fx
        .ok("GET", &format!("/v1/dreams/{d_id}"), Value::Null)
        .await;
    assert_eq!(got["status"], "running");
    assert!(got["ended_at"].is_null());
    // The output store is still writable while the dream runs.
    fx.update(&out, &b_id, &content("Beta dream", "still writable"))
        .await;
    // One dream runs at a time (X10): cancel it before the next start.
    fx.ok("POST", &format!("/v1/dreams/{d_id}/cancel"), json!({}))
        .await;

    // adopt_conflict on an added path both sides created.
    let d = fx.dream().await;
    let (d_id, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    fx.create(&out, "/global/new.md", &content("New dream", "dream new"))
        .await;
    fx.create("live", "/global/new.md", &content("New live", "live new"))
        .await;
    assert_conflict(
        &fx.adopt("admin", &d_id).await,
        "adopt_conflict",
        Some(&["/global/new.md"]),
    );
    // Cancel releases it; cancelling again is a no-op.
    assert_eq!(
        fx.ok("POST", &format!("/v1/dreams/{d_id}/cancel"), json!({}))
            .await["status"],
        "canceled"
    );
    assert_eq!(
        fx.ok("POST", &format!("/v1/dreams/{d_id}/cancel"), json!({}))
            .await["status"],
        "canceled"
    );

    // store_archived: writes to the output store of a canceled dream.
    let archived = fx
        .call(
            "POST",
            &format!("/v1/memory_stores/{out}/memories"),
            "admin",
            json!({"path": "/global/late.md", "content": content("Late", "late body")}),
        )
        .await;
    assert_conflict(&archived, "store_archived", None);
    let archived = fx
        .call(
            "DELETE",
            &format!("/v1/memory_stores/{out}/memories/{a_id}"),
            "admin",
            Value::Null,
        )
        .await;
    assert_conflict(&archived, "store_archived", None);
    // Reads of an archived store still work.
    assert_eq!(
        fx.call(
            "GET",
            &format!("/v1/memory_stores/{out}/memories"),
            "admin",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );

    // X10 removed observations_consumed: a second dream cannot start while the first
    // runs, so two dreams can no longer share one pending observation.
    let fx = Fx::new();
    fx.ok(
        "POST",
        "/v1/observations",
        json!({"path": "/global/obs-1.md", "content": content("Observed", "seen once")}),
    )
    .await;
    let first = fx.dream().await;
    assert_eq!(
        first["observations"].as_array().unwrap().len(),
        1,
        "ASSUMPTION: `observations` is a list"
    );
    let first_id = first["id"].as_str().unwrap().to_owned();
    let second = fx
        .call("POST", "/v1/dreams", "admin", json!({"sessions": []}))
        .await;
    assert_conflict(&second, "dream_running", Some(&[first_id.as_str()]));
    assert_eq!(fx.adopt("admin", &first_id).await.0, StatusCode::OK);
    assert_eq!(fx.status().await["observations_pending"], 0);

    // store_archived after a successful adopt as well.
    let out = first["output_store_id"].as_str().unwrap();
    let archived = fx
        .call(
            "POST",
            &format!("/v1/memory_stores/{out}/memories"),
            "admin",
            json!({"path": "/global/late.md", "content": content("Late", "late body")}),
        )
        .await;
    assert_conflict(&archived, "store_archived", None);
}

#[tokio::test]
async fn error_mapping_store_full() {
    // 10,000 rows seeded with raw SQL (frozen §3.1 DDL): 10,000 API creates would rebuild the mirror each time.
    let text = content("Seed", "seed body");
    let hash = sha256(text.as_bytes());
    let fx = Fx::seeded(|db| {
        let mut conn = rusqlite::Connection::open(db).unwrap();
        let live: String = conn
            .query_row("SELECT id FROM stores WHERE kind='live'", [], |r| r.get(0))
            .unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..10_000 {
            let (mem, ver, path) = (
                format!("mem_{i:032x}"),
                format!("memver_{i:032x}"),
                format!("/global/seed-{i:05}.md"),
            );
            tx.execute(
                "INSERT INTO memory_versions (id, store_id, memory_id, operation, path, content, content_sha256, created_at, created_by)
                 VALUES (?1, ?2, ?3, 'created', ?4, ?5, ?6, '2026-11-01T00:00:00Z', 'test:seed')",
                rusqlite::params![ver, live, mem, path, text, hash],
            ).unwrap();
            tx.execute(
                "INSERT INTO memories (store_id, id, path, content, content_sha256, memory_version_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, '2026-11-01T00:00:00Z', '2026-11-01T00:00:00Z')",
                rusqlite::params![live, mem, path, text, hash, ver],
            ).unwrap();
        }
        tx.commit().unwrap();
    });
    assert_eq!(fx.status().await["memories"], 10_000);
    let (code, body) = fx
        .call(
            "POST",
            "/v1/memory_stores/live/memories",
            "admin",
            json!({"path": "/global/one-too-many.md", "content": content("Extra", "extra body")}),
        )
        .await;
    assert_invalid(&(code, body), "store_full");
    assert_eq!(fx.status().await["memories"], 10_000);
    // Deleting one frees a slot.
    fx.ok(
        "DELETE",
        &format!("/v1/memory_stores/live/memories/mem_{:032x}", 0),
        Value::Null,
    )
    .await;
    fx.create("live", "/global/fits-now.md", &content("Fits", "fits body"))
        .await;
}

#[tokio::test]
async fn error_mapping_bad_requests() {
    let fx = Fx::new();
    let url = "/v1/memory_stores/live/memories";
    let a = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let id = a["id"].as_str().unwrap();
    let good = content("Fine", "fine body");
    let bad_creates = [
        (
            "path traversal",
            json!({"path": "/global/../x.md", "content": good}),
        ),
        ("no .md", json!({"path": "/global/x", "content": good})),
        (
            "unknown top dir",
            json!({"path": "/other/x.md", "content": good}),
        ),
        (
            "bad slug",
            json!({"path": "/global/Bad_Slug.md", "content": good}),
        ),
        (
            "non-ascii segment",
            json!({"path": "/topics/မြန်မာ/x.md", "content": good}),
        ),
        (
            "no frontmatter",
            json!({"path": "/global/x.md", "content": "just text"}),
        ),
        (
            "empty evidence",
            json!({"path": "/global/x.md", "content": content_with("T", "b", json!({"evidence": []}))}),
        ),
        (
            "empty body",
            json!({"path": "/global/x.md", "content": content("T", "")}),
        ),
        (
            "credential",
            json!({"path": "/global/x.md", "content": content("T", "api_key=abcdefghijklmnop")}),
        ),
        (
            "pinned topic",
            json!({"path": "/topics/t/x.md", "content": content_with("T", "b", json!({"pinned": true}))}),
        ),
        (
            "forbidden key",
            json!({"path": "/global/x.md", "content": content_with("T", "b", json!({"scope": "global"}))}),
        ),
        ("missing content", json!({"path": "/global/x.md"})),
        ("missing path", json!({"content": good})),
        (
            "content not a string",
            json!({"path": "/global/x.md", "content": 7}),
        ),
    ];
    for (name, body) in bad_creates {
        let (code, response) = fx.call("POST", url, "admin", body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{name}: {response}");
        assert_invalid(&(code, response), "");
    }
    // Malformed JSON.
    assert_invalid(
        &call_bytes(&fx.app, "POST", url, Some("admin"), b"{not json".to_vec()).await,
        "",
    );
    // Updates: bad content, bad path, empty update, malformed precondition.
    for (name, body) in [
        ("bad content", json!({"content": "no frontmatter"})),
        ("bad path", json!({"path": "/global/../y.md"})),
        (
            "bad precondition type",
            json!({"content": good, "precondition": {"type": "etag", "content_sha256": "x"}}),
        ),
        (
            "precondition without hash",
            json!({"content": good, "precondition": {"type": "content_sha256"}}),
        ),
    ] {
        let (code, response) = fx.call("POST", &format!("{url}/{id}"), "admin", body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{name}: {response}");
        assert_invalid(&(code, response), "");
    }
    // Dream start: 101 sessions and duplicate locators.
    let session = |n: usize| json!({"source": "claude", "locator": format!("/s/{n}.jsonl")});
    let many: Vec<_> = (0..101).map(session).collect();
    assert_eq!(
        fx.call("POST", "/v1/dreams", "admin", json!({"sessions": many}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fx.call(
            "POST",
            "/v1/dreams",
            "admin",
            json!({"sessions": [session(1), session(1)]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let hundred: Vec<_> = (0..100).map(session).collect();
    let (code, started) = fx
        .call("POST", "/v1/dreams", "admin", json!({"sessions": hundred}))
        .await;
    assert_eq!(code, StatusCode::OK);
    // One dream runs at a time (X10): cancel it before the next start.
    let started = started["id"].as_str().unwrap();
    fx.ok("POST", &format!("/v1/dreams/{started}/cancel"), json!({}))
        .await;
    // Adopt review gates: no reviewed key (X14), coverage, summary, resolved conflicts.
    let d = fx.dream().await;
    let d_id = d["id"].as_str().unwrap();
    for (name, patch) in [
        ("removed reviewed key", json!({"reviewed": false})),
        ("empty coverage", json!({"coverage": ""})),
        ("empty summary", json!({"summary": ""})),
        (
            "unresolved conflict",
            json!({"conflicts": [{"status": "unresolved"}]}),
        ),
    ] {
        let mut body = review();
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (code, response) = fx
            .call("POST", &format!("/v1/dreams/{d_id}/adopt"), "admin", body)
            .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{name}: {response}");
        assert_invalid(&(code, response), "");
    }
    assert_eq!(
        fx.ok("GET", &format!("/v1/dreams/{d_id}"), Value::Null)
            .await["status"],
        "running"
    );
    // Cancelling a completed dream is a 400 (§3.5).
    assert_eq!(fx.adopt("admin", d_id).await.0, StatusCode::OK);
    assert_invalid(
        &fx.call(
            "POST",
            &format!("/v1/dreams/{d_id}/cancel"),
            "admin",
            json!({}),
        )
        .await,
        "",
    );
    // Adopting a dream that is no longer running is a 400 as well.
    assert_invalid(&fx.adopt("admin", d_id).await, "");
    let c = fx.dream().await;
    let c_id = c["id"].as_str().unwrap();
    fx.ok("POST", &format!("/v1/dreams/{c_id}/cancel"), json!({}))
        .await;
    assert_invalid(&fx.adopt("admin", c_id).await, "");
    // Observations validate like memories.
    for body in [
        json!({"path": "/global/../o.md", "content": good}),
        json!({"path": "/global/o.md", "content": "plain"}),
        json!({}),
    ] {
        assert_eq!(
            fx.call("POST", "/v1/observations", "admin", body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    // Nothing from the failed requests was stored.
    assert_eq!(
        fx.ok("GET", url, Value::Null).await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn error_mapping_not_found() {
    let fx = Fx::new();
    let a = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let id = a["id"].as_str().unwrap();
    let d = fx.dream().await;
    let d_id = d["id"].as_str().unwrap();
    let no_dream = "drm_00000000000000000000000000000000";
    let no_version = "memver_00000000000000000000000000000000";
    let cases: Vec<(&str, String, Value)> = vec![
        (
            "GET",
            format!("/v1/memory_stores/{NO_STORE}/memories"),
            Value::Null,
        ),
        (
            "GET",
            format!("/v1/memory_stores/{NO_STORE}/memories/{id}"),
            Value::Null,
        ),
        (
            "POST",
            format!("/v1/memory_stores/{NO_STORE}/memories"),
            json!({"path": "/global/z.md", "content": content("Z", "z")}),
        ),
        (
            "GET",
            format!("/v1/memory_stores/{NO_STORE}/memory_versions"),
            Value::Null,
        ),
        (
            "GET",
            format!("/v1/memory_stores/live/memories/{NO_ID}"),
            Value::Null,
        ),
        (
            "GET",
            "/v1/memory_stores/live/memories/not-an-id".into(),
            Value::Null,
        ),
        (
            "POST",
            format!("/v1/memory_stores/live/memories/{NO_ID}"),
            json!({"content": content("Z", "z")}),
        ),
        (
            "DELETE",
            format!("/v1/memory_stores/live/memories/{NO_ID}"),
            Value::Null,
        ),
        (
            "GET",
            format!("/v1/memory_stores/live/memory_versions/{no_version}"),
            Value::Null,
        ),
        (
            "POST",
            format!("/v1/memory_stores/live/memory_versions/{no_version}/redact"),
            json!({}),
        ),
        ("GET", format!("/v1/dreams/{no_dream}"), Value::Null),
        (
            "GET",
            format!("/v1/dreams/{no_dream}?diff=true"),
            Value::Null,
        ),
        ("POST", format!("/v1/dreams/{no_dream}/adopt"), review()),
        ("POST", format!("/v1/dreams/{no_dream}/cancel"), json!({})),
    ];
    for (method, path, body) in cases {
        let (code, response) = fx.call(method, &path, "admin", body).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{method} {path}: {response}");
        assert_eq!(response["type"], "not_found", "{method} {path}: {response}");
        assert!(
            response["error"].as_str().is_some(),
            "{method} {path}: {response}"
        );
    }
    // A memory of the live store is not found in a dream's output store under another id, and vice versa.
    let out = d["output_store_id"].as_str().unwrap();
    let in_out = fx
        .create(out, "/global/only-in-dream.md", &content("D", "dream only"))
        .await;
    let dream_only = in_out["id"].as_str().unwrap();
    assert_eq!(
        fx.call(
            "GET",
            &format!("/v1/memory_stores/live/memories/{dream_only}"),
            "admin",
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fx.call(
            "GET",
            &format!("/v1/memory_stores/{out}/memories/{id}"),
            "admin",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK,
        "clones keep their ids"
    );
    let _ = d_id;
}

// ---------------------------------------------------------------------------
// 4. actor_recorded
// ---------------------------------------------------------------------------

#[tokio::test]
async fn actor_recorded() {
    let fx = Fx::new();
    let m = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let id = m["id"].as_str().unwrap().to_owned();
    // A second token writes the next version.
    let (code, _) = fx
        .call(
            "POST",
            &format!("/v1/memory_stores/live/memories/{id}"),
            "maintainer",
            json!({"content": content("Alpha two", "second body")}),
        )
        .await;
    assert_eq!(code, StatusCode::OK);
    let versions = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(versions[0]["created_by"], "token:maintainer");
    assert_eq!(versions[1]["created_by"], "token:admin");
    // The created_by filter picks out one actor's versions.
    let only_admin = versions_of(
        &fx.ok(
            "GET",
            "/v1/memory_stores/live/memory_versions?created_by=token:admin",
            Value::Null,
        )
        .await,
    );
    assert_eq!(only_admin.len(), 1);
    assert_eq!(only_admin[0]["id"], versions[1]["id"]);
    let nobody = versions_of(
        &fx.ok(
            "GET",
            "/v1/memory_stores/live/memory_versions?created_by=token:nobody",
            Value::Null,
        )
        .await,
    );
    assert!(nobody.is_empty());
    // Delete is recorded too.
    fx.call(
        "DELETE",
        &format!("/v1/memory_stores/live/memories/{id}"),
        "maintainer",
        Value::Null,
    )
    .await;
    let versions = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={id}"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(
        (
            versions[0]["operation"].as_str(),
            versions[0]["created_by"].as_str()
        ),
        (Some("deleted"), Some("token:maintainer"))
    );
    // A dream's clone is created by `dream:<id>`; adopted changes land in the live store under the same actor.
    fx.create("live", "/global/b.md", &content("Beta", "beta body"))
        .await;
    let d = fx.dream().await;
    let (d_id, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    let clones = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/{out}/memory_versions"),
            Value::Null,
        )
        .await,
    );
    assert!(
        !clones.is_empty()
            && clones
                .iter()
                .all(|v| v["created_by"] == format!("dream:{d_id}")),
        "{clones:?}"
    );
    fx.create(&out, "/global/c.md", &content("Gamma", "gamma body"))
        .await;
    let in_out = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/{out}/memory_versions?created_by=token:admin"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(
        in_out.len(),
        1,
        "writes to the output store are attributed to the token"
    );
    assert_eq!(fx.adopt("admin", &d_id).await.0, StatusCode::OK);
    let adopted = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?created_by=dream:{d_id}"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(
        adopted.len(),
        1,
        "only the added memory changed: {adopted:?}"
    );
    assert_eq!(adopted[0]["operation"], "created");
    assert_eq!(adopted[0]["path"], "/global/c.md");
}

// ---------------------------------------------------------------------------
// 5. removed_routes_404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removed_routes_404() {
    let fx = Fx::new();
    for (method, path) in [
        ("POST", "/v1/proposals/validate"),
        ("POST", "/v1/proposals/diff"),
        ("POST", "/v1/proposals/promote"),
        ("POST", "/v1/rollback"),
        ("GET", "/v1/export"),
    ] {
        let (code, _) = fx.call(method, path, "admin", json!({})).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{method} {path}");
    }
    // The old dream body shape (no sessions list) is not the v1 flow any more: no `proposal` is returned.
    let dream = fx.dream().await;
    assert!(dream.get("proposal").is_none(), "{dream}");
}

// ---------------------------------------------------------------------------
// 6. recall_endpoint
// ---------------------------------------------------------------------------

async fn seed_recall_fixture(fx: &Fx) {
    fx.create(
        "live",
        "/global/keep-it-simple.md",
        &content_with(
            "Keep it simple",
            "Remove what is not needed.",
            json!({"pinned": true, "priority": 10}),
        ),
    )
    .await;
    fx.create(
        "live",
        "/global/other-rule.md",
        &content("Other rule", "Prefer concrete reports."),
    )
    .await;
    fx.create(
        "live",
        "/projects/projects/a/build.md",
        &content("Build the project", "Run cargo build here."),
    )
    .await;
    fx.create(
        "live",
        "/projects/projects/b/elsewhere.md",
        &content("Another project", "Not for project a."),
    )
    .await;
    fx.create(
        "live",
        "/topics/hardware/usb.md",
        &content(
            "Raspberry USB power",
            "Switching one USB port is unverified.",
        ),
    )
    .await;
    fx.create(
        "live",
        "/roles/coordinator/playbook.md",
        &content("Coordinator playbook", "Delegate everything."),
    )
    .await;
}

fn oracle_recall(
    fx: &Fx,
    project: &str,
    role: &str,
    topics: &[&str],
    query: &str,
) -> (i64, recall::Recalled) {
    let store = fx.oracle();
    let (revision, memories) = store.live_memories().unwrap();
    let mirror = store.status().unwrap()["mirror_dir"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = recall::Request {
        project: project.into(),
        role: role.into(),
        topics: topics.iter().map(|t| (*t).to_owned()).collect(),
        query: query.into(),
    };
    (
        revision,
        recall::recall(&memories, revision, &mirror, &request),
    )
}

#[tokio::test]
async fn recall_endpoint() {
    let fx = Fx::new();
    seed_recall_fixture(&fx).await;
    let cases = [
        (json!({}), "", "", vec![], ""),
        (
            json!({"project": "/projects/a"}),
            "/projects/a",
            "",
            vec![],
            "",
        ),
        (
            json!({"project": "/projects/a", "role": "coordinator", "topics": ["hardware"]}),
            "/projects/a",
            "coordinator",
            vec!["hardware"],
            "",
        ),
        (
            json!({"query": "raspberry usb"}),
            "",
            "",
            vec![],
            "raspberry usb",
        ),
        (
            json!({"project": "/projects/a", "query": "cargo build"}),
            "/projects/a",
            "",
            vec![],
            "cargo build",
        ),
        (
            json!({"query": "kubernetes ingress yaml"}),
            "",
            "",
            vec![],
            "kubernetes ingress yaml",
        ),
    ];
    for (request, project, role, topics, query) in cases {
        let (code, body) = fx
            .call("POST", "/v1/recall", "reader", request.clone())
            .await;
        assert_eq!(code, StatusCode::OK, "{request}: {body}");
        let (revision, expected) = oracle_recall(&fx, project, role, &topics, query);
        assert_eq!(
            body["text"],
            expected.text.as_str(),
            "text equals recall::recall for {request}"
        );
        assert_eq!(body["revision"], revision);
        assert_eq!(body["view"], expected.view);
        assert_eq!(body["pinned"], json!(expected.pinned));
        assert_eq!(body["returned"], json!(expected.returned));
        let keys: HashSet<_> = body.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            HashSet::from_iter(
                ["revision", "view", "pinned", "returned", "text"].map(String::from)
            ),
            "no structured memories"
        );
    }
    // Sanity on the content itself, so a renderer that returned nothing would not satisfy the oracle alone.
    let (_, start) = fx
        .call(
            "POST",
            "/v1/recall",
            "reader",
            json!({"project": "/projects/a"}),
        )
        .await;
    assert_eq!(start["view"], "start");
    assert_eq!(start["pinned"], json!(["/global/keep-it-simple.md"]));
    assert!(
        start["text"]
            .as_str()
            .unwrap()
            .contains("Remove what is not needed.")
    );
    assert!(
        !start["text"]
            .as_str()
            .unwrap()
            .contains("Not for project a.")
    );
    let (_, task) = fx
        .call(
            "POST",
            "/v1/recall",
            "reader",
            json!({"query": "raspberry usb"}),
        )
        .await;
    assert_eq!(task["view"], "task");
    assert_eq!(task["returned"], json!(["/topics/hardware/usb.md"]));
    let (_, none) = fx
        .call(
            "POST",
            "/v1/recall",
            "reader",
            json!({"query": "kubernetes ingress yaml"}),
        )
        .await;
    assert!(
        none["text"]
            .as_str()
            .unwrap()
            .starts_with("No memory matches \"kubernetes ingress yaml\".")
    );
    assert_eq!(none["returned"], json!([]));
}

#[tokio::test]
async fn recall_request_validation() {
    let fx = Fx::new();
    seed_recall_fixture(&fx).await;
    for (name, body) in [
        ("limit is gone", json!({"limit": 5})),
        ("budget is gone", json!({"budget": 1000})),
        ("mode is gone", json!({"mode": "start"})),
        ("unknown field", json!({"mystery": true})),
        ("relative project", json!({"project": "relative/path"})),
        ("topics not a list", json!({"topics": "hardware"})),
        ("query not a string", json!({"query": 5})),
    ] {
        let (code, response) = fx.call("POST", "/v1/recall", "reader", body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{name}: {response}");
        assert_invalid(&(code, response), "");
    }
    // Rejected requests are not logged.
    assert!(!fx.tmp.path().join("recall-log.jsonl").exists());
}

#[tokio::test]
async fn recall_usage_log() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    seed_recall_fixture(&fx).await;
    let (_, hook) = fx
        .call(
            "POST",
            "/v1/recall",
            "reader",
            json!({"project": "/projects/a", "client": "session-start"}),
        )
        .await;
    let (_, task) = fx
        .call(
            "POST",
            "/v1/recall",
            "reader",
            json!({"query": "raspberry usb", "role": "coordinator", "topics": ["hardware"]}),
        )
        .await;
    fx.call("POST", "/v1/recall", "reader", json!({"limit": 3}))
        .await;
    let log = fx.tmp.path().join("recall-log.jsonl");
    let entries: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(entries.len(), 2, "only successful recalls are logged");
    for e in &entries {
        let keys: HashSet<_> = e.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            HashSet::from_iter(
                [
                    "ts", "client", "project", "query", "role", "topics", "revision", "view",
                    "pinned", "returned", "chars"
                ]
                .map(String::from)
            ),
            "{e}"
        );
        assert!(e["ts"].as_str().is_some_and(|t| t.contains('T')));
    }
    let (first, second) = (&entries[0], &entries[1]);
    assert_eq!(
        first["client"], "session-start",
        "client label from the request"
    );
    assert_eq!(first["project"], "/projects/a", "project as sent");
    assert_eq!(first["view"], "start");
    assert_eq!(first["pinned"], hook["pinned"]);
    assert_eq!(first["returned"], hook["returned"]);
    assert_eq!(first["revision"], hook["revision"]);
    assert_eq!(
        first["chars"],
        hook["text"].as_str().unwrap().chars().count()
    );
    assert_eq!(
        second["client"], "reader",
        "token name when there is no label"
    );
    assert_eq!(second["query"], "raspberry usb");
    assert_eq!(second["role"], "coordinator");
    assert_eq!(second["topics"], json!(["hardware"]));
    assert_eq!(second["view"], "task");
    assert_eq!(second["returned"], task["returned"]);
    assert_eq!(
        second["chars"],
        task["text"].as_str().unwrap().chars().count()
    );
    assert_eq!(
        std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

// ---------------------------------------------------------------------------
// 7. http_dream_workflow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_dream_workflow() {
    let fx = Fx::new();
    let a = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let a_id = a["id"].as_str().unwrap().to_owned();
    fx.create("live", "/global/gone.md", &content("Gone", "to be removed"))
        .await;
    fx.ok(
        "POST",
        "/v1/observations",
        json!({"path": "/global/obs.md", "content": content("Observed", "seen once")}),
    )
    .await;

    let dream = fx
        .ok(
            "POST",
            "/v1/dreams",
            json!({"sessions": [{"source": "claude", "locator": "/s/one.jsonl"}]}),
        )
        .await;
    let (d_id, out) = (
        dream["id"].as_str().unwrap().to_owned(),
        dream["output_store_id"].as_str().unwrap().to_owned(),
    );
    assert!(d_id.starts_with("drm_") && out.starts_with("memstore_"));
    assert_eq!(dream["status"], "running");
    assert_eq!(dream["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(dream["observations"].as_array().unwrap().len(), 1);
    assert!(dream["ended_at"].is_null());
    let keys: HashSet<_> = dream.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        HashSet::from_iter(
            [
                "id",
                "status",
                "output_store_id",
                "sessions",
                "observations",
                "created_at",
                "ended_at",
                "adoption"
            ]
            .map(String::from)
        )
    );

    // The output store starts as a clone: same ids, paths and hashes.
    let live_rows = fx
        .ok("GET", "/v1/memory_stores/live/memories", Value::Null)
        .await;
    let out_rows = fx
        .ok(
            "GET",
            &format!("/v1/memory_stores/{out}/memories"),
            Value::Null,
        )
        .await;
    let strip = |v: &Value| -> Vec<(Value, Value, Value)> {
        let mut rows: Vec<_> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["id"].clone(),
                    r["path"].clone(),
                    r["content_sha256"].clone(),
                )
            })
            .collect();
        rows.sort_by_key(|r| r.0.to_string());
        rows
    };
    assert_eq!(strip(&live_rows), strip(&out_rows));
    let empty = fx
        .ok("GET", &format!("/v1/dreams/{d_id}?diff=true"), Value::Null)
        .await;
    assert_eq!(
        empty["diff"],
        json!({"added": [], "removed": [], "changed": []})
    );

    // The agent edits the output store only.
    let new_text = content(
        "Quokkas are not a fixture",
        "A quokka memory added by the dream.",
    );
    let added = fx
        .create(&out, "/topics/animals/quokka.md", &new_text)
        .await;
    fx.update(&out, &a_id, &content("Alpha revised", "alpha body revised"))
        .await;
    let gone = out_rows["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == "/global/gone.md")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fx.ok(
        "DELETE",
        &format!("/v1/memory_stores/{out}/memories/{gone}"),
        Value::Null,
    )
    .await;
    assert_eq!(
        fx.ok("GET", "/v1/memory_stores/live/memories", Value::Null)
            .await,
        live_rows,
        "live untouched before adopt"
    );
    assert!(
        !fx.mnt
            .join("shared-agent-memory/topics/animals/quokka.md")
            .exists()
    );

    let without = fx
        .ok("GET", &format!("/v1/dreams/{d_id}"), Value::Null)
        .await;
    assert!(
        without.get("diff").is_none(),
        "diff only when asked: {without}"
    );
    let with = fx
        .ok("GET", &format!("/v1/dreams/{d_id}?diff=true"), Value::Null)
        .await;
    let diff = &with["diff"];
    assert_eq!(
        (
            diff["added"].as_array().unwrap().len(),
            diff["changed"].as_array().unwrap().len(),
            diff["removed"].as_array().unwrap().len()
        ),
        (1, 1, 1),
        "{diff}"
    );
    // Frozen shapes (design §4.1): added/removed {memory_id, path, content}; changed {memory_id, before, after}.
    assert_eq!(
        diff["added"][0],
        json!({"memory_id": added["id"], "path": "/topics/animals/quokka.md", "content": new_text})
    );
    assert_eq!(
        diff["removed"][0],
        json!({"memory_id": gone, "path": "/global/gone.md", "content": content("Gone", "to be removed")})
    );
    assert_eq!(
        diff["changed"][0],
        json!({
            "memory_id": a_id,
            "before": {"path": "/global/a.md", "content": content("Alpha", "alpha body")},
            "after": {"path": "/global/a.md", "content": content("Alpha revised", "alpha body revised")},
        })
    );

    // The task recall cannot find it before adopt, and does after.
    let find = || async {
        fx.call("POST", "/v1/recall", "reader", json!({"query": "quokkas"}))
            .await
            .1
    };
    assert_eq!(find().await["returned"], json!([]));
    let adopted = fx.adopt("admin", &d_id).await;
    assert_eq!(adopted.0, StatusCode::OK, "{}", adopted.1);
    assert_eq!(adopted.1["status"], "completed");
    assert!(adopted.1["ended_at"].as_str().is_some());
    assert_eq!(adopted.1["adoption"]["summary"], "Test adoption");
    assert_eq!(adopted.1["adoption"]["coverage"], "Fixtures only");
    assert_eq!(adopted.1["adoption"]["conflicts"], json!([]));
    assert_eq!(
        adopted.1["adoption"]["added"],
        json!(["/topics/animals/quokka.md"])
    );
    assert_eq!(adopted.1["adoption"]["changed"], json!(["/global/a.md"]));
    assert_eq!(adopted.1["adoption"]["removed"], json!(["/global/gone.md"]));
    let found = find().await;
    assert_eq!(found["returned"], json!(["/topics/animals/quokka.md"]));
    assert!(
        found["text"]
            .as_str()
            .unwrap()
            .contains("A quokka memory added by the dream.")
    );

    // Live now holds the dream's work, attributed to it; the removed memory is a tombstone.
    let live = fx
        .ok("GET", "/v1/memory_stores/live/memories", Value::Null)
        .await;
    let paths: HashSet<_> = live["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        paths,
        HashSet::from([
            "/global/a.md".to_owned(),
            "/topics/animals/quokka.md".to_owned()
        ])
    );
    assert_eq!(
        fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memories/{a_id}"),
            Value::Null
        )
        .await["content"],
        content("Alpha revised", "alpha body revised")
    );
    assert_ne!(added["id"], Value::Null);
    let by_dream = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?created_by=dream:{d_id}"),
            Value::Null,
        )
        .await,
    );
    assert_eq!(by_dream.len(), 3);

    // The mirror holds the exact bytes; marker and index follow.
    let mirror = fx.mnt.join("shared-agent-memory");
    assert_eq!(
        std::fs::read_to_string(mirror.join("topics/animals/quokka.md")).unwrap(),
        new_text
    );
    assert!(!mirror.join("global/gone.md").exists());
    let marker: Value =
        serde_json::from_str(&std::fs::read_to_string(mirror.join(".agent-memory-store")).unwrap())
            .unwrap();
    let status = fx.status().await;
    assert_eq!(marker["revision"], status["revision"]);
    assert_eq!(marker["store_id"], status["store_id"]);
    assert!(
        std::fs::read_to_string(mirror.join("INDEX.md"))
            .unwrap()
            .contains("/topics/animals/quokka.md")
    );
    assert_eq!(status["dreams_running"], 0);
    assert_eq!(status["observations_pending"], 0);

    // The dream is listed (newest first) and the next one starts without the consumed observation.
    let next = fx.dream().await;
    assert_eq!(next["observations"], json!([]));
    let list = fx.ok("GET", "/v1/dreams", Value::Null).await;
    let ids: Vec<_> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].clone())
        .collect();
    assert_eq!(ids, vec![next["id"].clone(), json!(d_id)]);
}

#[tokio::test]
async fn adopt_is_per_memory_over_http() {
    let fx = Fx::new();
    let x = fx
        .create("live", "/global/x.md", &content("X", "x body"))
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let y = fx
        .create("live", "/global/y.md", &content("Y", "y body"))
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let d = fx.dream().await;
    let (d_id, out) = (
        d["id"].as_str().unwrap().to_owned(),
        d["output_store_id"].as_str().unwrap().to_owned(),
    );
    // Live changes X while the dream changes Y: no conflict; the live X is kept.
    fx.update("live", &x, &content("X live", "x live body"))
        .await;
    fx.update(&out, &y, &content("Y dream", "y dream body"))
        .await;
    assert_eq!(fx.adopt("admin", &d_id).await.0, StatusCode::OK);
    assert_eq!(
        fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memories/{x}"),
            Value::Null
        )
        .await["content"],
        content("X live", "x live body")
    );
    assert_eq!(
        fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memories/{y}"),
            Value::Null
        )
        .await["content"],
        content("Y dream", "y dream body")
    );
}

// ---------------------------------------------------------------------------
// Body limits and 413
// ---------------------------------------------------------------------------

#[tokio::test]
async fn body_limits_and_413() {
    let fx = Fx::new();
    let m = fx
        .create("live", "/global/a.md", &content("Alpha", "alpha body"))
        .await;
    let id = m["id"].as_str().unwrap().to_owned();
    let d = fx.dream().await;
    let d_id = d["id"].as_str().unwrap().to_owned();

    // Above 4 MiB every body-reading route answers 413, after authentication and before parsing.
    let huge = format!("{{\"padding\":\"{}\"}}", "x".repeat(MIB4)).into_bytes();
    for (method, path) in [
        ("POST", "/v1/recall".to_string()),
        ("POST", "/v1/observations".into()),
        ("POST", "/v1/memory_stores/live/memories".into()),
        ("POST", format!("/v1/memory_stores/live/memories/{id}")),
        ("POST", "/v1/dreams".into()),
        ("POST", format!("/v1/dreams/{d_id}/adopt")),
    ] {
        let (code, _) = call_bytes(&fx.app, method, &path, Some("admin"), huge.clone()).await;
        assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE, "{method} {path}");
    }
    // Authentication still comes first.
    assert_eq!(
        call_bytes(&fx.app, "POST", "/v1/recall", None, huge.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call_bytes(
            &fx.app,
            "POST",
            "/v1/memory_stores/live/memories",
            Some("reader"),
            huge
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // Content is limited to 102,400 bytes (a 400, not a 413), independent of the 4 MiB body limit.
    let exact = |total: usize| {
        let overhead = content("Big", "").len();
        content("Big", &"b".repeat(total - overhead))
    };
    let fits = exact(102_400);
    assert_eq!(fits.len(), 102_400);
    let created = fx.create("live", "/global/big.md", &fits).await;
    assert_eq!(created["content_sha256"], sha256(fits.as_bytes()));
    let too_big = exact(102_401);
    assert_eq!(too_big.len(), 102_401);
    let (code, body) = fx
        .call(
            "POST",
            "/v1/memory_stores/live/memories",
            "admin",
            json!({"path": "/global/bigger.md", "content": too_big}),
        )
        .await;
    assert_invalid(&(code, body), "");
    assert_invalid(
        &fx.call(
            "POST",
            &format!("/v1/memory_stores/live/memories/{id}"),
            "admin",
            json!({"content": too_big}),
        )
        .await,
        "",
    );
    let three_mib = content("Huge", &"h".repeat(3 * 1024 * 1024));
    let (code, _) = fx
        .call(
            "POST",
            "/v1/memory_stores/live/memories",
            "admin",
            json!({"path": "/global/huge.md", "content": three_mib}),
        )
        .await;
    assert_eq!(
        code,
        StatusCode::BAD_REQUEST,
        "a 3 MiB body is under the body limit"
    );
    let (code, _) = fx
        .call(
            "POST",
            "/v1/observations",
            "admin",
            json!({"path": "/global/huge.md", "content": three_mib}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    // Nothing from the refused requests was stored.
    let names: Vec<_> = fx
        .ok("GET", "/v1/memory_stores/live/memories", Value::Null)
        .await["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].clone())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(fx.status().await["observations_pending"], 0);
}

// ---------------------------------------------------------------------------
// Fix round (docs/coordination/v2-review-triage.md, WS-H)
// ---------------------------------------------------------------------------

/// X9: history includes deleted memories, so a recall token may not read versions
/// (§4.1: version reads always need `review`).
#[tokio::test]
async fn version_reads_need_review() {
    let fx = Fx::new();
    let gone = fx
        .create("live", "/global/gone.md", &content("Gone", "deleted later"))
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fx.ok(
        "DELETE",
        &format!("/v1/memory_stores/live/memories/{gone}"),
        Value::Null,
    )
    .await;
    let versions = versions_of(
        &fx.ok(
            "GET",
            &format!("/v1/memory_stores/live/memory_versions?memory_id={gone}"),
            Value::Null,
        )
        .await,
    );
    let version = versions[0]["id"].as_str().unwrap().to_owned();
    let paths = [
        "/v1/memory_stores/live/memory_versions".to_string(),
        format!("/v1/memory_stores/live/memory_versions?memory_id={gone}"),
        format!("/v1/memory_stores/live/memory_versions/{version}"),
    ];
    for token in ["reader", "maintainer"] {
        for path in &paths {
            let (code, body) = fx.call("GET", path, token, Value::Null).await;
            assert_eq!(code, StatusCode::FORBIDDEN, "{token} GET {path}: {body}");
        }
    }
    for path in &paths {
        let (code, body) = fx.call("GET", path, "reviewer", Value::Null).await;
        assert_eq!(code, StatusCode::OK, "reviewer GET {path}: {body}");
    }
}

/// X7: a leaked secret in an observation can be removed, from `observations` and from
/// every `dream_jobs.input` copy, pending or consumed. Architect ruling: 200 `{id, created, body}`
/// with body `{path: null, content: null, created_by, redacted_at, redacted_by}`; a second
/// redaction is 200 too; the request body is empty or `{}`, any key is 400.
#[tokio::test]
async fn observation_redact_route() {
    let count = |fx: &Fx, table: &str, column: &str, marker: &str| -> i64 {
        rusqlite::Connection::open(&fx.db)
            .unwrap()
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column} LIKE ?1"),
                [format!("%{marker}%")],
                |r| r.get(0),
            )
            .unwrap()
    };
    let fx = Fx::new();
    let observe = |marker: &'static str| {
        let fx = &fx;
        async move {
            fx.ok(
                "POST",
                "/v1/observations",
                json!({"path": format!("/global/{marker}.md"),
                       "content": content("Observed", &format!("leaked {marker} text"))}),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    // `consumed` goes through an adopted dream; `pending` is still waiting for one.
    let consumed = observe("marker-consumed").await;
    let dream = fx.dream().await;
    assert_eq!(
        fx.adopt("admin", dream["id"].as_str().unwrap()).await.0,
        StatusCode::OK
    );
    let pending = observe("marker-pending").await;
    let running = fx.dream().await;
    let running_id = running["id"].as_str().unwrap().to_owned();
    assert_eq!(count(&fx, "observations", "body", "marker-"), 2);
    assert_eq!(count(&fx, "dream_jobs", "input", "marker-pending"), 1);

    for token in ["reader", "observer", "reviewonly", "reviewer"] {
        let (code, body) = fx
            .call(
                "POST",
                &format!("/v1/observations/{pending}/redact"),
                token,
                json!({}),
            )
            .await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{token}: {body}");
    }
    assert_eq!(
        count(&fx, "observations", "body", "marker-pending"),
        1,
        "refused calls change nothing"
    );
    let (code, body) = fx
        .call(
            "POST",
            "/v1/observations/obs_unknown/redact",
            "maintainer",
            json!({}),
        )
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["type"], "not_found", "{body}");

    for (id, marker) in [(&pending, "marker-pending"), (&consumed, "marker-consumed")] {
        let (code, body) = fx
            .call(
                "POST",
                &format!("/v1/observations/{id}/redact"),
                "maintainer",
                json!({}),
            )
            .await;
        assert_eq!(code, StatusCode::OK, "{marker}: {body}");
        assert_eq!(body["id"], json!(id), "{body}");
        assert!(body["created"].is_string(), "{body}");
        let redacted = &body["body"];
        assert!(
            redacted["path"].is_null() && redacted["content"].is_null(),
            "{body}"
        );
        assert_eq!(redacted["created_by"], "token:admin", "{body}");
        assert!(redacted["redacted_at"].is_string(), "{body}");
        assert_eq!(redacted["redacted_by"], "token:maintainer", "{body}");
        assert_eq!(
            count(&fx, "observations", "body", marker),
            0,
            "{marker} still in observations"
        );
        assert_eq!(
            count(&fx, "dream_jobs", "input", marker),
            0,
            "{marker} still in dream_jobs.input"
        );
        let (code, again) = fx
            .call(
                "POST",
                &format!("/v1/observations/{id}/redact"),
                "maintainer",
                json!({}),
            )
            .await;
        assert_eq!(code, StatusCode::OK, "second redaction: {again}");
        assert!(again["body"]["content"].is_null(), "{again}");
    }
    // Empty body is fine; any key is 400 and changes nothing.
    let (code, body) = fx
        .call_bytes_to(
            &format!("/v1/observations/{pending}/redact"),
            "maintainer",
            Vec::new(),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "empty body: {body}");
    let (code, body) = fx
        .call(
            "POST",
            &format!("/v1/observations/{pending}/redact"),
            "maintainer",
            json!({"force": true}),
        )
        .await;
    assert_invalid(&(code, body), "");
    // The running dream still lists the observation by id, without the leaked text.
    let after = fx
        .ok("GET", &format!("/v1/dreams/{running_id}"), Value::Null)
        .await;
    let listed = after["observations"].as_array().unwrap();
    let entry = listed
        .iter()
        .find(|o| o["id"] == json!(pending))
        .expect("listed");
    assert!(
        entry["body"]["path"].is_null() && entry["body"]["content"].is_null(),
        "{after}"
    );
    assert_eq!(entry["body"]["created_by"], "token:admin", "{after}");
    assert!(!after.to_string().contains("marker-pending"), "{after}");
    // Redaction is not a delete: the other observations and rows are untouched.
    assert_eq!(count(&fx, "observations", "id", "%"), 2);
}

/// X15: `project_id` and the `[projects]` table are gone; a request that sends one is refused
/// like any other unknown field, and nothing is logged.
#[tokio::test]
async fn recall_rejects_project_id() {
    let fx = Fx::new();
    seed_recall_fixture(&fx).await;
    for (name, body) in [
        ("configured id", json!({"project_id": "example"})),
        ("unknown id", json!({"project_id": "unknown"})),
        (
            "with project",
            json!({"project_id": "example", "project": "/projects/a"}),
        ),
    ] {
        let (code, response) = fx.call("POST", "/v1/recall", "reader", body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{name}: {response}");
        assert_invalid(&(code, response), "");
    }
    assert!(!fx.tmp.path().join("recall-log.jsonl").exists());
}

/// X15, X16: `mount` and `projects` are not config keys; the mirror is always `<database dir>/mnt`.
#[test]
fn config_has_no_mount_or_projects() {
    let dir = TempDir::new().unwrap();
    let load = |name: &str, extra: &str| {
        let text = format!(
            "listen = \"127.0.0.1:8787\"\ndatabase = \"{}\"\n{extra}\n\n[[tokens]]\nname = \"t\"\nsha256 = \"{}\"\ncapabilities = [\"recall\"]\n",
            dir.path().join("memory.sqlite3").display(),
            sha256(b"t"),
        );
        let path = dir.path().join(format!("{name}.toml"));
        std::fs::write(&path, text).unwrap();
        Config::load(&path)
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    };
    load("plain", "").expect("a config without either key loads");
    for (name, extra, key) in [
        ("mount", "mount = \"/somewhere/else\"", "mount"),
        (
            "projects",
            "[projects]\nexample = \"/projects/a\"",
            "projects",
        ),
    ] {
        let err = load(name, extra).expect_err(name);
        assert!(
            err.contains(key),
            "{name}: the message names the key: {err}"
        );
    }
}

/// X31: a passing run of this suite leaves nothing in `TMPDIR`. The test re-runs the other
/// tests of this binary with `TMPDIR` pointing at a fresh directory and lists what remains.
#[test]
fn api_v2_leaves_nothing_in_tmpdir() {
    fn unseal(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.is_dir() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
            for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
                unseal(&entry.path());
            }
        }
    }
    let scratch = TempDir::new().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--skip", "api_v2_leaves_nothing_in_tmpdir"])
        .env("TMPDIR", scratch.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    let mut leftovers: Vec<_> = std::fs::read_dir(scratch.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    leftovers.sort();
    unseal(scratch.path());
    assert!(
        leftovers.is_empty(),
        "{} entries left in TMPDIR: {leftovers:?}",
        leftovers.len()
    );
    assert!(
        status.success(),
        "the inner run of the suite failed: {status}"
    );
}

/// X10: at most one dream runs; a second start is 409 `dream_running` whose `paths` is the
/// running dream's id. Canceling or adopting the stale one frees the slot.
/// X7: `?diff=true` works only while the dream is running; afterwards it is 400.
#[tokio::test]
async fn error_mapping_dream_running() {
    let fx = Fx::new();
    let first = fx.dream().await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    for token in ["admin", "reviewer"] {
        let again = fx
            .call("POST", "/v1/dreams", token, json!({"sessions": []}))
            .await;
        assert_conflict(&again, "dream_running", Some(&[first_id.as_str()]));
    }
    assert_eq!(fx.status().await["dreams_running"], 1);
    assert_eq!(
        fx.call(
            "GET",
            &format!("/v1/dreams/{first_id}?diff=true"),
            "admin",
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    fx.ok("POST", &format!("/v1/dreams/{first_id}/cancel"), json!({}))
        .await;
    assert_invalid(
        &fx.call(
            "GET",
            &format!("/v1/dreams/{first_id}?diff=true"),
            "admin",
            Value::Null,
        )
        .await,
        "",
    );
    let second = fx.dream().await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    assert_conflict(
        &fx.call("POST", "/v1/dreams", "admin", json!({"sessions": []}))
            .await,
        "dream_running",
        Some(&[second_id.as_str()]),
    );
    assert_eq!(fx.adopt("admin", &second_id).await.0, StatusCode::OK);
    assert_invalid(
        &fx.call(
            "GET",
            &format!("/v1/dreams/{second_id}?diff=true"),
            "admin",
            Value::Null,
        )
        .await,
        "",
    );
    // A finished dream's record is still readable without the diff.
    assert_eq!(
        fx.ok("GET", &format!("/v1/dreams/{second_id}"), Value::Null)
            .await["status"],
        "completed"
    );
    fx.dream().await;
}
