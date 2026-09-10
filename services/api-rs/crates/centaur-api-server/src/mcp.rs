use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use centaur_session_runtime::{
    SessionRuntime, ToolHostCallInput, ToolHostCallOutput, ToolHostCallPolicy, ToolHostInvocation,
    ToolHostToolFilter, tool_host_thread_key,
};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing::{Instrument as _, Span, info_span};

use crate::{
    ApiError,
    api_jwt::jwt_signing_secret,
    routes::{AppState, header_value},
    tool_discovery::{DiscoveredTool, ToolDiscoveryConfig, discover_tool_catalog},
};

mod search;

#[cfg(test)]
pub(crate) static MCP_ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) async fn mcp_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "ok": false,
            "error": "MCP Streamable HTTP requests must use POST for this endpoint",
        })),
    )
        .into_response()
}

pub(crate) async fn mcp_protected_resource_metadata(headers: HeaderMap) -> Json<Value> {
    let authorization_servers = mcp_authorization_server_url()
        .into_iter()
        .collect::<Vec<_>>();
    Json(json!({
        "resource": mcp_resource_url(&headers),
        "authorization_servers": authorization_servers,
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["mcp:tools"],
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct McpJsonRpcRequest {
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
struct McpToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct CentaurToolMcpArguments {
    method: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CentaurMcpArguments {
    command: Vec<String>,
}

struct McpToolCallOutcome {
    result: Value,
    timed_out: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct McpPrincipal {
    token_id: String,
    principal_id: String,
    console_user_email: Option<String>,
    console_user_name: Option<String>,
    name: String,
    scopes: Vec<String>,
    expires_at: Option<OffsetDateTime>,
}

pub(crate) async fn mcp_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<McpJsonRpcRequest>,
) -> Result<Response, ApiError> {
    let Some(principal) = authenticate_mcp_bearer(&headers)? else {
        return Ok(mcp_unauthorized(&headers));
    };
    if request.jsonrpc.as_deref().unwrap_or("2.0") != "2.0" {
        return Ok(mcp_json_error(
            request.id.unwrap_or(Value::Null),
            -32600,
            "invalid JSON-RPC version",
        ));
    }
    let Some(id) = request.id.clone() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };

    let result = match request.method.as_str() {
        "initialize" => mcp_initialize_result(&request.params),
        "ping" => json!({}),
        "tools/list" => {
            ensure_mcp_scope(&principal.scopes, "mcp:tools")?;
            let policy = mcp_tool_host_call_policy(&state, &principal).await?;
            let filter = parse_sandbox_tool_filter(policy.tool_filter());
            json!({
                "tools": mcp_tool_entries(&filter)?,
            })
        }
        "tools/call" => {
            ensure_mcp_scope(&principal.scopes, "mcp:tools")?;
            let params = serde_json::from_value::<McpToolCallParams>(request.params.clone())
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
            if params.name == "centaur" && !mcp_v2_enabled() {
                return Ok(mcp_json_error(id, -32602, "unknown tool"));
            }
            let tool = if matches!(params.name.as_str(), "centaur" | "centaur_whoami") {
                None
            } else {
                let policy = mcp_tool_host_call_policy(&state, &principal).await?;
                let filter = parse_sandbox_tool_filter(policy.tool_filter());
                let Some(tool) = mcp_find_centaur_tool(&params.name, &filter)? else {
                    return Ok(mcp_json_error(id, -32602, "unknown tool"));
                };
                Some((tool, policy))
            };
            mcp_tool_call_result(&state, &principal, params, tool).await?
        }
        _ => return Ok(mcp_json_error(id, -32601, "method not found")),
    };

    Ok(Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .into_response())
}

async fn mcp_tool_call_result(
    state: &AppState,
    principal: &McpPrincipal,
    params: McpToolCallParams,
    tool: Option<(DiscoveredTool, ToolHostCallPolicy)>,
) -> Result<Value, ApiError> {
    let thread_key = tool_host_thread_key(&principal.principal_id)?;
    let span = info_span!(
        parent: None,
        "centaur.api_rs.mcp.tool",
        component = "mcp",
        event = "mcp_tool_call",
        "lmnr.span.type" = "TOOL",
        "lmnr.span.input" = tracing::field::Empty,
        "lmnr.span.output" = tracing::field::Empty,
        "lmnr.association.properties.session_id" = thread_key.as_str(),
        "lmnr.association.properties.metadata.thread_key" = thread_key.as_str(),
        "lmnr.association.properties.metadata.execution_id" = tracing::field::Empty,
        "lmnr.association.properties.metadata.request_id" = tracing::field::Empty,
        "otel.status_code" = tracing::field::Empty,
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.execution_id" = tracing::field::Empty,
        "centaur.sandbox_id" = tracing::field::Empty,
        "tool.kind" = "centaur",
        "centaur.tool.entry_point" = "mcp",
        "tool.name" = params.name.as_str(),
        "tool.method" = tracing::field::Empty,
        "tool.status" = tracing::field::Empty,
    );

    async move {
        let outcome = if let Some((tool, policy)) = tool {
            mcp_v1_tool_result(state, principal, tool, params.arguments, policy).await
        } else if params.name == "centaur" {
            mcp_v2_command_result(state, principal, params.arguments).await
        } else {
            mcp_whoami_result(principal, params.arguments).map(|result| McpToolCallOutcome {
                result,
                timed_out: false,
            })
        };
        match outcome {
            Ok(outcome) => {
                let status = if outcome.timed_out {
                    "timed_out"
                } else if mcp_result_is_error(&outcome.result) {
                    "failed"
                } else {
                    "completed"
                };
                finish_mcp_tool_span(&Span::current(), status);
                Ok(outcome.result)
            }
            Err(error) => {
                finish_mcp_tool_span(&Span::current(), "failed");
                Err(error)
            }
        }
    }
    .instrument(span)
    .await
}

fn mcp_tool_trace_input(name: &str, method: &str) -> String {
    json!({
        "kind": "centaur",
        "name": name,
        "method": method,
    })
    .to_string()
}

fn record_mcp_tool_method(span: &Span, name: &str, method: &str) {
    span.record("tool.method", method);
    span.record("lmnr.span.input", mcp_tool_trace_input(name, method));
}

fn record_mcp_tool_correlation(
    span: &Span,
    request_id: Option<&str>,
    execution_id: Option<&str>,
    sandbox_id: Option<&str>,
) {
    if let Some(request_id) = request_id {
        span.record(
            "lmnr.association.properties.metadata.request_id",
            request_id,
        );
    }
    if let Some(execution_id) = execution_id {
        span.record("centaur.execution_id", execution_id);
        span.record(
            "lmnr.association.properties.metadata.execution_id",
            execution_id,
        );
    }
    if let Some(sandbox_id) = sandbox_id.filter(|sandbox_id| !sandbox_id.is_empty()) {
        span.record("centaur.sandbox_id", sandbox_id);
    }
}

fn finish_mcp_tool_span(span: &Span, status: &str) {
    span.record("tool.status", status);
    span.record("lmnr.span.output", json!({ "status": status }).to_string());
    span.record(
        "otel.status_code",
        if status == "completed" { "OK" } else { "ERROR" },
    );
}

fn mcp_whoami_tool() -> Value {
    json!({
        "name": "centaur_whoami",
        "description": "Show the authenticated Centaur MCP principal and token metadata.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        },
    })
}

fn mcp_initialize_result(params: &Value) -> Value {
    let mut result = json!({
        "protocolVersion": requested_mcp_protocol_version(params),
        "capabilities": {
            "tools": {
                "listChanged": false,
            },
        },
        "serverInfo": {
            "name": "centaur",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });
    if mcp_v2_enabled() {
        result["instructions"] = Value::String(
            concat!(
                "Prefer the `centaur` tool for all Centaur tool discovery and execution. ",
                "Use its `list`, `search`, and `run` commands instead of calling legacy ",
                "per-service MCP tools directly. Use a legacy per-service tool only when ",
                "the `centaur` tool cannot complete the request."
            )
            .to_owned(),
        );
    }
    result
}

fn mcp_builtin_tools() -> Vec<Value> {
    let mut tools = Vec::new();
    if mcp_v2_enabled() {
        tools.push(mcp_v2_tool());
    }
    tools.push(mcp_whoami_tool());
    tools
}

fn mcp_tool_entries(filter: &SandboxToolFilter) -> Result<Vec<Value>, ApiError> {
    let mut tools = mcp_builtin_tools();
    if !mcp_v2_enabled() {
        tools.extend(mcp_v1_tool_entries(filter)?);
    }
    Ok(tools)
}

fn mcp_v2_tool() -> Value {
    json!({
        "name": "centaur",
        "description": concat!(
            "Discover and run Centaur tools. Centaur exposes tools for internal and third-party services ",
            "(company data, Slack, Google Workspace, market data, on-chain analytics, and more) ",
            "through MCP.\n\n",
            "Pass command as an argv array. Position 0 is a verb.\n\n",
            "Commands:\n",
            "  [\"list\"]                          tools available under your Console policy\n",
            "  [\"search\", \"slack messages\"]      search names and descriptions, ranked\n",
            "  [\"run\", \"slack\", \"--help\"]       show a tool's CLI help\n",
            "  [\"run\", \"<tool>\", \"<argv>\", ...] run a tool CLI in your sandbox\n\n",
            "Use run <tool> --help to discover commands and options. Pass each argument ",
            "as a separate token, preserving spaces within values. Arguments are passed ",
            "literally, without shell expansion, pipes, or redirection. Run returns stdout, ",
            "stderr, exit_status, and timed_out; nonzero exits and timeouts are tool errors.\n\n",
            "Search matches words in names and descriptions, ignoring case and punctuation. ",
            "Exact name matches rank first. Results reflect the current catalog without ",
            "refreshing MCP tool definitions, so they stay accurate even when this tool list ",
            "is stale.",
        ),
        "inputSchema": {
            "type": "object",
            "required": ["command"],
            "additionalProperties": false,
            "properties": {
                "command": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": concat!(
                        "argv array. Each element is one token; do not pre-join with spaces. ",
                        "Examples: [\"list\"] · [\"search\", \"slack messages\"] · [\"run\", \"slack\", \"--help\"]",
                    ),
                },
            },
        },
    })
}

#[derive(Debug, Eq, PartialEq)]
enum CentaurMcpCommand {
    List,
    Search(String),
    Run { tool: String, argv: Vec<String> },
}

fn parse_mcp_v2_command(arguments: Value) -> Result<CentaurMcpCommand, String> {
    let CentaurMcpArguments { command } = serde_json::from_value(arguments)
        .map_err(|error| format!("invalid centaur arguments: {error}"))?;
    if command.iter().any(|arg| arg.contains('\0')) {
        return Err("command tokens must not contain NUL characters".to_owned());
    }
    match command.as_slice() {
        [verb] if verb == "list" => Ok(CentaurMcpCommand::List),
        [verb, query] if verb == "search" => {
            if query.trim().is_empty() {
                return Err("query must not be blank".to_owned());
            }
            Ok(CentaurMcpCommand::Search(query.clone()))
        }
        [verb, tool, argv @ ..] if verb == "run" => {
            if tool.trim().is_empty() {
                return Err("tool must not be blank".to_owned());
            }
            Ok(CentaurMcpCommand::Run {
                tool: tool.clone(),
                argv: argv.to_vec(),
            })
        }
        [verb, ..] if verb == "list" => Err("list takes no arguments".to_owned()),
        [verb, ..] if verb == "search" => Err("search takes exactly one query token".to_owned()),
        [verb, ..] if verb == "run" => Err("run requires a tool name".to_owned()),
        _ => Err("command must start with a supported verb: list, search, or run".to_owned()),
    }
}

async fn mcp_v2_command_result(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let command = match parse_mcp_v2_command(arguments) {
        Ok(command) => command,
        Err(error) => {
            return Ok(McpToolCallOutcome {
                result: mcp_text_result(
                    format!(
                        "{error}. Use {{\"command\":[\"list\"]}}, {{\"command\":[\"search\",\"slack messages\"]}}, or {{\"command\":[\"run\",\"<tool>\",\"--help\"]}}."
                    ),
                    true,
                ),
                timed_out: false,
            });
        }
    };
    let result = match command {
        CentaurMcpCommand::List => {
            record_mcp_tool_method(&Span::current(), "centaur", "list");
            let policy = mcp_tool_host_call_policy(state, principal).await?;
            mcp_v2_service_list_result(&parse_sandbox_tool_filter(policy.tool_filter()))
        }
        CentaurMcpCommand::Search(query) => {
            record_mcp_tool_method(&Span::current(), "centaur", "search");
            let policy = mcp_tool_host_call_policy(state, principal).await?;
            mcp_v2_service_search_result(&parse_sandbox_tool_filter(policy.tool_filter()), &query)
        }
        CentaurMcpCommand::Run {
            tool: tool_name,
            argv,
        } => {
            record_mcp_tool_method(&Span::current(), "centaur", "run");
            let policy = mcp_tool_host_call_policy(state, principal).await?;
            let filter = parse_sandbox_tool_filter(policy.tool_filter());
            let Some(tool) = mcp_find_centaur_tool(&tool_name, &filter)? else {
                return Ok(McpToolCallOutcome {
                    result: mcp_text_result(
                        format!(
                            "Unknown or unavailable tool {tool_name}. Use {{\"command\":[\"list\"]}} to discover available tools."
                        ),
                        true,
                    ),
                    timed_out: false,
                });
            };
            Span::current().record("tool.name", tool.name.as_str());
            return run_mcp_v2_tool(state.runtime()?, principal, &tool, argv, policy).await;
        }
    }?;
    Ok(McpToolCallOutcome {
        result,
        timed_out: false,
    })
}

fn mcp_v2_service_list_result(filter: &SandboxToolFilter) -> Result<Value, ApiError> {
    mcp_v2_service_summaries_result(mcp_centaur_tool_catalog(filter)?)
}

fn mcp_v2_service_search_result(
    filter: &SandboxToolFilter,
    query: &str,
) -> Result<Value, ApiError> {
    mcp_v2_service_summaries_result(search::search(mcp_centaur_tool_catalog(filter)?, query))
}

fn mcp_v2_service_summaries_result(tools: Vec<DiscoveredTool>) -> Result<Value, ApiError> {
    let services = tools
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
            })
        })
        .collect::<Vec<_>>();
    let content = json!({"services": services});
    let mut result = mcp_text_result(serde_json::to_string_pretty(&content)?, false);
    result["structuredContent"] = content;
    Ok(result)
}

