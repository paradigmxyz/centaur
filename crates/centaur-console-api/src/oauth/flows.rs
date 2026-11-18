//! The broker consent flow, keyed by an app's slug. `/oauth/{slug}/start`
//! sends a signed-in team member to the IdP's consent screen;
//! `/oauth/{slug}/callback` exchanges the code, upserts a broker credential
//! linked to the app (plus its wrapping static secret), and redirects back to
//! the console, or renders the result page on failure.
//!
//! The flow is bound to the browser by a signed `state` and an encrypted
//! cookie carrying the same nonce and the PKCE verifier.
//!
//! SECURITY: never logs the code, tokens, client secret, or response bodies.

use askama::Template;
use axum::{
    extract::{OriginalUri, State},
    http::HeaderMap,
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_auth::{AuthUser, csrf, random::random_bytes, return_path};
use centaur_console_db::{Ciphertext, oid};
use chrono::{Duration, NaiveDateTime};
use http::{HeaderValue, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::{
    callback_url, credentials, enrich,
    exchange::{self, Exchange, ExchangeError, TokenSet},
    providers::{Identity, Provider, ProviderKey},
    request_base, url_safe, wrapping,
};
use crate::{
    ApiError, ApiResult, AppState,
    assets::Assets,
    auth::{Session, is_https, session_user},
    extract::{Path, Query},
    identity::landing_path,
    pages::{redirect, render},
};

pub const STATE_PURPOSE: &str = "oauth_consent_flow";
pub const FLOW_TTL: Duration = Duration::minutes(10);
pub const FLOW_COOKIE: &str = "oauth_flow";

/// What the signed `state` carries.
#[derive(Serialize, Deserialize)]
struct FlowState {
    app: String,
    scopes: Vec<String>,
    nonce: String,
}

/// What the browser's flow cookie carries.
#[derive(Serialize, Deserialize)]
struct FlowCookie {
    nonce: String,
    code_verifier: String,
}

#[derive(sqlx::FromRow)]
struct App {
    id: i64,
    slug: String,
    provider: String,
    client_id: String,
    client_secret: Option<Ciphertext>,
    enabled: bool,
    shared: bool,
    allowed_scopes: Value,
    provider_config: Value,
}

impl App {
    fn oid(&self) -> String {
        oid("oap", self.id)
    }

    fn allowed_scopes(&self) -> Vec<String> {
        serde_json::from_value(self.allowed_scopes.clone()).unwrap_or_default()
    }
}

#[derive(Template)]
#[template(path = "pages/flow_result.html")]
struct ResultPage<'a> {
    assets: &'a Assets,
    heading: &'static str,
    tone: &'static str,
    message: String,
    retry_path: Option<String>,
}

#[derive(Clone, Copy)]
enum Kind {
    Denied,
    Error,
}

fn result_page(
    state: &AppState,
    app: Option<&App>,
    kind: Kind,
    status: StatusCode,
    message: impl Into<String>,
) -> Response {
    let (heading, tone) = match kind {
        Kind::Denied => ("Not connected", "is-denied"),
        Kind::Error => ("Something went wrong", "is-error"),
    };
    render(
        status,
        &ResultPage {
            assets: &state.assets,
            heading,
            tone,
            message: message.into(),
            retry_path: app
                .filter(|app| app.enabled)
                .map(|app| format!("/oauth/{}/start", app.slug)),
        },
    )
}

fn unprocessable(state: &AppState, app: &App, message: impl Into<String>) -> Response {
    result_page(
        state,
        Some(app),
        Kind::Error,
        StatusCode::UNPROCESSABLE_ENTITY,
        message,
    )
}

/// The signed-in active user, the app, and its strategy; or the response to
/// send instead (sign-in, holding page, 404 page, landing page).
async fn authorize(
    state: &AppState,
    session: &Session,
    uri: &axum::http::Uri,
    slug: &str,
) -> Result<(AuthUser, App, Provider), ApiResult> {
    let user = session_user(state, session).await.map_err(Err)?;
    let Some(user) = user else {
        let target = uri.path_and_query().map(|target| target.as_str());
        if let Some(path) = target.and_then(return_path::safe) {
            session.lock().set_return_to(path);
        }
        return Err(Ok(redirect("/login")));
    };
    if !user.is_active() {
        return Err(Ok(redirect("/pending")));
    }
    let app: Option<App> = sqlx::query_as(
        "SELECT id, slug, provider, client_id, client_secret, enabled, shared, allowed_scopes, \
         provider_config FROM oauth_apps WHERE slug = $1",
    )
    .bind(slug)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| Err(error.into()))?;
    let unknown = |app: Option<&App>| {
        Err(Ok(result_page(
            state,
            app,
            Kind::Error,
            StatusCode::NOT_FOUND,
            "Unknown integration.",
        )))
    };
    let Some(app) = app else { return unknown(None) };
    let Some(provider) = Provider::for_app(&app.provider, &app.slug, &app.provider_config) else {
        return unknown(Some(&app));
    };
    if app.shared && !user.admin {
        return Err(Ok(redirect(landing_path(false))));
    }
    Ok((user, app, provider))
}

