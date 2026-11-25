//! The server-rendered OAuth consent flow: `/oauth/:slug/start` and
//! `/oauth/:slug/callback`.
//!
//! The signed state and the browser-binding cookie are carried in the same
//! AES-256-GCM envelope as the Console session cookie, so both halves share one
//! secret without introducing a second key. The state and cookie are
//! flow-scoped: the state is bound to the app, user, scopes, nonce, and the
//! app's provider-config digest, and expires after ten minutes.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{OriginalUri, Path, State},
    http::{
        HeaderMap, HeaderValue, StatusCode, Uri,
        header::{COOKIE, LOCATION, SET_COOKIE},
    },
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_db::{
    ConsoleUser, ConsoleUserStatus,
    areas::oauth::{
        DEFAULT_EARLY_REFRESH_FRACTION, DEFAULT_EARLY_REFRESH_SLACK_SECONDS,
        DEFAULT_MAX_REFRESH_INTERVAL_SECONDS, FlowCredentialWrite, NewFlowCredential,
        OauthApp as DbOauthApp, OauthEncryption, WrappingRule, WrappingSecretSpec,
        compute_next_attempt_at, credential_expires_at, credential_for_app_subject,
        first_credential_for_app, lock_oauth_app_row, upsert_flow_credential,
        upsert_wrapping_secret, url_safe,
    },
    find_console_user,
    model::{ConsoleModel, OauthApp as OauthAppModel, User as UserModel},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Digest;
use url::Url;

use super::{
    http,
    providers::{CodeExchangeResult, ExchangeError, Identity, Provider, SLACK_KEY, tool_label},
};
use crate::{
    AppState,
    console::{
        LOGIN_PATH, PENDING_PATH,
        pages::{login_error, server_page_csp},
        request::ConsoleQuery,
        return_path,
        session_cookie::ConsoleSession,
    },
    console_session::ConsoleSessionConfig,
};

const FLOW_TTL_SECONDS: i64 = 10 * 60;
const FLOW_COOKIE: &str = "oauth_flow";
/// `/oauth/:slug/start?scopes=`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct StartQuery {
    #[serde(default)]
    scopes: Option<String>,
}

/// `/oauth/:slug/callback?code=&state=&error=`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct CallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// The authenticated browser context the flow pages operate on.
struct PageContext {
    config: ConsoleSessionConfig,
    session: ConsoleSession,
    user: ConsoleUser,
}

/// Runs the login and active-account gates in order.
async fn gate(state: &AppState, headers: &HeaderMap, uri: &Uri) -> Result<PageContext, Response> {
    let Some(config) = state.console_session().cloned() else {
        return Err(crate::ApiError::ServiceUnavailable(
            "the console session API is not configured".to_owned(),
        )
        .into_response());
    };
    let mut session = config
        .session_from_headers(headers)
        .unwrap_or_else(ConsoleSession::anonymous);
    let user = match session.user_id() {
        Some(user_id) => {
            let pool = state
                .console_session_db()
                .map_err(|error| error.into_response())?;
            find_console_user(&pool, user_id)
                .await
                .map_err(|error| crate::ApiError::Internal(error.to_string()).into_response())?
        }
        None => None,
    };
    let Some(user) = user else {
        if let Some(path) = uri.path_and_query().map(|value| value.as_str())
            && let Some(safe) = return_path::safe(path)
        {
            session.set_return_to(safe);
        }
        return Err(redirect(LOGIN_PATH, &config, &session, None));
    };
    match user.status {
        ConsoleUserStatus::Disabled => {
            let cleared = ConsoleSession::anonymous();
            Err(redirect(
                &format!("/login?error={}", login_error::ACCOUNT_DISABLED),
                &config,
                &cleared,
                None,
            ))
        }
        ConsoleUserStatus::Pending => Err(redirect(PENDING_PATH, &config, &session, None)),
        ConsoleUserStatus::Active => Ok(PageContext {
            config,
            session,
            user,
        }),
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Runtime's `SecureRandom.urlsafe_base64(n)`: unpadded URL-safe base64.
fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0; bytes];
    getrandom::fill(&mut buffer).expect("operating system randomness");
    URL_SAFE_NO_PAD.encode(buffer)
}

