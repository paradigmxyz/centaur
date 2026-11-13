//! HTTP API for api-rs. In a deployment only api pods reach this port
//! (NetworkPolicy). Which principal a runner serves is set by where it runs
//! and which iron-proxy it sits behind, not by anything in a request.

use axum::{
    Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

use crate::engine::{Engine, ExecRequest};

#[derive(Clone)]
struct AppState {
    engine: Engine,
}

pub fn router(engine: Engine) -> Router {
    Router::new()
        .route("/exec", post(exec))
        .route("/catalog", get(catalog))
        .route("/healthz", get(healthz))
        .with_state(AppState { engine })
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

async fn healthz(State(state): State<AppState>) -> Response {
    Json(json!({
        "ok": true,
        "connectors": state.engine.catalog().connectors.len(),
    }))
    .into_response()
}