fn mcp_v1_tool_entries(filter: &SandboxToolFilter) -> Result<Vec<Value>, ApiError> {
    let mut entries = Vec::new();
    for tool in mcp_centaur_tool_catalog(filter)? {
        let methods = mcp_v1_tool_methods(&tool);
        let signatures = methods
            .iter()
            .map(|method| method.signature.as_str())
            .collect::<Vec<_>>();
        let names = methods
            .iter()
            .map(|method| method.name.as_str())
            .collect::<Vec<_>>();
        let mut description = tool
            .description
            .clone()
            .unwrap_or_else(|| format!("Centaur tool package {}", tool.package));
        if !methods.is_empty() {
            description.push_str(" Available methods: ");
            description.push_str(&signatures.join(", "));
            description.push_str(". Pass keyword arguments matching the method signature; call method=help for this list.");
        }
        let mut method_schema = json!({
            "type": "string",
            "description": "Public method on the tool client to call. Use help to list available methods.",
        });
        if !methods.is_empty() {
            method_schema["enum"] = json!(names);
        }
        entries.push(json!({
            "name": tool.name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "required": ["method"],
                "properties": {
                    "method": method_schema,
                    "arguments": {
                        "type": "object",
                        "description": "Keyword arguments passed to the selected method.",
                        "additionalProperties": true,
                    },
                },
                "additionalProperties": false,
            },
        }));
    }
    Ok(entries)
}

struct McpToolMethod {
    name: String,
    signature: String,
}

fn mcp_v1_tool_methods(tool: &DiscoveredTool) -> Vec<McpToolMethod> {
    let mut methods = BTreeMap::from([("help".to_owned(), "help()".to_owned())]);
    let path = tool.project_dir.join(&tool.client_module);
    if let Ok(contents) = fs::read_to_string(&path) {
        for line in contents.lines() {
            let indent = line.chars().take_while(|ch| *ch == ' ').count();
            if indent != 0 && indent != 4 {
                continue;
            }
            let trimmed = line.trim_start();
            let definition = trimmed
                .strip_prefix("def ")
                .or_else(|| trimmed.strip_prefix("async def "));
            let Some(definition) = definition else {
                continue;
            };
            let Some((name, params)) = definition.split_once('(') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() || name.starts_with('_') {
                continue;
            }
            methods.insert(name.to_owned(), mcp_method_signature(name, params));
        }
    }
    methods
        .into_iter()
        .map(|(name, signature)| McpToolMethod { name, signature })
        .collect()
}

