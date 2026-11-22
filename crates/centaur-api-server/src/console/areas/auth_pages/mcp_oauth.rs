//! MCP OAuth authorization server: metadata, dynamic client registration, the
//! server-rendered consent page, and the token endpoint.
//!
//! Ports `Mcp::OauthController`. Responses keep the wire contract of the Rails
//! endpoint: identical metadata, redirect errors, consent form fields, JSON
//! error messages, and `Cache-Control: no-store` on every token response.
//!
//! Requests are typed per endpoint instead of read out of Rails' merged
//! params: registration takes the RFC 7591 JSON body, the token endpoint takes
//! the RFC 6749 form body, and the consent page takes typed query/form fields.

use std::collections::BTreeMap;

use axum::{
    extract::{OriginalUri, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_db::{
    ConsoleUser, ConsoleUserStatus,
    areas::auth_pages::{
        DEFAULT_GRANT_TYPES, DEFAULT_RESPONSE_TYPES, DEFAULT_SCOPES, McpClient, NewMcpClient,
        StoredAuthorizationCode, StoredRefreshToken, allowed_redirect_uri,
        consume_authorization_code, create_authorization_code, create_mcp_client,
        create_refresh_token, find_mcp_client, find_usable_authorization_code,
        find_usable_refresh_token, principal_foreign_id, provision_user_principal,
        revoke_refresh_tokens_for_user, rotate_refresh_token, touch_mcp_client_last_used,
    },
    find_console_user,
    model::{self, ConsoleModel},
    validation::plain_message,
};
use garde::Validate;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use super::{
    SERVER_PAGE_CSP, absolute_url, console_config, console_env, console_unavailable, env_any,
    html_page, login_redirect, public_base_url, redirect, require_active_user, server_page,
};
use crate::{
    AppState,
    console::{
        assets,
        http::{ConsoleError, request_origin},
        pages::login_error,
        request::{ConsoleBody, ConsoleForm, ConsoleQuery},
    },
    console_session::ConsoleSessionConfig,
};

const ACCESS_TOKEN_TTL_SECONDS: i64 = 60 * 60;

/// `POST /mcp/oauth/register`: RFC 7591 client metadata as a JSON body.
///
/// The four client-metadata lists deserialize already normalized — the
/// controller's `redirect_uris` trimming, `normalize_list_param`, and
/// `normalize_scope_param` — and validate with `McpOauthClient`'s rules, so the
/// endpoint only turns their report into its OAuth error message.
#[derive(Debug, Deserialize, garde::Validate)]
pub(super) struct RegisterRequest {
    #[serde(default)]
    #[garde(skip)]
    client_name: Option<String>,
    /// `Array(params[:redirect_uris]).map(&:to_s).map(&:strip).reject(&:blank?)`.
    #[serde(default, deserialize_with = "deserialize_redirect_uris")]
    #[garde(length(min = 1), inner(custom(public_redirect_uri)))]
    redirect_uris: Vec<String>,
    #[serde(
        default = "default_grant_types",
        deserialize_with = "deserialize_grant_types"
    )]
    #[garde(inner(custom(grant_type)))]
    grant_types: Vec<String>,
    #[serde(
        default = "default_response_types",
        deserialize_with = "deserialize_response_types"
    )]
    #[garde(inner(custom(response_type)))]
    response_types: Vec<String>,
    #[serde(default = "default_scopes", deserialize_with = "deserialize_scopes")]
    #[garde(inner(custom(supported_scope)))]
    scope: Vec<String>,
    #[serde(default)]
    #[garde(skip)]
    client_uri: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    logo_uri: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    contacts: Option<Vec<String>>,
    #[serde(default)]
    #[garde(skip)]
    software_id: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    software_version: Option<String>,
}

fn deserialize_redirect_uris<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect())
}

fn deserialize_grant_types<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(normalize_list_param(
        Vec::<String>::deserialize(deserializer)?,
        &DEFAULT_GRANT_TYPES,
    ))
}

fn default_grant_types() -> Vec<String> {
    DEFAULT_GRANT_TYPES.map(str::to_owned).to_vec()
}

fn deserialize_response_types<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(normalize_list_param(
        Vec::<String>::deserialize(deserializer)?,
        &DEFAULT_RESPONSE_TYPES,
    ))
}

fn default_response_types() -> Vec<String> {
    DEFAULT_RESPONSE_TYPES.map(str::to_owned).to_vec()
}

fn deserialize_scopes<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(normalize_scope_param(
        Option::<String>::deserialize(deserializer)?,
        &DEFAULT_SCOPES,
    ))
}

fn default_scopes() -> Vec<String> {
    DEFAULT_SCOPES.map(str::to_owned).to_vec()
}

fn public_redirect_uri(value: &str, _: &()) -> garde::Result {
    if allowed_redirect_uri(value) {
        return Ok(());
    }
    Err(garde::Error::new(
        "is not an allowed public-client redirect URI",
    ))
}

fn grant_type(value: &str, _: &()) -> garde::Result {
    included(value, &DEFAULT_GRANT_TYPES)
}