/// `requested` followed by the entries of `extra` it lacks.
fn union(requested: Vec<String>, extra: &[&str]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for scope in requested
        .into_iter()
        .chain(extra.iter().map(|scope| (*scope).to_owned()))
    {
        if !merged.contains(&scope) {
            merged.push(scope);
        }
    }
    merged
}

fn with_cookie(mut response: Response, cookie: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

#[derive(Deserialize)]
pub struct StartQuery {
    scopes: Option<String>,
}

/// `GET /oauth/{slug}/start[?scopes=]`.
pub async fn start(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Path(slug): Path<String>,
    Query(query): Query<StartQuery>,
) -> ApiResult {
    let (_user, app, provider) = match authorize(&state, &session, &uri, &slug).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    if !app.enabled {
        return Ok(unprocessable(&state, &app, "This integration is disabled."));
    }
    let requested: Vec<String> = match query.scopes.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(scopes) => exchange::split_scopes(Some(&Value::String(scopes.to_owned()))),
        None => app.allowed_scopes(),
    };
    let allowed = app.allowed_scopes();
    if !requested.iter().all(|scope| allowed.contains(scope)) {
        return Ok(unprocessable(
            &state,
            &app,
            "One or more requested scopes are not allowed for this integration.",
        ));
    }
    let scopes = union(requested, &provider.required_scopes());
    let nonce = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
    let code_verifier = URL_SAFE_NO_PAD.encode(random_bytes::<64>());
    let signed = state.signer.sign(
        STATE_PURPOSE,
        &FlowState {
            app: app.oid(),
            scopes: scopes.clone(),
            nonce: nonce.clone(),
        },
        FLOW_TTL,
        state.now(),
    );
    let base = request_base(&state, &headers);
    let location = authorization_url(&app, &provider, &scopes, &signed, &code_verifier, &base)?;
    let cookie = state.flow_cookies.set(
        FLOW_COOKIE,
        &FlowCookie {
            nonce,
            code_verifier,
        },
        FLOW_TTL,
        is_https(&state, &headers),
    );
    Ok(with_cookie(redirect(&location), &cookie))
}

