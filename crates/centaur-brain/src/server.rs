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
use centaur_codemode_protocol::token::TurnTokenVerifier;
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::{
    codemode::TurnToken,
    projector::{Entry, Projector, StreamEvent},
    sessions::SessionIds,
    worker::{Command, Page, SubmitError, ThreadRef},
};

/// Re-read the log at least this often, in case a wake-up was missed.
const TAIL_POLL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct AppState {
    pub commands: mpsc::UnboundedSender<Command>,
    pub sessions: SessionIds,
    /// Verifies submitted Code Mode turn tokens; without it they are ignored.
    pub verifier: Option<TurnTokenVerifier>,
}

impl AppState {
    /// The thread with its nanocode session, assigning one if it is new.
    async fn thread(&self, thread_key: String) -> Result<ThreadRef, Box<Response>> {
        match self.sessions.resolve(&thread_key).await {
            Ok(session_id) => Ok(ThreadRef {
                key: thread_key,
                session_id,
            }),
            Err(failure) => {
                warn!(%failure, "cannot resolve the thread's session");
                Err(Box::new(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "cannot resolve the thread's session",
                )))
            }
        }
    }
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
    /// The turn's Code Mode token, minted by api-rs. It is kept per thread,
    /// not in the nanocode submission: api-rs mints a fresh one on every
    /// retry of a request, which must not look like a different turn.
    #[serde(default)]
    tool_token: Option<String>,
}