fn response_type(value: &str, _: &()) -> garde::Result {
    included(value, &DEFAULT_RESPONSE_TYPES)
}

fn supported_scope(value: &str, _: &()) -> garde::Result {
    included(value, &DEFAULT_SCOPES)
}

fn included(value: &str, allowed: &[&str]) -> garde::Result {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(garde::Error::new(format!(
        "must be one of {}",
        allowed.join(", ")
    )))
}

/// Rails' `values - allowed`: the offenders in list order. `Array#-` preserves
/// duplicates, so a repeated offender is reported twice.
fn unsupported_values<'a>(values: &'a [String], allowed: &[&str]) -> Vec<&'a str> {
    values
        .iter()
        .filter(|value| !allowed.contains(&value.as_str()))
        .map(String::as_str)
        .collect()
}

/// The authorization request: the consent page's query string and the consent
/// form share these flat field names. `decision` is the consent form's addition.
#[derive(Debug, Deserialize)]
pub(super) struct AuthorizationParams {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    response_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default)]
    decision: Option<String>,
}

/// `POST /mcp/oauth/token`: RFC 6749 token request, either grant type.
#[derive(Debug, Deserialize)]
pub(super) struct TokenRequest {
    #[serde(default)]
    grant_type: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// GET /.well-known/oauth-authorization-server and openid-configuration.
pub(super) async fn metadata(State(_state): State<AppState>, headers: HeaderMap) -> Response {
    let base = public_base_url(&headers);
    server_page(
        axum::Json(MetadataPayload {
            authorization_endpoint: absolute_url(&base, "/mcp/oauth/authorize"),
            token_endpoint: absolute_url(&base, "/mcp/oauth/token"),
            registration_endpoint: absolute_url(&base, "/mcp/oauth/register"),
            issuer: base,
            response_types_supported: DEFAULT_RESPONSE_TYPES,
            grant_types_supported: DEFAULT_GRANT_TYPES,
            code_challenge_methods_supported: ["S256"],
            token_endpoint_auth_methods_supported: ["none"],
            scopes_supported: DEFAULT_SCOPES,
            resource_parameter_supported: true,
        })
        .into_response(),
    )
}

/// The authorization-server metadata document, with Rails' fields.
#[derive(serde::Serialize)]
struct MetadataPayload {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: String,
    response_types_supported: [&'static str; 1],
    grant_types_supported: [&'static str; 2],
    code_challenge_methods_supported: [&'static str; 1],
    token_endpoint_auth_methods_supported: [&'static str; 1],
    scopes_supported: [&'static str; 1],
    resource_parameter_supported: bool,
}

/// POST /mcp/oauth/register
pub(super) async fn register(
    State(state): State<AppState>,
    ConsoleBody(request): ConsoleBody<RegisterRequest>,
) -> Response {
    let Some(_config) = console_config(&state) else {
        return console_unavailable();
    };

    if let Err(report) = request.validate() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            &plain_message(&report).expect("a failed validation carries a message"),
        );
    }
    let name = request
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("MCP client")
        .to_owned();
    let metadata = registration_metadata(&request);
    let client = NewMcpClient {
        name,
        redirect_uris: request.redirect_uris,
        grant_types: request.grant_types,
        response_types: request.response_types,
        scopes: request.scope,
        metadata,
    };

    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let client = match create_mcp_client(&pool, &client).await {
        Ok(client) => client,
        Err(error) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                &error.to_string(),
            );
        }
    };
    let body = ClientPayload {
        client_id: client.public_client_id(),
        client_name: client.name,
        redirect_uris: client.redirect_uris,
        grant_types: client.grant_types,
        response_types: client.response_types,
        scope: client.scopes.join(" "),
        token_endpoint_auth_method: "none",
    };
    server_page((StatusCode::CREATED, axum::Json(body)).into_response())
}

/// The created client Rails' `register` renders.
#[derive(serde::Serialize)]
struct ClientPayload {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    scope: String,
    token_endpoint_auth_method: &'static str,
}

/// The registration metadata Rails kept from the request parameters. An absent
/// or `null` field is dropped; any other value is stored as sent.
fn registration_metadata(request: &RegisterRequest) -> Value {
    serde_json::to_value(RegistrationMetadata {
        client_uri: request.client_uri.clone(),
        logo_uri: request.logo_uri.clone(),
        contacts: request.contacts.clone(),
        software_id: request.software_id.clone(),
        software_version: request.software_version.clone(),
    })
    .unwrap_or_default()
}

/// `Mcp::OauthController#registration_metadata`: the registration parameters
/// that were present, stored as the client's `metadata`.
#[derive(serde::Serialize)]
struct RegistrationMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    client_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logo_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contacts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    software_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    software_version: Option<String>,
}

/// GET /mcp/oauth/authorize
pub(super) async fn authorize_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsoleQuery(params): ConsoleQuery<AuthorizationParams>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let Some(config) = console_config(&state).cloned() else {
        return console_unavailable();
    };
    let full_path = match uri.query() {
        Some(query) if !query.is_empty() => format!("{}?{query}", uri.path()),
        _ => uri.path().to_owned(),
    };
    let user =
        match require_active_user(&state, &config, &headers, Some(&full_path), SERVER_PAGE_CSP)
            .await
        {
            Ok(user) => user,
            Err(response) => return response,
        };
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let authorization = match validate_authorization(&pool, &params).await {
        Ok(authorization) => authorization,
        Err(response) => return response,
    };
    if config.session_from_headers(&headers).is_none() {
        return login_redirect(&config, &headers, login_error::SESSION_EXPIRED, Vec::new());
    }
    let form_fields = authorization.hidden_fields();
    let html = consent_html(
        &authorization.client.name,
        &redirect_host(&authorization.redirect_uri),
        &authorization.resource,
        &authorization.scopes.join(" "),
        &user.email,
        &form_fields,
    );
    html_page(StatusCode::OK, html)
}

