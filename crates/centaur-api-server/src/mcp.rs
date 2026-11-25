use std::{collections::BTreeSet, env, sync::OnceLock};

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use centaur_session_runtime::{ToolHostCallPolicy, tool_host_thread_key};
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
};

mod codemode;

#[cfg(test)]
pub(crate) static MCP_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
#[serde(deny_unknown_fields)]
struct CentaurCatalogLoadArguments {
    tool: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CentaurCodemodeSearchArguments {
    query: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CentaurCodemodeExecuteArguments {
    code: String,
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
        "initialize" => {
            prewarm_mcp_codemode(&state, &principal).await;
            mcp_initialize_result(&request.params)
        }
        "ping" => json!({}),
        "tools/list" => {
            ensure_mcp_scope(&principal.scopes, "mcp:tools")?;
            let policy = mcp_tool_host_call_policy(&state, &principal).await?;
            prewarm_mcp_codemode_policy(&state, &policy);
            json!({
                "tools": mcp_tools(),
            })
        }
        "tools/call" => {
            ensure_mcp_scope(&principal.scopes, "mcp:tools")?;
            let params = serde_json::from_value::<McpToolCallParams>(request.params.clone())
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
            if !mcp_tool_name(&params.name) {
                return Ok(mcp_json_error(id, -32602, "unknown tool"));
            }
            mcp_tool_call_result(&state, &principal, params).await?
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
        "otel.status_code" = tracing::field::Empty,
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.sandbox_id" = tracing::field::Empty,
        "tool.kind" = "centaur",
        "centaur.tool.entry_point" = "mcp",
        "tool.name" = params.name.as_str(),
        "tool.method" = tracing::field::Empty,
        "tool.status" = tracing::field::Empty,
    );

    async move {
        let outcome = match params.name.as_str() {
            "centaur_catalog_search" => {
                mcp_codemode_search_result(state, principal, params.arguments).await
            }
            "centaur_catalog_load" => {
                mcp_codemode_load_result(state, principal, params.arguments).await
            }
            "centaur_execute" => {
                mcp_codemode_execute_result(state, principal, params.arguments).await
            }
            "centaur_whoami" => {
                mcp_whoami_result(principal, params.arguments).map(|result| McpToolCallOutcome {
                    result,
                    timed_out: false,
                })
            }
            _ => unreachable!("unknown MCP tools are rejected before dispatch"),
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
    json!({
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
        "instructions": concat!(
                "Centaur Code Mode exposes a large, dynamic connector catalog through a few tools. ",
                "Whenever a task involves an external service, private or current data, an external ",
                "action, or a service URL, first call `centaur_catalog_search` with the relevant ",
                "service names, URLs, domains and action; results include each function's ",
                "signature. Load a connector's full TypeScript declarations with ",
                "`centaur_catalog_load` when the one-line signature is not enough. Then call ",
                "connectors by writing JavaScript for `centaur_execute`: `code` is the body of an ",
                "async function, call connectors as ",
                "`await tools.<connector>.<function>(args)`, run independent calls together with ",
                "`Promise.all` or `Promise.allSettled` in one cell instead of several tool calls, ",
                "and `return` the JSON-serializable value you want back. `console.log` output is ",
                "returned with the result. A cell has no `fetch`, no module imports, no file access ",
                "and no state between calls, and is limited to 120 s, 1000 calls and 8 concurrent ",
                "calls."
        ),
    })
}

fn mcp_tools() -> Vec<Value> {
    let mut tools = mcp_codemode_tools();
    tools.push(mcp_whoami_tool());
    tools
}

fn mcp_tool_name(name: &str) -> bool {
    matches!(
        name,
        "centaur_catalog_search" | "centaur_catalog_load" | "centaur_execute" | "centaur_whoami"
    )
}

fn mcp_codemode_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "centaur_catalog_search",
            "description": concat!(
                "Search Centaur's live connector catalog. The catalog is large and dynamic, so ",
                "search whenever a task involves an external service, private or current data, an ",
                "external action, or a service URL; include service names, URLs, domains and the ",
                "requested action as terms. Results include each function's signature. Load a ",
                "connector's full TypeScript declarations with `centaur_catalog_load` when needed."
            ),
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "additionalProperties": false,
                "properties": {
                    "query": {"type": "string", "minLength": 1},
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 50,
                        "description": "Maximum number of results (default 8).",
                    },
                },
            },
        }),
        json!({
            "name": "centaur_catalog_load",
            "description": concat!(
                "Load a connector's full TypeScript declarations and JSDoc, for a connector found ",
                "with `centaur_catalog_search`. Use it when the one-line signature from search is ",
                "not enough to call the connector correctly."
            ),
            "inputSchema": {
                "type": "object",
                "required": ["tool"],
                "additionalProperties": false,
                "properties": {
                    "tool": {"type": "string", "minLength": 1},
                },
            },
        }),
        json!({
            "name": "centaur_execute",
            "description": concat!(
                "Run one JavaScript cell in the principal's Code Mode sandbox. `code` is the body of ",
                "an async function: call connectors as `await tools.<connector>.<function>(args)`, ",
                "run independent calls together with `Promise.all` or `Promise.allSettled` in the ",
                "same cell instead of several tool calls, and `return` a JSON-serializable value, ",
                "which becomes the result. `console.log` output is returned too. There is no ",
                "`fetch`, no module imports, no file access and no state between cells. A cell is ",
                "limited to 120 s, 1000 calls and 8 concurrent calls."
            ),
            "inputSchema": {
                "type": "object",
                "required": ["code"],
                "additionalProperties": false,
                "properties": {
                    "code": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Async function body; `return` value is the cell result.",
                    },
                },
            },
        }),
    ]
}