/// `JSON.generate(hash.sort.to_h)`: top-level keys in sorted order.
fn provider_config_digest(config: &Map<String, Value>) -> String {
    let sorted: BTreeMap<&String, &Value> = config.iter().collect();
    let json = serde_json::to_string(&sorted).unwrap_or_default();
    hex::encode(sha2::Sha256::digest(json.as_bytes()))
}

/// Union with first-occurrence order.
fn union(left: Vec<String>, right: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result = left;
    for value in right {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

fn parse_scopes(raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(
        raw.split(|character: char| character == ',' || character.is_whitespace())
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn scope_blank(scope: &Value) -> bool {
    match scope {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    }
}

/// The signed state payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StatePayload {
    app: String,
    scopes: Vec<String>,
    nonce: String,
    user_id: i64,
    config: String,
    #[serde(rename = "exp")]
    expires_at: i64,
}

fn encode_state(config: &ConsoleSessionConfig, payload: &StatePayload) -> Option<String> {
    let json = serde_json::to_string(payload).ok()?;
    let mut session = ConsoleSession::signed_in(payload.user_id);
    session.set_return_to(json);
    config.cookie().encode(&session)
}

fn decode_state(config: &ConsoleSessionConfig, raw: &str) -> Option<StatePayload> {
    let session = config.cookie().decode(raw)?;
    serde_json::from_str(session.return_to()?).ok()
}

/// The flow-state expiry is inside the signed payload, because the shared
/// envelope enforces its own 30-day lifetime.
fn state_valid(payload: &StatePayload, now: i64) -> bool {
    payload.expires_at >= now
}

/// The PKCE verifier a start redirect stashes in its own cookie.
#[derive(Serialize)]
struct FlowCookiePayload<'a> {
    nonce: &'a str,
    code_verifier: &'a str,
    exp: i64,
}

fn flow_cookie(config: &ConsoleSessionConfig, nonce: &str, verifier: &str) -> Option<HeaderValue> {
    let json = serde_json::to_string(&FlowCookiePayload {
        nonce,
        code_verifier: verifier,
        exp: unix_now() + FLOW_TTL_SECONDS,
    })
    .ok()?;
    let mut session = ConsoleSession::anonymous();
    session.set_return_to(json);
    let value = config.cookie().encode(&session)?;
    let secure = config.public_origin().starts_with("https://");
    let mut cookie = format!(
        "{FLOW_COOKIE}={value}; path=/; Max-Age={FLOW_TTL_SECONDS}; HttpOnly; SameSite=Lax"
    );
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).ok()
}

fn clear_flow_cookie(config: &ConsoleSessionConfig) -> Option<HeaderValue> {
    let secure = config.public_origin().starts_with("https://");
    let mut cookie = format!(
        "{FLOW_COOKIE}=; path=/; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT; \
         HttpOnly; SameSite=Lax"
    );
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).ok()
}

fn read_flow_cookie(
    config: &ConsoleSessionConfig,
    headers: &HeaderMap,
) -> Option<Map<String, Value>> {
    let value = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|header| header.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .filter(|(name, _)| *name == FLOW_COOKIE)
        .map(|(_, value)| value)
        .next_back()?;
    let session = config.cookie().decode(value)?;
    let payload: Value = serde_json::from_str(session.return_to()?).ok()?;
    payload.as_object().cloned()
}

fn flow_valid(flow: &Map<String, Value>) -> bool {
    flow.get("exp")
        .and_then(Value::as_i64)
        .is_some_and(|expires_at| expires_at >= unix_now())
}