/// POST /mcp/oauth/authorize
pub(super) async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsoleForm(params): ConsoleForm<AuthorizationParams>,
) -> Response {
    let Some(config) = console_config(&state).cloned() else {
        return console_unavailable();
    };
    if config.session_from_headers(&headers).is_none() {
        return csrf_rejection();
    }
    if !origin_valid(&headers, &config) {
        return csrf_rejection();
    }
    let user = match require_active_user(&state, &config, &headers, None, SERVER_PAGE_CSP).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let authorization = match validate_authorization(&pool, &params).await {
        Ok(authorization) => authorization,
        Err(response) => return response,
    };
    if params.decision.as_deref() != Some("approve") {
        return authorization_redirect(
            &authorization.client,
            &authorization.redirect_uri,
            params.state.as_deref(),
            "access_denied",
            "The user denied the authorization request.",
        );
    }

    let principal = match provision_user_principal(&pool, &user).await {
        Ok(principal) => principal,
        Err(error) => {
            return oauth_error(StatusCode::BAD_REQUEST, "server_error", &error.to_string());
        }
    };
    let code = match create_authorization_code(
        &pool,
        authorization.client.id,
        user.id,
        principal.id,
        &authorization.redirect_uri,
        &authorization.code_challenge,
        &authorization.resource,
        &authorization.scopes,
    )
    .await
    {
        Ok(code) => code,
        Err(error) => {
            return oauth_error(StatusCode::BAD_REQUEST, "server_error", &error.to_string());
        }
    };
    if let Err(error) = touch_mcp_client_last_used(&pool, authorization.client.id).await {
        return ConsoleError::internal(error).into_response();
    }
    success_redirect(&authorization.redirect_uri, params.state.as_deref(), &code)
}

/// POST /mcp/oauth/token
pub(super) async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsoleForm(request): ConsoleForm<TokenRequest>,
) -> Response {
    let Some(_config) = console_config(&state) else {
        return console_unavailable();
    };
    // The TTL is a strict positive integer now: an invalid value is this
    // endpoint's own 400 in the RFC 6749 error body, not Ruby's silent `to_i`
    // prefix/fallback. Check it before a grant can consume a code.
    let ttl = match access_token_ttl_seconds() {
        Ok(ttl) => ttl,
        Err(message) => {
            return no_store(oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &message,
            ));
        }
    };
    let response = match request.grant_type.as_deref() {
        Some("authorization_code") => {
            exchange_authorization_code(&state, &headers, &request, ttl).await
        }
        Some("refresh_token") => exchange_refresh_token(&state, &headers, &request, ttl).await,
        _ => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "Unsupported grant_type.",
        ),
    };
    no_store(response)
}

async fn exchange_authorization_code(
    state: &AppState,
    headers: &HeaderMap,
    request: &TokenRequest,
    ttl: i64,
) -> Response {
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let client_id = request.client_id.clone().unwrap_or_default();
    let Some(client) = find_mcp_client_or_none(&pool, &client_id).await else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Unknown client.",
        );
    };
    let code_value = request.code.clone().unwrap_or_default();
    let Some(code) = find_usable_authorization_code_or_none(&pool, &code_value).await else {
        return invalid_grant("Authorization code is invalid or expired.");
    };
    if code.client_id != client.id {
        return invalid_grant("Authorization code was not issued to this client.");
    }
    if code.redirect_uri != request.redirect_uri.clone().unwrap_or_default() {
        return invalid_grant("redirect_uri does not match the authorization request.");
    }
    if !pkce_valid(
        &code.code_challenge,
        &request.code_verifier.clone().unwrap_or_default(),
    ) {
        return invalid_grant("PKCE verification failed.");
    }
    let user = match find_console_user(&pool, code.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) | Err(_) => return invalid_grant("Authorization code is invalid or expired."),
    };
    let consumed = match consume_authorization_code(&pool, code.id).await {
        Ok(consumed) => consumed,
        Err(error) => return ConsoleError::internal(error).into_response(),
    };
    if !consumed {
        return invalid_grant("Authorization code is invalid or expired.");
    }
    if user.status != ConsoleUserStatus::Active {
        if let Err(error) = revoke_refresh_tokens_for_user(&pool, user.id).await {
            return ConsoleError::internal(error).into_response();
        }
        return invalid_grant("User account is not active.");
    }
    let refresh = match create_refresh_token(
        &pool,
        client.id,
        user.id,
        code.principal_id,
        &code.resource,
        &code.scopes,
    )
    .await
    {
        Ok(refresh) => refresh,
        Err(error) => return ConsoleError::internal(error).into_response(),
    };
    if let Err(error) = touch_mcp_client_last_used(&pool, client.id).await {
        return ConsoleError::internal(error).into_response();
    }
    render_token_response(
        &pool,
        &client,
        &user,
        code.principal_id,
        &code.resource,
        &code.scopes,
        &refresh,
        headers,
        ttl,
    )
    .await
}

