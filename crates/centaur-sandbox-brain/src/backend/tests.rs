use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use centaur_sandbox_core::{SandboxBackend, SandboxError, SandboxId, SandboxSpec, SandboxStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::BrainSandboxBackend;

#[derive(Clone, Default)]
struct TestState {
    sandboxes: Arc<Mutex<HashSet<String>>>,
    create_requests: Arc<Mutex<Vec<Value>>>,
    received: Arc<Mutex<Vec<String>>>,
    output_queries: Arc<Mutex<Vec<Option<u64>>>>,
}

async fn spawn_server() -> (String, TestState) {
    let state = TestState::default();
    let app = Router::new()
        .route("/v1/sandboxes", post(create_sandbox))
        .route(
            "/v1/sandboxes/{id}",
            get(sandbox_status).delete(stop_sandbox),
        )
        .route("/v1/sandboxes/{id}/io", get(sandbox_io))
        .route("/v1/sandboxes/{id}/output", get(sandbox_output))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), state)
}

async fn create_sandbox(
    State(state): State<TestState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let id = body["id"].as_str().unwrap().to_owned();
    state.sandboxes.lock().unwrap().insert(id.clone());
    state.create_requests.lock().unwrap().push(body);
    (StatusCode::CREATED, Json(json!({ "id": id })))
}

async fn sandbox_status(
    State(state): State<TestState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if state.sandboxes.lock().unwrap().contains(&id) {
        (StatusCode::OK, Json(json!({ "status": "running" })))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown sandbox" })),
        )
    }
}

async fn stop_sandbox(State(state): State<TestState>, Path(id): Path<String>) -> impl IntoResponse {
    if state.sandboxes.lock().unwrap().remove(&id) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Deserialize)]
struct OutputQuery {
    since_ms: Option<u64>,
}

async fn sandbox_output(
    State(state): State<TestState>,
    Path(id): Path<String>,
    Query(query): Query<OutputQuery>,
) -> impl IntoResponse {
    if !state.sandboxes.lock().unwrap().contains(&id) {
        return (StatusCode::NOT_FOUND, Json(json!({ "lines": [] })));
    }
    state.output_queries.lock().unwrap().push(query.since_ms);
    (
        StatusCode::OK,
        Json(json!({ "lines": ["line-one", "line-two"] })),
    )
}

async fn sandbox_io(
    ws: WebSocketUpgrade,
    State(state): State<TestState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, id))
}