fn authorization_url(
    provider: &Provider,
    app: &DbOauthApp,
    scopes: &[String],
    state: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Option<String> {
    let endpoint = provider.authorization_endpoint()?;
    let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let mut pairs: Vec<(String, String)> = vec![
        ("client_id".to_owned(), app.client_id.clone()),
        ("redirect_uri".to_owned(), redirect_uri.to_owned()),
        ("response_type".to_owned(), "code".to_owned()),
        ("state".to_owned(), state.to_owned()),
        ("code_challenge".to_owned(), challenge),
        ("code_challenge_method".to_owned(), "S256".to_owned()),
    ];
    let set = |pairs: &mut Vec<(String, String)>, key: &str, value: String| {
        if let Some(pair) = pairs.iter_mut().find(|(name, _)| name == key) {
            pair.1 = value;
        } else {
            pairs.push((key.to_owned(), value));
        }
    };
    for (key, value) in provider.extra_authorization_params() {
        set(&mut pairs, &key, value);
    }
    let consent = union(scopes.to_vec(), provider.identity_scopes());
    set(
        &mut pairs,
        provider.authorization_scope_param(),
        consent.join(provider.scope_separator()),
    );
    let mut url = Url::parse(&endpoint).ok()?;
    let query = pairs
        .iter()
        .fold(
            url::form_urlencoded::Serializer::new(String::new()),
            |mut serializer, (key, value)| {
                serializer.append_pair(key, value);
                serializer
            },
        )
        .finish();
    url.set_query(Some(&query));
    Some(url.to_string())
}

async fn exchange_code(
    provider: &Provider,
    app: &DbOauthApp,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    client_secret: Option<&str>,
) -> Result<CodeExchangeResult, ExchangeError> {
    let Some(token_endpoint) = provider.token_endpoint() else {
        return Err(ExchangeError::new("parse"));
    };
    let mut form: Vec<(String, String)> = vec![
        ("grant_type".to_owned(), "authorization_code".to_owned()),
        ("code".to_owned(), code.to_owned()),
        ("redirect_uri".to_owned(), redirect_uri.to_owned()),
    ];
    let mut headers: Vec<(String, String)> = Vec::new();
    match provider.token_endpoint_auth_method() {
        "client_secret_basic" => {
            let Some(secret) = client_secret.filter(|value| !value.is_empty()) else {
                return Err(ExchangeError::new("parse"));
            };
            let encode = |value: &str| {
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            };
            let credentials = format!("{}:{}", encode(&app.client_id), encode(secret));
            headers.push((
                "Authorization".to_owned(),
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(credentials)
                ),
            ));
        }
        _ => {
            form.push(("client_id".to_owned(), app.client_id.clone()));
            if let Some(secret) = client_secret.filter(|value| !value.is_empty()) {
                form.push(("client_secret".to_owned(), secret.to_owned()));
            }
        }
    }
    if !verifier.is_empty() {
        form.push(("code_verifier".to_owned(), verifier.to_owned()));
    }
    for (key, value) in provider.extra_token_params() {
        if !form.iter().any(|(name, _)| name == &key) {
            form.push((key, value));
        }
    }
    let response = http::send(
        reqwest::Method::POST,
        &token_endpoint,
        &headers,
        Some(&form),
        None,
        Duration::from_secs(30),
        64 * 1024,
    )
    .await
    .map_err(|_| ExchangeError::new("network"))?;
    if !response.success() {
        let oauth_error = response.json().and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        return Err(ExchangeError {
            stage: if oauth_error.is_some() {
                "oauth"
            } else {
                "http"
            },
            code: oauth_error,
        });
    }
    let parsed: Value = serde_json::from_str(&response.body).map_err(|_| ExchangeError {
        stage: "parse",
        code: None,
    })?;
    let object = parsed.as_object().cloned().unwrap_or_default();
    if object.get("ok") == Some(&Value::Bool(false))
        && let Some(error) = object.get("error").and_then(Value::as_str)
    {
        return Err(ExchangeError {
            stage: "oauth",
            code: Some(error.to_owned()),
        });
    }
    let token_payload = object
        .get("authed_user")
        .filter(|value| {
            value
                .get("access_token")
                .and_then(Value::as_str)
                .is_some_and(|token| !token.is_empty())
        })
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| object.clone());
    let access_token = token_payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(ExchangeError {
            stage: "parse",
            code: None,
        })?
        .to_owned();
    let refresh_token = token_payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    if provider.require_refresh_token() && refresh_token.is_none() {
        return Err(ExchangeError {
            stage: "oauth",
            code: Some("missing_refresh_token".to_owned()),
        });
    }
    let expires_in = token_payload
        .get("expires_in")
        .and_then(|value| match value {
            Value::Number(number) => number
                .as_i64()
                .or_else(|| number.as_f64().map(|value| value.trunc() as i64)),
            Value::String(value) => value.trim().parse::<i64>().ok().or_else(|| {
                value
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .map(|value| value.trunc() as i64)
            }),
            _ => None,
        });
    let scope = token_payload
        .get("scope")
        .or_else(|| object.get("scope"))
        .cloned();
    let id_token = token_payload
        .get("id_token")
        .or_else(|| object.get("id_token"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(CodeExchangeResult {
        access_token,
        refresh_token,
        expires_in,
        scope,
        id_token,
        response: parsed,
    })
}

enum UpsertFailure {
    AccountMismatch,
    Concurrent,
    /// The credential could not be built; the reason is a fixed label.
    SaveFailed(&'static str),
    /// A database step failed; carries the stage and the error text.
    Internal(&'static str, String),
}

impl From<centaur_console_db::areas::oauth::FlowUpsertError> for UpsertFailure {
    fn from(error: centaur_console_db::areas::oauth::FlowUpsertError) -> Self {
        use centaur_console_db::areas::oauth::FlowUpsertError;
        match error {
            FlowUpsertError::AccountMismatch => Self::AccountMismatch,
            FlowUpsertError::UniqueViolation => Self::Concurrent,
            FlowUpsertError::Console(error) => Self::Internal("upsert", error.to_string()),
        }
    }
}

fn display_name(identity: &Identity) -> String {
    identity
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| identity.email.clone().filter(|email| !email.is_empty()))
        .unwrap_or_else(|| identity.subject.clone())
}

async fn upsert_credential(
    pool: &sqlx::PgPool,
    app: &DbOauthApp,
    provider: &Provider,
    user: &ConsoleUser,
    state_scopes: &[String],
    result: &CodeExchangeResult,
    identity: &Identity,
) -> Result<i64, UpsertFailure> {
    let encryption =
        OauthEncryption::from_env().ok_or(UpsertFailure::SaveFailed("encryption_unavailable"))?;
    let now = centaur_console_db::timestamp::now();
    let refreshable = provider.refreshable_result(result);
    let scopes = match result.scope.as_ref() {
        Some(scope) if !scope_blank(scope) => provider.parse_granted_scopes(scope),
        _ => state_scopes.to_vec(),
    };
    let expires_at = credential_expires_at(result.expires_in, refreshable, now);
    let next_attempt_at = refreshable.then(|| {
        compute_next_attempt_at(
            expires_at,
            Some(now),
            DEFAULT_EARLY_REFRESH_SLACK_SECONDS,
            DEFAULT_EARLY_REFRESH_FRACTION,
            DEFAULT_MAX_REFRESH_INTERVAL_SECONDS,
            now,
        )
    });
    let subject = identity.subject.clone();
    let foreign_id = format!(
        "{}-{}-{}",
        app.provider,
        app.slug,
        provider.foreign_id_subject(&subject)
    );
    if !url_safe(&foreign_id) {
        return Err(UpsertFailure::SaveFailed("foreign_id_not_url_safe"));
    }
    let display = display_name(identity);
    let token_endpoint = provider.token_endpoint().unwrap_or_default();
    let name = format!("{} – {}", provider.display_name(), display);
    let external_user_key = random_token(16);

    let mut tx = pool
        .begin()
        .await
        .map_err(|error| UpsertFailure::Internal("begin", error.to_string()))?;
    if app.shared {
        lock_oauth_app_row(&mut tx, &app.slug)
            .await
            .map_err(|error| UpsertFailure::Internal("lock_app", error.to_string()))?;
        if let Some(first) = first_credential_for_app(&mut tx, app.id).await?
            && first.provider_subject.as_deref() != Some(subject.as_str())
        {
            return Err(UpsertFailure::AccountMismatch);
        }
    }
    let existing = credential_for_app_subject(&mut tx, app.id, &subject).await?;
    if let Some(existing) = &existing
        && app.owner_only
        && existing.created_by_id != Some(user.id)
    {
        return Err(UpsertFailure::AccountMismatch);
    }
    let mut labels = existing
        .as_ref()
        .map(|credential| credential.labels.clone())
        .unwrap_or_default();
    for (key, value) in &identity.labels {
        labels.insert(key.clone(), value.clone());
    }
    if app.provider == SLACK_KEY
        && let Some(team_id) = identity
            .team_id
            .as_deref()
            .filter(|value| !value.is_empty())
    {
        labels.insert(
            "slack_team_id".to_owned(),
            Value::String(team_id.to_owned()),
        );
    }
    let new = NewFlowCredential {
        provider_subject: subject.clone(),
        foreign_id,
        display_name: name.clone(),
        token_endpoint: token_endpoint.clone(),
        external_user_key,
        created_by_id: user.id,
    };
    let write = FlowCredentialWrite {
        access_token: result.access_token.clone(),
        refresh_token: result.refresh_token.clone(),
        expires_at,
        last_refresh: now,
        next_attempt_at,
        scopes,
        labels,
        provider_email: identity.email.clone(),
        token_endpoint: app.owner_only.then_some(token_endpoint),
    };
    let credential = upsert_flow_credential(&mut tx, &encryption, app, &new, &write).await?;
    let (inject, replace) = provider.wrapping_secret_config();
    let rules = provider.credential_request_rules().unwrap_or_else(|| {
        provider
            .api_hosts()
            .into_iter()
            .map(|host| WrappingRule {
                host,
                http_methods: Vec::new(),
                paths: Vec::new(),
            })
            .collect()
    });
    let spec = WrappingSecretSpec {
        name: format!("{} token", credential.name.clone().unwrap_or_default()),
        labels: tool_label(app),
        kind: provider.wrapping_secret_kind(),
        inject_config: inject,
        replace_config: replace,
        rules,
        rebuild: app.owner_only,
    };
    upsert_wrapping_secret(&mut tx, credential.id, &spec).await?;
    centaur_console_db::areas::reconciliation::apply_for_credential(&mut tx, credential.id)
        .await
        .map_err(|error| UpsertFailure::Internal("reconcile", error.to_string()))?;
    tx.commit()
        .await
        .map_err(|error| UpsertFailure::Internal("commit", error.to_string()))?;
    Ok(credential.id)
}

pub(super) async fn start(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    ConsoleQuery(query): ConsoleQuery<StartQuery>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let context = match gate(&state, &headers, &uri).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    match start_inner(&state, &context, &slug, query.scopes.as_deref()).await {
        Ok(response) => response,
        Err(response) => response,
    }
}

async fn start_inner(
    state: &AppState,
    context: &PageContext,
    slug: &str,
    scopes: Option<&str>,
) -> Result<Response, Response> {
    let pool = state
        .console_session_db()
        .map_err(|error| error.into_response())?;
    let (app, provider) = load_app(&pool, context, slug).await?;
    if !app.enabled {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::UNPROCESSABLE_ENTITY,
            "This integration is disabled.",
        ));
    }
    let requested = parse_scopes(scopes).unwrap_or_else(|| app.allowed_scopes.clone());
    if requested
        .iter()
        .any(|scope| !app.allowed_scopes.iter().any(|allowed| allowed == scope))
    {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::UNPROCESSABLE_ENTITY,
            "One or more requested scopes are not allowed for this integration.",
        ));
    }
    let consent_scopes = union(requested, provider.required_scopes());
    let nonce = random_token(32);
    let verifier = random_token(64);
    let payload = StatePayload {
        app: OauthAppModel::oid(app.id),
        scopes: consent_scopes.clone(),
        nonce: nonce.clone(),
        user_id: context.user.id,
        config: provider_config_digest(&app.provider_config),
        expires_at: unix_now() + FLOW_TTL_SECONDS,
    };
    let Some(signed) = encode_state(&context.config, &payload) else {
        return Err(
            crate::ApiError::Internal("cannot sign the OAuth state".to_owned()).into_response(),
        );
    };
    let redirect_uri = super::oauth_callback_redirect_uri(&context.config, &app.slug);
    let Some(location) = authorization_url(
        &provider,
        &app,
        &consent_scopes,
        &signed,
        &verifier,
        &redirect_uri,
    ) else {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::NOT_FOUND,
            "Unknown integration.",
        ));
    };
    Ok(redirect(
        &location,
        &context.config,
        &context.session,
        flow_cookie(&context.config, &nonce, &verifier),
    ))
}

