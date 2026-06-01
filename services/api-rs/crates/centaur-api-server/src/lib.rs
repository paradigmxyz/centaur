use std::{
    collections::{HashSet, VecDeque},
    convert::Infallible,
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use bytes::Bytes;
use centaur_sandbox_core::{
    OutputStream, ReadOptions, ReadResult, SandboxBackend, SandboxError, SandboxId, SandboxSpec,
    SandboxStatus,
};
use centaur_session_core::{
    HarnessType, SessionEvent, SessionMessageInput, ThreadKey, ThreadKeyError,
};
use centaur_session_sqlx::{PgSessionStore, SessionStoreError, default_metadata};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{sync::Mutex, time::sleep};
use tracing::warn;

const SESSION_OUTPUT_LINE_EVENT: &str = "session.output.line";
const DEFAULT_IDLE_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_MAX_DURATION_MS: u64 = 60_000;
type SandboxSpecFactory = Arc<dyn Fn(&ThreadKey, &str) -> SandboxSpec + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    store: PgSessionStore,
    sandbox_runtime: SandboxRuntime,
    stdout_pumps: Arc<Mutex<HashSet<String>>>,
}

pub fn build_router(store: PgSessionStore) -> Router {
    build_router_with_runtime(store, SandboxRuntime::Mock)
}

pub fn build_router_with_runtime(store: PgSessionStore, sandbox_runtime: SandboxRuntime) -> Router {
    let state = AppState {
        store,
        sandbox_runtime,
        stdout_pumps: Arc::new(Mutex::new(HashSet::new())),
    };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/session/{thread_key}", post(create_or_get_session))
        .route("/api/session/{thread_key}/messages", post(append_messages))
        .route("/api/session/{thread_key}/execute", post(execute_session))
        .route("/api/session/{thread_key}/events", get(stream_events))
        .with_state(state)
}

#[derive(Clone)]
pub enum SandboxRuntime {
    Mock,
    Backend {
        backend: Arc<dyn SandboxBackend>,
        spec_factory: SandboxSpecFactory,
    },
}

impl SandboxRuntime {
    pub fn backend(backend: Arc<dyn SandboxBackend>, spec: SandboxSpec) -> Self {
        let spec_factory = move |_thread_key: &ThreadKey, _execution_id: &str| spec.clone();
        Self::backend_with_spec_factory(backend, spec_factory)
    }

    pub fn backend_with_spec_factory<F>(backend: Arc<dyn SandboxBackend>, spec_factory: F) -> Self
    where
        F: Fn(&ThreadKey, &str) -> SandboxSpec + Send + Sync + 'static,
    {
        Self::Backend {
            backend,
            spec_factory: Arc::new(spec_factory),
        }
    }
}

async fn healthz() -> Json<Value> {
    Json(json!({"ok": true}))
}

