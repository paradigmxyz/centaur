use std::{
    collections::BTreeMap,
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
use centaur_session_runtime::{SessionRuntime, ToolHostCallInput};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    ApiError,
    routes::{AppState, header_value},
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct McpPrincipal {
    token_id: String,
    principal_id: String,
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
            ensure_mcp_scope(&principal.scopes, "mcp:tools")?;
            let mut tools = vec![mcp_whoami_tool()];
            tools.extend(mcp_centaur_tool_entries()?);
            json!({
                "tools": tools,
            })
        }
        "tools/call" => {
            ensure_mcp_scope(&principal.scopes, "mcp:tools")?;
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
        "description": "Show the authenticated Centaur MCP principal and token metadata.",
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
    let output = runtime
        .run_tool_host_call(ToolHostCallInput {
            principal_id: principal.principal_id.clone(),
            token_id: Some(principal.token_id.clone()),
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
    let name = first_non_empty_owned([
        claims.name,
        claims.email,
        claims.sub,
        Some(claims.principal_id.clone()),
    ])
    .unwrap_or_else(|| claims.principal_id.clone());

    Ok(Some(McpPrincipal {
        token_id,
        principal_id: claims.principal_id,
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

fn jwt_signing_secret() -> Option<String> {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "CENTAUR_JWT_SIGNING_SECRET")
}

fn mcp_public_url_env() -> Option<String> {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    static_env(&CELL, "CENTAUR_MCP_PUBLIC_URL")
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
    use std::{
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use futures_util::FutureExt;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
                principal_id: "mcp:test".to_owned(),
                token_id: "mcp_tok_test".to_owned(),
                name: "test".to_owned(),
                scopes: vec!["mcp:tools".to_owned()],
                expires_at: None,
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
                principal_id: "mcp:test".to_owned(),
                token_id: "mcp_tok_test".to_owned(),
                name: "test".to_owned(),
                scopes: vec!["mcp:tools".to_owned()],
                expires_at: None,
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
            }),
        );

        let principal = authenticate_mcp_bearer(&mcp_auth_headers(&token))
            .unwrap()
            .unwrap();

        assert_eq!(principal.token_id, "mcpjwt_test");
        assert_eq!(principal.principal_id, "prn_test");
        assert_eq!(principal.name, "test@example.com");
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
}
