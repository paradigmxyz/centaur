//! The MCP OAuth authorization server for public PKCE clients: metadata,
//! RFC 7591 client registration, the server-rendered consent page, and the
//! token endpoint (authorization-code and refresh-token grants).
//!
//! Authorization codes and refresh tokens are stored as SHA-256 hashes. A
//! code is single use, a refresh token rotates on every use (a replayed
//! token is `invalid_grant`), and an inactive user's tokens are revoked when
//! they are presented.

use askama::Template;
use axum::{
    body::Bytes,
    extract::{OriginalUri, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_auth::{AuthUser, csrf, random, return_path, tokens};
use centaur_console_db::{decode_oid, oid};
use chrono::{Duration, NaiveDateTime};
use http::{HeaderValue, StatusCode, header};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::request_base;
use crate::{
    ApiError, ApiResult, AppState,
    assets::Assets,
    auth::{Session, session_user, verify_form_csrf},
    extract::{Form, Query},
    pages::{redirect, render},
};

pub const DEFAULT_GRANT_TYPES: [&str; 2] = ["authorization_code", "refresh_token"];
pub const DEFAULT_RESPONSE_TYPES: [&str; 1] = ["code"];
pub const DEFAULT_SCOPES: [&str; 1] = ["mcp:tools"];
pub const ACCESS_TOKEN_TTL_SECONDS: i64 = 3600;
const CODE_TTL: Duration = Duration::minutes(10);
const REFRESH_TTL: Duration = Duration::days(90);

/// SHA-256 hex of a presented code or token.
pub fn hash_token(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn random_token(prefix: &str) -> String {
    format!(
        "{prefix}_{}",
        URL_SAFE_NO_PAD.encode(random::random_bytes::<48>())
    )
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        axum::Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

fn text_error(description: &str) -> Response {
    (StatusCode::BAD_REQUEST, description.to_owned()).into_response()
}

// --- redirect URIs ----------------------------------------------------------------

/// Schemes a browser must never be sent to as an "app" redirect.
const FORBIDDEN_SCHEMES: [&str; 6] = ["javascript", "data", "vbscript", "file", "about", "blob"];

fn loopback_host(host: url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => {
            address.is_loopback() || address.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// A redirect URI a public client may register (RFC 8252): `https` with a
/// host, `http` on a loopback host, or a private-use scheme; never with a
/// fragment or a wildcard.
pub fn allowed_redirect_uri(value: &str) -> bool {
    let Ok(uri) = url::Url::parse(value) else {
        return false;
    };
    if uri.fragment().is_some() || value.contains('*') {
        return false;
    }
    match uri.scheme() {
        "https" => uri.host_str().is_some_and(|host| !host.is_empty()),
        "http" => uri.host().is_some_and(loopback_host),
        scheme => !FORBIDDEN_SCHEMES.contains(&scheme),
    }
}

/// Native apps listen on a free loopback port, so a registered loopback URI
/// without a port matches any port with the same path and query.
fn loopback_match(registered: &str, requested: &url::Url) -> bool {
    let Ok(registered) = url::Url::parse(registered) else {
        return false;
    };
    registered.scheme() == "http"
        && requested.scheme() == "http"
        && registered.host().is_some_and(loopback_host)
        && requested.host().is_some_and(loopback_host)
        && registered.port().is_none()
        && registered.path() == requested.path()
        && registered.query() == requested.query()
}

/// Whether `requested` is one of the client's registered redirect URIs.
pub fn redirect_uri_allowed(registered: &[String], requested: &str) -> bool {
    if !allowed_redirect_uri(requested) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(requested) else {
        return false;
    };
    registered.iter().any(|candidate| {
        allowed_redirect_uri(candidate)
            && (candidate == requested || loopback_match(candidate, &parsed))
    })
}

/// `redirect_uri` with `extra` parameters added to (or replacing those in)
/// its query.
fn redirect_with(redirect_uri: &str, extra: &[(&str, &str)]) -> Option<String> {
    let mut uri = url::Url::parse(redirect_uri).ok()?;
    let kept: Vec<(String, String)> = uri
        .query_pairs()
        .filter(|(key, _)| !extra.iter().any(|(name, _)| key == name))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    uri.set_query(None);
    uri.query_pairs_mut()
        .extend_pairs(kept)
        .extend_pairs(extra.iter().copied());
    Some(uri.into())
}

// --- clients ----------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Client {
    id: i64,
    name: Option<String>,
    redirect_uris: Value,
}

impl Client {
    fn redirect_uris(&self) -> Vec<String> {
        serde_json::from_value(self.redirect_uris.clone()).unwrap_or_default()
    }

    fn oid(&self) -> String {
        oid("moc", self.id)
    }
}

async fn find_client(
    state: &AppState,
    client_id: Option<&str>,
) -> Result<Option<Client>, ApiError> {
    let Some(id) = client_id.and_then(|id| decode_oid("moc", id)) else {
        return Ok(None);
    };
    Ok(
        sqlx::query_as("SELECT id, name, redirect_uris FROM mcp_oauth_clients WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?,
    )
}

async fn touch_client(state: &AppState, client_id: i64) -> Result<(), ApiError> {
    let now = state.now().naive_utc();
    sqlx::query("UPDATE mcp_oauth_clients SET last_used_at = $2, updated_at = $2 WHERE id = $1")
        .bind(client_id)
        .bind(now)
        .execute(&state.db)
        .await?;
    Ok(())
}

// --- metadata and registration --------------------------------------------------------

/// `GET /.well-known/oauth-authorization-server` (and
/// `/.well-known/openid-configuration`).
pub async fn metadata(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    let base = request_base(&state, &headers);
    let url = |path: &str| super::join(&base, path);
    Ok(axum::Json(json!({
        "issuer": base,
        "authorization_endpoint": url("/mcp/oauth/authorize")?,
        "token_endpoint": url("/mcp/oauth/token")?,
        "registration_endpoint": url("/mcp/oauth/register")?,
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp:tools"],
        "resource_parameter_supported": true,
    }))
    .into_response())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Registration {
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    scope: Option<String>,
    client_uri: Option<Value>,
    logo_uri: Option<Value>,
    contacts: Option<Value>,
    software_id: Option<Value>,
    software_version: Option<Value>,
}

fn or_default(values: Vec<String>, default: &[&str]) -> Vec<String> {
    let values: Vec<String> = values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect();
    if values.is_empty() {
        default.iter().map(|value| (*value).to_owned()).collect()
    } else {
        values
    }
}

fn unsupported(values: &[String], allowed: &[&str]) -> Vec<String> {
    values
        .iter()
        .filter(|value| !allowed.contains(&value.as_str()))
        .cloned()
        .collect()
}

/// `POST /mcp/oauth/register`: RFC 7591 registration of a public PKCE
/// client.
pub async fn register(State(state): State<AppState>, body: Bytes) -> Response {
    let invalid = |description: &str| {
        oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            description,
        )
    };
    let Ok(request) = serde_json::from_slice::<Registration>(&body) else {
        return invalid("The registration request is not valid JSON client metadata.");
    };
    let redirect_uris: Vec<String> = request
        .redirect_uris
        .iter()
        .map(|uri| uri.trim().to_owned())
        .filter(|uri| !uri.is_empty())
        .collect();
    let grant_types = or_default(request.grant_types, &DEFAULT_GRANT_TYPES);
    let response_types = or_default(request.response_types, &DEFAULT_RESPONSE_TYPES);
    let scopes: Vec<String> = match request
        .scope
        .as_deref()
        .filter(|scope| !scope.trim().is_empty())
    {
        Some(scope) => scope
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect(),
        None => DEFAULT_SCOPES
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect(),
    };
    let mut problems = Vec::new();
    if redirect_uris.is_empty() {
        problems.push("redirect_uris must not be empty".to_owned());
    }
    for uri in redirect_uris
        .iter()
        .filter(|uri| !allowed_redirect_uri(uri))
    {
        problems.push(format!(
            "redirect_uris: {uri:?} is not an allowed public-client redirect URI"
        ));
    }
    for (name, values, allowed) in [
        ("grant_types", &grant_types, &DEFAULT_GRANT_TYPES[..]),
        (
            "response_types",
            &response_types,
            &DEFAULT_RESPONSE_TYPES[..],
        ),
        ("scopes", &scopes, &DEFAULT_SCOPES[..]),
    ] {
        let bad = unsupported(values, allowed);
        if !bad.is_empty() {
            problems.push(format!(
                "{name} contains unsupported values: {}",
                bad.join(", ")
            ));
        }
    }
    if !problems.is_empty() {
        return invalid(&problems.join("; "));
    }
    let mut metadata = Map::new();
    for (key, value) in [
        ("client_uri", request.client_uri),
        ("logo_uri", request.logo_uri),
        ("contacts", request.contacts),
        ("software_id", request.software_id),
        ("software_version", request.software_version),
    ] {
        if let Some(value) = value.filter(|value| !value.is_null()) {
            metadata.insert(key.into(), value);
        }
    }
    let name = request
        .client_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "MCP client".into());
    let now = state.now().naive_utc();
    let id: Result<i64, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO mcp_oauth_clients (name, redirect_uris, grant_types, response_types, scopes, \
         metadata, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $7) RETURNING id",
    )
    .bind(&name)
    .bind(json!(redirect_uris))
    .bind(json!(grant_types))
    .bind(json!(response_types))
    .bind(json!(scopes))
    .bind(Value::Object(metadata))
    .bind(now)
    .fetch_one(&state.db)
    .await;
    match id {
        Ok(id) => (
            StatusCode::CREATED,
            axum::Json(json!({
                "client_id": oid("moc", id),
                "client_name": name,
                "redirect_uris": redirect_uris,
                "grant_types": grant_types,
                "response_types": response_types,
                "scope": scopes.join(" "),
                "token_endpoint_auth_method": "none",
            })),
        )
            .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

// --- authorization --------------------------------------------------------------------

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    resource: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    decision: Option<String>,
    authenticity_token: Option<String>,
}

impl AuthorizeParams {
    fn get(field: &Option<String>) -> &str {
        field.as_deref().unwrap_or_default()
    }
}

/// An authorization request that passed every check.
struct Authorization {
    client: Client,
    scopes: Vec<String>,
    resource: String,
}

/// Back to a trusted redirect URI with the error, else the description as a
/// plain 400 (an untrusted URI is never redirected to).
fn authorization_error(
    client: Option<&Client>,
    params: &AuthorizeParams,
    error: &str,
    description: &str,
) -> Response {
    let redirect_uri = AuthorizeParams::get(&params.redirect_uri);
    if let Some(client) = client
        && redirect_uri_allowed(&client.redirect_uris(), redirect_uri)
    {
        let mut extra = vec![("error", error), ("error_description", description)];
        if let Some(state) = params.state.as_deref().filter(|state| !state.is_empty()) {
            extra.push(("state", state));
        }
        if let Some(location) = redirect_with(redirect_uri, &extra) {
            return redirect(&location);
        }
    }
    text_error(description)
}

/// `value` as an MCP resource URL: an `http(s)` URL without a fragment whose
/// path ends in `/mcp`.
pub fn normalize_resource(value: &str) -> Option<String> {
    let mut uri = url::Url::parse(value.trim()).ok()?;
    if !matches!(uri.scheme(), "http" | "https") || uri.host_str().is_none_or(str::is_empty) {
        return None;
    }
    uri.set_fragment(None);
    let path = uri.path().trim_end_matches('/').to_owned();
    uri.set_path(&if path.ends_with("/mcp") {
        path
    } else {
        format!("{path}/mcp")
    });
    if uri.query() == Some("") {
        uri.set_query(None);
    }
    Some(uri.into())
}

/// The configured MCP resource; a request may only name that one. Fails
/// closed when none is configured.
fn resolve_resource(state: &AppState, requested: Option<&str>) -> Option<String> {
    let env = &state.config.env;
    let configured = env
        .var_present("CENTAUR_MCP_PUBLIC_URL")
        .or_else(|| env.present("MCP_PUBLIC_URL"))
        .and_then(normalize_resource)?;
    match requested.filter(|requested| !requested.trim().is_empty()) {
        Some(requested)
            if normalize_resource(requested).as_deref() != Some(configured.as_str()) =>
        {
            None
        }
        _ => Some(configured),
    }
}

async fn validate(state: &AppState, params: &AuthorizeParams) -> Result<Authorization, Response> {
    let client = find_client(state, params.client_id.as_deref())
        .await
        .map_err(IntoResponse::into_response)?;
    let Some(client) = client else {
        return Err(authorization_error(
            None,
            params,
            "invalid_request",
            "Unknown client.",
        ));
    };
    let fail = |error: &str, description: &str| {
        Err(authorization_error(
            Some(&client),
            params,
            error,
            description,
        ))
    };
    if params.response_type.as_deref() != Some("code") {
        return fail(
            "unsupported_response_type",
            "Only response_type=code is supported.",
        );
    }
    if !redirect_uri_allowed(
        &client.redirect_uris(),
        AuthorizeParams::get(&params.redirect_uri),
    ) {
        return fail(
            "invalid_request",
            "redirect_uri is not registered for this client.",
        );
    }
    if params.code_challenge_method.as_deref() != Some("S256") {
        return fail("invalid_request", "code_challenge_method must be S256.");
    }
    if AuthorizeParams::get(&params.code_challenge)
        .trim()
        .is_empty()
    {
        return fail("invalid_request", "code_challenge is required.");
    }
    let scopes: Vec<String> = match params
        .scope
        .as_deref()
        .filter(|scope| !scope.trim().is_empty())
    {
        Some(scope) => scope
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect(),
        None => DEFAULT_SCOPES
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect(),
    };
    let bad = unsupported(&scopes, &DEFAULT_SCOPES);
    if !bad.is_empty() {
        return fail(
            "invalid_scope",
            &format!("Unsupported scope: {}.", bad.join(" ")),
        );
    }
    let Some(resource) = resolve_resource(state, params.resource.as_deref()) else {
        return fail("invalid_target", "resource is required.");
    };
    Ok(Authorization {
        client,
        scopes,
        resource,
    })
}

#[derive(Template)]
#[template(path = "pages/mcp_authorize.html")]
struct ConsentPage<'a> {
    assets: &'a Assets,
    client_name: &'a str,
    redirect_host: String,
    resource: &'a str,
    scope: String,
    email: &'a str,
    csrf: String,
    fields: Vec<(&'static str, String)>,
}

/// `GET /mcp/oauth/authorize`: the consent page.
pub async fn authorize(
    State(state): State<AppState>,
    session: Session,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<AuthorizeParams>,
) -> ApiResult {
    let Some(user) = session_user(&state, &session).await? else {
        if let Some(path) = uri
            .path_and_query()
            .and_then(|target| return_path::safe(target.as_str()))
        {
            session.lock().set_return_to(path);
        }
        return Ok(redirect("/login"));
    };
    if !user.is_active() {
        return Ok(redirect("/pending"));
    }
    let authorization = match validate(&state, &params).await {
        Ok(authorization) => authorization,
        Err(response) => return Ok(response),
    };
    let redirect_uri = AuthorizeParams::get(&params.redirect_uri);
    let redirect_host = url::Url::parse(redirect_uri)
        .ok()
        .and_then(|uri| uri.host_str().map(str::to_owned))
        .unwrap_or_default();
    let scope = authorization.scopes.join(" ");
    let fields = vec![
        ("response_type", "code".to_owned()),
        ("client_id", authorization.client.oid()),
        ("redirect_uri", redirect_uri.to_owned()),
        ("scope", scope.clone()),
        ("state", AuthorizeParams::get(&params.state).to_owned()),
        ("resource", authorization.resource.clone()),
        (
            "code_challenge",
            AuthorizeParams::get(&params.code_challenge).to_owned(),
        ),
        ("code_challenge_method", "S256".to_owned()),
    ];
    let csrf = session.lock().csrf_token().to_owned();
    Ok(render(
        StatusCode::OK,
        &ConsentPage {
            assets: &state.assets,
            client_name: authorization.client.name.as_deref().unwrap_or_default(),
            redirect_host,
            resource: &authorization.resource,
            scope,
            email: &user.email,
            csrf,
            fields,
        },
    ))
}

/// `POST /mcp/oauth/authorize`: the consent decision.
pub async fn approve(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Form(params): Form<AuthorizeParams>,
) -> ApiResult {
    verify_form_csrf(
        &state,
        &session,
        &headers,
        params.authenticity_token.as_deref(),
    )?;
    let Some(user) = session_user(&state, &session).await? else {
        return Ok(redirect("/login"));
    };
    if !user.is_active() {
        return Ok(redirect("/pending"));
    }
    let authorization = match validate(&state, &params).await {
        Ok(authorization) => authorization,
        Err(response) => return Ok(response),
    };
    if params.decision.as_deref() != Some("approve") {
        return Ok(authorization_error(
            Some(&authorization.client),
            &params,
            "access_denied",
            "The user denied the authorization request.",
        ));
    }
    issue_code(&state, &params, &user, &authorization).await
}

async fn issue_code(
    state: &AppState,
    params: &AuthorizeParams,
    user: &AuthUser,
    authorization: &Authorization,
) -> ApiResult {
    let principal_id = crate::principals::provision::provision(
        state,
        &crate::principals::provision::ConsoleUser {
            id: user.id,
            email: &user.email,
            name: user.name.as_deref(),
        },
    )
    .await?;
    let code = random_token("mcpauth");
    let now = state.now().naive_utc();
    let redirect_uri = AuthorizeParams::get(&params.redirect_uri);
    sqlx::query(
        "INSERT INTO mcp_oauth_authorization_codes (code_hash, code_challenge, mcp_oauth_client_id, \
         user_id, principal_id, redirect_uri, resource, scopes, expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
    )
    .bind(hash_token(&code))
    .bind(AuthorizeParams::get(&params.code_challenge))
    .bind(authorization.client.id)
    .bind(user.id)
    .bind(principal_id)
    .bind(redirect_uri)
    .bind(&authorization.resource)
    .bind(json!(authorization.scopes))
    .bind(now + CODE_TTL)
    .bind(now)
    .execute(&state.db)
    .await?;
    touch_client(state, authorization.client.id).await?;
    let mut extra = vec![("code", code.as_str())];
    if let Some(state_param) = params.state.as_deref().filter(|state| !state.is_empty()) {
        extra.push(("state", state_param));
    }
    let location = redirect_with(redirect_uri, &extra)
        .ok_or_else(|| ApiError::internal("redirect_uri did not parse"))?;
    Ok(redirect(&location))
}

// --- token endpoint -------------------------------------------------------------------

#[derive(Default, Deserialize)]
#[serde(default)]
struct TokenParams {
    grant_type: Option<String>,
    client_id: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

/// Token responses, errors included, are never cached (RFC 6749 §5.1). The
/// metadata, registration, and consent routes get the same treatment.
pub(crate) fn no_store(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

/// `POST /mcp/oauth/token`.
pub async fn token(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let issuer = request_base(&state, &headers);
    let Ok(params) = serde_urlencoded::from_bytes::<TokenParams>(&body) else {
        return no_store(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Malformed request body.",
        ));
    };
    let result = match params.grant_type.as_deref() {
        Some("authorization_code") => exchange_code(&state, &params, &issuer).await,
        Some("refresh_token") => exchange_refresh_token(&state, &params, &issuer).await,
        _ => Ok(oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "Unsupported grant_type.",
        )),
    };
    no_store(result.unwrap_or_else(IntoResponse::into_response))
}

/// RFC 7636 S256: `BASE64URL(SHA256(verifier)) == challenge`.
pub fn pkce_valid(challenge: &str, verifier: &str) -> bool {
    !verifier.trim().is_empty()
        && csrf::eq(
            &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            challenge,
        )
}

fn invalid_grant(description: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", description)
}

/// What a grant was claimed for.
struct Grant {
    user_id: i64,
    principal_id: i64,
    resource: String,
    scopes: Value,
}

enum Outcome {
    Invalid,
    Inactive,
    Issued(String),
}

async fn user_active(conn: &mut sqlx::PgConnection, user_id: i64) -> Result<bool, ApiError> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(conn)
        .await?;
    Ok(status.as_deref() == Some("active"))
}

/// Revokes every usable refresh token of the user.
async fn revoke_user_tokens(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
    now: NaiveDateTime,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE mcp_oauth_refresh_tokens SET revoked_at = $2, updated_at = $2 \
         WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > $2",
    )
    .bind(user_id)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

async fn issue_refresh_token(
    conn: &mut sqlx::PgConnection,
    now: NaiveDateTime,
    client_id: i64,
    grant: &Grant,
) -> Result<String, ApiError> {
    let token = random_token("mcprt");
    sqlx::query(
        "INSERT INTO mcp_oauth_refresh_tokens (token_hash, mcp_oauth_client_id, user_id, principal_id, \
         resource, scopes, expires_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(hash_token(&token))
    .bind(client_id)
    .bind(grant.user_id)
    .bind(grant.principal_id)
    .bind(&grant.resource)
    .bind(&grant.scopes)
    .bind(now + REFRESH_TTL)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(token)
}

#[derive(sqlx::FromRow)]
struct CodeRow {
    id: i64,
    mcp_oauth_client_id: i64,
    redirect_uri: String,
    code_challenge: String,
    user_id: i64,
    principal_id: i64,
    resource: String,
    scopes: Value,
}

async fn exchange_code(
    state: &AppState,
    params: &TokenParams,
    issuer: &str,
) -> Result<Response, ApiError> {
    let Some(client) = find_client(state, params.client_id.as_deref()).await? else {
        return Ok(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Unknown client.",
        ));
    };
    let now = state.now().naive_utc();
    let hash = hash_token(params.code.as_deref().unwrap_or_default());
    let code: Option<CodeRow> = sqlx::query_as(
        "SELECT id, mcp_oauth_client_id, redirect_uri, code_challenge, user_id, principal_id, resource, \
         scopes FROM mcp_oauth_authorization_codes \
         WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > $2",
    )
    .bind(&hash)
    .bind(now)
    .fetch_optional(&state.db)
    .await?;
    let invalid = "Authorization code is invalid or expired.";
    let Some(code) = code else {
        return Ok(invalid_grant(invalid));
    };
    if code.mcp_oauth_client_id != client.id {
        return Ok(invalid_grant(
            "Authorization code was not issued to this client.",
        ));
    }
    if code.redirect_uri != params.redirect_uri.as_deref().unwrap_or_default() {
        return Ok(invalid_grant(
            "redirect_uri does not match the authorization request.",
        ));
    }
    if !pkce_valid(
        &code.code_challenge,
        params.code_verifier.as_deref().unwrap_or_default(),
    ) {
        return Ok(invalid_grant("PKCE verification failed."));
    }
    ensure_signing_secret(state)?;
    let mut tx = state.db.begin().await?;
    // Single use: only one request can flip `consumed_at`.
    let claimed = sqlx::query(
        "UPDATE mcp_oauth_authorization_codes SET consumed_at = $2, updated_at = $2 \
         WHERE id = $1 AND consumed_at IS NULL AND expires_at > $2",
    )
    .bind(code.id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let grant = Grant {
        user_id: code.user_id,
        principal_id: code.principal_id,
        resource: code.resource,
        scopes: code.scopes,
    };
    let outcome = if claimed.rows_affected() == 0 {
        Outcome::Invalid
    } else if !user_active(&mut tx, grant.user_id).await? {
        revoke_user_tokens(&mut tx, grant.user_id, now).await?;
        Outcome::Inactive
    } else {
        Outcome::Issued(issue_refresh_token(&mut tx, now, client.id, &grant).await?)
    };
    tx.commit().await?;
    finish(state, issuer, &client, &grant, outcome, invalid).await
}

#[derive(sqlx::FromRow)]
struct RefreshRow {
    id: i64,
    mcp_oauth_client_id: i64,
}

async fn exchange_refresh_token(
    state: &AppState,
    params: &TokenParams,
    issuer: &str,
) -> Result<Response, ApiError> {
    let Some(client) = find_client(state, params.client_id.as_deref()).await? else {
        return Ok(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Unknown client.",
        ));
    };
    let now = state.now().naive_utc();
    let hash = hash_token(params.refresh_token.as_deref().unwrap_or_default());
    let token: Option<RefreshRow> = sqlx::query_as(
        "SELECT id, mcp_oauth_client_id FROM mcp_oauth_refresh_tokens \
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > $2",
    )
    .bind(&hash)
    .bind(now)
    .fetch_optional(&state.db)
    .await?;
    let invalid = "Refresh token is invalid or expired.";
    let Some(token) = token else {
        return Ok(invalid_grant(invalid));
    };
    if token.mcp_oauth_client_id != client.id {
        return Ok(invalid_grant(
            "Refresh token was not issued to this client.",
        ));
    }
    ensure_signing_secret(state)?;
    let mut tx = state.db.begin().await?;
    // Rotation: only one request can revoke the token; a replay finds it
    // revoked.
    let claimed: Option<(i64, i64, String, Value)> = sqlx::query_as(
        "UPDATE mcp_oauth_refresh_tokens SET revoked_at = $2, last_used_at = $2, updated_at = $2 \
         WHERE id = $1 AND revoked_at IS NULL AND expires_at > $2 \
         RETURNING user_id, principal_id, resource, scopes",
    )
    .bind(token.id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((user_id, principal_id, resource, scopes)) = claimed else {
        tx.rollback().await?;
        return Ok(invalid_grant(invalid));
    };
    let grant = Grant {
        user_id,
        principal_id,
        resource,
        scopes,
    };
    let outcome = if !user_active(&mut tx, grant.user_id).await? {
        revoke_user_tokens(&mut tx, grant.user_id, now).await?;
        Outcome::Inactive
    } else {
        Outcome::Issued(issue_refresh_token(&mut tx, now, client.id, &grant).await?)
    };
    tx.commit().await?;
    finish(state, issuer, &client, &grant, outcome, invalid).await
}

fn ensure_signing_secret(state: &AppState) -> Result<(), ApiError> {
    if state.config.jwt_signing_secret.is_empty() {
        Err(ApiError::internal(
            "CENTAUR_JWT_SIGNING_SECRET is not configured",
        ))
    } else {
        Ok(())
    }
}

async fn finish(
    state: &AppState,
    issuer: &str,
    client: &Client,
    grant: &Grant,
    outcome: Outcome,
    invalid: &str,
) -> Result<Response, ApiError> {
    let refresh_token = match outcome {
        Outcome::Invalid => return Ok(invalid_grant(invalid)),
        Outcome::Inactive => return Ok(invalid_grant("User account is not active.")),
        Outcome::Issued(token) => token,
    };
    touch_client(state, client.id).await?;
    token_response(state, issuer, client, grant, &refresh_token).await
}

fn access_token_ttl(state: &AppState) -> i64 {
    let env = &state.config.env;
    env.var_present("CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS")
        .or_else(|| env.present("MCP_ACCESS_TOKEN_TTL_SECONDS"))
        .and_then(|seconds| seconds.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(ACCESS_TOKEN_TTL_SECONDS)
}

async fn token_response(
    state: &AppState,
    issuer: &str,
    client: &Client,
    grant: &Grant,
    refresh_token: &str,
) -> Result<Response, ApiError> {
    let (email, name): (String, Option<String>) =
        sqlx::query_as("SELECT email, name FROM users WHERE id = $1")
            .bind(grant.user_id)
            .fetch_one(&state.db)
            .await?;
    let principal_foreign_id: Option<String> =
        sqlx::query_scalar("SELECT foreign_id FROM principals WHERE id = $1")
            .bind(grant.principal_id)
            .fetch_one(&state.db)
            .await?;
    let now = state.now().timestamp();
    let ttl = access_token_ttl(state);
    let scope = grant
        .scopes
        .as_array()
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let claims = json!({
        "iss": issuer,
        "sub": oid("usr", grant.user_id),
        "aud": grant.resource,
        "exp": now + ttl,
        "nbf": now - 5,
        "iat": now,
        "jti": format!("mcpjwt_{}", random::hex16()),
        "scope": scope,
        "client_id": client.oid(),
        "principal_id": oid("prn", grant.principal_id),
        "principal_foreign_id": principal_foreign_id,
        "email": email,
        "name": name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| email.clone()),
    });
    let access_token = match tokens::mcp_access(&state.config.jwt_signing_secret, &claims) {
        Ok(token) => token,
        Err(_) => {
            return Ok(oauth_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                "The token signing key is not configured.",
            ));
        }
    };
    Ok(axum::Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ttl,
        "scope": scope,
        "refresh_token": refresh_token,
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_only_public_client_redirects() {
        for uri in [
            "https://claude.ai/api/mcp/auth_callback",
            "http://localhost:49152/callback",
            "http://127.0.0.1:49152/callback",
            "http://127.1.2.3:49152/callback",
            "http://[::1]:49152/callback",
            "cursor://anysphere.cursor-mcp/oauth/callback",
            "vscode://mcp/callback",
            "com.example.app:/oauth2redirect",
        ] {
            assert!(allowed_redirect_uri(uri), "{uri}");
        }
        for uri in [
            "http://127.evil.com/callback",
            "http://127.0.0.1.evil.com/callback",
            "http://localhost.evil.com/callback",
            "http://example.com/callback",
            "/oauth/callback",
            "callback",
            "https://*.example.com/callback",
            "https://example.com/callback#token",
            "cursor://anysphere.cursor-mcp/oauth/callback#token",
            "javascript:alert(1)",
            "data:text/html,x",
        ] {
            assert!(!allowed_redirect_uri(uri), "{uri}");
        }
    }

    #[test]
    fn loopback_redirects_match_any_port_when_registered_without_one() {
        let registered =
            |uris: &[&str]| uris.iter().map(|uri| (*uri).to_owned()).collect::<Vec<_>>();
        let cursor = registered(&["cursor://anysphere.cursor-mcp/oauth/callback"]);
        assert!(redirect_uri_allowed(
            &cursor,
            "cursor://anysphere.cursor-mcp/oauth/callback"
        ));
        assert!(!redirect_uri_allowed(
            &cursor,
            "cursor://anysphere.cursor-mcp/oauth/callback/extra"
        ));
        assert!(!redirect_uri_allowed(
            &cursor,
            "cursor://evil/oauth/callback"
        ));
        let localhost = registered(&["http://localhost/callback"]);
        assert!(redirect_uri_allowed(
            &localhost,
            "http://localhost:49153/callback"
        ));
        assert!(redirect_uri_allowed(
            &localhost,
            "http://127.0.0.1:1/callback"
        ));
        assert!(!redirect_uri_allowed(
            &localhost,
            "http://localhost:49153/other"
        ));
        assert!(!redirect_uri_allowed(
            &localhost,
            "http://127.evil.com/callback"
        ));
        let fixed = registered(&["http://localhost:8080/callback"]);
        assert!(!redirect_uri_allowed(
            &fixed,
            "http://localhost:49153/callback"
        ));
        assert!(redirect_uri_allowed(
            &fixed,
            "http://localhost:8080/callback"
        ));
    }

    #[test]
    fn redirects_keep_the_query_and_replace_protocol_parameters() {
        let location = redirect_with(
            "https://app.example/cb?tenant=acme&state=old",
            &[("code", "a b"), ("state", "new")],
        )
        .unwrap();
        assert_eq!(
            location,
            "https://app.example/cb?tenant=acme&code=a+b&state=new"
        );
        assert!(redirect_with("not a url", &[]).is_none());
    }

    #[test]
    fn normalizes_resource_urls() {
        assert_eq!(
            normalize_resource("http://localhost:3000/").as_deref(),
            Some("http://localhost:3000/mcp")
        );
        assert_eq!(
            normalize_resource("https://host:443/mcp/?#x").as_deref(),
            Some("https://host/mcp")
        );
        assert_eq!(
            normalize_resource("https://host/a/mcp").as_deref(),
            Some("https://host/a/mcp")
        );
        assert_eq!(normalize_resource("ftp://host/mcp"), None);
        assert_eq!(normalize_resource("nope"), None);
    }

    #[test]
    fn pkce_compares_the_s256_challenge() {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"test-code-verifier"));
        assert!(pkce_valid(&challenge, "test-code-verifier"));
        assert!(!pkce_valid(&challenge, "other"));
        assert!(!pkce_valid("x", ""));
    }
}