async fn exchange_refresh_token(
    state: &AppState,
    headers: &HeaderMap,
    request: &TokenRequest,
    ttl: i64,
) -> Response {
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let client_id = request.client_id.clone().unwrap_or_default();
    let Some(client) = find_mcp_client_or_none(&pool, &client_id).await else {
        return oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Unknown client.",
        );
    };
    let refresh_value = request.refresh_token.clone().unwrap_or_default();
    let Some(refresh) = find_usable_refresh_token_or_none(&pool, &refresh_value).await else {
        return invalid_grant("Refresh token is invalid or expired.");
    };
    if refresh.client_id != client.id {
        return invalid_grant("Refresh token was not issued to this client.");
    }
    let user = match find_console_user(&pool, refresh.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) | Err(_) => return invalid_grant("Refresh token is invalid or expired."),
    };
    if user.status != ConsoleUserStatus::Active {
        if let Err(error) = revoke_refresh_tokens_for_user(&pool, user.id).await {
            return ConsoleError::internal(error).into_response();
        }
        return invalid_grant("User account is not active.");
    }
    let rotated = match rotate_refresh_token(
        &pool,
        refresh.id,
        client.id,
        user.id,
        refresh.principal_id,
        &refresh.resource,
        &refresh.scopes,
    )
    .await
    {
        Ok(Some(token)) => token,
        Ok(None) => return invalid_grant("Refresh token is invalid or expired."),
        Err(error) => return ConsoleError::internal(error).into_response(),
    };
    if let Err(error) = touch_mcp_client_last_used(&pool, client.id).await {
        return ConsoleError::internal(error).into_response();
    }
    render_token_response(
        &pool,
        &client,
        &user,
        refresh.principal_id,
        &refresh.resource,
        &refresh.scopes,
        &rotated,
        headers,
        ttl,
    )
    .await
}

async fn find_mcp_client_or_none(pool: &sqlx::PgPool, client_id: &str) -> Option<McpClient> {
    find_mcp_client(pool, client_id).await.ok().flatten()
}

async fn find_usable_authorization_code_or_none(
    pool: &sqlx::PgPool,
    value: &str,
) -> Option<StoredAuthorizationCode> {
    find_usable_authorization_code(pool, value)
        .await
        .ok()
        .flatten()
}

async fn find_usable_refresh_token_or_none(
    pool: &sqlx::PgPool,
    value: &str,
) -> Option<StoredRefreshToken> {
    find_usable_refresh_token(pool, value).await.ok().flatten()
}

#[allow(clippy::too_many_arguments)]
async fn render_token_response(
    pool: &sqlx::PgPool,
    client: &McpClient,
    user: &ConsoleUser,
    principal_id: i64,
    resource: &str,
    scopes: &[String],
    refresh_token: &str,
    headers: &HeaderMap,
    ttl: i64,
) -> Response {
    let secret = match env_any(&["CENTAUR_JWT_SIGNING_SECRET"]) {
        Some(secret) => secret,
        None => {
            return oauth_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                "CENTAUR_JWT_SIGNING_SECRET is not configured",
            );
        }
    };
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let scope = scopes.join(" ");
    let foreign_id = match principal_foreign_id(pool, principal_id).await {
        Ok(Some(foreign_id)) => foreign_id,
        Ok(None) | Err(_) => {
            return ConsoleError::internal("the console principal is missing").into_response();
        }
    };
    let payload = AccessTokenClaims {
        iss: public_base_url(headers),
        sub: model::User::oid(user.id),
        aud: resource,
        exp: now + ttl,
        nbf: now - 5,
        iat: now,
        jti: format!("mcpjwt_{}", random_hex(16)),
        scope: &scope,
        client_id: client.public_client_id(),
        principal_id: model::Principal::oid(principal_id),
        principal_foreign_id: foreign_id,
        email: &user.email,
        name: user
            .name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| user.email.clone()),
    };
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_owned());
    let access_token = match encode(
        &header,
        &payload,
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(token) => token,
        Err(_) => {
            return oauth_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                "CENTAUR_JWT_SIGNING_SECRET is not configured",
            );
        }
    };
    server_page(
        axum::Json(TokenPayload {
            access_token,
            token_type: "Bearer",
            expires_in: ttl,
            scope,
            refresh_token: refresh_token.to_owned(),
        })
        .into_response(),
    )
}

/// The access token's claim set, with the fields `Mcp::Jwt.encode` receives.
#[derive(serde::Serialize)]
struct AccessTokenClaims<'a> {
    iss: String,
    sub: String,
    aud: &'a str,
    exp: i64,
    nbf: i64,
    iat: i64,
    jti: String,
    scope: &'a str,
    client_id: String,
    principal_id: String,
    principal_foreign_id: String,
    email: &'a str,
    name: String,
}

