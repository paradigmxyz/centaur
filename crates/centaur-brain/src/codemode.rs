//! The Code Mode tools of a nanocode turn: `centaur_catalog_search`,
//! `centaur_catalog_load`, and `centaur_execute` all call the executor (RFC
//! 0006). Each call carries the thread's current turn token and nothing else:
//! the executor verifies it, and presents it to the tenant's iron-proxy for the
//! connectors' upstream credentials. The brain never sees a credential.
//!
//! The tools run on nanocode's executor, which is not tokio, while reqwest and
//! sqlx need a tokio reactor. Each call therefore runs as a task on the brain's
//! tokio runtime, and the tool awaits its join handle, which any executor can
//! poll. Dropping the tool's future (a cancelled turn) aborts the task.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use centaur_codemode_protocol::{
    EXECUTOR_URL_ENV, TOKEN_PUBLIC_KEY_ENV,
    token::TurnTokenVerifier,
    tools::{self, Rendered, ToolSpec},
    wire::{EXEC_PATH, ExecRequest, LOAD_PATH, LoadRequest, SEARCH_PATH, SearchRequest},
};
use nanocode::{
    session_host::TurnState,
    tool_contract::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult},
    turnlog::{EffectClass, Effects, OneLog},
};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

/// Search and load answer from the executor's catalog.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);
/// A cell runs for at most 120 s, but may queue behind others for a slot.
const EXEC_TIMEOUT: Duration = Duration::from_secs(12 * 60);
const MAX_RESPONSE_BYTES: usize = 24 * 1024 * 1024;

pub const UNAVAILABLE: &str = "Code Mode is not available for this conversation.";
pub const EXPIRED: &str = "This turn's Code Mode access expired, so the tool was not run. \
     Access is renewed with the next message in this conversation.";
pub const BUSY: &str = "Code Mode is busy; retry in a few seconds.";

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Where the Code Mode executor is, when Code Mode is configured.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub executor_url: String,
}

/// The brain's Code Mode configuration, from its environment.
pub struct Setup {
    /// Verifies the turn tokens api-rs submits; `None` ignores them.
    pub verifier: Option<TurnTokenVerifier>,
    /// `Some` registers the tools.
    pub endpoints: Option<Endpoints>,
}

impl Setup {
    pub fn from_env() -> Result<Self, Error> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let get = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
        let verifier = get(TOKEN_PUBLIC_KEY_ENV)
            .map(|pem| TurnTokenVerifier::from_pem(pem.as_bytes()))
            .transpose()
            .map_err(|error| format!("{TOKEN_PUBLIC_KEY_ENV}: {error}"))?;
        let executor_url = get(EXECUTOR_URL_ENV);
        let endpoints = match (executor_url, &verifier) {
            (Some(executor_url), Some(_)) => Some(Endpoints {
                executor_url: executor_url.trim_end_matches('/').to_owned(),
            }),
            (executor, _) => {
                let missing = [
                    (EXECUTOR_URL_ENV, executor.is_none()),
                    (TOKEN_PUBLIC_KEY_ENV, verifier.is_none()),
                ]
                .into_iter()
                .filter_map(|(name, missing)| missing.then_some(name))
                .collect::<Vec<_>>();
                if missing.len() < 2 {
                    warn!(
                        missing = missing.join(", "),
                        "Code Mode is partly configured; its tools are off"
                    );
                }
                None
            }
        };
        Ok(Self {
            verifier,
            endpoints,
        })
    }
}

/// Which of the three tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Search,
    Load,
    Execute,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Search, Self::Load, Self::Execute];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Search => tools::SEARCH,
            Self::Load => tools::LOAD,
            Self::Execute => tools::EXECUTE,
        }
    }

    fn spec(self) -> ToolSpec {
        tools::specs()
            .into_iter()
            .find(|spec| spec.name == self.name())
            .expect("the protocol defines every Code Mode tool")
    }
}

