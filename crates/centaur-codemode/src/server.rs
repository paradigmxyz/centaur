//! HTTP API for api-rs. In a deployment only api pods reach this port
//! (NetworkPolicy). Which principal a runner serves is set by where it runs
//! and which iron-proxy it sits behind, not by anything in a request.

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    engine::{Engine, ExecRequest, MAX_FILE_BYTES},
    search::search,
};

#[derive(Clone)]
struct AppState {
    engine: Engine,
}

pub fn router(engine: Engine) -> Router {
    Router::new()
        .route("/exec", post(exec))
        .route("/catalog", get(catalog))
        .route("/search", post(search_tools))
        .route("/files/{*path}", get(read_file).put(write_file))
        .route("/healthz", get(healthz))
        .layer(DefaultBodyLimit::max(MAX_FILE_BYTES))
        .with_state(AppState { engine })
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

async fn exec(State(state): State<AppState>, Json(request): Json<ExecRequest>) -> Response {
    Json(state.engine.exec(request).await).into_response()
}

#[derive(Deserialize)]
struct ToolsQuery {
    tools: Option<String>,
}

fn allowlist(raw: Option<&str>) -> Option<Vec<String>> {
    raw.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

async fn catalog(State(state): State<AppState>, Query(query): Query<ToolsQuery>) -> Response {
    let allowed = allowlist(query.tools.as_deref());
    let catalog = state.engine.catalog();
    Json(json!({
        "connectors": catalog.visible(allowed.as_deref()).collect::<Vec<_>>(),
        "brief": catalog.brief(allowed.as_deref()),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    tools: Option<Vec<String>>,
}

fn default_limit() -> usize {
    8
}

async fn search_tools(
    State(state): State<AppState>,
    Json(request): Json<SearchRequest>,
) -> Response {
    let results = search(
        state.engine.catalog(),
        request.tools.as_deref(),
        &request.query,
        request.limit.min(50),
    );
    Json(json!({ "results": results })).into_response()
}

async fn read_file(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    match state.engine.files().read(&path).await {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Err(err) => error(StatusCode::NOT_FOUND, format!("{err:#}")),
    }
}

async fn write_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    body: Bytes,
) -> Response {
    let len = body.len();
    match state.engine.files().write(&path, body.to_vec()).await {
        Ok(()) => Json(json!({ "path": path, "bytes": len })).into_response(),
        Err(err) => error(StatusCode::BAD_REQUEST, format!("{err:#}")),
    }
}

async fn healthz(State(state): State<AppState>) -> Response {
    Json(json!({
        "ok": true,
        "connectors": state.engine.catalog().connectors.len(),
    }))
    .into_response()
}