async fn create_or_get_session(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<Value>, ApiError> {
    let thread_key = parse_thread_key(raw_thread_key)?;
    let metadata = default_metadata(request.metadata);
    let session = state
        .store
        .create_or_get_session(&thread_key, &request.harness_type, metadata)
        .await?;
    Ok(Json(serde_json::to_value(session)?))
}

async fn append_messages(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<AppendMessagesRequest>,
) -> Result<Json<AppendMessagesResponse>, ApiError> {
    if request.messages.is_empty() {
        return Err(ApiError::BadRequest(
            "messages must not be empty".to_owned(),
        ));
    }
    let thread_key = parse_thread_key(raw_thread_key)?;
    let message_ids = state
        .store
        .append_messages(&thread_key, &request.messages)
        .await?;
    Ok(Json(AppendMessagesResponse {
        ok: true,
        message_ids,
    }))
}

async fn execute_session(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<ExecuteSessionRequest>,
) -> Result<Json<ExecuteSessionResponse>, ApiError> {
    let thread_key = parse_thread_key(raw_thread_key)?;
    let session = state.store.get_session(&thread_key).await?;
    validate_input_lines(&request.input_lines)?;
    let pipe_options = PipeOptions::from_request(&request)?;

    let execution = state
        .store
        .create_execution(&thread_key, default_metadata(request.metadata))
        .await?;
    let execution = state
        .store
        .mark_execution_running(&execution.execution_id)
        .await?;
    let sandbox_id = ensure_session_sandbox(
        &state.store,
        &state.sandbox_runtime,
        &thread_key,
        session.sandbox_id.as_deref(),
        &execution.execution_id,
    )
    .await?;

    state
        .store
        .append_event(
            &thread_key,
            Some(&execution.execution_id),
            "session.execution_started",
            json!({
                "execution_id": execution.execution_id,
                "thread_key": thread_key.as_str(),
                "input_line_count": request.input_lines.len(),
            }),
        )
        .await?;

    let run_result = match &state.sandbox_runtime {
        SandboxRuntime::Mock => {
            run_mock_session_pipe(
                &state.store,
                &thread_key,
                &execution.execution_id,
                &sandbox_id,
                pipe_options,
            )
            .await
        }
        SandboxRuntime::Backend { backend, .. } => {
            match ensure_stdout_pump(&state, &thread_key, &sandbox_id).await {
                Ok(()) => write_input_lines(
                    backend.as_ref(),
                    &SandboxId::new(&sandbox_id),
                    &request.input_lines,
                )
                .await
                .map(|()| 0),
                Err(error) => Err(error),
            }
        }
    };
    let output_line_count = match run_result {
        Ok(output_line_count) => output_line_count,
        Err(error) => {
            let error_message = error.to_string();
            let _ = state
                .store
                .append_event(
                    &thread_key,
                    Some(&execution.execution_id),
                    "session.execution_failed",
                    json!({
                        "execution_id": execution.execution_id,
                        "thread_key": thread_key.as_str(),
                        "error": error_message,
                    }),
                )
                .await;
            let _ = state
                .store
                .fail_execution(&execution.execution_id, &error_message)
                .await;
            return Err(error);
        }
    };

    state
        .store
        .append_event(
            &thread_key,
            Some(&execution.execution_id),
            "session.execution_completed",
            json!({
                "execution_id": execution.execution_id,
                "thread_key": thread_key.as_str(),
                "output_line_count": output_line_count,
                "completion_reason": "input_accepted",
            }),
        )
        .await?;

    let execution = state
        .store
        .complete_execution(&execution.execution_id)
        .await?;
    Ok(Json(ExecuteSessionResponse {
        ok: true,
        execution_id: execution.execution_id,
        thread_key: execution.thread_key,
        status: execution.status.to_string(),
    }))
}

async fn ensure_session_sandbox(
    store: &PgSessionStore,
    runtime: &SandboxRuntime,
    thread_key: &ThreadKey,
    existing_sandbox_id: Option<&str>,
    execution_id: &str,
) -> Result<String, ApiError> {
    match runtime {
        SandboxRuntime::Mock => {
            if let Some(sandbox_id) = existing_sandbox_id {
                return Ok(sandbox_id.to_owned());
            }
            let sandbox_id = format!("mock-sandbox-{execution_id}");
            store
                .update_sandbox_id(thread_key, Some(&sandbox_id))
                .await?;
            Ok(sandbox_id)
        }
        SandboxRuntime::Backend {
            backend,
            spec_factory,
        } => {
            if let Some(sandbox_id) = existing_sandbox_id {
                let id = SandboxId::new(sandbox_id);
                match backend.status(&id).await {
                    Ok(SandboxStatus::Running | SandboxStatus::Created) => {
                        return Ok(sandbox_id.to_owned());
                    }
                    Ok(_) | Err(SandboxError::NotFound(_)) => {}
                    Err(error) => return Err(ApiError::Sandbox(error)),
                }
            }

            let spec = spec_factory(thread_key, execution_id);
            let handle = backend.create(spec).await.map_err(ApiError::Sandbox)?;
            store
                .update_sandbox_id(thread_key, Some(handle.id.as_str()))
                .await?;
            Ok(handle.id.into_string())
        }
    }
}

async fn run_mock_session_pipe(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: &str,
    sandbox_id: &str,
    options: PipeOptions,
) -> Result<usize, ApiError> {
    let mut output_line_count = 0;
    let output_lines = mock_app_server_output_lines(thread_key, execution_id, sandbox_id);
    for (index, line) in output_lines.iter().enumerate() {
        append_output_line(store, thread_key, Some(execution_id), line).await?;
        output_line_count += 1;
        if index + 1 < output_lines.len() {
            sleep(Duration::from_millis(200)).await;
        }
    }
    if options.idle_timeout < Duration::from_millis(DEFAULT_IDLE_TIMEOUT_MS)
        || options.max_duration < Duration::from_millis(DEFAULT_MAX_DURATION_MS)
    {
        sleep(options.idle_timeout).await;
    }
    Ok(output_line_count)
}

async fn ensure_stdout_pump(
    state: &AppState,
    thread_key: &ThreadKey,
    sandbox_id: &str,
) -> Result<(), ApiError> {
    let SandboxRuntime::Backend { backend, .. } = &state.sandbox_runtime else {
        return Ok(());
    };

    let mut pumps = state.stdout_pumps.lock().await;
    if !pumps.insert(sandbox_id.to_owned()) {
        return Ok(());
    }
    drop(pumps);

    let store = state.store.clone();
    let backend = backend.clone();
    let thread_key = thread_key.clone();
    let sandbox_id = SandboxId::new(sandbox_id);
    let pump_key = sandbox_id.as_str().to_owned();
    let stdout_pumps = state.stdout_pumps.clone();

    tokio::spawn(async move {
        let result = run_stdout_pump(store.clone(), backend, thread_key.clone(), sandbox_id).await;
        if let Err(error) = result {
            warn!(%pump_key, %error, "session stdout pump failed");
            let _ = store
                .append_event(
                    &thread_key,
                    None,
                    "session.stdout_pump_failed",
                    json!({
                        "sandbox_id": pump_key,
                        "error": error.to_string(),
                    }),
                )
                .await;
        }
        stdout_pumps.lock().await.remove(&pump_key);
    });

    Ok(())
}

async fn run_stdout_pump(
    store: PgSessionStore,
    backend: Arc<dyn SandboxBackend>,
    thread_key: ThreadKey,
    sandbox_id: SandboxId,
) -> Result<(), ApiError> {
    let mut buffer = String::new();
    loop {
        match backend
            .read_bytes(
                &sandbox_id,
                ReadOptions {
                    stream: OutputStream::Stdout,
                    after_offset: None,
                    max_bytes: 8192,
                    timeout_ms: Some(1_000),
                },
            )
            .await
            .map_err(ApiError::Sandbox)?
        {
            ReadResult::Bytes { bytes, .. } => {
                let chunk = std::str::from_utf8(&bytes).map_err(|err| {
                    ApiError::BadRequest(format!("sandbox stdout was not UTF-8: {err}"))
                })?;
                buffer.push_str(chunk);

                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim_end_matches('\r').to_owned();
                    buffer.drain(..=index);
                    append_output_line(&store, &thread_key, None, &line).await?;
                }
            }
            ReadResult::TimedOut => {}
            ReadResult::Eof => {
                flush_partial_output(&store, &thread_key, None, &mut buffer).await?;
                store
                    .append_event(
                        &thread_key,
                        None,
                        "session.stdout_eof",
                        json!({
                            "sandbox_id": sandbox_id.as_str(),
                        }),
                    )
                    .await?;
                return Ok(());
            }
        }
    }
}