/// How the session log treats each tool. nanocode looks a call's class up by
/// tool name when it runs (undeclared tools are `External`). Search and load
/// only read the catalog, so they get no log row and are simply re-run on
/// resume. A cell can act on the world: its call is committed to the log
/// before it runs, and a turn resumed after a crash mid-cell records that
/// call's outcome as unknown instead of running it again.
pub fn effects() -> Effects {
    Effects::default()
        .declare(tools::SEARCH, EffectClass::None)
        .declare(tools::LOAD, EffectClass::None)
        .declare(tools::EXECUTE, EffectClass::External)
}

/// Calls the executor for every thread's tools.
pub struct CodeMode {
    endpoints: Endpoints,
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
}

impl CodeMode {
    pub fn new(
        endpoints: Endpoints,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, reqwest::Error> {
        // In-cluster calls: never through an ambient egress proxy.
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            endpoints,
            client,
            runtime,
        })
    }

    /// The three tools, bound to one thread. `log` is the thread's session
    /// log: a call reads the running turn from it and uses the token stored
    /// in that turn's own record, never another turn's.
    pub fn tools(self: &Arc<Self>, thread_key: &str, log: &Arc<dyn Turns>) -> Vec<CentaurTool> {
        Kind::ALL
            .into_iter()
            .map(|kind| CentaurTool {
                code_mode: Arc::clone(self),
                kind,
                spec: kind.spec(),
                thread_key: thread_key.to_owned(),
                log: Arc::clone(log),
            })
            .collect()
    }

    /// Run one call on the tokio runtime, from any executor.
    async fn run(self: &Arc<Self>, kind: Kind, turn: Turn, call_id: &str, args: Value) -> Rendered {
        let this = Arc::clone(self);
        let call_id = call_id.to_owned();
        let task = self
            .runtime
            .spawn(async move { this.call(kind, &turn, &call_id, args).await });
        match AbortOnDrop(task).await {
            Ok(rendered) => rendered,
            Err(error) => Rendered::error(format!("The Code Mode call did not finish: {error}")),
        }
    }

    /// One call, on the tokio runtime.
    async fn call(&self, kind: Kind, turn: &Turn, call_id: &str, args: Value) -> Rendered {
        let thread_key = turn.thread_key.as_str();
        let request_id = turn.request_id.as_str();
        let Some(token) = &turn.token else {
            return Rendered::error(UNAVAILABLE);
        };
        if token.expires_at <= now_secs() {
            return Rendered::error(EXPIRED);
        }
        let started = std::time::Instant::now();
        let rendered = self.request(kind, &token.token, call_id, args).await;
        info!(
            thread_key,
            request_id,
            tool = kind.name(),
            call_id,
            ok = !rendered.is_error,
            ms = started.elapsed().as_millis() as u64,
            "Code Mode tool call"
        );
        rendered
    }

    async fn request(&self, kind: Kind, token: &str, call_id: &str, args: Value) -> Rendered {
        let invalid = |error: serde_json::Error| {
            Rendered::error(format!("Invalid arguments for {}: {error}", kind.name()))
        };
        match kind {
            Kind::Search => {
                let body: SearchRequest = match serde_json::from_value(args) {
                    Ok(body) => body,
                    Err(error) => return invalid(error),
                };
                let url = format!("{}{SEARCH_PATH}", self.endpoints.executor_url);
                match self.post(&url, token, &body, CATALOG_TIMEOUT).await {
                    Ok(Reply::Ok(body)) => tools::render_search(body),
                    Ok(reply) => reply.failure("search"),
                    Err(message) => Rendered::error(message),
                }
            }
            Kind::Load => {
                let body: LoadRequest = match serde_json::from_value(args) {
                    Ok(body) => body,
                    Err(error) => return invalid(error),
                };
                let url = format!("{}{LOAD_PATH}", self.endpoints.executor_url);
                match self.post(&url, token, &body, CATALOG_TIMEOUT).await {
                    Ok(Reply::Ok(reply)) => tools::render_load(&reply),
                    Ok(Reply::Status(StatusCode::NOT_FOUND, _)) => {
                        tools::render_unknown_tool(&body.tool)
                    }
                    Ok(reply) => reply.failure("load"),
                    Err(message) => Rendered::error(message),
                }
            }
            Kind::Execute => {
                #[derive(Deserialize)]
                struct Args {
                    code: String,
                }
                let Args { code } = match serde_json::from_value(args) {
                    Ok(args) => args,
                    Err(error) => return invalid(error),
                };
                let body = ExecRequest {
                    code,
                    cell_id: call_id.to_owned(),
                };
                let url = format!("{}{EXEC_PATH}", self.endpoints.executor_url);
                match self.post(&url, token, &body, EXEC_TIMEOUT).await {
                    Ok(Reply::Ok(body)) => tools::render_exec(body).unwrap_or_else(|error| {
                        Rendered::error(format!(
                            "The executor answered an unreadable cell: {error}"
                        ))
                    }),
                    Ok(reply) => reply.failure("execute"),
                    Err(message) => Rendered::error(message),
                }
            }
        }
    }

    async fn post(
        &self,
        url: &str,
        token: &str,
        body: &impl Serialize,
        timeout: Duration,
    ) -> Result<Reply, String> {
        let unreachable = |error: reqwest::Error| {
            if error.is_timeout() {
                format!("Code Mode did not answer within {} s.", timeout.as_secs())
            } else {
                format!("Code Mode is unreachable: {}", without_url(&error))
            }
        };
        let mut response = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        if status != StatusCode::OK {
            return Ok(Reply::Status(status, error_message(&bytes)));
        }
        serde_json::from_slice(&bytes)
            .map(Reply::Ok)
            .map_err(|error| format!("Code Mode answered unreadable JSON: {error}"))
    }
}

