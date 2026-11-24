//! The HTTP API api-rs calls for `nanocode` threads.

use std::{convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::{
    projector::{Entry, Projector, StreamEvent},
    worker::{Command, Page, SubmitError},
};

/// Re-read the log at least this often, in case a wake-up was missed.
const TAIL_POLL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct AppState {
    pub commands: mpsc::UnboundedSender<Command>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/threads/{thread_key}", get(status))
        .route("/v1/threads/{thread_key}/turns", post(submit))
        .route("/v1/threads/{thread_key}/interrupt", post(interrupt))
        .route("/v1/threads/{thread_key}/events", get(events))
        .with_state(state)
}

#[derive(Deserialize)]
struct SubmitRequest {
    request_id: String,
    input_line: Value,
    #[serde(default)]
    steer: bool,
}

async fn submit(
    State(state): State<AppState>,
    Path(thread_key): Path<String>,
    Json(request): Json<SubmitRequest>,
) -> Response {
    if request.request_id.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "request_id is required");
    }
    let outcome = ask(&state, |reply| Command::Submit {
        thread_key,
        request_id: request.request_id.clone(),
        input_line: request.input_line,
        steer: request.steer,
        reply,
    })
    .await;
    match outcome {
        Some(Ok(submitted)) => Json(json!({
            "request_id": request.request_id,
            "status": submitted.status,
            "created": submitted.created,
            "steered": submitted.steered,
        }))
        .into_response(),
        Some(Err(SubmitError::BadRequest(message))) => error(StatusCode::BAD_REQUEST, &message),
        Some(Err(SubmitError::Conflict(message))) => error(StatusCode::CONFLICT, &message),
        Some(Err(SubmitError::Internal(message))) => {
            error(StatusCode::INTERNAL_SERVER_ERROR, &message)
        }
        None => unavailable(),
    }
}

async fn status(State(state): State<AppState>, Path(thread_key): Path<String>) -> Response {
    match ask(&state, |reply| Command::Status { thread_key, reply }).await {
        Some(Ok(status)) => Json(json!({
            "running": status.running,
            "queued": status.queued,
        }))
        .into_response(),
        Some(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, &message),
        None => unavailable(),
    }
}

async fn interrupt(State(state): State<AppState>, Path(thread_key): Path<String>) -> Response {
    match ask(&state, |reply| Command::Interrupt { thread_key, reply }).await {
        Some(Ok(request_id)) => Json(json!({
            "interrupted": request_id.is_some(),
            "request_id": request_id,
        }))
        .into_response(),
        Some(Err(message)) => error(StatusCode::INTERNAL_SERVER_ERROR, &message),
        None => unavailable(),
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    after_event_id: Option<u64>,
    execution_id: Option<String>,
}

async fn events(
    State(state): State<AppState>,
    Path(thread_key): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (sender, receiver) = mpsc::channel(256);
    tokio::spawn(follow(
        state,
        thread_key,
        query.after_event_id.unwrap_or(0),
        query.execution_id,
        sender,
    ));
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|event| (Ok(event), receiver))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Replay the thread's log from the start (the projection is stateful per
/// turn), sending events after the cursor, then tail it.
async fn follow(
    state: AppState,
    thread_key: String,
    after_event_id: u64,
    execution_id: Option<String>,
    sender: mpsc::Sender<Event>,
) {
    let mut projector: Option<Projector> = None;
    let mut cursor = 0;
    loop {
        let Some(page) = page(&state, &thread_key, cursor).await else {
            let _ = sender.send(stream_error("the brain is unavailable")).await;
            return;
        };
        let page = match page {
            Ok(page) => page,
            Err(message) => {
                let _ = sender.send(stream_error(&message)).await;
                return;
            }
        };
        let Page {
            session_id,
            events,
            next_cursor,
            has_more,
            mut wake,
        } = page;
        let projector = projector.get_or_insert_with(|| Projector::new(&session_id, &thread_key));
        for logged in &events {
            let body: Value = serde_json::from_str(logged.event.get()).unwrap_or_default();
            let entry = Entry {
                seq: logged.seq,
                request_id: logged.request_id.as_deref(),
                created_at_ms: logged.created_at,
                body: &body,
            };
            for event in projector.project(&entry) {
                if event.id <= after_event_id
                    || execution_id
                        .as_deref()
                        .is_some_and(|wanted| wanted != event.execution_id)
                {
                    continue;
                }
                let done = execution_id.is_some() && event.terminal;
                if sender.send(sse(&event)).await.is_err() {
                    return;
                }
                if done {
                    return;
                }
            }
        }
        cursor = next_cursor;
        if !has_more {
            tokio::select! {
                changed = wake.changed() => {
                    if changed.is_err() {
                        tokio::time::sleep(TAIL_POLL).await;
                    }
                }
                () = tokio::time::sleep(TAIL_POLL) => {}
                () = sender.closed() => return,
            }
        }
    }
}

async fn page(state: &AppState, thread_key: &str, after: u64) -> Option<Result<Page, String>> {
    ask(state, |reply| Command::Page {
        thread_key: thread_key.to_owned(),
        after,
        reply,
    })
    .await
}

fn sse(event: &StreamEvent) -> Event {
    Event::default()
        .id(event.id.to_string())
        .event(event.event_type)
        .data(&event.data)
}

fn stream_error(message: &str) -> Event {
    warn!(message, "event stream failed");
    Event::default()
        .event("session.stream_error")
        .data(json!({"error": message}).to_string())
}

async fn ask<T>(
    state: &AppState,
    command: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (reply, answer) = oneshot::channel();
    state.commands.send(command(reply)).ok()?;
    answer.await.ok()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "the nanocode worker is unavailable",
    )
}