/// The token endpoint's RFC 6749 response.
#[derive(serde::Serialize)]
struct TokenPayload {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    scope: String,
    refresh_token: String,
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0; bytes];
    getrandom::fill(&mut buffer).expect("operating system randomness");
    hex::encode(buffer)
}

/// The access-token lifetime. The environment value is a strict positive
/// integer; an invalid value is a 400 from the token endpoint instead of
/// Ruby's `String#to_i` prefix, which read `"3600s"` as 3600 and fell back to
/// the default for anything non-positive.
fn access_token_ttl_seconds() -> Result<i64, String> {
    let Some(raw) = env_any(&["CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS"]).or_else(|| {
        console_env("MCP_ACCESS_TOKEN_TTL_SECONDS").filter(|value| !value.trim().is_empty())
    }) else {
        return Ok(ACCESS_TOKEN_TTL_SECONDS);
    };
    parse_access_token_ttl(&raw)
}

fn parse_access_token_ttl(raw: &str) -> Result<i64, String> {
    match raw.trim().parse::<i64>() {
        Ok(value) if value > 0 => Ok(value),
        _ => Err("CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS must be a positive integer".to_owned()),
    }
}

struct Authorization {
    client: McpClient,
    redirect_uri: String,
    resource: String,
    scopes: Vec<String>,
    code_challenge: String,
    state: Option<String>,
}

impl Authorization {
    fn hidden_fields(&self) -> Vec<(String, String)> {
        vec![
            ("response_type".to_owned(), "code".to_owned()),
            ("client_id".to_owned(), self.client.public_client_id()),
            ("redirect_uri".to_owned(), self.redirect_uri.clone()),
            ("scope".to_owned(), self.scopes.join(" ")),
            ("state".to_owned(), self.state.clone().unwrap_or_default()),
            ("resource".to_owned(), self.resource.clone()),
            ("code_challenge".to_owned(), self.code_challenge.clone()),
            ("code_challenge_method".to_owned(), "S256".to_owned()),
        ]
    }
}

// The checks in this function stay hand-written. Each failure is an OAuth error
// response that names an error code and, when the client is known and the
// redirect URI is registered, redirects to it; Rails stops at the first check it
// reaches, and one of them (`redirect_uri`) needs the stored client. garde's
// report carries neither an error code nor a redirect target and aggregates
// instead of stopping, so the rules that are expressible (`response_type` is
// `code`, `code_challenge_method` is `S256`, `code_challenge` is present, the
// scope list is supported) would only move behind a message-to-error-code table.
#[allow(clippy::result_large_err)]
async fn validate_authorization(
    pool: &sqlx::PgPool,
    params: &AuthorizationParams,
) -> Result<Authorization, Response> {
    let client_id = params.client_id.clone().unwrap_or_default();
    let Some(client) = find_mcp_client_or_none(pool, &client_id).await else {
        return Err(authorization_request_error(
            None,
            "invalid_request",
            "Unknown client.",
        ));
    };
    let redirect_uri = params.redirect_uri.clone().unwrap_or_default();
    if params.response_type.as_deref() != Some("code") {
        return Err(authorization_redirect(
            &client,
            &redirect_uri,
            params.state.as_deref(),
            "unsupported_response_type",
            "Only response_type=code is supported.",
        ));
    }
    if !client.redirect_uri_allowed(&redirect_uri) {
        return Err(authorization_redirect(
            &client,
            &redirect_uri,
            params.state.as_deref(),
            "invalid_request",
            "redirect_uri is not registered for this client.",
        ));
    }
    if params.code_challenge_method.as_deref() != Some("S256") {
        return Err(authorization_redirect(
            &client,
            &redirect_uri,
            params.state.as_deref(),
            "invalid_request",
            "code_challenge_method must be S256.",
        ));
    }
    let code_challenge = params.code_challenge.clone().unwrap_or_default();
    if code_challenge.trim().is_empty() {
        return Err(authorization_redirect(
            &client,
            &redirect_uri,
            params.state.as_deref(),
            "invalid_request",
            "code_challenge is required.",
        ));
    }
    let scopes = normalize_scope_param(params.scope.clone(), &DEFAULT_SCOPES);
    let unsupported = unsupported_values(&scopes, &DEFAULT_SCOPES);
    if !unsupported.is_empty() {
        return Err(authorization_redirect(
            &client,
            &redirect_uri,
            params.state.as_deref(),
            "invalid_scope",
            &format!("Unsupported scope: {}.", unsupported.join(" ")),
        ));
    }
    let resource = match resolve_requested_resource(params.resource.as_deref()) {
        Some(resource) => resource,
        None => {
            return Err(authorization_redirect(
                &client,
                &redirect_uri,
                params.state.as_deref(),
                "invalid_target",
                "resource is required.",
            ));
        }
    };
    Ok(Authorization {
        client,
        redirect_uri,
        resource,
        scopes,
        code_challenge,
        state: params.state.clone(),
    })
}