fn invalid_mcp_arguments(error: impl std::fmt::Display) -> McpToolCallOutcome {
    McpToolCallOutcome {
        result: mcp_text_result(format!("invalid arguments: {error}"), true),
        timed_out: false,
    }
}

async fn mcp_codemode_search_result(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match serde_json::from_value::<CentaurCodemodeSearchArguments>(arguments) {
        Ok(args) if !args.query.trim().is_empty() => args,
        Ok(_) => return Ok(invalid_mcp_arguments("query must not be blank")),
        Err(error) => return Ok(invalid_mcp_arguments(error)),
    };
    record_mcp_tool_method(&Span::current(), "centaur_catalog_search", "search");
    mcp_codemode_result(
        state,
        principal,
        codemode::CodemodeRequest::Search {
            query: args.query,
            limit: args.limit.unwrap_or(8).clamp(1, 50),
        },
    )
    .await
}

async fn mcp_codemode_load_result(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match serde_json::from_value::<CentaurCatalogLoadArguments>(arguments) {
        Ok(args) if !args.tool.trim().is_empty() => args,
        Ok(_) => return Ok(invalid_mcp_arguments("tool must not be blank")),
        Err(error) => return Ok(invalid_mcp_arguments(error)),
    };
    record_mcp_tool_method(&Span::current(), "centaur_catalog_load", "load");
    mcp_codemode_result(
        state,
        principal,
        codemode::CodemodeRequest::Load { tool: args.tool },
    )
    .await
}

async fn mcp_codemode_execute_result(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match serde_json::from_value::<CentaurCodemodeExecuteArguments>(arguments) {
        Ok(args) if !args.code.trim().is_empty() => args,
        Ok(_) => return Ok(invalid_mcp_arguments("code must not be blank")),
        Err(error) => return Ok(invalid_mcp_arguments(error)),
    };
    record_mcp_tool_method(&Span::current(), "centaur_execute", "exec");
    mcp_codemode_result(
        state,
        principal,
        codemode::CodemodeRequest::Exec { code: args.code },
    )
    .await
}