/// Render `name(params)` from the text after the opening paren of a `def`
/// line, dropping a leading `self`. Multi-line parameter lists fall back to
/// `name(...)`.
fn mcp_method_signature(name: &str, params: &str) -> String {
    let mut depth = 1usize;
    let Some(end) = params.find(|ch| {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        depth == 0
    }) else {
        return format!("{name}(...)");
    };
    let mut params = params[..end].trim();
    if let Some(rest) = params.strip_prefix("self") {
        params = rest.trim_start().trim_start_matches(',').trim_start();
    }
    format!("{name}({params})")
}

fn mcp_v1_tool_help_result(
    tool: &DiscoveredTool,
    methods: &[McpToolMethod],
) -> Result<Value, ApiError> {
    Ok(mcp_text_result(
        serde_json::to_string_pretty(&json!({
            "tool": tool.name,
            "description": tool.description,
            "methods": methods
                .iter()
                .map(|method| method.signature.as_str())
                .collect::<Vec<_>>(),
            "usage": "Call this tool with {\"method\": \"<name>\", \"arguments\": {<keyword arguments matching the signature>}}.",
        }))?,
        false,
    ))
}

fn mcp_centaur_tool_catalog(filter: &SandboxToolFilter) -> Result<Vec<DiscoveredTool>, ApiError> {
    // Discovery scans the tool dirs and parses package metadata on every
    // call; reuse a recent result so each MCP request does not redo that
    // I/O while still picking up newly synced tools quickly. Tests point
    // the discovery env vars at per-case temp dirs, so they read live.
    const CATALOG_TTL: Duration = Duration::from_secs(10);
    static CATALOG_CACHE: Mutex<Option<(Instant, Vec<DiscoveredTool>)>> = Mutex::new(None);
    let tools = if !cfg!(test)
        && let Some((discovered_at, tools)) = CATALOG_CACHE.lock().unwrap().as_ref()
        && discovered_at.elapsed() < CATALOG_TTL
    {
        tools.clone()
    } else {
        let dirs = ToolDiscoveryConfig {
            tool_dirs: env::var("TOOL_DIRS").ok(),
            public_tool_dirs: env::var("KUBERNETES_PUBLIC_TOOL_DIRS").ok(),
            tools_path: env::var("TOOLS_PATH").ok().map(PathBuf::from),
            tools_overlay_path: env::var("TOOLS_OVERLAY_PATH").ok().map(PathBuf::from),
            plugins_dir: env::var("PLUGINS_DIR").ok().map(PathBuf::from),
            tools_config: env::var("TOOLS_CONFIG").ok().map(PathBuf::from),
        }
        .resolve_tool_dirs()
        .map_err(|error| ApiError::Internal(error.to_string()))?;
        let tools = discover_tool_catalog(&dirs)
            .map_err(|error| ApiError::Internal(error.to_string()))?
            .tools;
        if !cfg!(test) {
            *CATALOG_CACHE.lock().unwrap() = Some((Instant::now(), tools.clone()));
        }
        tools
    };
    Ok(tools
        .into_iter()
        // Built-in names take precedence over scripts from tool sources.
        .filter(|tool| !matches!(tool.name.as_str(), "centaur" | "centaur_whoami"))
        .filter(|tool| filter.admits(tool))
        .collect())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SandboxToolFilter {
    allowlist: Option<BTreeSet<String>>,
    blocklist: BTreeSet<String>,
}

impl SandboxToolFilter {
    // Mirrors the sandbox shim gate: allowlists match package directory or
    // project names, while blocklists also match individual script names.
    fn admits(&self, tool: &DiscoveredTool) -> bool {
        let package_dir = tool
            .project_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if self.blocklist.contains(package_dir)
            || self.blocklist.contains(&tool.package)
            || self.blocklist.contains(&tool.name)
        {
            return false;
        }
        match &self.allowlist {
            None => true,
            Some(allowlist) => allowlist.contains(package_dir) || allowlist.contains(&tool.package),
        }
    }
}

async fn mcp_tool_host_call_policy(
    state: &AppState,
    principal: &McpPrincipal,
) -> Result<ToolHostCallPolicy, ApiError> {
    let policy = state
        .runtime()?
        .resolve_tool_host_call_policy(&principal.principal_id)
        .await?;
    Ok(policy)
}

fn parse_sandbox_tool_filter(filter: &ToolHostToolFilter) -> SandboxToolFilter {
    SandboxToolFilter {
        allowlist: filter.allowlist.as_deref().and_then(split_tool_list),
        blocklist: filter
            .blocklist
            .as_deref()
            .and_then(split_tool_list)
            .unwrap_or_default(),
    }
}

// Unset or empty means no restriction, matching the sandbox installer.
fn split_tool_list(raw: &str) -> Option<BTreeSet<String>> {
    let entries = raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn mcp_find_centaur_tool(
    name: &str,
    filter: &SandboxToolFilter,
) -> Result<Option<DiscoveredTool>, ApiError> {
    Ok(mcp_centaur_tool_catalog(filter)?
        .into_iter()
        .find(|tool| tool.name == name))
}

fn mcp_whoami_result(principal: &McpPrincipal, arguments: Value) -> Result<Value, ApiError> {
    if !arguments.is_null() && !arguments.as_object().is_some_and(serde_json::Map::is_empty) {
        return Err(ApiError::BadRequest(
            "centaur_whoami does not accept arguments".to_owned(),
        ));
    }
    Ok(mcp_text_result(
        serde_json::to_string_pretty(&json!({
            "principal_id": principal.principal_id,
            "token_id": principal.token_id,
            "token_name": principal.name,
            "scopes": principal.scopes,
            "expires_at": principal
                .expires_at
                .map(|value| value.format(&time::format_description::well_known::Rfc3339))
                .transpose()
                .map_err(|error| ApiError::Internal(error.to_string()))?,
        }))?,
        false,
    ))
}

async fn mcp_v1_tool_result(
    state: &AppState,
    principal: &McpPrincipal,
    tool: DiscoveredTool,
    arguments: Value,
    policy: ToolHostCallPolicy,
) -> Result<McpToolCallOutcome, ApiError> {
    match prepare_mcp_v1_tool_call(&tool, arguments)? {
        McpCentaurToolAction::Return { result, method } => {
            if let Some(method) = method.as_deref() {
                record_mcp_tool_method(&Span::current(), &tool.name, method);
            }
            Ok(McpToolCallOutcome {
                result,
                timed_out: false,
            })
        }
        McpCentaurToolAction::Run { method, arguments } => {
            record_mcp_tool_method(&Span::current(), &tool.name, &method);
            run_mcp_v1_tool(
                state.runtime()?,
                principal,
                &tool,
                method,
                arguments,
                policy,
            )
            .await
        }
    }
}

enum McpCentaurToolAction {
    Return {
        result: Value,
        method: Option<String>,
    },
    Run {
        method: String,
        arguments: Value,
    },
}

fn prepare_mcp_v1_tool_call(
    tool: &DiscoveredTool,
    arguments: Value,
) -> Result<McpCentaurToolAction, ApiError> {
    let params = serde_json::from_value::<CentaurToolMcpArguments>(arguments)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    if params.method.trim().is_empty() {
        return Err(ApiError::BadRequest("method is required".to_owned()));
    }
    let method = params.method.trim().to_owned();
    let methods = mcp_v1_tool_methods(tool);
    if method == "help" {
        return mcp_v1_tool_help_result(tool, &methods).map(|result| {
            McpCentaurToolAction::Return {
                result,
                method: Some(method),
            }
        });
    }
    if !methods.iter().any(|candidate| candidate.name == method) {
        return Ok(McpCentaurToolAction::Return {
            result: mcp_text_result(
                format!(
                    "centaur tool {} has no method {method}. Available methods: {}",
                    tool.name,
                    methods
                        .iter()
                        .map(|method| method.signature.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                true,
            ),
            method: None,
        });
    }
    let arguments = match normalize_mcp_v1_tool_arguments(params.arguments) {
        Ok(arguments) => arguments,
        Err(kind) => {
            return Ok(McpCentaurToolAction::Return {
                result: mcp_text_result(
                    format!(
                        "centaur tool {}.{method} arguments must be an object; got {kind}",
                        tool.name
                    ),
                    true,
                ),
                method: Some(method),
            });
        }
    };
    Ok(McpCentaurToolAction::Run { method, arguments })
}

fn mcp_result_is_error(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) == Some(true)
}

fn normalize_mcp_v1_tool_arguments(arguments: Value) -> Result<Value, &'static str> {
    if arguments.is_null() {
        return Ok(json!({}));
    }
    if arguments.is_object() {
        return Ok(arguments);
    }
    Err(json_type_name(&arguments))
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// V1 calls Python client methods and requires JSON output.
async fn run_mcp_v1_tool(
    runtime: SessionRuntime,
    principal: &McpPrincipal,
    tool: &DiscoveredTool,
    method: String,
    arguments: Value,
    policy: ToolHostCallPolicy,
) -> Result<McpToolCallOutcome, ApiError> {
    let output = run_mcp_tool_host(
        runtime,
        principal,
        tool,
        ToolHostInvocation::V1 {
            method: method.clone(),
            arguments,
        },
        policy,
    )
    .await?;
    mcp_v1_output_result(tool, &method, output)
}

// V2 runs tool CLIs and preserves both output streams and exit status.
async fn run_mcp_v2_tool(
    runtime: SessionRuntime,
    principal: &McpPrincipal,
    tool: &DiscoveredTool,
    argv: Vec<String>,
    policy: ToolHostCallPolicy,
) -> Result<McpToolCallOutcome, ApiError> {
    let output = run_mcp_tool_host(
        runtime,
        principal,
        tool,
        ToolHostInvocation::V2 { argv },
        policy,
    )
    .await?;
    mcp_v2_run_output_result(output)
}

// Both MCP versions share principal-bound sandbox execution and tracing.
async fn run_mcp_tool_host(
    runtime: SessionRuntime,
    principal: &McpPrincipal,
    tool: &DiscoveredTool,
    invocation: ToolHostInvocation,
    policy: ToolHostCallPolicy,
) -> Result<ToolHostCallOutput, ApiError> {
    let output = match runtime
        .run_tool_host_call(
            ToolHostCallInput {
                principal_id: principal.principal_id.clone(),
                console_user_email: principal.console_user_email.clone(),
                console_user_name: principal.console_user_name.clone(),
                token_id: Some(principal.token_id.clone()),
                tool_name: tool.name.clone(),
                invocation,
                timeout: Duration::from_secs(120),
            },
            policy,
        )
        .await
    {
        Ok(output) => output,
        Err(error) => {
            record_mcp_tool_correlation(
                &Span::current(),
                error.request_id(),
                error.execution_id(),
                error.sandbox_id(),
            );
            return Err(error.into_source().into());
        }
    };
    let span = Span::current();
    record_mcp_tool_correlation(
        &span,
        Some(&output.request_id),
        Some(&output.execution_id),
        Some(&output.sandbox_id),
    );
    Ok(output)
}

fn mcp_v1_output_result(
    tool: &DiscoveredTool,
    method: &str,
    output: ToolHostCallOutput,
) -> Result<McpToolCallOutcome, ApiError> {
    if output.timed_out {
        return Ok(McpToolCallOutcome {
            result: mcp_text_result(
                format!(
                    "centaur tool {}.{method} timed out in {}: {}",
                    tool.name,
                    tool_host_error_context(&output),
                    output.stderr
                ),
                true,
            ),
            timed_out: true,
        });
    }
    if output.exit_status != Some(0) {
        let raw = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        let detail = mcp_tool_failure_detail(raw);
        return Ok(McpToolCallOutcome {
            result: mcp_text_result(
                format!(
                    "centaur tool {}.{method} failed in {} with status {:?}: {detail}\n\nCall the {} tool with method \"help\" to list available methods and their signatures.",
                    tool.name,
                    tool_host_error_context(&output),
                    output.exit_status,
                    tool.name
                ),
                true,
            ),
            timed_out: false,
        });
    }
    let stdout = output.stdout.trim();
    if stdout.is_empty() {
        return Ok(McpToolCallOutcome {
            result: mcp_text_result("null".to_owned(), false),
            timed_out: false,
        });
    }
    match serde_json::from_str::<Value>(stdout) {
        Ok(value) => Ok(McpToolCallOutcome {
            result: mcp_text_result(serde_json::to_string_pretty(&value)?, false),
            timed_out: false,
        }),
        Err(error) => Ok(McpToolCallOutcome {
            result: mcp_text_result(
                format!(
                    "centaur tool {}.{method} returned non-json output in {}: {error}: {stdout}",
                    tool.name,
                    tool_host_error_context(&output)
                ),
                true,
            ),
            timed_out: false,
        }),
    }
}

fn mcp_v2_run_output_result(output: ToolHostCallOutput) -> Result<McpToolCallOutcome, ApiError> {
    let is_error = output.timed_out || output.exit_status != Some(0);
    let content = json!({
        "stdout": output.stdout,
        "stderr": output.stderr,
        "exit_status": output.exit_status,
        "timed_out": output.timed_out,
    });
    let mut result = mcp_text_result(serde_json::to_string_pretty(&content)?, is_error);
    result["structuredContent"] = content;
    Ok(McpToolCallOutcome {
        result,
        timed_out: output.timed_out,
    })
}

fn tool_host_error_context(output: &ToolHostCallOutput) -> String {
    let mut parts = Vec::new();
    let sandbox_id = output.sandbox_id.trim();
    parts.push(if sandbox_id.is_empty() {
        "sandbox unknown".to_owned()
    } else {
        format!("sandbox {sandbox_id}")
    });

    let execution_id = output.execution_id.trim();
    if !execution_id.is_empty() {
        parts.push(format!("execution {execution_id}"));
    }

    let request_id = output.request_id.trim();
    if !request_id.is_empty() {
        parts.push(format!("request {request_id}"));
    }

    parts.join(", ")
}

/// Reduce a Python traceback to its final exception message: agents act on
/// the error line, not on stack frames or build noise, so keep everything
/// from the last traceback's exception message to the end.
fn mcp_tool_failure_detail(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(index) = trimmed.rfind("Traceback (most recent call last):") else {
        return trimmed.to_owned();
    };
    let lines = trimmed[index..].lines().collect::<Vec<_>>();
    let message_start = lines
        .iter()
        .skip(1)
        .position(|line| !line.is_empty() && !line.starts_with(char::is_whitespace));
    match message_start {
        Some(position) => lines[position + 1..].join("\n"),
        None => trimmed.to_owned(),
    }
}

fn mcp_text_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            },
        ],
        "isError": is_error,
    })
}