fn authorization_request_error(
    _client: Option<&McpClient>,
    _error: &str,
    description: &str,
) -> Response {
    plain_error(description)
}

fn authorization_redirect(
    client: &McpClient,
    redirect_uri: &str,
    state: Option<&str>,
    error: &str,
    description: &str,
) -> Response {
    if client.id > 0 && client.redirect_uri_allowed(redirect_uri) {
        let mut query = BTreeMap::new();
        if let Ok(url) = Url::parse(redirect_uri) {
            for (key, value) in url.query_pairs() {
                query.insert(key.into_owned(), value.into_owned());
            }
        }
        query.insert("error".to_owned(), error.to_owned());
        query.insert("error_description".to_owned(), description.to_owned());
        if let Some(state) = state.filter(|value| !value.trim().is_empty()) {
            query.insert("state".to_owned(), state.to_owned());
        }
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in &query {
            serializer.append_pair(key, value);
        }
        return redirect(
            &redirect_with_query(redirect_uri, &serializer.finish()),
            Vec::new(),
            Some(SERVER_PAGE_CSP),
        );
    }
    plain_error(description)
}

fn success_redirect(redirect_uri: &str, state: Option<&str>, code: &str) -> Response {
    let mut query = BTreeMap::new();
    if let Ok(url) = Url::parse(redirect_uri) {
        for (key, value) in url.query_pairs() {
            query.insert(key.into_owned(), value.into_owned());
        }
    }
    query.insert("code".to_owned(), code.to_owned());
    if let Some(state) = state.filter(|value| !value.trim().is_empty()) {
        query.insert("state".to_owned(), state.to_owned());
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in &query {
        serializer.append_pair(key, value);
    }
    redirect(
        &redirect_with_query(redirect_uri, &serializer.finish()),
        Vec::new(),
        Some(SERVER_PAGE_CSP),
    )
}

fn redirect_with_query(redirect_uri: &str, query: &str) -> String {
    // `URI#query=` replaces the query string but keeps the URI's original
    // shape, including a host-only URI with no trailing slash and any fragment.
    let (without_fragment, fragment) = match redirect_uri.find('#') {
        Some(index) => redirect_uri.split_at(index),
        None => (redirect_uri, ""),
    };
    let base = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(base, _)| base);
    format!("{base}?{query}{fragment}")
}

fn plain_error(message: &str) -> Response {
    let mut response = (StatusCode::BAD_REQUEST, message.to_owned()).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    server_page(response)
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    server_page(
        (
            status,
            axum::Json(OAuthErrorPayload {
                error,
                error_description: description,
            }),
        )
            .into_response(),
    )
}

/// RFC 6749 section 5.2's error body: this endpoint's wire contract, not
/// Console's `{"error":{"message":...}}` envelope.
#[derive(serde::Serialize)]
struct OAuthErrorPayload<'a> {
    error: &'a str,
    error_description: &'a str,
}

fn invalid_grant(description: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", description)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert("pragma", HeaderValue::from_static("no-cache"));
    response
}

/// A consent-form post must carry the configured Console public origin
/// verbatim. Unlike Rails' form origin check, a missing Origin and
/// `Origin: null` are both refused, matching the console JSON API.
fn origin_valid(headers: &HeaderMap, config: &ConsoleSessionConfig) -> bool {
    request_origin(headers).as_deref() == Some(config.public_origin())
}

fn csrf_rejection() -> Response {
    // Rails answered a consent forgery rejection with its static
    // `public/422.html`; the port renders the shared server-page error template
    // with the same 422 status.
    crate::console::pages::server_error_page(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Your request was rejected",
        "The consent form could not be verified. Go back and try again.",
    )
}

fn normalize_list_param(values: Vec<String>, default: &[&str]) -> Vec<String> {
    let values = values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        default.iter().map(|value| (*value).to_owned()).collect()
    } else {
        values
    }
}

