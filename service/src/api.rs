use crate::{
    Error, Result,
    config::{Capability, Config},
    model::{self, absolute_path},
    recall,
    store::Store,
};
use axum::{
    Json, RequestExt, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, MatchedPath, Path, Query, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

/// Name of the token that authorized the request.
#[derive(Clone)]
struct Client(String);

impl Client {
    fn actor(&self) -> String {
        format!("token:{}", self.0)
    }
}

/// A rejected request; boxed so that handler results stay small.
struct Reject(Box<Response>);
impl IntoResponse for Reject {
    fn into_response(self) -> Response {
        *self.0
    }
}
impl From<Error> for Reject {
    fn from(e: Error) -> Self {
        Self(Box::new(e.into_response()))
    }
}
fn reject(response: impl IntoResponse) -> Reject {
    Reject(Box::new(response.into_response()))
}

type Reply = std::result::Result<Json<Value>, Reject>;

#[derive(Clone)]
pub struct App {
    store: Arc<Mutex<Store>>,
    config: Arc<Config>,
    permits: Arc<Semaphore>,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Invalid(s) => error(StatusCode::BAD_REQUEST, &s),
            Self::Conflict {
                kind,
                message,
                paths,
            } => {
                let mut body = json!({"error":message,"type":kind});
                if !paths.is_empty() {
                    body["paths"] = json!(paths);
                }
                (StatusCode::CONFLICT, Json(body)).into_response()
            }
            Self::NotFound => error(StatusCode::NOT_FOUND, "Not found"),
            Self::Internal(e) => {
                eprintln!("Database operation failed: {e}");
                error(StatusCode::INTERNAL_SERVER_ERROR, "Internal storage error")
            }
        }
    }
}

/// Every error body is `{error, type}`; apart from 409, the type follows from the status.
fn error(code: StatusCode, message: &str) -> Response {
    let kind = match code {
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::PAYLOAD_TOO_LARGE => "too_large",
        StatusCode::SERVICE_UNAVAILABLE => "busy",
        c if c.is_client_error() => "invalid",
        _ => "internal",
    };
    (code, Json(json!({"error":message,"type":kind}))).into_response()
}

/// Gives axum's own plain-text rejections (unknown route, bad query, 413) the same JSON shape.
async fn typed_errors(response: Response) -> Response {
    let code = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v == "application/json");
    if is_json || !(code.is_client_error() || code.is_server_error()) {
        return response;
    }
    let text = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let reason = code.canonical_reason().unwrap_or("Error");
    error(code, if text.is_empty() { reason } else { &text })
}

fn deny(code: StatusCode, message: &str) -> Reject {
    reject(error(code, message))
}

/// Bodies are read as bytes so that every malformed or unknown field is a 400, not axum's 422.
fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(if body.is_empty() { b"{}" } else { body })
        .map_err(|e| Error::Invalid(format!("Invalid request body: {e}")))
}

impl App {
    async fn run<T, F>(&self, operation: F) -> std::result::Result<T, Reject>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> Result<T> + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| deny(StatusCode::SERVICE_UNAVAILABLE, "Service busy; retry later"))?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut guard = store
                .lock()
                .map_err(|_| Error::Internal(anyhow::anyhow!("Storage lock poisoned")))?;
            operation(&mut guard)
        })
        .await
        .map_err(|_| Error::Internal(anyhow::anyhow!("Storage task failed")))?
        .map_err(Reject::from)
    }
    async fn json<F>(&self, operation: F) -> Reply
    where
        F: FnOnce(&mut Store) -> Result<Value> + Send + 'static,
    {
        self.run(operation).await.map(Json)
    }
}

pub fn router(config: Config, store: Store) -> Router {
    let app = App {
        store: Arc::new(Mutex::new(store)),
        config: Arc::new(config),
        permits: Arc::new(Semaphore::new(32)),
    };
    const STORE: &str = "/v1/memory_stores/{store}";
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/recall", post(recall))
        .route("/v1/observations", post(observe))
        .route("/v1/dreams", post(dream_start).get(dreams))
        .route("/v1/dreams/{id}", get(dream))
        .route("/v1/dreams/{id}/adopt", post(dream_adopt))
        .route("/v1/dreams/{id}/cancel", post(dream_cancel))
        .route(&format!("{STORE}/memories"), get(list).post(create))
        .route(
            &format!("{STORE}/memories/{{id}}"),
            get(read).post(update).delete(delete),
        )
        .route(&format!("{STORE}/memory_versions"), get(versions))
        .route(&format!("{STORE}/memory_versions/{{id}}"), get(version))
        .route(
            &format!("{STORE}/memory_versions/{{id}}/redact"),
            post(redact),
        )
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize))
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(middleware::map_response(typed_errors))
        .with_state(app)
}