async fn write_input_lines(
    backend: &dyn SandboxBackend,
    sandbox_id: &SandboxId,
    input_lines: &[String],
) -> Result<(), ApiError> {
    for line in input_lines {
        write_input_line(backend, sandbox_id, line).await?;
    }
    Ok(())
}

async fn write_input_line(
    backend: &dyn SandboxBackend,
    sandbox_id: &SandboxId,
    line: &str,
) -> Result<(), ApiError> {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    backend
        .write_bytes(sandbox_id, Bytes::from(bytes))
        .await
        .map_err(ApiError::Sandbox)?;
    Ok(())
}

async fn flush_partial_output(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: Option<&str>,
    buffer: &mut String,
) -> Result<usize, ApiError> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let line = buffer.trim_end_matches('\r').to_owned();
    buffer.clear();
    append_output_line(store, thread_key, execution_id, &line).await?;
    Ok(1)
}

async fn append_output_line(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: Option<&str>,
    line: &str,
) -> Result<(), ApiError> {
    store
        .append_event(
            thread_key,
            execution_id,
            SESSION_OUTPUT_LINE_EVENT,
            Value::String(line.to_owned()),
        )
        .await?;
    Ok(())
}

fn mock_app_server_output_lines(
    thread_key: &ThreadKey,
    execution_id: &str,
    sandbox_id: &str,
) -> Vec<String> {
    let mock_thread_id = format!("mock-codex-thread-{sandbox_id}");
    let mut lines = vec![
        json!({
            "type": "system",
            "subtype": "wrapper_heartbeat",
            "phase": "startup",
            "thread_key": thread_key.as_str(),
            "execution_id": execution_id,
            "sandbox_id": sandbox_id,
        }),
        json!({
            "type": "system",
            "subtype": "wrapper_heartbeat",
            "phase": "app_server_started",
            "thread_key": thread_key.as_str(),
            "execution_id": execution_id,
            "sandbox_id": sandbox_id,
        }),
        json!({
            "type": "thread.started",
            "thread_id": mock_thread_id,
        }),
    ];

    for turn_index in 1..=3 {
        let mock_turn_id = format!("mock-turn-{execution_id}-{turn_index}");
        lines.extend([
            json!({
                "type": "turn.started",
                "turn_id": mock_turn_id,
            }),
            json!({
                "type": "item.agentMessage.delta",
                "turnId": mock_turn_id,
                "session_id": mock_thread_id,
                "delta": format!("PONG {turn_index}"),
            }),
            json!({
                "type": "turn.completed",
                "turn": {
                    "id": mock_turn_id,
                },
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": 1,
                },
            }),
        ]);
    }

    lines.into_iter().map(|value| value.to_string()).collect()
}