fn normalize_scope_param(value: Option<String>, default: &[&str]) -> Vec<String> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return default.iter().map(|value| (*value).to_owned()).collect();
    };
    value
        .split(|character: char| character == ',' || character.is_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn pkce_valid(challenge: &str, verifier: &str) -> bool {
    if verifier.trim().is_empty() {
        return false;
    }
    let actual = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    actual.as_bytes().ct_eq(challenge.as_bytes()).unwrap_u8() == 1
}

fn resolve_requested_resource(requested: Option<&str>) -> Option<String> {
    let configured = normalize_mcp_resource_url(&configured_mcp_resource_url()?)?;
    // Rails uses `params[:resource].presence`: blank values defer to the
    // configured resource.
    if let Some(requested) = requested.filter(|value| !value.trim().is_empty())
        && normalize_mcp_resource_url(requested).as_deref() != Some(configured.as_str())
    {
        return None;
    }
    Some(configured)
}

fn configured_mcp_resource_url() -> Option<String> {
    env_any(&["CENTAUR_MCP_PUBLIC_URL"])
        .or_else(|| console_env("MCP_PUBLIC_URL").filter(|value| !value.trim().is_empty()))
}

fn normalize_mcp_resource_url(value: &str) -> Option<String> {
    let mut url = Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    let path = if path.ends_with("/mcp") {
        path
    } else {
        format!("{path}/mcp")
    };
    url.set_path(&path);
    let value = url.to_string();
    Some(value.strip_suffix('?').unwrap_or(&value).to_owned())
}

fn redirect_host(value: &str) -> String {
    match Url::parse(value) {
        Ok(url) => url
            .host_str()
            .map(|host| {
                // `URI#host` keeps the brackets Ruby's parser adds around IPv6
                // literals; `Url::host_str` returns the bare address.
                if host.contains(':') && !host.starts_with('[') {
                    format!("[{host}]")
                } else {
                    host.to_owned()
                }
            })
            .unwrap_or_default(),
        // `redirect_uri_host` falls back to the raw value only when URI.parse
        // raises; a valid URI without a host renders as a blank host.
        Err(_) => value.to_owned(),
    }
}

fn consent_html(
    client_name: &str,
    redirect_host: &str,
    resource: &str,
    scope: &str,
    email: &str,
    form_fields: &[(String, String)],
) -> String {
    let mut fields = String::new();
    for (name, value) in form_fields {
        fields.push_str(&format!(
            "<input type=\"hidden\" name=\"{}\" value=\"{}\">\n",
            escape_html(name),
            escape_html(value)
        ));
    }
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n  <head>\n    <meta charset=\"utf-8\">\n\
    <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
    <title>Authorize MCP Client · Centaur Console</title>\n\
    <meta name=\"color-scheme\" content=\"dark light\">\n\
    <meta name=\"theme-color\" content=\"#0a0a0c\" media=\"(prefers-color-scheme: dark)\">\n\
    <meta name=\"theme-color\" content=\"#ffffff\" media=\"(prefers-color-scheme: light)\">\n\
    <link rel=\"icon\" href=\"/icon.svg\" type=\"image/svg+xml\">\n\
    <link rel=\"stylesheet\" href=\"{css}\">\n  </head>\n\n  <body>\n\
    <main class=\"oauth-main\">\n      <div class=\"oauth-column\">\n\
<div class=\"oauth-header\">\n  <img src=\"{logo}\" alt=\"Centaur\" class=\"oauth-logo\" width=\"497\" height=\"127\">\n\
  <p class=\"oauth-subtitle\">Authorize MCP access.</p>\n</div>\n\n\
<section class=\"oauth-card\" aria-labelledby=\"oauth-client-name\">\n  <div>\n\
    <h1 id=\"oauth-client-name\" class=\"oauth-client-name\">{client}</h1>\n\
    <p class=\"oauth-client-host\">{host}</p>\n  </div>\n\n\
  <dl class=\"oauth-details\">\n    <div>\n      <dt>Resource</dt>\n      <dd>{resource}</dd>\n    </div>\n\
    <div>\n      <dt>Scope</dt>\n      <dd>{scope}</dd>\n    </div>\n\
    <div>\n      <dt>Signed in as</dt>\n      <dd>{email}</dd>\n    </div>\n  </dl>\n\n\
  <form class=\"oauth-form\" action=\"/mcp/oauth/authorize\" accept-charset=\"UTF-8\" method=\"post\">\n\
{fields}\n    <div class=\"oauth-actions\">\n\
      <button type=\"submit\" name=\"decision\" value=\"deny\" class=\"oauth-button oauth-button-outline\">Deny</button>\n\
      <button type=\"submit\" name=\"decision\" value=\"approve\" class=\"oauth-button oauth-button-primary\">Allow</button>\n\
    </div>\n  </form>\n</section>\n\
      </div>\n    </main>\n  </body>\n</html>\n",
        css = asset_path("oauth.css"),
        logo = asset_path("centaur-lockup-white.svg"),
        client = escape_html(client_name),
        host = escape_html(redirect_host),
        resource = escape_html(resource),
        scope = escape_html(scope),
        email = escape_html(email),
        fields = fields,
    )
}