pub(super) async fn callback(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    ConsoleQuery(query): ConsoleQuery<CallbackQuery>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let context = match gate(&state, &headers, &uri).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    // The flow cookie is cleared inside `read_and_clear_flow_cookie`, which runs
    // only after the signed state has been verified: a callback rejected for a
    // bad state leaves the cookie in place. Let the inner handler raise the flag
    // at the point of deletion.
    let mut clear_flow = false;
    let mut response =
        match callback_inner(&state, &context, &slug, &query, &headers, &mut clear_flow).await {
            Ok(response) | Err(response) => response,
        };
    if clear_flow && let Some(cookie) = clear_flow_cookie(&context.config) {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

async fn callback_inner(
    state: &AppState,
    context: &PageContext,
    slug: &str,
    query: &CallbackQuery,
    headers: &HeaderMap,
    clear_flow: &mut bool,
) -> Result<Response, Response> {
    let pool = state
        .console_session_db()
        .map_err(|error| error.into_response())?;
    let (app, provider) = load_app(&pool, context, slug).await?;

    let Some(state_payload) = query
        .state
        .as_deref()
        .and_then(|raw| decode_state(&context.config, raw))
        .filter(|payload| state_valid(payload, unix_now()))
    else {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::BAD_REQUEST,
            "This consent link is invalid or has expired. Start again.",
        ));
    };

    if state_payload.app != OauthAppModel::oid(app.id)
        || !app.enabled
        || state_payload.user_id != context.user.id
        || state_payload.config != provider_config_digest(&app.provider_config)
    {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::BAD_REQUEST,
            "This integration is no longer available.",
        ));
    }

    // The cookie is dropped from here on, even
    // when it is missing or fails the nonce check.
    *clear_flow = true;
    let Some(flow) = read_flow_cookie(&context.config, headers) else {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::BAD_REQUEST,
            "This flow expired or was started in another browser. Start again.",
        ));
    };
    let flow_nonce = flow.get("nonce").and_then(Value::as_str);
    if !flow_valid(&flow) || flow_nonce != Some(state_payload.nonce.as_str()) {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::BAD_REQUEST,
            "This flow expired or was started in another browser. Start again.",
        ));
    }
    let verifier = flow
        .get("code_verifier")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    if let Some(error) = query.error.as_deref().filter(|value| !value.is_empty()) {
        return Err(result_page(
            Some(&app),
            "denied",
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Consent was not granted ({error})."),
        ));
    }

    let code = query.code.clone().unwrap_or_default();
    let redirect_uri = super::oauth_callback_redirect_uri(&context.config, &app.slug);
    let client_secret = app.client_secret.as_deref().and_then(|ciphertext| {
        OauthEncryption::from_env().and_then(|encryption| encryption.decrypt(ciphertext).ok())
    });
    let result = exchange_code(
        &provider,
        &app,
        &code,
        &verifier,
        &redirect_uri,
        client_secret.as_deref(),
    )
    .await
    .map_err(|error| {
        result_page(
            Some(&app),
            "error",
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Connecting the integration failed ({}).", error.reason()),
        )
    })?;
    provider.validate_result(&result).map_err(|error| {
        result_page(
            Some(&app),
            "error",
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("Connecting the integration failed ({}).", error.reason()),
        )
    })?;

    let identity = if app.owner_only {
        Identity {
            subject: format!("owner-{}", UserModel::oid(context.user.id)),
            email: Some(context.user.email.clone()),
            name: Some(context.user.email.clone()),
            labels: Map::new(),
            team_id: None,
        }
    } else {
        provider
            .identity_from(&result, &app.client_id)
            .await
            .map_err(|error| {
                result_page(
                    Some(&app),
                    "error",
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &format!("Connecting the integration failed ({}).", error.reason()),
                )
            })?
    };

    match upsert_credential(
        &pool,
        &app,
        &provider,
        &context.user,
        &state_payload.scopes,
        &result,
        &identity,
    )
    .await
    {
        Ok(credential_id) => {
            // Identity enrichment runs here. Its failure must never fail the
            // connect, so log and continue.
            if app.provider == SLACK_KEY
                && let Err(error) = super::enrich::enrich_after_connect(&pool, credential_id).await
            {
                tracing::warn!(
                    credential_id,
                    error = %error,
                    "OAuth credential identity enrichment failed"
                );
            }
        }
        Err(UpsertFailure::AccountMismatch) => {
            return Err(result_page(
                Some(&app),
                "error",
                StatusCode::UNPROCESSABLE_ENTITY,
                "Connecting the integration failed (account_mismatch).",
            ));
        }
        Err(UpsertFailure::Concurrent) => {
            return Err(result_page(
                Some(&app),
                "error",
                StatusCode::UNPROCESSABLE_ENTITY,
                "Another consent for this account is in progress. Try again.",
            ));
        }
        Err(UpsertFailure::SaveFailed(reason)) => {
            return Err(save_failed(&app, "build", reason));
        }
        Err(UpsertFailure::Internal(stage, error)) => {
            return Err(save_failed(&app, stage, &error));
        }
    }

    let next = context.session.clone();
    let location = if app.shared {
        format!("/console/oauth_apps/{}", OauthAppModel::oid(app.id))
    } else {
        "/console/integrations".to_owned()
    };
    Ok(redirect(&location, &context.config, &next, None))
}