async fn stream_events(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let thread_key = parse_thread_key(raw_thread_key)?;
    let session = state.store.get_session(&thread_key).await?;
    if let Some(sandbox_id) = session.sandbox_id.as_deref() {
        ensure_stdout_pump(&state, &thread_key, sandbox_id).await?;
    }

    let stream = stream::unfold(
        SessionEventStreamState {
            store: state.store,
            thread_key,
            after_event_id: query.after_event_id.unwrap_or(0),
            pending: VecDeque::new(),
            done: false,
        },
        |mut state| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    state.after_event_id = event.event_id;
                    return Some((Ok(to_sse_event(event)), state));
                }
                if state.done {
                    return None;
                }
                match state
                    .store
                    .list_events_after(&state.thread_key, state.after_event_id, 100)
                    .await
                {
                    Ok(events) if events.is_empty() => sleep(Duration::from_millis(250)).await,
                    Ok(events) => state.pending = events.into(),
                    Err(error) => {
                        state.done = true;
                        let event = Event::default()
                            .event("session.stream_error")
                            .data(json!({"error": error.to_string()}).to_string());
                        return Some((Ok(event), state));
                    }
                }
            }
        },
    );

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

struct SessionEventStreamState {
    store: PgSessionStore,
    thread_key: ThreadKey,
    after_event_id: i64,
    pending: VecDeque<SessionEvent>,
    done: bool,
}

fn to_sse_event(event: SessionEvent) -> Event {
    let data = if event.event_type == SESSION_OUTPUT_LINE_EVENT {
        event.payload.as_str().unwrap_or_default().to_owned()
    } else {
        serde_json::to_string(&event.payload).unwrap_or_else(|_| "{}".to_owned())
    };
    Event::default()
        .id(event.event_id.to_string())
        .event(event.event_type)
        .data(data)
}

fn parse_thread_key(value: String) -> Result<ThreadKey, ApiError> {
    ThreadKey::parse(value).map_err(ApiError::InvalidThreadKey)
}

fn validate_input_lines(lines: &[String]) -> Result<(), ApiError> {
    for (index, line) in lines.iter().enumerate() {
        if line.contains('\n') || line.contains('\r') {
            return Err(ApiError::BadRequest(format!(
                "input_lines[{index}] must be one line"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct PipeOptions {
    idle_timeout: Duration,
    max_duration: Duration,
}

impl PipeOptions {
    fn from_request(request: &ExecuteSessionRequest) -> Result<Self, ApiError> {
        let idle_timeout = request
            .idle_timeout_ms
            .map(nonzero_duration_millis)
            .transpose()?
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_IDLE_TIMEOUT_MS));
        let max_duration = request
            .max_duration_ms
            .map(nonzero_duration_millis)
            .transpose()?
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_MAX_DURATION_MS));

        if idle_timeout > max_duration {
            return Err(ApiError::BadRequest(
                "idle_timeout_ms must be less than or equal to max_duration_ms".to_owned(),
            ));
        }

        Ok(Self {
            idle_timeout,
            max_duration,
        })
    }
}

fn nonzero_duration_millis(value: u64) -> Result<Duration, ApiError> {
    if value == 0 {
        return Err(ApiError::BadRequest(
            "duration values must be greater than zero".to_owned(),
        ));
    }
    Ok(Duration::from_millis(value))
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    harness_type: HarnessType,
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AppendMessagesRequest {
    messages: Vec<SessionMessageInput>,
}

#[derive(Debug, Serialize)]
struct AppendMessagesResponse {
    ok: bool,
    message_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ExecuteSessionRequest {
    metadata: Option<Value>,
    #[serde(default)]
    input_lines: Vec<String>,
    idle_timeout_ms: Option<u64>,
    max_duration_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ExecuteSessionResponse {
    ok: bool,
    execution_id: String,
    thread_key: ThreadKey,
    status: String,
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    after_event_id: Option<i64>,
}

#[derive(Debug, Error)]
enum ApiError {
    #[error(transparent)]
    InvalidThreadKey(#[from] ThreadKeyError),
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Store(#[from] SessionStoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::InvalidThreadKey(_) | Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Store(SessionStoreError::NotFound { .. }) => StatusCode::NOT_FOUND,
            Self::Store(SessionStoreError::HarnessConflict { .. }) => StatusCode::CONFLICT,
            Self::Store(_) | Self::Sandbox(_) | Self::Serialize(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = Json(json!({
            "ok": false,
            "error": self.to_string(),
        }));
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::build_router;
    use centaur_session_sqlx::PgSessionStore;
    use sqlx::PgPool;

    #[tokio::test]
    async fn router_builds() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let _router = build_router(PgSessionStore::new(pool));
    }
}