fn authorization_url(
    app: &App,
    provider: &Provider,
    scopes: &[String],
    signed_state: &str,
    code_verifier: &str,
    base: &str,
) -> Result<String, ApiError> {
    let mut query: Vec<(String, String)> = vec![
        ("client_id".into(), app.client_id.clone()),
        ("redirect_uri".into(), callback_url(base, &app.slug)?),
        ("response_type".into(), "code".into()),
        ("state".into(), signed_state.to_owned()),
        (
            "code_challenge".into(),
            URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes())),
        ),
        ("code_challenge_method".into(), "S256".into()),
    ];
    let scope =
        union(scopes.to_vec(), &provider.identity_scopes()).join(provider.scope_separator());
    let extra = provider
        .extra_authorization_params()
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .chain([(provider.scope_param().to_owned(), scope)]);
    for (key, value) in extra {
        match query.iter_mut().find(|(existing, _)| *existing == key) {
            Some(entry) => entry.1 = value,
            None => query.push((key, value)),
        }
    }
    let mut url = url::Url::parse(&provider.authorization_endpoint().unwrap_or_default())
        .map_err(|_| ApiError::internal("authorization endpoint is not a URL"))?;
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(&query);
    Ok(url.into())
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Why the code exchange or credential upsert failed.
enum Failure {
    Exchange(ExchangeError),
    /// The credential's identifiers are not storable.
    Invalid,
    /// A concurrent consent for the same account won.
    NotUnique,
    Api(ApiError),
}

impl From<ExchangeError> for Failure {
    fn from(error: ExchangeError) -> Self {
        Self::Exchange(error)
    }
}

impl From<ApiError> for Failure {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .is_some_and(|error| error.is_unique_violation())
        {
            Self::NotUnique
        } else {
            Self::Api(error.into())
        }
    }
}

