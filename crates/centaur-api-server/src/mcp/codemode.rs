//! MCP forwarding to a principal's Code Mode sandbox.
//!
//! When Code Mode is enabled, `/mcp` does not run the tool host: it resolves
//! the principal's endpoint and forwards the request to that sandbox's HTTP
//! API. The wire shapes are documented in `crates/centaur-codemode/README.md`;
//! api-rs deliberately declares its own serde types instead of depending on
//! the runtime crate. Forwarded requests carry no credential: the NetworkPolicy
//! that admits only api-rs to the Code Mode port is the boundary.

use std::{sync::OnceLock, time::Duration};

use centaur_codemode_protocol::tools;
use centaur_sandbox_core::SandboxError;
use centaur_session_runtime::{
    CodemodeEndpoint, SessionRuntime, SessionRuntimeError, ToolHostCallPolicy, ToolHostToolFilter,
};
use serde_json::{Value, json};
use tracing::Span;

use crate::ApiError;

use super::{McpToolCallOutcome, mcp_text_result};

/// `/search` and `/load` only touch the catalog, so they are fast.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(20);
/// A cell is capped at 120 s, but one connector request may extend it to
/// 10 min; keep enough headroom for a whole cell.
const CELL_TIMEOUT: Duration = Duration::from_secs(12 * 60);
/// Largest response body read from a sandbox. A cell result is capped at
/// 10 MiB and logs at 1 MiB by the runtime; this bounds what a misbehaving or
/// compromised sandbox can make the shared control plane buffer.
const MAX_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
/// Longest upstream error detail kept in a control-plane error.
const MAX_ERROR_DETAIL_CHARS: usize = 200;

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // Code Mode sandboxes are in-cluster; an operator's HTTP proxy
            // environment must not intercept this traffic.
            .no_proxy()
            .build()
            .expect("reqwest client configuration is valid")
    })
}

/// The deployment tool filter as the sandbox's policy object.
fn policy_json(filter: &ToolHostToolFilter) -> Value {
    let split = |raw: Option<&str>| raw.and_then(super::split_tool_list);
    json!({
        "allowlist": split(filter.allowlist.as_deref()),
        "blocklist": split(filter.blocklist.as_deref()).unwrap_or_default(),
    })
}

/// One request to a Code Mode sandbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CodemodeRequest {
    Search { query: String, limit: u32 },
    Load { tool: String },
    Exec { code: String },
}

impl CodemodeRequest {
    fn path(&self) -> &'static str {
        match self {
            Self::Search { .. } => "/search",
            Self::Load { .. } => "/load",
            Self::Exec { .. } => "/exec",
        }
    }

    fn timeout(&self) -> Duration {
        match self {
            Self::Search { .. } | Self::Load { .. } => CATALOG_TIMEOUT,
            Self::Exec { .. } => CELL_TIMEOUT,
        }
    }

    fn body(&self, policy: &Value) -> Value {
        match self {
            Self::Search { query, limit } => {
                json!({"query": query, "limit": limit, "policy": policy})
            }
            Self::Load { tool } => json!({"tool": tool, "policy": policy}),
            Self::Exec { code } => json!({"code": code, "policy": policy}),
        }
    }
}