/// Logs why the credential save failed (the page stays generic) and renders
/// the error page.
fn save_failed(app: &DbOauthApp, stage: &str, error: &str) -> Response {
    tracing::error!(
        app = %app.slug,
        provider = %app.provider,
        stage,
        error,
        "OAuth credential save failed"
    );
    result_page(
        Some(app),
        "error",
        StatusCode::UNPROCESSABLE_ENTITY,
        "Connecting the integration failed while saving the credential.",
    )
}

async fn find_app(pool: &sqlx::PgPool, slug: &str) -> Result<Option<DbOauthApp>, Response> {
    centaur_console_db::areas::oauth::find_oauth_app_by_slug(pool, slug)
        .await
        .map_err(|error| crate::ApiError::Internal(error.to_string()).into_response())
}

async fn load_app(
    pool: &sqlx::PgPool,
    context: &PageContext,
    slug: &str,
) -> Result<(DbOauthApp, Provider), Response> {
    let Some(app) = find_app(pool, slug).await? else {
        return Err(result_page(
            None,
            "error",
            StatusCode::NOT_FOUND,
            "Unknown integration.",
        ));
    };
    let Some(provider) = Provider::from_app(&app) else {
        return Err(result_page(
            Some(&app),
            "error",
            StatusCode::NOT_FOUND,
            "Unknown integration.",
        ));
    };
    if app.shared && !context.user.admin {
        return Err(redirect(
            "/console/integrations",
            &context.config,
            &context.session,
            None,
        ));
    }
    Ok((app, provider))
}