enum Reply {
    Ok(Value),
    /// A non-200 status and the body's error message.
    Status(StatusCode, String),
}

impl Reply {
    fn failure(self, what: &str) -> Rendered {
        let Self::Status(status, message) = self else {
            return Rendered::error(format!("Code Mode {what} failed"));
        };
        Rendered::error(match status {
            StatusCode::SERVICE_UNAVAILABLE => BUSY.to_owned(),
            // The executor verifies the turn token on every request.
            StatusCode::UNAUTHORIZED => EXPIRED.to_owned(),
            _ if message.is_empty() => {
                format!("Code Mode {what} failed (HTTP {}).", status.as_u16())
            }
            _ => format!(
                "Code Mode {what} failed (HTTP {}): {message}",
                status.as_u16()
            ),
        })
    }
}

fn too_large() -> String {
    format!(
        "Code Mode's answer is larger than {} MiB; return less data from the cell.",
        MAX_RESPONSE_BYTES / (1024 * 1024)
    )
}

/// The `error` of a JSON error body, or its text, shortened.
fn error_message(bytes: &[u8]) -> String {
    let text = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|body| body.get("error").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
    let text = text.trim();
    match text.char_indices().nth(500) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// A transport error without its URL, which names in-cluster hosts.
fn without_url(error: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut message = String::new();
    let mut source = error.source();
    while let Some(cause) = source {
        if !message.is_empty() {
            message.push_str(": ");
        }
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    if message.is_empty() {
        "request failed".to_owned()
    } else {
        message
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// A tokio task the dropped future cancels.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One Code Mode tool of one thread, as a native nanocode tool.
pub struct CentaurTool {
    code_mode: Arc<CodeMode>,
    kind: Kind,
    spec: ToolSpec,
    thread_key: String,
    log: Arc<dyn Turns>,
}

/// A turn's Code Mode token and its expiry, in unix seconds. The brain keeps
/// it in the turn's own nanocode metadata, so it is stored, resumed, and
/// retired with that turn and no other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnToken {
    pub token: String,
    pub expires_at: i64,
}

/// The turn a tool call belongs to, with its token if it was given one.
#[derive(Clone, Debug)]
pub struct Turn {
    pub thread_key: String,
    pub request_id: String,
    pub token: Option<TurnToken>,
}

/// The thread's running turn, as a tool call sees it.
#[derive(Clone, Debug)]
pub struct Running {
    pub request_id: String,
    pub token: Option<TurnToken>,
}

type RunningFuture<'a> = Pin<Box<dyn Future<Output = Result<Option<Running>, String>> + Send + 'a>>;

/// Where a tool call finds the thread's running turn. nanocode does not tell a
/// tool which turn it runs in; tool calls happen only while the thread's one
/// turn runs, and a steer folds into that turn, so its calls use its token.
pub trait Turns: Send + Sync {
    fn running(&self) -> RunningFuture<'_>;
}

impl Turns for OneLog {
    fn running(&self) -> RunningFuture<'_> {
        Box::pin(async move {
            let pending = self.pending().await.map_err(|error| error.to_string())?;
            let Some(record) = pending
                .into_iter()
                .find(|record| record.state == TurnState::Running)
            else {
                return Ok(None);
            };
            #[derive(Deserialize)]
            struct Metadata {
                #[serde(default)]
                tool_token: Option<TurnToken>,
            }
            // Metadata the brain did not write reads as no token.
            let token = serde_json::from_str::<Metadata>(record.metadata.get())
                .ok()
                .and_then(|metadata| metadata.tool_token);
            Ok(Some(Running {
                request_id: record.request_id,
                token,
            }))
        })
    }
}

impl CentaurTool {
    #[cfg(test)]
    pub const fn kind(&self) -> Kind {
        self.kind
    }
}

impl Tool for CentaurTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.spec.name,
            self.spec.description,
            self.spec.input_schema.clone(),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.kind != Kind::Execute
    }

    fn execute<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        context: ToolContext<'life1>,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let call_id = context.call_id().to_owned();
        let args = input.decode_json::<Value>();
        Box::pin(async move {
            // Read here, on nanocode's executor, where the session log lives.
            let running = match self.log.running().await {
                Ok(Some(running)) => running,
                Ok(None) => return Ok(output(Rendered::error(UNAVAILABLE))),
                Err(error) => {
                    warn!(thread_key = %self.thread_key, %error, "cannot read the running turn");
                    return Ok(output(Rendered::error(
                        "Code Mode could not load this turn's access; retry in a few seconds.",
                    )));
                }
            };
            let turn = Turn {
                thread_key: self.thread_key.clone(),
                request_id: running.request_id,
                token: running.token,
            };
            let rendered = match args {
                Ok(args) => self.code_mode.run(self.kind, turn, &call_id, args).await,
                Err(error) => Rendered::error(format!(
                    "Invalid arguments for {}: {error}",
                    self.kind.name()
                )),
            };
            Ok(output(rendered))
        })
    }
}