/// Reads of the live store need `recall` and its writes `promote`; any other store needs `review`.
async fn required(app: &App, request: &mut Request) -> std::result::Result<Capability, Reject> {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("", |m| m.as_str());
    Ok(match (&method, route) {
        (&Method::GET, "/v1/status") | (&Method::POST, "/v1/recall") => Capability::Recall,
        (&Method::POST, "/v1/observations") => Capability::Observe,
        (&Method::POST, "/v1/dreams/{id}/adopt")
        | (&Method::POST, "/v1/memory_stores/{store}/memory_versions/{id}/redact") => {
            Capability::Promote
        }
        (_, r) if r.starts_with("/v1/memory_stores/") => {
            let Path(params) = request
                .extract_parts::<Path<HashMap<String, String>>>()
                .await
                .map_err(reject)?;
            let store = params.get("store").cloned().unwrap_or_default();
            let live = app.run(move |s| s.is_live(&store)).await?;
            match (method == Method::GET, live) {
                (true, true) => Capability::Recall,
                (false, true) => Capability::Promote,
                _ => Capability::Review,
            }
        }
        _ => Capability::Review,
    })
}

async fn authorize(State(app): State<App>, request: Request, next: Next) -> Response {
    match authenticate(&app, request).await {
        Ok(request) => next.run(request).await,
        Err(reject) => reject.into_response(),
    }
}

async fn authenticate(app: &App, mut request: Request) -> std::result::Result<Request, Reject> {
    let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty() && s.len() <= 512)
    else {
        return Err(deny(StatusCode::UNAUTHORIZED, "Bearer token required"));
    };
    let hash = model::hash(token.as_bytes());
    let Some(identity) = app
        .config
        .tokens
        .iter()
        .find(|t| bool::from(hash.as_bytes().ct_eq(t.sha256.as_bytes())))
    else {
        return Err(deny(StatusCode::UNAUTHORIZED, "Invalid bearer token"));
    };
    let client = Client(identity.name.clone());
    let needed = required(app, &mut request).await?;
    if !identity.capabilities.contains(&needed) {
        return Err(deny(
            StatusCode::FORBIDDEN,
            "Token lacks the required capability",
        ));
    }
    request.extensions_mut().insert(client);
    Ok(request)
}