async fn handle_socket(mut socket: WebSocket, state: TestState, id: String) {
    if !state.sandboxes.lock().unwrap().contains(&id) {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    if socket.send(Message::Text("hello".into())).await.is_err() {
        return;
    }
    while let Some(message) = socket.recv().await {
        match message {
            Ok(Message::Text(text)) => {
                let text = text.to_string();
                state.received.lock().unwrap().push(text.clone());
                if text == "bye" {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                if socket.send(Message::Text(text.into())).await.is_err() {
                    return;
                }
            }
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn create_backend() -> (BrainSandboxBackend, TestState) {
    let (url, state) = spawn_server().await;
    (BrainSandboxBackend::new(&url).unwrap(), state)
}

async fn create_sandbox_handle(backend: &BrainSandboxBackend) -> SandboxId {
    let spec = SandboxSpec::new("ignored-image")
        .label("centaur.ai/harness", "nanocode")
        .label("centaur.ai/component", "session-sandbox")
        .env("CENTAUR_THREAD_KEY", "slack:C123:1.5");
    backend.create(spec).await.unwrap().id
}

#[tokio::test]
async fn brain_proxy_and_lifecycle_methods_are_noops() {
    let (backend, state) = create_backend().await;
    let id = SandboxId::new("brain-anything");
    let labels = std::collections::BTreeMap::new();

    backend
        .assign_iron_control_proxy_principal(&id, "prn_x", Some("prn_y"), &labels)
        .await
        .unwrap();
    backend
        .ensure_iron_control_proxy_resources(&id, "prn_x", None, &labels)
        .await
        .unwrap();
    backend.enable_observability(&id).await.unwrap();
    backend.pause(&id).await.unwrap();
    backend.resume(&id).await.unwrap();
    assert!(backend.list_observed().await.unwrap().is_empty());
    assert!(
        backend
            .reap_orphan_iron_proxy_resources(Duration::ZERO)
            .await
            .unwrap()
            .is_empty()
    );
    // None of those calls touched the brain control API.
    assert!(state.sandboxes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn create_posts_id_thread_key_and_labels() {
    let (backend, state) = create_backend().await;
    let id = create_sandbox_handle(&backend).await;

    assert!(id.as_str().starts_with("brain-"));
    assert_eq!(id.as_str().len(), "brain-".len() + 32);
    assert!(state.sandboxes.lock().unwrap().contains(id.as_str()));

    let requests = state.create_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["id"], id.as_str());
    assert_eq!(requests[0]["thread_key"], "slack:C123:1.5");
    assert_eq!(requests[0]["labels"]["centaur.ai/harness"], "nanocode");
    assert_eq!(
        requests[0]["labels"]["centaur.ai/component"],
        "session-sandbox"
    );
}

#[tokio::test]
async fn create_without_thread_key_sends_null() {
    let (backend, state) = create_backend().await;
    backend
        .create(SandboxSpec::new("ignored").label("centaur.ai/harness", "nanocode"))
        .await
        .unwrap();

    let requests = state.create_requests.lock().unwrap();
    assert!(requests[0]["thread_key"].is_null());
}

#[tokio::test]
async fn status_and_observe_and_stop() {
    let (backend, _state) = create_backend().await;
    let id = create_sandbox_handle(&backend).await;

    assert_eq!(backend.status(&id).await.unwrap(), SandboxStatus::Running);
    let observed = backend.observe(&id).await.unwrap();
    assert_eq!(observed.id, id);
    assert_eq!(observed.backend, "brain");
    assert_eq!(observed.status, SandboxStatus::Running);

    backend.stop(&id).await.unwrap();
    // A second stop is idempotent, and status now reports the handle gone.
    backend.stop(&id).await.unwrap();
    assert_eq!(backend.status(&id).await.unwrap(), SandboxStatus::Gone);
}

#[tokio::test]
async fn unknown_status_is_gone() {
    let (backend, _state) = create_backend().await;
    assert_eq!(
        backend
            .status(&SandboxId::new("brain-never-created"))
            .await
            .unwrap(),
        SandboxStatus::Gone
    );
}

#[tokio::test]
async fn read_output_since_passes_timestamp_and_parses_lines() {
    let (backend, state) = create_backend().await;
    let id = create_sandbox_handle(&backend).await;

    let lines = backend.read_output_since(&id, None).await.unwrap();
    assert_eq!(lines, vec!["line-one", "line-two"]);

    let since = SystemTime::UNIX_EPOCH + Duration::from_millis(1_500);
    let lines = backend.read_output_since(&id, Some(since)).await.unwrap();
    assert_eq!(lines, vec!["line-one", "line-two"]);

    assert_eq!(
        *state.output_queries.lock().unwrap(),
        vec![None, Some(1_500)]
    );

    let missing = backend
        .read_output_since(&SandboxId::new("brain-missing"), None)
        .await
        .unwrap_err();
    assert!(matches!(missing, SandboxError::NotFound(_)));
}

#[tokio::test]
async fn open_io_frames_lines_in_both_directions() {
    let (backend, state) = create_backend().await;
    let id = create_sandbox_handle(&backend).await;
    let parts = backend.open_io(&id).await.unwrap().into_parts();
    let _guard = parts.guard;
    let mut stdin = parts.stdin;
    let mut stdout = BufReader::new(parts.stdout);

    // The server greets every attach with one Text frame.
    let mut greeting = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        stdout.read_line(&mut greeting).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(greeting, "hello\n");

    // Two complete lines in one write plus a partial third line: each complete
    // line becomes one frame; the partial line waits for its remainder.
    stdin.write_all(b"one\ntwo\nthr").await.unwrap();
    stdin.flush().await.unwrap();

    let mut line = String::new();
    for expected in ["one\n", "two\n"] {
        line.clear();
        tokio::time::timeout(Duration::from_secs(5), async {
            stdout.read_line(&mut line).await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(line, expected, "echoed frames keep their newline framing");
    }

    // The buffered partial line plus this write form one frame; CRLF framing
    // is normalized away.
    stdin.write_all(b"ee\r\n").await.unwrap();
    stdin.flush().await.unwrap();
    line.clear();
    tokio::time::timeout(Duration::from_secs(5), async {
        stdout.read_line(&mut line).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(line, "three\n");

    assert_eq!(*state.received.lock().unwrap(), vec!["one", "two", "three"]);
}

#[tokio::test]
async fn stdout_hits_eof_when_the_server_closes() {
    let (backend, state) = create_backend().await;
    let id = create_sandbox_handle(&backend).await;
    let parts = backend.open_io(&id).await.unwrap().into_parts();
    let _guard = parts.guard;
    let mut stdin = parts.stdin;
    let mut stdout = BufReader::new(parts.stdout);

    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    assert_eq!(line, "hello\n");

    stdin.write_all(b"bye\n").await.unwrap();
    stdin.flush().await.unwrap();

    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).await.unwrap();
        rest
    })
    .await
    .unwrap();
    assert!(read.is_empty(), "server close becomes stdout EOF");
    assert_eq!(*state.received.lock().unwrap(), vec!["bye"]);
}

#[tokio::test]
async fn dropping_the_guard_closes_the_socket() {
    let (backend, _state) = create_backend().await;
    let id = create_sandbox_handle(&backend).await;
    let handle = tokio::spawn({
        let backend_url = backend.config().http_base().to_owned();
        let id = id.clone();
        async move {
            let backend = BrainSandboxBackend::new(backend_url).unwrap();
            let parts = backend.open_io(&id).await.unwrap().into_parts();
            // Drop everything: the connection task is aborted by the guard, so
            // a later attach must succeed instead of waiting on a dead socket.
            drop(parts);
        }
    });
    handle.await.unwrap();

    let parts = backend.open_io(&id).await.unwrap().into_parts();
    let _guard = parts.guard;
    assert!(parts.instance_id.is_none(), "brain io has no instance id");
}