/// The consent flow's failure page, with a retry link while the app is enabled.
fn result_page(
    app: Option<&DbOauthApp>,
    kind: &str,
    status: StatusCode,
    message: &str,
) -> Response {
    let heading = if kind == "denied" {
        "Not connected"
    } else {
        "Something went wrong"
    };
    let retry = app
        .filter(|app| app.enabled)
        .map(|app| format!("/oauth/{}/start", app.slug));
    crate::console::pages::server_error_page(status, heading, message, retry.as_deref())
}

fn redirect(
    location: &str,
    config: &ConsoleSessionConfig,
    session: &ConsoleSession,
    cookie: Option<HeaderValue>,
) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(location).unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    response = server_page_csp(response);
    if let Some(cookie) = cookie {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    if let Some(cookie) = config.cookie().set_cookie_header(session) {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> ConsoleSessionConfig {
        ConsoleSessionConfig::from_settings(
            Some("test-only-console-session-secret-0000000000000000000000000000000000000000000000"),
            Some("https://console.example.test"),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn state_round_trips_and_rejects_tampering() {
        let config = config();
        let payload = StatePayload {
            app: "oap_abc".to_owned(),
            scopes: vec!["read".to_owned()],
            nonce: "n".to_owned(),
            user_id: 7,
            config: "digest".to_owned(),
            expires_at: unix_now() + 60,
        };
        let encoded = encode_state(&config, &payload).unwrap();
        let decoded = decode_state(&config, &encoded).unwrap();
        assert_eq!(decoded.app, payload.app);
        assert_eq!(decoded.scopes, payload.scopes);
        assert_eq!(decoded.user_id, 7);
        assert!(state_valid(&decoded, unix_now()));
        assert!(decode_state(&config, &format!("{encoded}x")).is_none());
    }

    #[test]
    fn flow_cookie_round_trips_and_clears() {
        let config = config();
        let mut headers = HeaderMap::new();
        let cookie = flow_cookie(&config, "nonce", "verifier").unwrap();
        headers.insert(COOKIE, cookie);
        let flow = read_flow_cookie(&config, &headers).unwrap();
        assert_eq!(flow.get("nonce"), Some(&json!("nonce")));
        assert_eq!(flow.get("code_verifier"), Some(&json!("verifier")));
        assert!(flow_valid(&flow));
        assert!(clear_flow_cookie(&config).is_some());
    }

    #[test]
    fn scope_parsing_and_union_match_ruby() {
        assert_eq!(
            parse_scopes(Some("a, b  c")).unwrap(),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
        assert!(parse_scopes(Some("  ")).is_none());
        assert_eq!(
            union(
                vec!["a".to_owned(), "b".to_owned()],
                ["b".to_owned(), "c".to_owned()]
            ),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn provider_config_digest_sorts_top_level_keys() {
        let mut left = Map::new();
        left.insert("b".to_owned(), json!(2));
        left.insert("a".to_owned(), json!(1));
        let mut right = Map::new();
        right.insert("a".to_owned(), json!(1));
        right.insert("b".to_owned(), json!(2));
        assert_eq!(
            provider_config_digest(&left),
            provider_config_digest(&right)
        );
        assert_eq!(provider_config_digest(&left).len(), 64);
    }
}