/// A rendered result as nanocode output; an error result is a failure.
fn output(rendered: Rendered) -> ToolOutput {
    if rendered.is_error {
        ToolOutput::error(rendered.text)
    } else {
        ToolOutput::text(rendered.text)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode as AxumStatus},
        response::IntoResponse,
        routing::post,
    };
    use serde_json::json;
    use std::sync::Mutex;

    /// What a mock server saw: path, authorization, body.
    pub(crate) type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

    async fn mock(
        State((seen, path, status, reply)): State<(Seen, &'static str, u16, Value)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        seen.lock()
            .unwrap()
            .push((path.to_owned(), header("authorization"), body));
        (AxumStatus::from_u16(status).unwrap(), Json(reply))
    }

    /// A server answering `routes` (path, status, body); returns its base URL.
    pub(crate) async fn serve(routes: Vec<(&'static str, u16, Value)>, seen: &Seen) -> String {
        let mut router = Router::new();
        for (path, status, reply) in routes {
            router = router.route(
                path,
                post(mock).with_state((Arc::clone(seen), path, status, reply)),
            );
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        format!("http://{address}/")
    }

    fn code_mode(executor_url: &str) -> Arc<CodeMode> {
        Arc::new(
            CodeMode::new(
                Endpoints {
                    executor_url: executor_url.trim_end_matches('/').to_owned(),
                },
                tokio::runtime::Handle::current(),
            )
            .unwrap(),
        )
    }

    /// A turn of `thread-1`, with a token when given one.
    fn turn(token: Option<(&str, i64)>) -> Turn {
        Turn {
            thread_key: "thread-1".into(),
            request_id: "exe_1".into(),
            token: token.map(|(token, expires_at)| TurnToken {
                token: token.into(),
                expires_at,
            }),
        }
    }

    /// A thread whose running turn is fixed.
    struct Fixed(Option<Running>);

    impl Turns for Fixed {
        fn running(&self) -> RunningFuture<'_> {
            let running = self.0.clone();
            Box::pin(async move { Ok(running) })
        }
    }

    #[tokio::test]
    async fn forwards_the_tokens_and_renders_results() {
        let seen = Seen::default();
        let executor = serve(
            vec![
                (
                    "/v1/search",
                    200,
                    json!({"results": [{"tool": "slack"}], "catalog_revision": "r1"}),
                ),
                (
                    "/v1/load",
                    200,
                    json!({"tool": "slack", "description": "Slack", "brief": "declare const slack"}),
                ),
                (
                    "/v1/exec",
                    200,
                    json!({"ok": true, "result": {"n": 1}, "logs": ["hi"], "calls": 1, "ms": 3}),
                ),
            ],
            &seen,
        )
        .await;
        let code_mode = code_mode(&executor);

        let search = code_mode
            .call(
                Kind::Search,
                &turn(Some(("turn-token", now_secs() + 600))),
                "call-1",
                json!({"query": "slack", "limit": 3}),
            )
            .await;
        assert!(!search.is_error);
        assert!(search.text.contains("\"catalog_revision\": \"r1\""));

        let load = code_mode
            .call(
                Kind::Load,
                &turn(Some(("turn-token", now_secs() + 600))),
                "call-2",
                json!({"tool": "slack"}),
            )
            .await;
        assert_eq!(load.text, "Slack\n\ndeclare const slack");

        let exec = code_mode
            .call(
                Kind::Execute,
                &turn(Some(("turn-token", now_secs() + 600))),
                "call-3",
                json!({"code": "return 1"}),
            )
            .await;
        assert!(!exec.is_error);
        assert_eq!(exec.text, "{\n  \"n\": 1\n}\n\nlogs:\nhi");

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.iter()
                .map(|entry| entry.0.as_str())
                .collect::<Vec<_>>(),
            ["/v1/search", "/v1/load", "/v1/exec"]
        );
        for (_, authorization, _) in seen.iter() {
            assert_eq!(authorization, "Bearer turn-token");
        }
        assert_eq!(seen[0].2, json!({"query": "slack", "limit": 3}));
        assert_eq!(seen[2].2, json!({"code": "return 1", "cell_id": "call-3"}));
    }

    #[tokio::test]
    async fn maps_failures_to_error_results() {
        let seen = Seen::default();
        let executor = serve(
            vec![
                (
                    "/v1/search",
                    503,
                    json!({"error": "busy", "retryable": true}),
                ),
                ("/v1/load", 404, json!({"error": "unknown tool"})),
                (
                    "/v1/exec",
                    200,
                    json!({"ok": false, "error": {"message": "boom", "tool": "slack.post"}, "logs": []}),
                ),
            ],
            &seen,
        )
        .await;
        let code_mode = code_mode(&executor);

        let busy = code_mode
            .call(
                Kind::Search,
                &turn(Some(("t", now_secs() + 600))),
                "c",
                json!({"query": "x"}),
            )
            .await;
        assert!(busy.is_error);
        assert_eq!(busy.text, BUSY);

        let unknown = code_mode
            .call(
                Kind::Load,
                &turn(Some(("t", now_secs() + 600))),
                "c",
                json!({"tool": "nope"}),
            )
            .await;
        assert_eq!(unknown, tools::render_unknown_tool("nope"));

        let failed = code_mode
            .call(
                Kind::Execute,
                &turn(Some(("t", now_secs() + 600))),
                "c",
                json!({"code": "throw 1"}),
            )
            .await;
        assert!(failed.is_error);
        assert_eq!(failed.text, "boom\n\ntool: slack.post");

        let invalid = code_mode
            .call(
                Kind::Execute,
                &turn(Some(("t", now_secs() + 600))),
                "c",
                json!({"source": "x"}),
            )
            .await;
        assert!(invalid.is_error);
        assert!(
            invalid
                .text
                .starts_with("Invalid arguments for centaur_execute")
        );

        assert!(!output(failed).success, "an error result is a failure");
    }

    #[tokio::test]
    async fn a_refused_turn_token_reads_as_expired() {
        let seen = Seen::default();
        let server = serve(
            vec![(
                "/v1/exec",
                401,
                json!({"error": "invalid or expired turn token"}),
            )],
            &seen,
        )
        .await;
        let code_mode = code_mode(&server);
        let refused = code_mode
            .call(
                Kind::Execute,
                &turn(Some(("t", now_secs() + 600))),
                "c",
                json!({"code": "1"}),
            )
            .await;
        assert!(refused.is_error);
        assert_eq!(refused.text, EXPIRED);
    }

    #[tokio::test]
    async fn a_thread_without_a_live_token_gets_no_calls() {
        let seen = Seen::default();
        let server = serve(
            vec![
                ("/v1/search", 200, json!({})),
                ("/v1/load", 200, json!({})),
                ("/v1/exec", 200, json!({"ok": true})),
            ],
            &seen,
        )
        .await;

        let none = code_mode(&server);
        let missing = none
            .call(Kind::Execute, &turn(None), "c", json!({"code": "1"}))
            .await;
        assert!(missing.is_error);
        assert_eq!(missing.text, UNAVAILABLE);

        let expired = code_mode(&server);
        let late = expired
            .call(
                Kind::Execute,
                &turn(Some(("old", now_secs() - 1))),
                "c",
                json!({"code": "1"}),
            )
            .await;
        assert!(late.is_error);
        assert_eq!(late.text, EXPIRED);

        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unreachable_services_and_oversized_answers_are_errors() {
        let seen = Seen::default();
        let huge = Value::String("x".repeat(MAX_RESPONSE_BYTES + 1));
        let server = serve(
            vec![
                ("/v1/search", 200, huge),
                ("/v1/exec", 200, json!({"ok": true})),
            ],
            &seen,
        )
        .await;
        let live = code_mode(&server);
        let large = live
            .call(
                Kind::Search,
                &turn(Some(("t", now_secs() + 60))),
                "c",
                json!({"query": "x"}),
            )
            .await;
        assert!(large.is_error);
        assert!(large.text.contains("larger than 24 MiB"), "{}", large.text);
        let down = code_mode("http://127.0.0.1:1")
            .call(
                Kind::Execute,
                &turn(Some(("t", now_secs() + 60))),
                "c",
                json!({"code": "1"}),
            )
            .await;
        assert!(down.is_error);
        assert!(
            down.text.starts_with("Code Mode is unreachable"),
            "{}",
            down.text
        );
        assert!(!down.text.contains("127.0.0.1"), "{}", down.text);
    }

    /// The tools run on nanocode's executor; the HTTP call runs on tokio.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_tool_runs_from_the_nanocode_executor() {
        let seen = Seen::default();
        let server = serve(
            vec![(
                "/v1/exec",
                200,
                json!({"ok": true, "result": 7, "logs": []}),
            )],
            &seen,
        )
        .await;
        let code_mode = code_mode(&server);
        let running: Arc<dyn Turns> = Arc::new(Fixed(Some(Running {
            request_id: "exe_1".into(),
            token: Some(TurnToken {
                token: "t".into(),
                expires_at: now_secs() + 60,
            }),
        })));
        let output = execute_on_nanocode(&code_mode, running, "return 7").await;
        assert!(output.success);
        assert_eq!(output.output.structured_result(), json!("7"));
        assert_eq!(seen.lock().unwrap()[0].2["cell_id"], "call-9");
    }

    /// Runs `centaur_execute` the way nanocode does, on its own executor.
    async fn execute_on_nanocode(
        code_mode: &Arc<CodeMode>,
        turns: Arc<dyn Turns>,
        code: &str,
    ) -> ToolOutput {
        let tools = code_mode.tools("thread-1", &turns);
        let input =
            ToolInput::Function(serde_json::value::to_raw_value(&json!({ "code": code })).unwrap());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let runtime = nanocode_executor::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let execute = tools
                .into_iter()
                .find(|tool| tool.kind() == Kind::Execute)
                .unwrap();
            let output = runtime.block_on(async move {
                let context = ToolContext::new("luna", "session-1", "call-9", &[], usize::MAX);
                execute.execute(input, context).await.unwrap()
            });
            let _ = sender.send(output);
        });
        receiver.await.unwrap()
    }

    /// Without a running turn, or when the running turn has no token, a call
    /// never reaches the executor: no other turn's token stands in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_uses_only_the_running_turns_own_token() {
        let seen = Seen::default();
        let server = serve(vec![("/v1/exec", 200, json!({"ok": true}))], &seen).await;
        let code_mode = code_mode(&server);
        for turns in [
            Fixed(None),
            Fixed(Some(Running {
                request_id: "exe_2".into(),
                token: None,
            })),
        ] {
            let output = execute_on_nanocode(&code_mode, Arc::new(turns), "1").await;
            assert!(!output.success);
            assert_eq!(output.output.structured_result(), json!(UNAVAILABLE));
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_session_log_journals_only_cells() {
        use nanocode::turnlog::{MemoryLog, Policy};
        let runtime = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let policy = Policy {
                effects: effects(),
                ..Policy::default()
            };
            let session_id = uuid::Uuid::now_v7().to_string();
            let parts = nanocode::turnlog(Arc::new(MemoryLog::new()), &session_id, policy)
                .await
                .unwrap();
            assert_eq!(parts.gate.effect(tools::SEARCH), EffectClass::None);
            assert_eq!(parts.gate.effect(tools::LOAD), EffectClass::None);
            assert_eq!(parts.gate.effect(tools::EXECUTE), EffectClass::External);
            assert!(parts.gate.effect(tools::EXECUTE).needs_row());
            assert!(!parts.gate.effect(tools::SEARCH).needs_row());
        });
    }

    #[test]
    fn tools_are_on_only_when_fully_configured() {
        let (_, public) = keys::pair();
        let all = |name: &str| {
            Some(match name {
                TOKEN_PUBLIC_KEY_ENV => public.clone(),
                EXECUTOR_URL_ENV => "http://executor:8080/".to_owned(),
                _ => return None,
            })
        };
        let setup = Setup::from_lookup(all).unwrap();
        assert!(setup.verifier.is_some());
        assert_eq!(
            setup.endpoints.unwrap().executor_url,
            "http://executor:8080"
        );

        let key_only =
            Setup::from_lookup(|name| (name == TOKEN_PUBLIC_KEY_ENV).then(|| public.clone()))
                .unwrap();
        assert!(key_only.verifier.is_some());
        assert!(key_only.endpoints.is_none());

        let url_only =
            Setup::from_lookup(|name| (name == EXECUTOR_URL_ENV).then(|| all(name)).flatten())
                .unwrap();
        assert!(url_only.verifier.is_none());
        assert!(url_only.endpoints.is_none());

        assert!(
            Setup::from_lookup(|name| (name == TOKEN_PUBLIC_KEY_ENV).then(|| "junk".to_owned()))
                .is_err()
        );
    }

    /// Ed25519 test keys, as PEM.
    pub(crate) mod keys {
        use aws_lc_rs::{
            encoding::AsDer,
            signature::{Ed25519KeyPair, KeyPair},
        };
        use base64::Engine as _;

        fn pem(label: &str, der: &[u8]) -> String {
            let encoded = base64::engine::general_purpose::STANDARD.encode(der);
            let lines = encoded
                .as_bytes()
                .chunks(64)
                .map(|line| std::str::from_utf8(line).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            format!("-----BEGIN {label}-----\n{lines}\n-----END {label}-----\n")
        }

        /// A fresh (private PEM, public PEM) pair.
        pub(crate) fn pair() -> (String, String) {
            let pkcs8 =
                Ed25519KeyPair::generate_pkcs8(&aws_lc_rs::rand::SystemRandom::new()).unwrap();
            let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
            let spki = pair.public_key().as_der().unwrap();
            (
                pem("PRIVATE KEY", pkcs8.as_ref()),
                pem("PUBLIC KEY", spki.as_ref()),
            )
        }
    }
}