fn authenticate_mcp_bearer(headers: &HeaderMap) -> Result<Option<McpPrincipal>, ApiError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(None);
    };
    verify_mcp_jwt(&token, headers)
}

#[derive(Debug, Deserialize)]
struct McpJwtHeader {
    alg: String,
}

#[derive(Debug, Deserialize)]
struct McpJwtClaims {
    aud: Value,
    exp: i64,
    #[serde(default)]
    iat: Option<i64>,
    iss: String,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    nbf: Option<i64>,
    principal_id: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    sub: Option<String>,
}

fn verify_mcp_jwt(token: &str, headers: &HeaderMap) -> Result<Option<McpPrincipal>, ApiError> {
    let secret = jwt_signing_secret()
        .filter(|secret| !secret.trim().is_empty())
        .ok_or_else(|| {
            ApiError::ServiceUnavailable("CENTAUR_JWT_SIGNING_SECRET is not configured".to_owned())
        })?;

    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Ok(None);
    }
    let Some(header) = decode_base64url_json::<McpJwtHeader>(parts[0]) else {
        return Ok(None);
    };
    if header.alg != "HS256" {
        return Ok(None);
    }

    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| {
        ApiError::Internal("CENTAUR_JWT_SIGNING_SECRET is not valid HMAC key material".to_owned())
    })?;
    mac.update(signing_input.as_bytes());
    let expected = mac.finalize().into_bytes();
    let Some(presented) = decode_base64url(parts[2]) else {
        return Ok(None);
    };
    if !constant_time_eq(&presented, expected.as_slice()) {
        return Ok(None);
    }

    let Some(claims) = decode_base64url_json::<McpJwtClaims>(parts[1]) else {
        return Ok(None);
    };
    let now = OffsetDateTime::now_utc().unix_timestamp();
    if claims.exp <= now {
        return Ok(None);
    }
    if claims.nbf.is_some_and(|nbf| nbf > now + 30) {
        return Ok(None);
    }
    if claims.iat.is_some_and(|iat| iat > now + 30) {
        return Ok(None);
    }
    let Some(issuer) = mcp_authorization_server_url() else {
        return Ok(None);
    };
    if !same_url(&claims.iss, &issuer) {
        return Ok(None);
    }
    if !audience_contains(&claims.aud, &mcp_resource_url(headers)) {
        return Ok(None);
    }
    if claims.principal_id.trim().is_empty() {
        return Ok(None);
    }

    let mut scopes = claims.scopes.unwrap_or_default();
    if let Some(scope) = claims.scope {
        scopes.extend(scope.split_whitespace().map(ToOwned::to_owned));
    }
    scopes = normalize_scope_list(scopes);
    if scopes.is_empty() {
        return Ok(None);
    }
    let expires_at = OffsetDateTime::from_unix_timestamp(claims.exp).ok();
    let token_id = claims.jti.unwrap_or_else(|| {
        let digest = Sha256::digest(token.as_bytes());
        format!("mcp_jwt_{}", hex::encode(&digest[..12]))
    });
    let console_user_email = first_non_empty_owned([claims.email]);
    let console_user_name = first_non_empty_owned([claims.name]);
    let name = first_non_empty_owned([
        console_user_name.clone(),
        console_user_email.clone(),
        claims.sub,
        Some(claims.principal_id.clone()),
    ])
    .unwrap_or_else(|| claims.principal_id.clone());

    Ok(Some(McpPrincipal {
        token_id,
        principal_id: claims.principal_id,
        console_user_email,
        console_user_name,
        name,
        scopes,
        expires_at,
    }))
}

