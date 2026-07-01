use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_session_runtime::{SessionRuntime, ToolHostCallInput};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiError,
    routes::AppState,
    tool_discovery::{DiscoveredTool, ToolDiscoveryConfig, discover_tool_catalog},
};

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

#[derive(Debug, Clone)]
struct McpPrincipal {
    principal_id: String,
}

pub(crate) async fn mcp_post(
    State(state): State<AppState>,
    Json(request): Json<McpJsonRpcRequest>,
) -> Result<Response, ApiError> {
    let principal = anonymous_mcp_principal();
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
        "initialize" => json!({
            "protocolVersion": requested_mcp_protocol_version(&request.params),
            "capabilities": {
                "tools": {
                    "listChanged": false,
                },
            },
            "serverInfo": {
                "name": "centaur",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
        "ping" => json!({}),
        "tools/list" => {
            let mut tools = vec![mcp_whoami_tool()];
            tools.extend(mcp_centaur_tool_entries()?);
            json!({
                "tools": tools,
            })
        }
        "tools/call" => {
            let params = serde_json::from_value::<McpToolCallParams>(request.params.clone())
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
            if params.name == "centaur_whoami" {
                mcp_whoami_result(&principal, params.arguments)?
            } else {
                let Some(tool) = mcp_find_centaur_tool(&params.name)? else {
                    return Ok(mcp_json_error(id, -32602, "unknown tool"));
                };
                mcp_centaur_tool_result(&state, &principal, tool, params.arguments).await?
            }
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

fn mcp_whoami_tool() -> Value {
    json!({
        "name": "centaur_whoami",
        "description": "Show the Centaur MCP tool-host principal.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        },
    })
}

fn mcp_centaur_tool_entries() -> Result<Vec<Value>, ApiError> {
    let mut entries = Vec::new();
    for tool in mcp_centaur_tool_catalog()? {
        let methods = mcp_tool_methods(&tool);
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

fn mcp_tool_methods(tool: &DiscoveredTool) -> Vec<McpToolMethod> {
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

fn mcp_tool_help_result(
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

fn mcp_centaur_tool_catalog() -> Result<Vec<DiscoveredTool>, ApiError> {
    // Discovery scans the tool dirs and parses package metadata on every
    // call; reuse a recent result so each MCP request does not redo that
    // I/O while still picking up newly synced tools quickly. Tests point
    // the discovery env vars at per-case temp dirs, so they read live.
    const CATALOG_TTL: Duration = Duration::from_secs(10);
    static CATALOG_CACHE: Mutex<Option<(Instant, Vec<DiscoveredTool>)>> = Mutex::new(None);
    if !cfg!(test)
        && let Some((discovered_at, tools)) = CATALOG_CACHE.lock().unwrap().as_ref()
        && discovered_at.elapsed() < CATALOG_TTL
    {
        return Ok(tools.clone());
    }

    let dirs = ToolDiscoveryConfig {
        tool_dirs: env::var("TOOL_DIRS").ok(),
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
    Ok(tools)
}

fn mcp_find_centaur_tool(name: &str) -> Result<Option<DiscoveredTool>, ApiError> {
    Ok(mcp_centaur_tool_catalog()?
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
        }))?,
        false,
    ))
}

async fn mcp_centaur_tool_result(
    state: &AppState,
    principal: &McpPrincipal,
    tool: DiscoveredTool,
    arguments: Value,
) -> Result<Value, ApiError> {
    let params = serde_json::from_value::<CentaurToolMcpArguments>(arguments)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    if params.method.trim().is_empty() {
        return Err(ApiError::BadRequest("method is required".to_owned()));
    }
    let method = params.method.trim().to_owned();
    let methods = mcp_tool_methods(&tool);
    if method == "help" {
        return mcp_tool_help_result(&tool, &methods);
    }
    if !methods.iter().any(|candidate| candidate.name == method) {
        return Ok(mcp_text_result(
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
        ));
    }
    run_tool_host_centaur_tool(
        state.runtime()?,
        principal,
        &tool,
        &method,
        params.arguments,
    )
    .await
}

async fn run_tool_host_centaur_tool(
    runtime: SessionRuntime,
    principal: &McpPrincipal,
    tool: &DiscoveredTool,
    method: &str,
    arguments: Value,
) -> Result<Value, ApiError> {
    let tool_host_principal_id = runtime
        .register_mcp_tool_host_principal(&principal.principal_id)
        .await?;
    let output = runtime
        .run_tool_host_call(ToolHostCallInput {
            principal_id: tool_host_principal_id,
            token_id: None,
            tool_name: tool.name.clone(),
            method: method.to_owned(),
            arguments,
            timeout: Duration::from_secs(120),
        })
        .await?;
    if output.timed_out {
        return Ok(mcp_text_result(
            format!(
                "centaur tool {}.{method} timed out in sandbox {}: {}",
                tool.name, output.sandbox_id, output.stderr
            ),
            true,
        ));
    }
    if output.exit_status != Some(0) {
        let raw = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        let detail = mcp_tool_failure_detail(raw);
        return Ok(mcp_text_result(
            format!(
                "centaur tool {}.{method} failed in sandbox {} with status {:?}: {detail}\n\nCall the {} tool with method \"help\" to list available methods and their signatures.",
                tool.name, output.sandbox_id, output.exit_status, tool.name
            ),
            true,
        ));
    }
    let stdout = output.stdout.trim();
    if stdout.is_empty() {
        return Ok(mcp_text_result("null".to_owned(), false));
    }
    match serde_json::from_str::<Value>(stdout) {
        Ok(value) => Ok(mcp_text_result(
            serde_json::to_string_pretty(&value)?,
            false,
        )),
        Err(error) => Ok(mcp_text_result(
            format!(
                "centaur tool {}.{method} returned non-json output in sandbox {}: {error}: {stdout}",
                tool.name, output.sandbox_id
            ),
            true,
        )),
    }
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

fn anonymous_mcp_principal() -> McpPrincipal {
    McpPrincipal {
        principal_id: "mcp-anonymous".to_owned(),
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

#[cfg(test)]
mod mcp_tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

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

        let parsed = mcp_tool_methods(&test_tool(temp.clone()));
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

    #[tokio::test]
    async fn mcp_unknown_method_returns_available_methods_without_running_tool() {
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

        let result = mcp_centaur_tool_result(
            &AppState::unready(),
            &McpPrincipal {
                principal_id: "mcp-test".to_owned(),
            },
            test_tool(temp.clone()),
            json!({"method": "missing", "arguments": {}}),
        )
        .await
        .unwrap();

        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("has no method missing"));
        assert!(text.contains("search"));

        let _ = fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn mcp_unknown_method_is_rejected_when_tool_has_no_public_methods() {
        let temp = temp_dir("centaur-api-rs-mcp-no-methods");
        fs::create_dir_all(&temp).unwrap();
        fs::write(temp.join("client.py"), "def _hidden():\n    return None\n").unwrap();

        let result = mcp_centaur_tool_result(
            &AppState::unready(),
            &McpPrincipal {
                principal_id: "mcp-test".to_owned(),
            },
            test_tool(temp.clone()),
            json!({"method": "missing", "arguments": {}}),
        )
        .await
        .unwrap();

        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("has no method missing"));

        let _ = fs::remove_dir_all(temp);
    }
}