async fn mcp_codemode_result(
    state: &AppState,
    principal: &McpPrincipal,
    request: codemode::CodemodeRequest,
) -> Result<McpToolCallOutcome, ApiError> {
    let policy = mcp_tool_host_call_policy(state, principal).await?;
    let runtime = state.runtime()?;
    codemode::call(&runtime, &principal.principal_id, &policy, request).await
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

/// Warm a principal's Code Mode sandbox while the client is still parsing the
/// bootstrap response. Best effort: never fail the request.
async fn prewarm_mcp_codemode(state: &AppState, principal: &McpPrincipal) {
    match mcp_tool_host_call_policy(state, principal).await {
        Ok(policy) => prewarm_mcp_codemode_policy(state, &policy),
        Err(error) => tracing::debug!(
            error = %error,
            "skipping MCP Code Mode prewarm: the call policy is not available yet"
        ),
    }
}

fn prewarm_mcp_codemode_policy(state: &AppState, policy: &ToolHostCallPolicy) {
    match state.runtime() {
        Ok(runtime) => runtime.prewarm_codemode(policy.clone()),
        Err(error) => tracing::debug!(
            error = %error,
            "skipping MCP Code Mode prewarm: the runtime is not ready yet"
        ),
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

fn mcp_result_is_error(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) == Some(true)
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

/// Whether api-rs serves `/mcp` and its protected-resource metadata. On by
/// default; `CENTAUR_MCP_ENABLED=false` turns the endpoint off. When on, `/mcp`
/// serves Code Mode: catalog search and load over Code Mode connectors, plus
/// `centaur_execute` for JavaScript cells, in one sandbox per principal.
pub fn mcp_enabled() -> bool {
    static CELL: OnceLock<Option<String>> = OnceLock::new();
    parse_mcp_enabled(static_env(&CELL, "CENTAUR_MCP_ENABLED").as_deref())
}

fn parse_mcp_enabled(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        )
    })
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
    fn mcp_is_enabled_by_default_and_can_be_disabled() {
        for (flag, enabled) in [
            (None, true),
            (Some(""), true),
            (Some("true"), true),
            (Some(" TRUE "), true),
            (Some("false"), false),
            (Some(" FALSE "), false),
            (Some("0"), false),
            (Some("off"), false),
        ] {
            assert_eq!(parse_mcp_enabled(flag), enabled, "flag: {flag:?}");
        }
    }

    #[test]
    fn mcp_serves_the_codemode_surface() {
        let names = mcp_tools()
            .into_iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "centaur_catalog_search",
                "centaur_catalog_load",
                "centaur_execute",
                "centaur_whoami",
            ]
        );
        assert!(names.iter().all(|name| mcp_tool_name(name)));

        let initialize = mcp_initialize_result(&json!({}));
        let instructions = initialize["instructions"].as_str().unwrap();
        for expected in [
            "centaur_execute",
            "await tools.<connector>.<function>(args)",
        ] {
            assert!(instructions.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn mcp_codemode_rejects_tools_outside_its_surface() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("CENTAUR_JWT_SIGNING_SECRET", "test-secret"),
            ("CENTAUR_MCP_PUBLIC_URL", "http://localhost:3000/mcp"),
            ("CENTAUR_CONSOLE_PUBLIC_URL", "http://localhost:3001"),
        ]);
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
        let call = |name: &str| {
            let response = mcp_post(
                State(state.clone()),
                mcp_auth_headers(&token),
                Json(McpJsonRpcRequest {
                    jsonrpc: Some("2.0".to_owned()),
                    id: Some(json!(1)),
                    method: "tools/call".to_owned(),
                    params: json!({"name": name, "arguments": {}}),
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
            serde_json::from_slice::<Value>(&body).unwrap()
        };
        let unknown = json!({"code": -32602, "message": "unknown tool"});
        for name in [
            "centaur_artifact_get",
            "centaur_tool_call",
            "demo",
            "centaur_unknown",
        ] {
            assert_eq!(call(name)["error"], unknown, "tool: {name}");
        }
    }

    #[test]
    fn mcp_codemode_tools_validate_arguments_before_resolving_the_endpoint() {
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
        for (tool, arguments) in [
            ("search", Value::Null),
            ("search", json!({"query": " \n\t "})),
            ("search", json!({"query": "slack", "extra": true})),
            ("load", json!({})),
            ("load", json!({"tool": ""})),
            ("execute", json!({"code": "  "})),
        ] {
            let result =
                match tool {
                    "search" => mcp_codemode_search_result(&state, &principal, arguments.clone())
                        .now_or_never(),
                    "load" => mcp_codemode_load_result(&state, &principal, arguments.clone())
                        .now_or_never(),
                    _ => mcp_codemode_execute_result(&state, &principal, arguments.clone())
                        .now_or_never(),
                }
                .unwrap()
                .unwrap()
                .result;
            assert!(
                mcp_result_is_error(&result),
                "tool: {tool}, arguments: {arguments}"
            );
        }
    }
}