async fn status(State(app): State<App>) -> Reply {
    app.json(|s| s.status()).await
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RecallRequest {
    project: String,
    project_id: String,
    role: String,
    topics: Vec<String>,
    query: String,
    client: String,
}

async fn recall(
    State(app): State<App>,
    Extension(Client(token)): Extension<Client>,
    body: Bytes,
) -> Reply {
    let mut r: RecallRequest = parse(&body)?;
    if !r.project_id.is_empty() {
        if !r.project.is_empty() {
            return Err(model::invalid("Use project_id or project, not both").into());
        }
        let Some(path) = app.config.projects.get(&r.project_id) else {
            return Err(model::invalid("Unknown project_id").into());
        };
        r.project = path.clone();
    }
    if !r.project.is_empty() {
        r.project = absolute_path(&r.project)?.to_string_lossy().into_owned();
    }
    let request = recall::Request {
        project: r.project,
        role: r.role,
        topics: r.topics,
        query: r.query,
    };
    let client = if r.client.is_empty() { token } else { r.client };
    let log = app.config.database.with_file_name("recall-log.jsonl");
    app.json(move |s| {
        let mirror_dir = s.status()?["mirror_dir"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let (revision, memories) = s.live_memories()?;
        let result = recall::recall(&memories, revision, &mirror_dir, &request);
        log_recall(&log, &client, &request, revision, &result);
        Ok(json!({
            "revision": revision,
            "view": result.view,
            "pinned": result.pinned,
            "returned": result.returned,
            "text": result.text,
        }))
    })
    .await
}

/// Usage evidence for audits, next to the database; a failed write never fails the recall.
fn log_recall(
    path: &std::path::Path,
    client: &str,
    request: &recall::Request,
    revision: i64,
    result: &recall::Recalled,
) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let entry = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "client": client,
        "project": request.project,
        "query": request.query,
        "role": request.role,
        "topics": request.topics,
        "revision": revision,
        "view": result.view,
        "pinned": result.pinned,
        "returned": result.returned,
        "chars": result.text.chars().count(),
    });
    let written = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(format!("{entry}\n").as_bytes()));
    if let Err(e) = written {
        eprintln!("Recall log write failed: {e}");
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathContent {
    path: String,
    content: String,
}

async fn observe(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Reply {
    let w: PathContent = parse(&body)?;
    app.json(move |s| s.observe(&w.path, &w.content, &client.actor()))
        .await
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DreamStart {
    sessions: Value,
}
impl Default for DreamStart {
    fn default() -> Self {
        Self {
            sessions: json!([]),
        }
    }
}

async fn dream_start(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Reply {
    let d: DreamStart = parse(&body)?;
    app.json(move |s| s.dream_start(&d.sessions, &client.actor()))
        .await
}
async fn dreams(State(app): State<App>) -> Reply {
    app.json(|s| s.dreams()).await
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DreamQuery {
    diff: bool,
}

async fn dream(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(q): Query<DreamQuery>,
) -> Reply {
    app.json(move |s| s.dream(&id, q.diff)).await
}
async fn dream_adopt(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
    body: Bytes,
) -> Reply {
    let review: Value = parse(&body)?;
    app.json(move |s| s.dream_adopt(&id, &review, &client.actor()))
        .await
}
async fn dream_cancel(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
) -> Reply {
    app.json(move |s| s.dream_cancel(&id, &client.actor()))
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default = "root")]
    path_prefix: String,
}
fn root() -> String {
    "/".into()
}

async fn list(
    State(app): State<App>,
    Path(store): Path<String>,
    Query(q): Query<ListQuery>,
) -> Reply {
    app.json(move |s| s.list(&store, &q.path_prefix)).await
}
async fn read(State(app): State<App>, Path((store, id)): Path<(String, String)>) -> Reply {
    app.json(move |s| s.get(&store, &id)).await
}
async fn create(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    Path(store): Path<String>,
    body: Bytes,
) -> Reply {
    let w: PathContent = parse(&body)?;
    app.json(move |s| s.create(&store, &w.path, &w.content, &client.actor()))
        .await
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Precondition {
    ContentSha256 { content_sha256: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    content: Option<String>,
    path: Option<String>,
    precondition: Option<Precondition>,
}

async fn update(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    Path((store, id)): Path<(String, String)>,
    body: Bytes,
) -> Reply {
    let u: Update = parse(&body)?;
    let expected = u
        .precondition
        .map(|Precondition::ContentSha256 { content_sha256 }| content_sha256);
    app.json(move |s| {
        s.update(
            &store,
            &id,
            u.content.as_deref(),
            u.path.as_deref(),
            expected.as_deref(),
            &client.actor(),
        )
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteQuery {
    expected_content_sha256: Option<String>,
}

async fn delete(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    Path((store, id)): Path<(String, String)>,
    Query(q): Query<DeleteQuery>,
) -> Reply {
    app.json(move |s| {
        s.delete(
            &store,
            &id,
            q.expected_content_sha256.as_deref(),
            &client.actor(),
        )
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionsQuery {
    memory_id: Option<String>,
    created_by: Option<String>,
}

async fn versions(
    State(app): State<App>,
    Path(store): Path<String>,
    Query(q): Query<VersionsQuery>,
) -> Reply {
    app.json(move |s| s.versions(&store, q.memory_id.as_deref(), q.created_by.as_deref()))
        .await
}
async fn version(State(app): State<App>, Path((store, id)): Path<(String, String)>) -> Reply {
    app.json(move |s| s.version(&store, &id)).await
}
async fn redact(
    State(app): State<App>,
    Extension(client): Extension<Client>,
    Path((store, id)): Path<(String, String)>,
) -> Reply {
    app.json(move |s| s.redact(&store, &id, &client.actor()))
        .await
}
