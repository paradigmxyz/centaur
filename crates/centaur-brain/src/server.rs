//! The HTTP and WebSocket surface api-rs talks to.

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{registry::Registry, worker::Command};

const ID_PREFIX: &str = "brain-";

#[derive(Clone)]
pub struct AppState {
    pub registry: Registry,
    pub commands: mpsc::UnboundedSender<Command>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/sandboxes", axum::routing::post(create))
        .route("/v1/sandboxes/{id}", get(status).delete(delete))
        .route("/v1/sandboxes/{id}/io", get(io))
        .route("/v1/sandboxes/{id}/output", get(output))
        .with_state(state)
}

#[derive(Deserialize)]
struct CreateRequest {
    id: String,
    thread_key: Option<String>,
}

async fn create(State(state): State<AppState>, Json(request): Json<CreateRequest>) -> Response {
    if !request.id.starts_with(ID_PREFIX) {
        return (StatusCode::BAD_REQUEST, "sandbox ids start with brain-").into_response();
    }
    if !state.registry.create(&request.id, request.thread_key) {
        return (StatusCode::GONE, "sandbox was deleted").into_response();
    }
    (StatusCode::CREATED, Json(json!({"id": request.id}))).into_response()
}

/// Sessions are durable in nanocode, so a `brain-*` handle stays usable after
/// a restart: an unknown id is recreated on first use. A deleted id is gone.
async fn status(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !id.starts_with(ID_PREFIX) || !state.registry.create(&id, None) {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"status": "running"})).into_response()
}

async fn delete(State(state): State<AppState>, Path(id): Path<String>) -> StatusCode {
    let _ = state.commands.send(Command::Stop {
        sandbox: id.clone(),
    });
    if state.registry.delete(&id) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Deserialize)]
struct OutputQuery {
    since_ms: Option<u64>,
}

async fn output(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<OutputQuery>,
) -> Response {
    match state.registry.output_since(&id, query.since_ms) {
        Some(lines) => Json(json!({"lines": lines})).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn io(
    State(state): State<AppState>,
    Path(id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !id.starts_with(ID_PREFIX) || !state.registry.create(&id, None) {
        return StatusCode::NOT_FOUND.into_response();
    }
    upgrade.on_upgrade(move |socket| attach(state, id, socket))
}

async fn attach(state: AppState, id: String, socket: WebSocket) {
    let Some(mut attachment) = state.registry.attach(&id) else {
        return;
    };
    info!(sandbox = %id, generation = attachment.generation, "attached");
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(line) = attachment.lines.recv().await {
            if sink.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Text(text)) => {
                for line in text.lines().filter(|line| !line.trim().is_empty()) {
                    let command = Command::Input {
                        sandbox: id.clone(),
                        line: line.to_owned(),
                    };
                    if state.commands.send(command).is_err() {
                        warn!("the nanocode worker is gone");
                        writer.abort();
                        return;
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    state.registry.detach(&id, attachment.generation);
    writer.abort();
    info!(sandbox = %id, "detached");
}