fn decode_base64url_json<T: for<'de> Deserialize<'de>>(value: &str) -> Option<T> {
    let decoded = decode_base64url(value)?;
    serde_json::from_slice(&decoded).ok()
}

fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| general_purpose::URL_SAFE.decode(value))
        .ok()
}

fn normalize_scope_list(scopes: Vec<String>) -> Vec<String> {
    let mut scopes = scopes
        .into_iter()
        .map(|scope| scope.trim().to_owned())
        .filter(|scope| !scope.is_empty())
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    scopes
}

fn first_non_empty_owned(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
}

fn audience_contains(audience: &Value, resource: &str) -> bool {
    match audience {
        Value::String(value) => same_url(value, resource),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .any(|value| same_url(value, resource)),
        _ => false,
    }
}

fn same_url(left: &str, right: &str) -> bool {
    normalize_public_url(left)
        .is_some_and(|left| normalize_public_url(right).is_some_and(|right| left == right))
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = header_value(headers, "Authorization")?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or(value.as_str())
        .trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn ensure_mcp_scope(scopes: &[String], required: &str) -> Result<(), ApiError> {
    if scopes
        .iter()
        .any(|scope| scope == "*" || scope == required || scope == "mcp:*")
    {
        Ok(())
    } else {
        Err(ApiError::Forbidden(format!(
            "missing required scope {required}"
        )))
    }
}

fn requested_mcp_protocol_version(params: &Value) -> &'static str {
    const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
    match params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|version| !version.trim().is_empty())
    {
        Some("2025-11-25") => "2025-11-25",
        Some("2025-06-18") => "2025-06-18",
        Some("2025-03-26") => "2025-03-26",
        _ => DEFAULT_PROTOCOL_VERSION,
    }
}

fn mcp_json_error(id: Value, code: i64, message: &str) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    }))
    .into_response()
}

fn mcp_unauthorized(headers: &HeaderMap) -> Response {
    let metadata = format!(
        "{}/.well-known/oauth-protected-resource/mcp",
        mcp_public_base_url(headers)
    );
    let challenge = format!(r#"Bearer resource_metadata="{metadata}", scope="mcp:tools""#);
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "ok": false,
            "error": "missing or invalid MCP bearer token",
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert("WWW-Authenticate", value);
    }
    response
}

fn mcp_resource_url(headers: &HeaderMap) -> String {
    if let Some(url) = mcp_public_url_env()
        .as_deref()
        .and_then(normalize_mcp_endpoint_url)
    {
        return url;
    }
    format!("{}/mcp", request_base_url(headers))
}

fn mcp_authorization_server_url() -> Option<String> {
    [console_public_url_env(), iron_control_public_url_env()]
        .into_iter()
        .find_map(|url| url.as_deref().and_then(normalize_public_url))
}

fn mcp_public_base_url(headers: &HeaderMap) -> String {
    if let Some(url) = mcp_public_url_env()
        .as_deref()
        .and_then(normalize_public_url)
    {
        return url.strip_suffix("/mcp").unwrap_or(&url).to_owned();
    }
    request_base_url(headers)
}

// The variables below are static deployment configuration, so each is resolved
// once per process. Tests mutate them per-case, so cfg!(test) reads live.
fn static_env(cell: &'static OnceLock<Option<String>>, name: &str) -> Option<String> {
    if cfg!(test) {
        return env::var(name).ok();
    }
    cell.get_or_init(|| env::var(name).ok()).clone()
}

fn mcp_public_url_env() -> Option<String> {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "CENTAUR_MCP_PUBLIC_URL")
}

fn mcp_v2_enabled() -> bool {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "CENTAUR_MCP_V2_ENABLED")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

fn console_public_url_env() -> Option<String> {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "CENTAUR_CONSOLE_PUBLIC_URL")
}

fn iron_control_public_url_env() -> Option<String> {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "IRON_CONTROL_PUBLIC_URL")
}

fn normalize_mcp_endpoint_url(value: &str) -> Option<String> {
    let mut url = normalize_public_url(value)?;
    if !url.ends_with("/mcp") {
        url.push_str("/mcp");
    }
    Some(url)
}

fn normalize_public_url(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

fn request_base_url(headers: &HeaderMap) -> String {
    let proto = header_value(headers, "X-Forwarded-Proto").unwrap_or_else(|| "http".to_owned());
    let host = header_value(headers, "X-Forwarded-Host")
        .or_else(|| header_value(headers, "Host"))
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned());
    format!("{}://{}", proto.trim(), host.trim())
}

/// Compare two byte strings in constant time (modulo length, which is not
/// secret here).
fn constant_time_eq(actual: &[u8], expected: &[u8]) -> bool {
    use subtle::ConstantTimeEq;

    actual.ct_eq(expected).into()
}