fn asset_path(name: &str) -> String {
    assets::path(name).unwrap_or_else(|| format!("/assets/oauth-page/{name}"))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_console_db::validation::ValidateFields;

    #[test]
    fn access_token_ttl_is_a_strict_positive_integer() {
        assert_eq!(parse_access_token_ttl("3600"), Ok(3600));
        assert_eq!(parse_access_token_ttl(" 120 "), Ok(120));
        for invalid in ["3600s", "nope", "0", "-5", "1_000"] {
            let error = parse_access_token_ttl(invalid).unwrap_err();
            assert_eq!(
                error, "CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS must be a positive integer",
                "{invalid}"
            );
        }
    }

    #[test]
    fn resource_requires_the_configured_mcp_endpoint() {
        // `normalize_mcp_resource_url` is the fail-closed audience check.
        assert_eq!(
            normalize_mcp_resource_url("https://console.example/base/"),
            Some("https://console.example/base/mcp".to_owned())
        );
        assert_eq!(
            normalize_mcp_resource_url("https://console.example/mcp/?x=1"),
            Some("https://console.example/mcp?x=1".to_owned())
        );
        assert_eq!(normalize_mcp_resource_url("ftp://console.example"), None);
    }

    #[test]
    fn consent_page_host_matches_uri_host_semantics() {
        assert_eq!(
            redirect_host("https://console.example/cb"),
            "console.example"
        );
        assert_eq!(redirect_host("http://[::1]:8080/cb"), "[::1]");
        assert_eq!(redirect_host("cursor:/callback"), "");
        assert_eq!(redirect_host("not a uri"), "not a uri");
    }

    #[test]
    fn scopes_split_on_commas_and_spaces_and_default_when_blank() {
        assert_eq!(
            normalize_scope_param(Some("mcp:tools,other".to_owned()), &["mcp:tools"]),
            vec!["mcp:tools", "other"]
        );
        assert_eq!(
            normalize_scope_param(Some("  ".to_owned()), &["mcp:tools"]),
            vec!["mcp:tools"]
        );
    }

    fn registration_valid(body: serde_json::Value) -> bool {
        let request: RegisterRequest = serde_json::from_value(body).unwrap();
        request.validate_fields().is_ok()
    }

    #[test]
    fn registration_keeps_the_client_metadata_rules() {
        for accepted in [
            serde_json::json!({ "redirect_uris": ["https://example.test/cb"] }),
            serde_json::json!({
                "redirect_uris": ["http://127.0.0.1:9/cb"],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "scope": "mcp:tools",
            }),
        ] {
            assert!(registration_valid(accepted.clone()), "{accepted}");
        }
        for rejected in [
            serde_json::json!({ "redirect_uris": [] }),
            serde_json::json!({ "redirect_uris": ["", "  "] }),
            serde_json::json!({ "redirect_uris": ["http://evil.example.test/cb"] }),
            serde_json::json!({
                "redirect_uris": ["https://example.test/cb"],
                "grant_types": ["nope"],
            }),
            serde_json::json!({
                "redirect_uris": ["https://example.test/cb"],
                "response_types": ["token"],
            }),
            serde_json::json!({
                "redirect_uris": ["https://example.test/cb"],
                "scope": "other",
            }),
        ] {
            assert!(!registration_valid(rejected.clone()), "{rejected}");
        }
    }

    #[test]
    fn registration_normalizes_the_lists_before_validating() {
        let request: RegisterRequest = serde_json::from_value(serde_json::json!({
            "redirect_uris": [" http://127.0.0.1:9/cb "],
            "grant_types": [" authorization_code ", ""],
        }))
        .unwrap();
        assert!(request.validate_fields().is_ok());
        assert_eq!(request.redirect_uris, ["http://127.0.0.1:9/cb"]);
        assert_eq!(request.grant_types, ["authorization_code"]);
        // Absent lists and a blank scope take the model's defaults.
        assert_eq!(
            request.response_types,
            DEFAULT_RESPONSE_TYPES.map(str::to_owned)
        );
        assert_eq!(request.scope, DEFAULT_SCOPES.map(str::to_owned));
    }

    #[test]
    fn redirect_errors_keep_existing_query_and_sort_keys() {
        let response = authorization_redirect(
            &McpClient {
                id: 1,
                name: String::new(),
                redirect_uris: vec!["http://127.0.0.1/cb?z=9&a=1".to_owned()],
                grant_types: Vec::new(),
                response_types: Vec::new(),
                scopes: Vec::new(),
                metadata: Value::Null,
            },
            "http://127.0.0.1/cb?z=9&a=1",
            Some("state value"),
            "access_denied",
            "The user denied the authorization request.",
        );
        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert_eq!(
            location,
            "http://127.0.0.1/cb?a=1&error=access_denied&error_description=The+user+denied+the+authorization+request.&state=state+value&z=9"
        );
    }

    #[test]
    fn consent_origin_requires_an_exact_public_origin() {
        let config = ConsoleSessionConfig::from_settings(
            Some("test-only-console-session-secret-0000000000000000000000000000000000000000000000000000"),
            Some("https://console.example.test"),
        )
        .unwrap()
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "https://console.example.test".parse().unwrap(),
        );
        assert!(origin_valid(&headers, &config));
        assert!(!origin_valid(&HeaderMap::new(), &config));
        let mut null_origin = HeaderMap::new();
        null_origin.insert(axum::http::header::ORIGIN, "null".parse().unwrap());
        assert!(!origin_valid(&null_origin, &config));
        let mut cross_origin = HeaderMap::new();
        cross_origin.insert(
            axum::http::header::ORIGIN,
            "https://evil.example.test".parse().unwrap(),
        );
        assert!(!origin_valid(&cross_origin, &config));
    }

    #[test]
    fn consent_form_carries_the_authorization_fields() {
        let html = consent_html(
            "Client",
            "client.example",
            "https://console.example/mcp",
            "mcp:tools",
            "operator@example.test",
            &[("response_type".to_owned(), "code".to_owned())],
        );
        assert!(html.contains("action=\"/mcp/oauth/authorize\""));
        assert!(html.contains("name=\"response_type\" value=\"code\""));
        assert!(html.contains("name=\"decision\" value=\"deny\""));
        assert!(html.contains("name=\"decision\" value=\"approve\""));
    }
}