/// Forward one call and render it as an MCP tool result.
///
/// A Code Mode failure (including a not-ready, busy or restarting sandbox) is
/// a tool error, not a JSON-RPC error; only control-plane failures stay
/// `ApiError`s.
pub(super) async fn call(
    runtime: &SessionRuntime,
    principal_id: &str,
    policy: &ToolHostCallPolicy,
    request: CodemodeRequest,
) -> Result<McpToolCallOutcome, ApiError> {
    let endpoint = match runtime.codemode_endpoint(policy).await {
        Ok(endpoint) => endpoint,
        Err(SessionRuntimeError::Sandbox(SandboxError::NotReady(_))) => {
            return Ok(tool_error(
                "Code Mode is starting; retry in a few seconds.".to_owned(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    Span::current().record("centaur.sandbox_id", endpoint.sandbox_id.as_str());
    match forward(&endpoint, request, &policy_json(policy.tool_filter())).await {
        Ok(outcome) => Ok(outcome),
        // A stopped or replaced sandbox: drop the cached endpoints so the next
        // call adopts a live one instead of reusing a dead address.
        Err(ForwardError::Connect(error)) => {
            tracing::info!(
                sandbox_id = %endpoint.sandbox_id,
                %error,
                "Code Mode sandbox is unreachable; forgetting its endpoint"
            );
            runtime.forget_codemode_endpoint(principal_id);
            Ok(tool_error(
                "Code Mode sandbox is restarting; retry in a few seconds.".to_owned(),
            ))
        }
        Err(ForwardError::Api(error)) => Err(error),
    }
}

/// Why a forwarded call produced no rendered tool result.
#[derive(Debug)]
enum ForwardError {
    /// The sandbox address refused the connection; the endpoint is stale.
    Connect(reqwest::Error),
    /// A failure that belongs to the control plane's request handling.
    Api(ApiError),
}

/// Send one request to an endpoint and render its response.
async fn forward(
    endpoint: &CodemodeEndpoint,
    request: CodemodeRequest,
    policy: &Value,
) -> Result<McpToolCallOutcome, ForwardError> {
    let url = format!(
        "{}{}",
        endpoint.base_url.trim_end_matches('/'),
        request.path()
    );
    let mut builder = client().post(url).timeout(request.timeout());
    // The sandbox parents its cell spans on the MCP tool call's span.
    if let Some(traceparent) = centaur_telemetry::traceparent_for_span(&Span::current()) {
        builder = builder.header("traceparent", traceparent);
    }
    let response = builder
        .json(&request.body(policy))
        .send()
        .await
        .map_err(ForwardError::Connect)?;
    decode_response(&request, response)
        .await
        .map_err(ForwardError::Api)
}

async fn decode_response(
    request: &CodemodeRequest,
    response: reqwest::Response,
) -> Result<McpToolCallOutcome, ApiError> {
    let status = response.status();
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Ok(tool_error(
            "Code Mode is busy; retry in a few seconds.".to_owned(),
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND
        && let CodemodeRequest::Load { tool } = request
    {
        return Ok(unknown_tool(tool));
    }
    let body = read_bounded(request, response).await?;
    let body = String::from_utf8_lossy(&body);
    if !status.is_success() {
        return Err(ApiError::Internal(format!(
            "Code Mode {} returned HTTP {status}: {}",
            request.path(),
            error_detail(&body)
        )));
    }
    let body: Value = serde_json::from_str(&body).map_err(|error| {
        ApiError::Internal(format!(
            "decoding Code Mode {} response: {error}",
            request.path()
        ))
    })?;
    match request {
        CodemodeRequest::Search { .. } => search_result(body),
        CodemodeRequest::Load { .. } => load_result(body),
        CodemodeRequest::Exec { .. } => exec_result(body),
    }
}

/// Read a sandbox response, refusing bodies over [`MAX_RESPONSE_BYTES`]
/// before they are buffered whole.
async fn read_bounded(
    request: &CodemodeRequest,
    mut response: reqwest::Response,
) -> Result<Vec<u8>, ApiError> {
    let path = request.path();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        ApiError::Internal(format!("reading Code Mode {path} response: {error}"))
    })? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::Internal(format!(
                "Code Mode {path} response exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A bounded prefix of an upstream error body, never a whole page.
fn error_detail(body: &str) -> String {
    body.trim().chars().take(MAX_ERROR_DETAIL_CHARS).collect()
}

fn search_result(body: Value) -> Result<McpToolCallOutcome, ApiError> {
    Ok(rendered(tools::render_search(body)))
}

fn load_result(body: Value) -> Result<McpToolCallOutcome, ApiError> {
    Ok(rendered(tools::render_load(&body)))
}

fn exec_result(body: Value) -> Result<McpToolCallOutcome, ApiError> {
    let rendered_cell = tools::render_exec(body).map_err(|error| {
        ApiError::Internal(format!("decoding Code Mode cell response: {error}"))
    })?;
    Ok(rendered(rendered_cell))
}

/// A rendered Code Mode result as an MCP tool result.
fn rendered(rendered: tools::Rendered) -> McpToolCallOutcome {
    let mut result = mcp_text_result(rendered.text, rendered.is_error);
    if let Some(structured) = rendered.structured {
        result["structuredContent"] = structured;
    }
    McpToolCallOutcome {
        result,
        timed_out: false,
    }
}

fn tool_error(text: String) -> McpToolCallOutcome {
    rendered(tools::Rendered::error(text))
}

fn unknown_tool(tool: &str) -> McpToolCallOutcome {
    rendered(tools::render_unknown_tool(tool))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex as StdMutex},
    };

    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::{super::mcp_result_is_error, *};

    #[derive(Clone)]
    struct MockState {
        responses: Arc<StdMutex<VecDeque<(StatusCode, Value)>>>,
        requests: Arc<StdMutex<Vec<Value>>>,
    }

    async fn mock_handler(
        State(state): State<MockState>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        state.requests.lock().unwrap().push(body);
        let (status, body) = state.responses.lock().unwrap().pop_front().unwrap_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "no response"}),
        ));
        (status, Json(body))
    }

    async fn mock_server(
        responses: Vec<(StatusCode, Value)>,
    ) -> (MockState, CodemodeEndpoint, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = MockState {
            responses: Arc::new(StdMutex::new(responses.into())),
            requests: Arc::new(StdMutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/search", post(mock_handler))
            .route("/load", post(mock_handler))
            .route("/exec", post(mock_handler))
            .with_state(state.clone());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = CodemodeEndpoint {
            sandbox_id: "cm-test".to_owned(),
            base_url,
        };
        (state, endpoint, handle)
    }

    fn requests(state: &MockState) -> Vec<Value> {
        state.requests.lock().unwrap().clone()
    }

    fn policy(allowlist: Option<&str>, blocklist: Option<&str>) -> Value {
        policy_json(&ToolHostToolFilter {
            allowlist: allowlist.map(str::to_owned),
            blocklist: blocklist.map(str::to_owned),
        })
    }

    fn text(outcome: &McpToolCallOutcome) -> &str {
        outcome.result["content"][0]["text"].as_str().unwrap()
    }

    fn internal_detail(error: ForwardError) -> String {
        match error {
            ForwardError::Api(ApiError::Internal(detail)) => detail,
            ForwardError::Api(error) => panic!("unexpected control-plane error: {error}"),
            ForwardError::Connect(error) => panic!("unexpected connection error: {error}"),
        }
    }

    #[tokio::test]
    async fn search_forwards_the_filter_and_returns_structured_results() {
        let body = json!({
            "results": [{
                "tool": "crunchbase",
                "function": "search_organizations",
                "signature": "search_organizations(args: {query: string}): Promise<unknown>",
                "description": "Search organizations",
            }],
            "catalog_revision": "abc123",
        });
        let (state, endpoint, server) = mock_server(vec![(StatusCode::OK, body.clone())]).await;

        let outcome = forward(
            &endpoint,
            CodemodeRequest::Search {
                query: "crunchbase organizations".to_owned(),
                limit: 8,
            },
            &policy(Some(" slack, crunchbase,,"), Some(" dune ")),
        )
        .await
        .unwrap();

        assert!(!mcp_result_is_error(&outcome.result));
        assert_eq!(outcome.result["structuredContent"], body);
        assert_eq!(serde_json::from_str::<Value>(text(&outcome)).unwrap(), body);
        assert_eq!(
            requests(&state),
            vec![json!({
                "query": "crunchbase organizations",
                "limit": 8,
                "policy": {"allowlist": ["crunchbase", "slack"], "blocklist": ["dune"]},
            })]
        );
        server.abort();
    }

    #[tokio::test]
    async fn load_renders_description_and_brief_and_maps_404() {
        let (state, endpoint, server) = mock_server(vec![
            (
                StatusCode::OK,
                json!({
                    "tool": "slack",
                    "description": "Slack workspace tools",
                    "brief": "export async function search_messages(args: { query: string }): Promise<unknown>;\n",
                }),
            ),
            (StatusCode::NOT_FOUND, json!({"error": "unknown tool nope"})),
        ])
        .await;

        let outcome = forward(
            &endpoint,
            CodemodeRequest::Load {
                tool: "slack".to_owned(),
            },
            &policy(None, None),
        )
        .await
        .unwrap();

        assert!(!mcp_result_is_error(&outcome.result));
        assert_eq!(
            text(&outcome),
            "Slack workspace tools\n\nexport async function search_messages(args: { query: string }): Promise<unknown>;"
        );

        let unknown = forward(
            &endpoint,
            CodemodeRequest::Load {
                tool: "nope".to_owned(),
            },
            &policy(None, None),
        )
        .await
        .unwrap();

        assert!(mcp_result_is_error(&unknown.result));
        assert!(text(&unknown).contains("Unknown or unavailable tool nope"));
        assert_eq!(
            requests(&state),
            vec![
                json!({"tool": "slack", "policy": {"allowlist": null, "blocklist": []}}),
                json!({"tool": "nope", "policy": {"allowlist": null, "blocklist": []}}),
            ]
        );
        server.abort();
    }

    #[tokio::test]
    async fn exec_renders_results_and_failures() {
        let (state, endpoint, server) = mock_server(vec![
            (
                StatusCode::OK,
                json!({"ok": true, "result": {"channels": 3}, "logs": ["listing channels"]}),
            ),
            (
                StatusCode::OK,
                json!({
                    "ok": false,
                    "error": {"message": "TypeError: tools.slack is undefined", "stack": "at cell:1:1", "tool": "slack"},
                    "logs": ["before"],
                }),
            ),
        ])
        .await;
        let no_filter = policy(None, None);
        let exec = || {
            forward(
                &endpoint,
                CodemodeRequest::Exec {
                    code: "return await tools.slack.list_channels({})".to_owned(),
                },
                &no_filter,
            )
        };

        let ok = exec().await.unwrap();
        assert!(!mcp_result_is_error(&ok.result));
        assert!(text(&ok).contains("\"channels\": 3"), "{}", text(&ok));
        assert!(text(&ok).contains("logs:\nlisting channels"));
        assert_eq!(
            ok.result["structuredContent"],
            json!({"result": {"channels": 3}, "logs": ["listing channels"]})
        );
        assert_eq!(
            requests(&state)[0],
            json!({
                "code": "return await tools.slack.list_channels({})",
                "policy": {"allowlist": null, "blocklist": []},
            })
        );

        let failed = exec().await.unwrap();
        assert!(mcp_result_is_error(&failed.result));
        assert_eq!(
            text(&failed),
            "TypeError: tools.slack is undefined\n\ntool: slack\n\nat cell:1:1\n\nlogs:\nbefore"
        );
        server.abort();
    }

    #[tokio::test]
    async fn busy_http_failures_and_oversized_responses() {
        let (_state, endpoint, server) = mock_server(vec![
            (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "busy", "retryable": true}),
            ),
            (
                StatusCode::UNAUTHORIZED,
                json!({"error": "e".repeat(10_000)}),
            ),
            (
                StatusCode::OK,
                json!({"ok": true, "result": "x".repeat(MAX_RESPONSE_BYTES + 1)}),
            ),
        ])
        .await;

        let busy = forward(
            &endpoint,
            CodemodeRequest::Exec {
                code: "return 1".to_owned(),
            },
            &policy(None, None),
        )
        .await
        .unwrap();
        assert!(mcp_result_is_error(&busy.result));
        assert_eq!(text(&busy), "Code Mode is busy; retry in a few seconds.");

        let error = forward(
            &endpoint,
            CodemodeRequest::Search {
                query: "slack".to_owned(),
                limit: 8,
            },
            &policy(None, None),
        )
        .await
        .err()
        .expect("an upstream HTTP failure is a control-plane error");
        let detail = internal_detail(error);
        assert!(
            detail.starts_with("Code Mode /search returned HTTP 401"),
            "{detail}"
        );
        assert!(
            detail.len() <= MAX_ERROR_DETAIL_CHARS + 64,
            "the upstream detail is bounded: {} chars",
            detail.len()
        );

        let oversized = forward(
            &endpoint,
            CodemodeRequest::Exec {
                code: "return 1".to_owned(),
            },
            &policy(None, None),
        )
        .await
        .err()
        .expect("an oversized response is refused");
        assert!(internal_detail(oversized).contains("exceeded"));
        server.abort();
    }
}