#[cfg(test)]
mod mcp_tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use futures_util::FutureExt;

    use super::*;

    use super::MCP_ENV_LOCK as ENV_LOCK;

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &'static str)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, _)| (*name, env::var(name).ok()))
                .collect();
            for (name, value) in vars {
                // SAFETY: tests that mutate process env hold ENV_LOCK for the
                // duration of the guard, so concurrent tests in this module
                // cannot observe partial mutations.
                unsafe {
                    env::set_var(name, value);
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..) {
                // SAFETY: see EnvGuard::set; the lock outlives the guard.
                unsafe {
                    if let Some(value) = value {
                        env::set_var(name, value);
                    } else {
                        env::remove_var(name);
                    }
                }
            }
        }
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{}-{suffix}", std::process::id()))
    }

    fn test_tool(project_dir: PathBuf) -> DiscoveredTool {
        DiscoveredTool {
            name: "demo".to_owned(),
            package: "demo".to_owned(),
            description: Some("Demo tool".to_owned()),
            client_module: "client.py".to_owned(),
            project_dir,
        }
    }

    fn returned_tool_action(action: McpCentaurToolAction) -> (Value, Option<String>) {
        match action {
            McpCentaurToolAction::Return { result, method } => (result, method),
            McpCentaurToolAction::Run { .. } => panic!("expected a local tool result"),
        }
    }

    fn test_jwt(secret: &str, claims: Value) -> String {
        let header = general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"alg": "HS256", "typ": "JWT"})).unwrap());
        let payload = general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signing_input = format!("{header}.{payload}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signing_input.as_bytes());
        let signature = general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{signing_input}.{signature}")
    }

    fn mcp_auth_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    #[test]
    fn mcp_tool_method_names_include_public_client_methods_and_help() {
        let temp = temp_dir("centaur-api-rs-mcp-methods");
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("client.py"),
            r#"
def search(query, limit=20):
    return []

def _hidden():
    return None

class DemoClient:
    def list_channels(self, limit=200):
        def nested_helper():
            return None
        return []

    async def search_messages(self, query):
        return []
"#,
        )
        .unwrap();

        let parsed = mcp_v1_tool_methods(&test_tool(temp.clone()));
        let methods = parsed
            .iter()
            .map(|method| method.name.clone())
            .collect::<Vec<_>>();

        assert!(methods.contains(&"help".to_owned()));
        assert!(methods.contains(&"search".to_owned()));
        assert!(methods.contains(&"list_channels".to_owned()));
        assert!(methods.contains(&"search_messages".to_owned()));
        assert!(!methods.contains(&"_hidden".to_owned()));
        assert!(!methods.contains(&"nested_helper".to_owned()));

        let signatures = parsed
            .into_iter()
            .map(|method| method.signature)
            .collect::<Vec<_>>();
        assert!(signatures.contains(&"search(query, limit=20)".to_owned()));
        assert!(signatures.contains(&"list_channels(limit=200)".to_owned()));
        assert!(signatures.contains(&"search_messages(query)".to_owned()));
        assert!(signatures.contains(&"help()".to_owned()));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn mcp_tool_failure_detail_keeps_final_exception_from_chained_traceback() {
        let stderr = r#"Building twitter @ file:///tools/comms/twitter
Installed 16 packages in 66ms
Traceback (most recent call last):
  File "/tools/comms/twitter/client.py", line 53, in _request
    response.raise_for_status()
httpx.HTTPStatusError: Client error '401 Unauthorized' for url 'https://api.x.com/2/tweets/search/recent'

The above exception was the direct cause of the following exception:

Traceback (most recent call last):
  File "<string>", line 45, in <module>
  File "/tools/comms/twitter/client.py", line 229, in search_tweets
    tweets, meta, includes = self._paged(
RuntimeError: X API error: 401 - {
  "title": "Unauthorized",
  "status": 401
}"#;

        let detail = mcp_tool_failure_detail(stderr);

        assert!(detail.starts_with("RuntimeError: X API error: 401"));
        assert!(detail.contains("\"title\": \"Unauthorized\""));
        assert!(!detail.contains("Traceback"));
        assert!(!detail.contains("Installed 16 packages"));

        let plain = "invalid arguments for search_tweets(query, limit=10): got an unexpected keyword argument 'max_results'";
        assert_eq!(mcp_tool_failure_detail(plain), plain);
    }

    #[test]
    fn mcp_tool_host_error_context_includes_correlation_ids() {
        let output = ToolHostCallOutput {
            request_id: "mcp-call-123".to_owned(),
            execution_id: "exe-456".to_owned(),
            sandbox_id: "sbx-789".to_owned(),
            stdout: String::new(),
            stderr: "boom".to_owned(),
            exit_status: Some(1),
            timed_out: false,
        };

        assert_eq!(
            tool_host_error_context(&output),
            "sandbox sbx-789, execution exe-456, request mcp-call-123"
        );
    }

    #[test]
    fn mcp_tool_host_error_context_handles_missing_sandbox_id() {
        let output = ToolHostCallOutput {
            request_id: "mcp-call-123".to_owned(),
            execution_id: "exe-456".to_owned(),
            sandbox_id: String::new(),
            stdout: String::new(),
            stderr: "boom".to_owned(),
            exit_status: None,
            timed_out: true,
        };

        assert_eq!(
            tool_host_error_context(&output),
            "sandbox unknown, execution exe-456, request mcp-call-123"
        );
    }

    #[test]
    fn mcp_unknown_method_returns_available_methods_without_running_tool() {
        let temp = temp_dir("centaur-api-rs-mcp-unknown-method");
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("client.py"),
            r#"
def search(query, limit=20):
    return []
"#,
        )
        .unwrap();

        let tool = test_tool(temp.clone());
        let (result, method) = returned_tool_action(
            prepare_mcp_v1_tool_call(&tool, json!({"method": "missing", "arguments": {}})).unwrap(),
        );

        assert_eq!(method, None);
        assert!(mcp_result_is_error(&result));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("has no method missing"));
        assert!(text.contains("search"));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn mcp_unknown_method_is_rejected_when_tool_has_no_public_methods() {
        let temp = temp_dir("centaur-api-rs-mcp-no-methods");
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("client.py"), "def _hidden():\n    return None\n").unwrap();

        let tool = test_tool(temp.clone());
        let (result, method) = returned_tool_action(
            prepare_mcp_v1_tool_call(&tool, json!({"method": "missing", "arguments": {}})).unwrap(),
        );

        assert_eq!(method, None);
        assert!(mcp_result_is_error(&result));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("has no method missing"));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn mcp_tool_arguments_must_be_an_object_before_running_sandbox() {
        let temp = temp_dir("centaur-api-rs-mcp-arguments-object");
        fs::create_dir_all(&temp).unwrap();
        fs::write(
            temp.join("client.py"),
            r#"
def search(query, limit=20):
    return []
"#,
        )
        .unwrap();

        let tool = test_tool(temp.clone());
        let (result, method) = returned_tool_action(
            prepare_mcp_v1_tool_call(
                &tool,
                json!({"method": "search", "arguments": ["not", "an", "object"]}),
            )
            .unwrap(),
        );

        assert_eq!(method.as_deref(), Some("search"));
        assert!(mcp_result_is_error(&result));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("arguments must be an object"));
        assert!(text.contains("demo.search"));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn mcp_tool_arguments_default_to_empty_object() {
        assert_eq!(
            normalize_mcp_v1_tool_arguments(Value::Null).unwrap(),
            json!({})
        );
        assert_eq!(
            normalize_mcp_v1_tool_arguments(json!({"query": "hello"})).unwrap(),
            json!({"query": "hello"})
        );
        assert_eq!(
            normalize_mcp_v1_tool_arguments(json!(["not", "object"])).unwrap_err(),
            "array"
        );
    }

    #[test]
    fn mcp_jwt_authenticates_console_principal() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_JWT_SIGNING_SECRET", "test-secret"),
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", "http://localhost:3001"),
        ]);
        let token = test_jwt(
            "test-secret",
            json!({
                "iss": "http://localhost:3001",
                "sub": "usr_test",
                "aud": "http://localhost:3000/mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600,
                "iat": OffsetDateTime::now_utc().unix_timestamp(),
                "jti": "mcpjwt_test",
                "scope": "mcp:tools",
                "principal_id": "prn_test",
                "email": "test@example.com",
                "name": "Test User",
            }),
        );

        let principal = authenticate_mcp_bearer(&mcp_auth_headers(&token))
            .unwrap()
            .unwrap();

        assert_eq!(principal.token_id, "mcpjwt_test");
        assert_eq!(principal.principal_id, "prn_test");
        assert_eq!(
            principal.console_user_email.as_deref(),
            Some("test@example.com")
        );
        assert_eq!(principal.console_user_name.as_deref(), Some("Test User"));
        assert_eq!(principal.name, "Test User");
        assert_eq!(principal.scopes, vec!["mcp:tools"]);
        assert!(principal.expires_at.is_some());
    }

    #[test]
    fn mcp_jwt_rejects_wrong_audience() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_JWT_SIGNING_SECRET", "test-secret"),
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", "http://localhost:3001"),
        ]);
        let token = test_jwt(
            "test-secret",
            json!({
                "iss": "http://localhost:3001",
                "aud": "http://other.example/mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600,
                "principal_id": "prn_test",
                "scope": "mcp:tools",
            }),
        );

        assert!(
            authenticate_mcp_bearer(&mcp_auth_headers(&token))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mcp_jwt_rejects_issued_at_in_the_future() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_JWT_SIGNING_SECRET", "test-secret"),
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", "http://localhost:3001"),
        ]);
        let token = test_jwt(
            "test-secret",
            json!({
                "iss": "http://localhost:3001",
                "aud": "http://localhost:3000/mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600,
                "iat": OffsetDateTime::now_utc().unix_timestamp() + 600,
                "principal_id": "prn_test",
                "scope": "mcp:tools",
            }),
        );

        assert!(
            authenticate_mcp_bearer(&mcp_auth_headers(&token))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mcp_jwt_rejects_internal_console_control_plane_issuer() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_JWT_SIGNING_SECRET", "test-secret"),
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", ""),
            ("IRON_CONTROL_PUBLIC_URL", ""),
            ("CENTAUR_CONSOLE_URL", "http://centaur-console:3000"),
            ("IRON_CONTROL_URL", "http://centaur-console:3000"),
        ]);
        let token = test_jwt(
            "test-secret",
            json!({
                "iss": "http://centaur-console:3000",
                "aud": "http://localhost:3000/mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600,
                "principal_id": "prn_test",
                "scope": "mcp:tools",
            }),
        );

        assert!(
            authenticate_mcp_bearer(&mcp_auth_headers(&token))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mcp_non_jwt_bearer_values_are_not_accepted() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[("CENTAUR_JWT_SIGNING_SECRET", "test-secret")]);

        assert!(
            authenticate_mcp_bearer(&mcp_auth_headers("not-a-jwt-token"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mcp_protected_resource_metadata_uses_configured_urls() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", "http://localhost:3001"),
        ]);

        let Json(metadata) = mcp_protected_resource_metadata(HeaderMap::new())
            .now_or_never()
            .unwrap();

        assert_eq!(metadata["resource"], "http://localhost:3000/mcp");
        assert_eq!(
            metadata["authorization_servers"][0],
            "http://localhost:3001"
        );
    }

    #[test]
    fn mcp_protected_resource_metadata_ignores_internal_console_control_plane_url() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_CONSOLE_PUBLIC_URL", ""),
            ("IRON_CONTROL_PUBLIC_URL", ""),
            ("CENTAUR_CONSOLE_URL", "http://centaur-console:3000"),
            ("IRON_CONTROL_URL", "http://centaur-console:3000"),
        ]);
        let Json(metadata) = mcp_protected_resource_metadata(HeaderMap::new())
            .now_or_never()
            .unwrap();

        assert_eq!(metadata["authorization_servers"], json!([]));
    }

    #[test]
    fn mcp_unauthorized_challenge_uses_public_metadata_url() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp")]);

        let response = mcp_unauthorized(&HeaderMap::new());
        let challenge = response
            .headers()
            .get("WWW-Authenticate")
            .unwrap()
            .to_str()
            .unwrap();

        assert!(challenge.contains(
            r#"resource_metadata="http://localhost:3000/.well-known/oauth-protected-resource/mcp""#
        ));
        assert!(!challenge.contains("/mcp/.well-known"));
    }

    #[test]
    fn sandbox_tool_filter_defaults_to_admit_all() {
        let filter = parse_sandbox_tool_filter(&ToolHostToolFilter::default());
        assert_eq!(filter, SandboxToolFilter::default());
        assert!(filter.admits(&test_tool(PathBuf::from("/tools/demo"))));

        let empty_lists = parse_sandbox_tool_filter(&ToolHostToolFilter {
            allowlist: Some(String::new()),
            blocklist: Some(" , ".to_owned()),
        });
        assert_eq!(empty_lists, SandboxToolFilter::default());
    }

    #[test]
    fn sandbox_tool_filter_matches_package_dir_project_or_script_name() {
        let filter = parse_sandbox_tool_filter(&ToolHostToolFilter {
            allowlist: Some("demo-dir, project-name".to_owned()),
            blocklist: Some("blocked, blocked-script".to_owned()),
        });

        // Admitted via the package directory name.
        assert!(filter.admits(&test_tool(PathBuf::from("/tools/demo-dir"))));
        // Admitted via the pyproject project name.
        let mut by_project = test_tool(PathBuf::from("/tools/other-dir"));
        by_project.package = "project-name".to_owned();
        assert!(filter.admits(&by_project));
        // Not listed -> filtered out.
        assert!(!filter.admits(&test_tool(PathBuf::from("/tools/unlisted"))));
        // Blocklist wins even when allowlisted.
        let mut blocked = test_tool(PathBuf::from("/tools/demo-dir"));
        blocked.package = "blocked".to_owned();
        assert!(!filter.admits(&blocked));
        let mut blocked_script = test_tool(PathBuf::from("/tools/demo-dir"));
        blocked_script.name = "blocked-script".to_owned();
        assert!(!filter.admits(&blocked_script));
    }

    #[test]
    fn mcp_v2_feature_flag_gates_discovery_and_cached_calls() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_JWT_SIGNING_SECRET", "test-secret"),
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", "http://localhost:3001"),
            ("CENTAUR_MCP_V2_ENABLED", ""),
        ]);
        // SAFETY: ENV_LOCK is held, and the guard restores the original value.
        unsafe { env::remove_var("CENTAUR_MCP_V2_ENABLED") };
        assert!(!mcp_v2_enabled());

        let state = AppState::unready(crate::auth::ApiAuthConfig::testing("test-secret"));
        let token = test_jwt(
            "test-secret",
            json!({
                "iss": "http://localhost:3001",
                "aud": "http://localhost:3000/mcp",
                "exp": OffsetDateTime::now_utc().unix_timestamp() + 3600,
                "principal_id": "prn_test",
                "scope": "mcp:tools",
            }),
        );
        for (flag, enabled) in [
            ("", false),
            ("false", false),
            ("invalid", false),
            ("true", true),
            (" TRUE ", true),
        ] {
            let _flag = EnvGuard::set(&[("CENTAUR_MCP_V2_ENABLED", flag)]);
            let names = mcp_builtin_tools()
                .into_iter()
                .map(|tool| tool["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                names,
                if enabled {
                    vec!["centaur", "centaur_whoami"]
                } else {
                    vec!["centaur_whoami"]
                }
            );

            // Invalid argv proves enabled requests reach the dispatcher
            // without requiring a runtime. Disabled cached calls fail earlier.
            let response = mcp_post(
                State(state.clone()),
                mcp_auth_headers(&token),
                Json(McpJsonRpcRequest {
                    jsonrpc: Some("2.0".to_owned()),
                    id: Some(json!(1)),
                    method: "tools/call".to_owned(),
                    params: json!({"name": "centaur", "arguments": {"command": "list"}}),
                }),
            )
            .now_or_never()
            .unwrap()
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .now_or_never()
                .unwrap()
                .unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            if enabled {
                assert!(mcp_result_is_error(&body["result"]));
                assert!(
                    body["result"]["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("invalid centaur arguments")
                );
            } else {
                assert_eq!(
                    body["error"],
                    json!({"code": -32602, "message": "unknown tool"})
                );
            }
        }
    }

    #[test]
    fn mcp_v2_hides_legacy_tools_from_discovery_but_keeps_them_callable() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = temp_dir("centaur-api-rs-mcp-v2-legacy-tools");
        let package_dir = temp.join("demo");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("pyproject.toml"),
            concat!(
                "[project]\n",
                "name = \"demo-tool\"\n",
                "description = \"Demo service\"\n\n",
                "[project.scripts]\n",
                "demo = \"demo.cli:main\"\n",
            ),
        )
        .unwrap();
        fs::write(
            package_dir.join("client.py"),
            "def ping():\n    return {}\n",
        )
        .unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_MCP_V2_ENABLED", "true"),
            (
                "TOOL_DIRS",
                Box::leak(temp.display().to_string().into_boxed_str()),
            ),
        ]);
        let filter = SandboxToolFilter::default();

        {
            let _v1 = EnvGuard::set(&[("CENTAUR_MCP_V2_ENABLED", "false")]);
            let names = mcp_tool_entries(&filter)
                .unwrap()
                .into_iter()
                .map(|tool| tool["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(names, vec!["centaur_whoami", "demo"]);
        }

        let names = mcp_tool_entries(&filter)
            .unwrap()
            .into_iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["centaur", "centaur_whoami"]);

        let tool = mcp_find_centaur_tool("demo", &filter).unwrap().unwrap();
        let action =
            prepare_mcp_v1_tool_call(&tool, json!({"method": "ping", "arguments": {}})).unwrap();
        assert!(matches!(action, McpCentaurToolAction::Run { .. }));

        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn mcp_v2_feature_flag_adds_server_instructions_preferring_centaur_tool() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[("CENTAUR_MCP_V2_ENABLED", "")]);

        for (flag, enabled) in [
            ("", false),
            ("false", false),
            ("invalid", false),
            ("true", true),
            (" TRUE ", true),
        ] {
            let _flag = EnvGuard::set(&[("CENTAUR_MCP_V2_ENABLED", flag)]);
            let result = mcp_initialize_result(&json!({
                "protocolVersion": "2025-06-18",
            }));

            if enabled {
                let instructions = result["instructions"].as_str().unwrap();
                assert!(instructions.contains("Prefer the `centaur` tool"));
                assert!(
                    instructions
                        .contains("instead of calling legacy per-service MCP tools directly")
                );
            } else {
                assert!(result.get("instructions").is_none());
            }
        }
    }

    #[test]
    fn mcp_service_list_returns_sorted_summaries_and_current_policy() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = temp_dir("centaur-api-rs-service-list");
        for name in ["beta", "alpha", "centaur"] {
            let package_dir = temp.join(name);
            fs::create_dir_all(&package_dir).unwrap();
            fs::write(
                package_dir.join("pyproject.toml"),
                format!(
                    "[project]\nname = \"{name}-tool\"\ndescription = \"{name} service\"\n\n[project.scripts]\n{name} = \"{name}.cli:main\"\n"
                ),
            )
            .unwrap();
        }
        let _env = EnvGuard::set(&[(
            "TOOL_DIRS",
            Box::leak(temp.display().to_string().into_boxed_str()),
        )]);
        let result = mcp_v2_service_list_result(&SandboxToolFilter::default()).unwrap();
        let expected = json!({"services": [
            {"name": "alpha", "description": "alpha service"},
            {"name": "beta", "description": "beta service"},
        ]});
        assert_eq!(result["structuredContent"], expected);
        assert!(!mcp_result_is_error(&result));
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, expected);
        let search =
            mcp_v2_service_search_result(&SandboxToolFilter::default(), "service").unwrap();
        assert_eq!(search["structuredContent"], expected);
        assert!(!mcp_result_is_error(&search));
        let text: Value =
            serde_json::from_str(search["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, expected);

        let mut filter = SandboxToolFilter {
            allowlist: Some(BTreeSet::from(["alpha-tool".to_owned()])),
            blocklist: BTreeSet::new(),
        };
        assert_eq!(
            mcp_v2_service_list_result(&filter).unwrap()["structuredContent"],
            json!({"services": [{"name": "alpha", "description": "alpha service"}]})
        );
        assert_eq!(
            mcp_v2_service_search_result(&filter, "service").unwrap()["structuredContent"],
            json!({"services": [{"name": "alpha", "description": "alpha service"}]})
        );
        assert_eq!(
            mcp_v2_service_search_result(&filter, "beta").unwrap()["structuredContent"],
            json!({"services": []})
        );
        assert!(mcp_find_centaur_tool("alpha", &filter).unwrap().is_some());
        assert!(mcp_find_centaur_tool("beta", &filter).unwrap().is_none());
        filter.blocklist.insert("alpha".to_owned());
        assert!(mcp_find_centaur_tool("alpha", &filter).unwrap().is_none());
        assert_eq!(
            mcp_v2_service_list_result(&filter).unwrap()["structuredContent"],
            json!({"services": []})
        );
        assert_eq!(
            mcp_v2_service_search_result(&filter, "alpha").unwrap()["structuredContent"],
            json!({"services": []})
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn mcp_service_list_reads_catalog_updates_and_optional_descriptions() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = temp_dir("centaur-api-rs-service-list-refresh");
        fs::create_dir_all(&temp).unwrap();
        let _env = EnvGuard::set(&[(
            "TOOL_DIRS",
            Box::leak(temp.display().to_string().into_boxed_str()),
        )]);
        let filter = SandboxToolFilter::default();
        assert_eq!(
            mcp_v2_service_list_result(&filter).unwrap()["structuredContent"],
            json!({"services": []})
        );
        let package_dir = temp.join("demo");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(
            package_dir.join("pyproject.toml"),
            "[project]\nname = \"demo\"\n\n[project.scripts]\ndemo = \"demo.cli:main\"\n",
        )
        .unwrap();
        assert_eq!(
            mcp_v2_service_list_result(&filter).unwrap()["structuredContent"],
            json!({"services": [{"name": "demo", "description": null}]})
        );
        assert_eq!(
            mcp_v2_service_search_result(&filter, "demo").unwrap()["structuredContent"],
            json!({"services": [{"name": "demo", "description": null}]})
        );
        fs::remove_dir_all(package_dir).unwrap();
        assert_eq!(
            mcp_v2_service_search_result(&filter, "demo").unwrap()["structuredContent"],
            json!({"services": []})
        );
        assert_eq!(
            mcp_v2_service_list_result(&filter).unwrap()["structuredContent"],
            json!({"services": []})
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn mcp_dispatcher_reports_invalid_arguments_as_tool_errors() {
        let state = AppState::unready(crate::auth::ApiAuthConfig::testing("test-secret"));
        let principal = McpPrincipal {
            token_id: "test-token".to_owned(),
            principal_id: "prn_test".to_owned(),
            console_user_email: None,
            console_user_name: None,
            name: "Test".to_owned(),
            scopes: vec!["mcp:tools".to_owned()],
            expires_at: None,
        };
        for arguments in [
            Value::Null,
            json!({}),
            json!([]),
            json!({"action": "list"}),
            json!({"command": "list"}),
            json!({"command": []}),
            json!({"command": [1]}),
            json!({"command": [" "]}),
            json!({"command": ["list"], "unexpected": true}),
            json!({"command": ["list", "extra"]}),
            json!({"command": ["search"]}),
            json!({"command": ["search", "slack", "messages"]}),
            json!({"command": ["search", ""]}),
            json!({"command": ["search", " \n\t "]}),
            json!({"command": ["run"]}),
            json!({"command": ["run", " \t"]}),
            json!({"command": ["run", "slack", "nul\0byte"]}),
            json!({"command": ["service", "nul\0byte"]}),
        ] {
            let result = mcp_v2_command_result(&state, &principal, arguments.clone())
                .now_or_never()
                .unwrap()
                .unwrap()
                .result;
            assert!(mcp_result_is_error(&result), "arguments: {arguments}");
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("Use {\"command\":[\"list\"]}")
            );
        }
    }

    #[test]
    fn centaur_commands_parse_discovery_and_preserve_query_text() {
        assert_eq!(
            parse_mcp_v2_command(json!({"command": ["list"]})).unwrap(),
            CentaurMcpCommand::List
        );
        for query in [
            "slack messages",
            "  spaced query  ",
            "CAFÉ",
            "company_context",
        ] {
            assert_eq!(
                parse_mcp_v2_command(json!({"command": ["search", query]})).unwrap(),
                CentaurMcpCommand::Search(query.to_owned()),
            );
        }
    }

    #[test]
    fn centaur_run_preserves_cli_arguments() {
        for argv in [
            vec![],
            vec!["--help"],
            vec![
                "search",
                "  café messages  ",
                "",
                "--limit",
                "2",
                "$(echo literal)",
                "|",
                ">",
            ],
        ] {
            let mut command = vec!["run", "slack"];
            command.extend(&argv);
            assert_eq!(
                parse_mcp_v2_command(json!({"command": command})).unwrap(),
                CentaurMcpCommand::Run {
                    tool: "slack".to_owned(),
                    argv: argv.into_iter().map(str::to_owned).collect(),
                },
            );
        }
    }

    #[test]
    fn mcp_cli_output_preserves_streams_and_reports_exit_and_timeout() {
        for (stdout, stderr, exit_status, timed_out, is_error) in [
            ("Usage: demo [OPTIONS]\n", "", Some(0), false, false),
            ("{\"ok\":true}\n", "warning\n", Some(0), false, false),
            ("", "", Some(0), false, false),
            ("partial\n", "invalid option\n", Some(2), false, true),
            ("partial\n", "timed out\n", None, true, true),
            ("", "terminated\n", None, false, true),
        ] {
            let outcome = mcp_v2_run_output_result(ToolHostCallOutput {
                request_id: "request".to_owned(),
                execution_id: "execution".to_owned(),
                sandbox_id: "sandbox".to_owned(),
                stdout: stdout.to_owned(),
                stderr: stderr.to_owned(),
                exit_status,
                timed_out,
            })
            .unwrap();
            assert_eq!(outcome.timed_out, timed_out);
            assert_eq!(mcp_result_is_error(&outcome.result), is_error);
            let expected = json!({"stdout": stdout, "stderr": stderr, "exit_status": exit_status, "timed_out": timed_out});
            assert_eq!(outcome.result["structuredContent"], expected);
            assert_eq!(
                serde_json::from_str::<Value>(
                    outcome.result["content"][0]["text"].as_str().unwrap()
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn mcp_tool_catalog_applies_sandbox_allowlist() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = temp_dir("centaur-api-rs-mcp-allowlist");
        for (dir, project) in [("alpha", "alpha-tool"), ("beta", "beta-tool")] {
            let package_dir = temp.join(dir);
            fs::create_dir_all(&package_dir).unwrap();
            fs::write(
                package_dir.join("pyproject.toml"),
                format!(
                    "[project]\nname = \"{project}\"\n\n[project.scripts]\n{dir} = \"{project}.cli:main\"\n"
                ),
            )
            .unwrap();
        }
        let _env = EnvGuard::set(&[(
            "TOOL_DIRS",
            Box::leak(temp.display().to_string().into_boxed_str()),
        )]);
        let filter = SandboxToolFilter {
            allowlist: Some(BTreeSet::from(["alpha".to_owned()])),
            blocklist: BTreeSet::new(),
        };

        let names = mcp_centaur_tool_catalog(&filter)
            .unwrap()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["alpha".to_owned()]);
    }
}
