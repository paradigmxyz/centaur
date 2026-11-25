//! In-process fake `centaur-brain` for unit tests.
//!
//! It speaks the real wire contract (thread status, turns, interrupt, SSE
//! events) so the client and the runtime's nanocode branches can be tested
//! without a brain process.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
pub(crate) struct RecordedRequest {
    pub thread_key: String,
    pub body: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct RecordedEventRequest {
    pub thread_key: String,
    pub query: String,
}

#[derive(Default)]
struct FakeBrainState {
    turns: Vec<RecordedRequest>,
    interrupts: Vec<RecordedRequest>,
    status_requests: Vec<RecordedRequest>,
    event_requests: Vec<RecordedEventRequest>,
    event_frames: Vec<String>,
    thread_status: Value,
    turn_response: Option<Value>,
    turn_rejection: Option<(u16, String)>,
    interrupt_response: Option<Value>,
    seen_request_ids: HashSet<String>,
}

type SharedState = Arc<Mutex<FakeBrainState>>;

pub(crate) struct FakeBrain {
    addr: SocketAddr,
    state: SharedState,
}

impl FakeBrain {
    pub(crate) async fn start() -> Self {
        let state: SharedState = Arc::new(Mutex::new(FakeBrainState::default()));
        let app = Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/v1/threads/{thread_key}", get(thread_status))
            .route("/v1/threads/{thread_key}/turns", post(submit_turn))
            .route("/v1/threads/{thread_key}/interrupt", post(interrupt))
            .route("/v1/threads/{thread_key}/events", get(events))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake brain");
        let addr = listener.local_addr().expect("fake brain address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, state }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(crate) fn recorded_turns(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().turns.clone()
    }

    pub(crate) fn recorded_interrupts(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().interrupts.clone()
    }

    pub(crate) fn recorded_status_requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().status_requests.clone()
    }

    pub(crate) fn recorded_event_requests(&self) -> Vec<RecordedEventRequest> {
        self.state.lock().unwrap().event_requests.clone()
    }

    pub(crate) fn set_event_frames(&self, frames: Vec<String>) {
        self.state.lock().unwrap().event_frames = frames;
    }

    pub(crate) fn set_thread_status(&self, response: Value) {
        self.state.lock().unwrap().thread_status = response;
    }

    pub(crate) fn set_turn_response(&self, response: Value) {
        self.state.lock().unwrap().turn_response = Some(response);
    }

    pub(crate) fn reject_turns(&self, status: u16, body: impl Into<String>) {
        self.state.lock().unwrap().turn_rejection = Some((status, body.into()));
    }

    pub(crate) fn set_interrupt_response(&self, response: Value) {
        self.state.lock().unwrap().interrupt_response = Some(response);
    }
}

async fn thread_status(
    State(state): State<SharedState>,
    Path(thread_key): Path<String>,
) -> Response {
    let mut state = state.lock().unwrap();
    state.status_requests.push(RecordedRequest {
        thread_key,
        body: Value::Null,
    });
    if state.thread_status.is_null() {
        return Json(json!({"running": null, "queued": []})).into_response();
    }
    Json(state.thread_status.clone()).into_response()
}

async fn submit_turn(
    State(state): State<SharedState>,
    Path(thread_key): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut state = state.lock().unwrap();
    state.turns.push(RecordedRequest {
        thread_key,
        body: body.clone(),
    });
    if let Some((status, message)) = state.turn_rejection.clone() {
        let status = StatusCode::from_u16(status).expect("valid rejection status");
        return (status, message).into_response();
    }
    if let Some(response) = state.turn_response.clone() {
        return Json(response).into_response();
    }
    let request_id = body
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let created = state.seen_request_ids.insert(request_id.clone());
    let steer = body.get("steer").and_then(Value::as_bool).unwrap_or(false);
    Json(json!({
        "request_id": request_id,
        "status": if steer { "running" } else { "queued" },
        "created": created,
        "steered": steer,
    }))
    .into_response()
}

async fn interrupt(
    State(state): State<SharedState>,
    Path(thread_key): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut state = state.lock().unwrap();
    state.interrupts.push(RecordedRequest {
        thread_key,
        body: body.clone(),
    });
    match state.interrupt_response.clone() {
        Some(response) => Json(response).into_response(),
        None => Json(json!({"interrupted": false, "request_id": null})).into_response(),
    }
}

async fn events(
    State(state): State<SharedState>,
    Path(thread_key): Path<String>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let mut state = state.lock().unwrap();
    let query = query
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    state
        .event_requests
        .push(RecordedEventRequest { thread_key, query });
    let body = state.event_frames.concat();
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .expect("build event stream")
}

/// One SSE frame exactly as api-rs frames it.
pub(crate) fn frame(id: impl std::fmt::Display, event: &str, data: Value) -> String {
    format!("id: {id}\nevent: {event}\ndata: {data}\n\n")
}