async fn submit(
    State(state): State<AppState>,
    Path(thread_key): Path<String>,
    Json(request): Json<SubmitRequest>,
) -> Response {
    if request.request_id.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "request_id is required");
    }
    let tool_token = verified_token(&state, &thread_key, request.tool_token.as_deref());
    let thread = match state.thread(thread_key).await {
        Ok(thread) => thread,
        Err(response) => return *response,
    };
    let outcome = ask(&state, |reply| Command::Submit {
        thread,
        request_id: request.request_id.clone(),
        input_line: request.input_line,
        steer: request.steer,
        tool_token,
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

/// The submitted token and its expiry, once verified for this thread.
///
/// A token that does not verify is dropped, not fatal: the turn still runs,
/// only without Code Mode tools, so a key mismatch or rotation never takes
/// every `nanocode` turn down. Nothing is stored for it.
fn verified_token(state: &AppState, thread_key: &str, token: Option<&str>) -> Option<TurnToken> {
    let token = token.map(str::trim).filter(|token| !token.is_empty())?;
    let Some(verifier) = &state.verifier else {
        warn!(
            "a turn brought a Code Mode token, but the brain has no public key to verify it; ignoring it"
        );
        return None;
    };
    let claims = match verifier.verify(token) {
        Ok(claims) => claims,
        Err(failure) => {
            warn!(thread_key, %failure, "ignoring a Code Mode token that does not verify");
            return None;
        }
    };
    if claims.thread_key != thread_key {
        warn!(
            thread_key,
            "ignoring a Code Mode token minted for another thread"
        );
        return None;
    }
    Some(TurnToken {
        token: token.to_owned(),
        expires_at: claims.exp,
    })
}

async fn status(State(state): State<AppState>, Path(thread_key): Path<String>) -> Response {
    let thread = match state.thread(thread_key).await {
        Ok(thread) => thread,
        Err(response) => return *response,
    };
    match ask(&state, |reply| Command::Status { thread, reply }).await {
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
    let thread = match state.thread(thread_key).await {
        Ok(thread) => thread,
        Err(response) => return *response,
    };
    match ask(&state, |reply| Command::Interrupt { thread, reply }).await {
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
    let thread = match state.sessions.resolve(&thread_key).await {
        Ok(session_id) => ThreadRef {
            key: thread_key.clone(),
            session_id,
        },
        Err(failure) => {
            let _ = sender
                .send(stream_error(&format!(
                    "cannot resolve the thread's session: {failure}"
                )))
                .await;
            return;
        }
    };
    let mut projector: Option<Projector> = None;
    let mut cursor = 0;
    loop {
        let Some(page) = page(&state, &thread, cursor).await else {
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

async fn page(state: &AppState, thread: &ThreadRef, after: u64) -> Option<Result<Page, String>> {
    ask(state, |reply| Command::Page {
        thread: thread.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{codemode::tests::keys, worker};
    use centaur_codemode_protocol::token::{ToolPolicy, TurnGrant, TurnTokenSigner};

    struct Brain {
        url: String,
        signer: TurnTokenSigner,
        client: reqwest::Client,
    }

    /// A brain on an in-memory log, with Code Mode when given its URL.
    async fn brain_with(verify: bool, model_url: &str, code_mode_url: Option<&str>) -> Brain {
        let (private, public) = keys::pair();
        let (commands, receiver) = mpsc::unbounded_channel();
        let code_mode = code_mode_url.map(|url| {
            std::sync::Arc::new(
                crate::codemode::CodeMode::new(
                    crate::codemode::Endpoints {
                        executor_url: url.trim_end_matches('/').to_owned(),
                    },
                    tokio::runtime::Handle::current(),
                )
                .unwrap(),
            )
        });
        worker::spawn(
            worker::Config {
                database_url: None,
                default_model: "luna".into(),
                thinking: "low".into(),
                openai_base_url: Some(model_url.into()),
                anthropic_base_url: Some(model_url.into()),
                code_mode,
            },
            receiver,
        )
        .unwrap();
        let state = AppState {
            commands,
            sessions: SessionIds::new(None).unwrap(),
            verifier: verify.then(|| TurnTokenVerifier::from_pem(public.as_bytes()).unwrap()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        Brain {
            url,
            signer: TurnTokenSigner::from_pem(private.as_bytes()).unwrap(),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        }
    }

    impl Brain {
        fn token(&self, thread_key: &str, issued_at: i64) -> (String, i64) {
            let grant = TurnGrant {
                principal: "prn_channel".into(),
                requester: None,
                thread_key: thread_key.into(),
                execution_id: "exe_1".into(),
                tools: ToolPolicy::default(),
            };
            let (token, claims) = self.signer.sign(&grant, issued_at).unwrap();
            (token, claims.exp)
        }

        async fn submit(
            &self,
            thread_key: &str,
            request_id: &str,
            token: Option<&str>,
        ) -> (u16, Value) {
            let response = self
                .client
                .post(format!("{}/v1/threads/{thread_key}/turns", self.url))
                .json(&json!({
                    "request_id": request_id,
                    "input_line": {"type": "user", "message": {"content": [{"type": "text", "text": "hi"}]}},
                    "tool_token": token,
                }))
                .send()
                .await
                .unwrap();
            (response.status().as_u16(), response.json().await.unwrap())
        }
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// A brain whose model runs one cell per turn, on a mock executor.
    async fn brain_with_cells(
        verify: bool,
        gate: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    ) -> (
        Brain,
        crate::codemode::tests::Seen,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    ) {
        use crate::codemode::tests::{Seen, serve};
        let seen = Seen::default();
        let executor = serve(
            vec![(
                "/v1/exec",
                200,
                json!({"ok": true, "result": 1, "logs": []}),
            )],
            &seen,
        )
        .await;
        let (model_url, bodies) = model_server(gate).await;
        (
            brain_with(verify, &model_url, Some(&executor)).await,
            seen,
            bodies,
        )
    }

    /// A token that does not verify, or one the brain cannot verify, is
    /// dropped: the turn still runs, and its tool calls reach no executor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_turn_token_that_does_not_verify_is_dropped_not_fatal() {
        for verify in [true, false] {
            let (brain, seen, _) = brain_with_cells(verify, None).await;
            let (valid, _) = brain.token("slack:T:C:1", now());
            let (other, _) = brain.token("slack:T:C:2", now());
            let (expired, _) = brain.token("slack:T:C:1", now() - 2 * 3600);
            let tokens: Vec<Option<&str>> = if verify {
                vec![Some(&other), Some("junk"), Some(&expired), None]
            } else {
                vec![Some(&valid)]
            };
            for (index, token) in tokens.into_iter().enumerate() {
                let request_id = format!("exe_{index}");
                let (status, body) = brain.submit("slack:T:C:1", &request_id, token).await;
                assert_eq!(status, 200, "{body}");
                assert_eq!(body["created"], true, "{body}");
                let stream = events(&brain, "slack:T:C:1", &request_id).await;
                assert!(stream.contains("The answer is 42."), "{stream}");
                assert!(stream.contains(crate::codemode::UNAVAILABLE), "{stream}");
            }
            assert!(seen.lock().unwrap().is_empty());
        }
    }

    /// api-rs mints a new token for every retry of a request. The retry is the
    /// same turn, which keeps the token it was first submitted with.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_retry_is_the_same_turn_with_its_first_token() {
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let (brain, seen, _) = brain_with_cells(true, Some(std::sync::Arc::clone(&gate))).await;
        let thread = "slack:T:C:1";
        let (first, _) = brain.token(thread, now() - 60);
        let (second, _) = brain.token(thread, now());
        let (status, body) = brain.submit(thread, "exe_1", Some(&first)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["created"], true);
        let (status, body) = brain.submit(thread, "exe_1", Some(&second)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["created"], false);
        gate.add_permits(1);
        let stream = events(&brain, thread, "exe_1").await;
        assert!(stream.contains("The answer is 42."), "{stream}");
        assert_eq!(seen.lock().unwrap()[0].1, format!("Bearer {first}"));
    }

    /// A scripted OpenAI Responses endpoint: it calls `centaur_execute` once,
    /// then answers. Returns its base URL and the request bodies it saw. With
    /// a `gate`, each tool call waits for a permit.
    async fn model_server(
        gate: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::{extract::State as Shared, response::IntoResponse};
        type Bodies = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;
        type Gate = Option<std::sync::Arc<tokio::sync::Semaphore>>;
        async fn responses(
            Shared((bodies, gate)): Shared<(Bodies, Gate)>,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            // Each turn calls the tool once, then answers: answered when a
            // tool output follows the turn's own (last) user message.
            let input = body["input"].as_array().cloned().unwrap_or_default();
            let last_user = input
                .iter()
                .rposition(|item| item["role"] == "user")
                .unwrap_or(0);
            let answered = input[last_user..]
                .iter()
                .any(|item| item["type"] == "function_call_output");
            // Numbered by the tool outputs before it, so each turn's call id
            // is new.
            let turn = 1 + input
                .iter()
                .filter(|item| item["type"] == "function_call_output")
                .count();
            bodies.lock().unwrap().push(body);
            if !answered && let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            let usage = json!({
                "input_tokens": 0, "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 0, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 0
            });
            let response = |status: &str, output: Value| json!({"id": "resp_1", "object": "response", "model": "luna", "status": status, "output": output, "usage": usage});
            let events = if answered {
                let part =
                    json!({"type": "output_text", "text": "The answer is 42.", "annotations": []});
                let message = |status: &str, content: Value| json!({"type": "message", "id": "msg_1", "role": "assistant", "status": status, "content": content});
                vec![
                    json!({"type": "response.created", "response": response("in_progress", json!([]))}),
                    json!({"type": "response.output_item.added", "output_index": 0, "item": message("in_progress", json!([]))}),
                    json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "The answer is 42."}),
                    json!({"type": "response.output_item.done", "output_index": 0, "item": message("completed", json!([part]))}),
                    json!({"type": "response.completed", "response": response("completed", json!([message("completed", json!([part]))]))}),
                ]
            } else {
                let call = json!({
                    "type": "function_call", "id": format!("fc_{turn}"), "call_id": format!("call_cell_{turn}"), "status": "completed",
                    "name": "centaur_execute", "arguments": "{\"code\":\"return 42\"}"
                });
                vec![
                    json!({"type": "response.created", "response": response("in_progress", json!([]))}),
                    json!({"type": "response.output_item.added", "output_index": 0, "item": call}),
                    json!({"type": "response.output_item.done", "output_index": 0, "item": call}),
                    json!({"type": "response.completed", "response": response("completed", json!([call]))}),
                ]
            };
            let stream = events
                .into_iter()
                .enumerate()
                .map(|(sequence, mut event)| {
                    event["sequence_number"] = json!(sequence);
                    format!(
                        "event: {}\ndata: {event}\n\n",
                        event["type"].as_str().unwrap()
                    )
                })
                .collect::<String>();
            ([("content-type", "text/event-stream")], stream)
        }
        let bodies = Bodies::default();
        let router = Router::new()
            .route("/v1/responses", post(responses))
            .with_state((std::sync::Arc::clone(&bodies), gate));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });
        (url, bodies)
    }

    async fn events(brain: &Brain, thread: &str, execution_id: &str) -> String {
        tokio::time::timeout(
            Duration::from_secs(60),
            brain
                .client
                .get(format!(
                    "{}/v1/threads/{thread}/events?execution_id={execution_id}",
                    brain.url
                ))
                .send()
                .await
                .unwrap()
                .text(),
        )
        .await
        .expect("the turn ends")
        .unwrap()
    }

    /// A running turn's tool calls use its own token, even after another
    /// requester's turn, with a later-expiring token, is queued behind it; the
    /// queued turn then runs with its own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_queued_turns_token_does_not_reach_the_running_turn() {
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let (brain, seen, bodies) =
            brain_with_cells(true, Some(std::sync::Arc::clone(&gate))).await;
        let thread = "slack:T:C:8";
        let (first, _) = brain.token(thread, now() - 60);
        let (second, _) = brain.token(thread, now());
        assert_eq!(brain.submit(thread, "exe_1", Some(&first)).await.0, 200);
        tokio::time::timeout(Duration::from_secs(30), async {
            while bodies.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first turn reaches the model");

        let (status, body) = brain.submit(thread, "exe_2", Some(&second)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["steered"], false, "{body}");
        gate.add_permits(2);
        for execution in ["exe_1", "exe_2"] {
            let stream = events(&brain, thread, execution).await;
            assert!(stream.contains("The answer is 42."), "{stream}");
        }
        let seen = seen.lock().unwrap();
        let tokens = seen.iter().map(|entry| entry.1.clone()).collect::<Vec<_>>();
        assert_eq!(
            tokens,
            [format!("Bearer {first}"), format!("Bearer {second}")],
            "each turn used its own token"
        );
    }

    /// A model's Code Mode call reaches the executor with the turn's token,
    /// its result reaches the model, and the stream shows it as an MCP call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_turn_runs_a_cell_through_the_executor() {
        use crate::codemode::tests::{Seen, serve};
        let seen = Seen::default();
        let executor = serve(
            vec![(
                "/v1/exec",
                200,
                json!({"ok": true, "result": {"answer": 42}, "logs": []}),
            )],
            &seen,
        )
        .await;
        let (model_url, bodies) = model_server(None).await;
        let brain = brain_with(true, &model_url, Some(&executor)).await;
        let thread = "slack:T:C:9";
        let (token, _) = brain.token(thread, now());
        let (status, body) = brain.submit(thread, "exe_1", Some(&token)).await;
        assert_eq!(status, 200, "{body}");

        let stream = tokio::time::timeout(
            Duration::from_secs(60),
            brain
                .client
                .get(format!(
                    "{}/v1/threads/{thread}/events?execution_id=exe_1",
                    brain.url
                ))
                .send()
                .await
                .unwrap()
                .text(),
        )
        .await
        .expect("the turn ends")
        .unwrap();
        let lines = stream
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .collect::<Vec<_>>();
        assert!(
            lines
                .iter()
                .any(|line| line["result_text"] == "The answer is 42."),
            "{stream}"
        );
        let tool_items = lines
            .iter()
            .filter(|line| line["params"]["item"]["type"] == "mcpToolCall")
            .map(|line| (line["method"].as_str().unwrap(), &line["params"]["item"]))
            .collect::<Vec<_>>();
        assert_eq!(tool_items.len(), 2, "{stream}");
        assert_eq!(tool_items[0].0, "item/started");
        assert_eq!(tool_items[1].0, "item/completed");
        let completed = tool_items[1].1;
        assert_eq!(completed["server"], "centaur");
        assert_eq!(completed["tool"], "centaur_execute");
        assert_eq!(completed["status"], "completed", "{completed}");
        assert_eq!(completed["id"], "call_cell_1");
        assert_eq!(completed["arguments"], json!({"code": "return 42"}));
        assert!(
            completed["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("\"answer\": 42")
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "/v1/exec");
        assert_eq!(seen[0].1, format!("Bearer {token}"));
        assert_eq!(
            seen[0].2,
            json!({"code": "return 42", "cell_id": "call_cell_1"})
        );

        // The model sees the tools as direct functions and the instructions
        // as developer instructions.
        let bodies = bodies.lock().unwrap();
        let developer = bodies[0]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["role"] == "developer")
            .collect::<Vec<_>>();
        let mut tool_names = Vec::new();
        let mut pending = developer
            .iter()
            .flat_map(|item| item["tools"].as_array().cloned().unwrap_or_default())
            .collect::<Vec<_>>();
        while let Some(tool) = pending.pop() {
            if let Some(nested) = tool["tools"].as_array() {
                pending.extend(nested.iter().cloned());
            } else if tool["type"] != "custom" {
                tool_names.push(tool["name"].as_str().unwrap_or_default().to_owned());
            }
        }
        // Exactly the Code Mode tools: no nanocode `exec`, no subagents.
        tool_names.sort();
        assert_eq!(
            tool_names,
            [
                "centaur_catalog_load",
                "centaur_catalog_search",
                "centaur_execute",
            ],
        );
        assert!(
            developer
                .iter()
                .any(|item| item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|part| part["text"].as_str().is_some_and(
                        |text| text.contains(centaur_codemode_protocol::tools::INSTRUCTIONS)
                    ))),
            "the Code Mode instructions reach the model"
        );
        let output = bodies[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(output["call_id"], "call_cell_1");
    }
}