/// `GET /oauth/{slug}/callback?code=&state=` (or `?error=`).
pub async fn callback(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Path(slug): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> ApiResult {
    let (user, app, provider) = match authorize(&state, &session, &uri, &slug).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let response = finish(&state, &session, &headers, &user, &app, &provider, query).await?;
    Ok(with_cookie(
        response,
        &state.flow_cookies.clear(FLOW_COOKIE),
    ))
}

async fn finish(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    user: &AuthUser,
    app: &App,
    provider: &Provider,
    query: CallbackQuery,
) -> ApiResult {
    let page = |status: StatusCode, message: &str| {
        result_page(state, Some(app), Kind::Error, status, message)
    };
    let flow_state: Option<FlowState> = query
        .state
        .as_deref()
        .and_then(|token| state.signer.verify(STATE_PURPOSE, token, state.now()));
    let Some(flow_state) = flow_state else {
        return Ok(page(
            StatusCode::BAD_REQUEST,
            "This consent link is invalid or has expired. Start again.",
        ));
    };
    if flow_state.app != app.oid() || !app.enabled {
        return Ok(page(
            StatusCode::BAD_REQUEST,
            "This integration is no longer available.",
        ));
    }
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok());
    let flow: Option<FlowCookie> = state.flow_cookies.read(FLOW_COOKIE, cookie_header);
    let Some(flow) = flow.filter(|flow| csrf::eq(&flow.nonce, &flow_state.nonce)) else {
        return Ok(page(
            StatusCode::BAD_REQUEST,
            "This flow expired or was started in another browser. Start again.",
        ));
    };
    if let Some(error) = query.error.as_deref().filter(|error| !error.is_empty()) {
        return Ok(result_page(
            state,
            Some(app),
            Kind::Denied,
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Consent was not granted ({error})."),
        ));
    }
    let base = request_base(state, headers);
    let outcome = connect(
        state,
        &base,
        user,
        app,
        provider,
        &flow_state,
        query.code.as_deref().unwrap_or_default(),
        &flow.code_verifier,
    )
    .await;
    match outcome {
        Ok((credential_id, identity)) => {
            if app.provider == ProviderKey::Slack.as_str() {
                enrich::spawn(state.clone(), credential_id);
            }
            let connected_as = identity
                .email
                .as_deref()
                .map(|email| format!(" as {email}"))
                .unwrap_or_default();
            session
                .lock()
                .set_notice(format!("{} connected{connected_as}.", app.slug));
            Ok(redirect(&if app.shared {
                format!("/console/oauth_apps/{}", app.oid())
            } else {
                "/console/integrations".into()
            }))
        }
        Err(Failure::Exchange(error)) => Ok(unprocessable(
            state,
            app,
            format!("Connecting the integration failed ({}).", error.reason()),
        )),
        Err(Failure::Invalid) => {
            tracing::error!("oauth flow credential could not be stored");
            Ok(unprocessable(
                state,
                app,
                "Connecting the integration failed while saving the credential.",
            ))
        }
        Err(Failure::NotUnique) => Ok(unprocessable(
            state,
            app,
            "Another consent for this account is in progress. Try again.",
        )),
        Err(Failure::Api(error)) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect(
    state: &AppState,
    base: &str,
    user: &AuthUser,
    app: &App,
    provider: &Provider,
    flow_state: &FlowState,
    code: &str,
    code_verifier: &str,
) -> Result<(i64, Identity), Failure> {
    let client_secret = state
        .encryptor
        .decrypt_optional(app.client_secret.as_ref())
        .map_err(ApiError::from)?;
    let redirect_uri = callback_url(base, &app.slug)?;
    let token_endpoint = provider.token_endpoint().unwrap_or_default();
    let extra_params = provider.extra_token_params();
    let tokens = exchange::exchange(
        state,
        &Exchange {
            token_endpoint: &token_endpoint,
            client_id: &app.client_id,
            client_secret: client_secret.as_deref(),
            code,
            redirect_uri: &redirect_uri,
            code_verifier: Some(code_verifier),
            auth_method: provider.auth_method(),
            require_refresh_token: provider.require_refresh_token(),
            extra_params: &extra_params,
        },
    )
    .await?;
    provider.validate_result(&tokens)?;
    let identity = provider.identity(state, &tokens, &app.client_id).await?;
    let credential_id =
        upsert_credential(state, user, app, provider, flow_state, &tokens, &identity).await?;
    crate::principals::effects::credential_changed(state, credential_id).await;
    Ok((credential_id, identity))
}

#[derive(sqlx::FromRow)]
struct Existing {
    id: i64,
    labels: Value,
    early_refresh_slack_seconds: i32,
    early_refresh_fraction: f64,
    max_refresh_interval_seconds: i32,
}

/// One credential per (app, provider account). A new one gets its identity,
/// endpoint, and external user key; every consent re-applies the token set
/// and revives a dead credential, writing only the columns a consent owns.
async fn upsert_credential(
    state: &AppState,
    user: &AuthUser,
    app: &App,
    provider: &Provider,
    flow_state: &FlowState,
    tokens: &TokenSet,
    identity: &Identity,
) -> Result<i64, Failure> {
    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    if app.shared {
        // One account per shared app: serialize consents and compare.
        sqlx::query("SELECT id FROM oauth_apps WHERE id = $1 FOR UPDATE")
            .bind(app.id)
            .execute(&mut *tx)
            .await?;
        let first: Option<Option<String>> = sqlx::query_scalar(
            "SELECT provider_subject FROM broker_credentials WHERE oauth_app_id = $1 ORDER BY id LIMIT 1",
        )
        .bind(app.id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(subject) = first
            && subject.as_deref() != Some(identity.subject.as_str())
        {
            return Err(
                ExchangeError::new("oauth", "Reconnect the same shared account")
                    .with_code("account_mismatch")
                    .into(),
            );
        }
    }
    let existing: Option<Existing> = sqlx::query_as(
        "SELECT id, labels, early_refresh_slack_seconds, early_refresh_fraction, \
         max_refresh_interval_seconds FROM broker_credentials \
         WHERE oauth_app_id = $1 AND provider_subject = $2 FOR UPDATE",
    )
    .bind(app.id)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;

    let mut labels: Map<String, Value> = existing
        .as_ref()
        .and_then(|row| row.labels.as_object().cloned())
        .unwrap_or_default();
    labels.extend(identity.labels.clone());
    if app.provider == ProviderKey::Slack.as_str()
        && let Some(team) = identity.team_id.as_deref()
    {
        labels.insert("slack_team_id".into(), team.into());
    }
    let scopes = if tokens.scopes.is_empty() {
        flow_state.scopes.clone()
    } else {
        tokens.scopes.clone()
    };
    let refreshable = provider.refreshable_result(tokens);
    let expires_at: Option<NaiveDateTime> = match tokens.expires_in {
        Some(seconds) if seconds > 0 => Some(now + Duration::seconds(seconds)),
        _ if refreshable => Some(now + Duration::seconds(300)),
        _ => None,
    };
    let (slack, fraction, max_interval) = existing.as_ref().map_or(
        (
            credentials::DEFAULT_SLACK_SECONDS,
            credentials::DEFAULT_FRACTION,
            credentials::DEFAULT_MAX_INTERVAL_SECONDS,
        ),
        |row| {
            (
                row.early_refresh_slack_seconds,
                row.early_refresh_fraction,
                row.max_refresh_interval_seconds,
            )
        },
    );
    let next_attempt = refreshable
        .then(|| credentials::next_attempt_at(now, expires_at, now, slack, fraction, max_interval));
    let refresh_token = state
        .encryptor
        .encrypt_optional(tokens.refresh_token.as_deref())
        .map_err(ApiError::from)?;
    let access_token = state
        .encryptor
        .encrypt(&tokens.access_token)
        .map_err(ApiError::from)?;

    let id = match existing {
        Some(row) => {
            sqlx::query(
                "UPDATE broker_credentials SET provider_email = $2, scopes = $3, labels = $4, \
                 refresh_token = $5, access_token = $6, expires_at = $7, last_refresh = $8, \
                 failure_count = 0, dead = FALSE, dead_reason = NULL, next_attempt_at = $9, \
                 created_by_id = COALESCE(created_by_id, $10), updated_at = $8 WHERE id = $1",
            )
            .bind(row.id)
            .bind(&identity.email)
            .bind(json!(scopes))
            .bind(Value::Object(labels))
            .bind(refresh_token)
            .bind(access_token)
            .bind(expires_at)
            .bind(now)
            .bind(next_attempt)
            .bind(user.id)
            .execute(&mut *tx)
            .await?;
            row.id
        }
        None => {
            let foreign_id = format!(
                "{}-{}-{}",
                app.provider,
                app.slug,
                provider.foreign_id_subject(&identity.subject)
            );
            if !url_safe(&foreign_id) {
                return Err(Failure::Invalid);
            }
            let display = identity
                .name
                .clone()
                .or_else(|| identity.email.clone())
                .unwrap_or_else(|| identity.subject.clone());
            sqlx::query_scalar(
                "INSERT INTO broker_credentials (oauth_app_id, provider_subject, provider_email, \
                 foreign_id, name, token_endpoint, external_user_key, scopes, labels, refresh_token, \
                 access_token, expires_at, last_refresh, next_attempt_at, created_by_id, created_at, \
                 updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                 $15, $13, $13) RETURNING id",
            )
            .bind(app.id)
            .bind(&identity.subject)
            .bind(&identity.email)
            .bind(foreign_id)
            .bind(format!("{} – {display}", provider.display_name()))
            .bind(provider.token_endpoint().unwrap_or_default())
            .bind(URL_SAFE_NO_PAD.encode(random_bytes::<16>()))
            .bind(json!(scopes))
            .bind(Value::Object(labels))
            .bind(refresh_token)
            .bind(access_token)
            .bind(expires_at)
            .bind(now)
            .bind(next_attempt)
            .bind(user.id)
            .fetch_one(&mut *tx)
            .await?
        }
    };
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM broker_credentials WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    wrapping::ensure(&mut tx, provider, id, &oid("bcr", id), name.as_deref(), now).await?;
    credentials::bump_grantees(&mut tx, id).await?;
    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_keeps_first_occurrences_in_order() {
        let merged = union(vec!["a".into(), "b".into(), "a".into()], &["b", "c"]);
        assert_eq!(merged, ["a", "b", "c"]);
    }
}
