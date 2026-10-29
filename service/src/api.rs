use crate::{
    Error, Result,
    config::{Capability, Config},
    model::{self, Recall},
    store::Store,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct App {
    store: Arc<Mutex<Store>>,
    config: Arc<Config>,
    permits: Arc<Semaphore>,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (code, message) = match self {
            Self::Invalid(s) => (StatusCode::BAD_REQUEST, s),
            Self::Conflict(s) => (StatusCode::CONFLICT, s),
            Self::NotFound => (StatusCode::NOT_FOUND, "Not found".into()),
            Self::Internal(e) => {
                eprintln!("Database operation failed: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal storage error".into(),
                )
            }
        };
        (code, Json(json!({"error":message}))).into_response()
    }
}

impl App {
    async fn run<F>(&self, operation: F) -> std::result::Result<Json<Value>, Response>
    where
        F: FnOnce(&mut Store) -> Result<Value> + Send + 'static,
    {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"Service busy; retry later"})),
            )
                .into_response()
        })?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut guard = store
                .lock()
                .map_err(|_| Error::Internal(anyhow::anyhow!("Storage lock poisoned")))?;
            operation(&mut guard)
        })
        .await
        .map_err(|_| Error::Internal(anyhow::anyhow!("Storage task failed")).into_response())?
        .map(Json)
        .map_err(IntoResponse::into_response)
    }
}

pub fn router(config: Config, store: Store) -> Router {
    let app = App {
        store: Arc::new(Mutex::new(store)),
        config: Arc::new(config),
        permits: Arc::new(Semaphore::new(32)),
    };
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/recall", post(recall))
        .route("/v1/observations", post(observe))
        .route("/v1/dreams", post(dream))
        .route("/v1/dreams/{id}", get(get_dream))
        .route("/v1/proposals/validate", post(validate))
        .route("/v1/proposals/diff", post(diff))
        .route("/v1/proposals/promote", post(promote))
        .route("/v1/rollback", post(rollback))
        .route("/v1/export", get(export))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize))
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .with_state(app)
}

async fn authorize(State(app): State<App>, request: Request, next: Next) -> Response {
    let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty() && s.len() <= 512)
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Bearer token required"})),
        )
            .into_response();
    };
    let hash = model::hash(token.as_bytes());
    let Some(identity) = app
        .config
        .tokens
        .iter()
        .find(|t| bool::from(hash.as_bytes().ct_eq(t.sha256.as_bytes())))
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Invalid bearer token"})),
        )
            .into_response();
    };
    let path = request.uri().path();
    let required = match (request.method(), path) {
        (&Method::GET, "/v1/status") | (&Method::POST, "/v1/recall") => Capability::Recall,
        (&Method::POST, "/v1/observations") => Capability::Observe,
        (&Method::POST, "/v1/proposals/promote" | "/v1/rollback") => Capability::Promote,
        _ => Capability::Review,
    };
    if !identity.capabilities.contains(&required) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Token lacks the required capability"})),
        )
            .into_response();
    }
    next.run(request).await
}

async fn status(State(app): State<App>) -> impl IntoResponse {
    app.run(|s| s.status()).await
}
async fn recall(State(app): State<App>, Json(mut request): Json<Recall>) -> Response {
    if !request.project_id.is_empty() {
        if !request.project.is_empty() {
            return model::invalid("Use project_id or project, not both").into_response();
        }
        let Some(path) = app.config.projects.get(&request.project_id) else {
            return model::invalid("Unknown project_id").into_response();
        };
        request.project = path.clone();
    }
    app.run(move |s| {
        let (revision, memories) = s.active()?;
        model::recall(&memories, revision, &request)
    })
    .await
    .into_response()
}
async fn observe(State(app): State<App>, Json(memory): Json<Value>) -> impl IntoResponse {
    app.run(move |s| s.observe(memory)).await
}
async fn dream(State(app): State<App>) -> impl IntoResponse {
    app.run(|s| s.dream()).await
}
async fn get_dream(State(app): State<App>, Path(id): Path<String>) -> impl IntoResponse {
    app.run(move |s| s.get_dream(&id)).await
}
async fn validate(State(app): State<App>, Json(proposal): Json<Value>) -> impl IntoResponse {
    app.run(move |s| { s.check_proposal(&proposal)?; Ok(json!({"valid":true,"note":"Schema validation does not establish truth or approve promotion."})) }).await
}
async fn diff(State(app): State<App>, Json(proposal): Json<Value>) -> impl IntoResponse {
    app.run(move |s| s.diff(&proposal)).await
}
async fn promote(State(app): State<App>, Json(proposal): Json<Value>) -> impl IntoResponse {
    app.run(move |s| s.promote(proposal)).await
}
async fn rollback(State(app): State<App>, Json(request): Json<Value>) -> impl IntoResponse {
    app.run(move |s| s.rollback(request)).await
}
async fn export(State(app): State<App>) -> impl IntoResponse {
    app.run(|s| {
        let (revision, memories) = s.active()?;
        Ok(json!({"revision":revision,"memories":memories}))
    })
    .await
}
