//! Console SSO: `GET /auth/{provider}/start` sends the browser to the IdP
//! with a signed `state`, a nonce, and (except Slack) PKCE, binding the
//! flow to the browser with an encrypted cookie; `GET
//! /auth/{provider}/callback` exchanges the code, links or provisions the
//! user, and signs them in. Failures go back to `/login` with an alert.
//!
//! Google and Slack id_tokens come straight from the token endpoint over
//! TLS (claims are checked, the signature is not); Okta is any OIDC server
//! found through discovery, and its id_token is verified against its JWKS.
//!
//! SECURITY: nothing here logs codes, tokens, or response bodies.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration as StdDuration, Instant},
};

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_auth::{csrf, random::random_bytes};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    pages::sign_in,
    provision::{ProvisionError, SsoIdentity, link_or_provision},
    settings::{self, SsoPolicy},
};
use crate::{
    ApiResult, AppState,
    auth::{Session, is_https, request_base},
    extract::{Path, Query},
    http::without_url,
    pages::redirect,
};

pub const STATE_PURPOSE: &str = "console_login_flow";
pub const FLOW_COOKIE: &str = "console_login_flow";
const FLOW_TTL: Duration = Duration::minutes(10);
const SCOPES: &str = "openid email profile";
const MAX_BODY: usize = 256 * 1024;

#[derive(Serialize, Deserialize)]
struct StateData {
    provider: String,
    nonce: String,
}

#[derive(Serialize, Deserialize)]
struct Flow {
    nonce: String,
    code_verifier: Option<String>,
}

/// Why a sign-in attempt failed; only the code is ever logged.
#[derive(Debug)]
struct Failure(&'static str);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provider {
    Google,
    Slack,
    Okta,
}

impl Provider {
    fn parse(key: &str) -> Option<Self> {
        match key {
            "google" => Some(Self::Google),
            "slack" => Some(Self::Slack),
            "okta" => Some(Self::Okta),
            _ => None,
        }
    }

    /// Slack's confidential OIDC exchange uses no PKCE.
    fn pkce(self) -> bool {
        self != Self::Slack
    }

    fn valid_issuers(self) -> &'static [&'static str] {
        match self {
            Self::Google => &["https://accounts.google.com", "accounts.google.com"],
            Self::Slack => &["https://slack.com"],
            Self::Okta => &[],
        }
    }
}

struct Endpoints {
    authorization: String,
    token: String,
    userinfo: Option<String>,
    jwks_uri: Option<String>,
    issuer: Option<String>,
}

/// Debug builds only: point Google/Slack at a local stub IdP.
fn test_base(state: &AppState) -> Option<String> {
    cfg!(debug_assertions)
        .then(|| {
            state
                .config
                .env
                .var_present("CENTAUR_CONSOLE_SSO_TEST_BASE_URL")
        })
        .flatten()
        .map(|base| base.trim_end_matches('/').to_owned())
}

async fn endpoints(state: &AppState, provider: Provider) -> Result<Endpoints, Failure> {
    let fixed = |authorization: &str, token: &str| Endpoints {
        authorization: authorization.to_owned(),
        token: token.to_owned(),
        userinfo: None,
        jwks_uri: None,
        issuer: None,
    };
    let base = test_base(state);
    Ok(match provider {
        Provider::Google => match base {
            Some(base) => fixed(
                &format!("{base}/google/authorize"),
                &format!("{base}/google/token"),
            ),
            None => fixed(
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
            ),
        },
        Provider::Slack => match base {
            Some(base) => fixed(
                &format!("{base}/slack/authorize"),
                &format!("{base}/slack/token"),
            ),
            None => fixed(
                "https://slack.com/openid/connect/authorize",
                "https://slack.com/api/openid.connect.token",
            ),
        },
        Provider::Okta => {
            let issuer = normalized_issuer(settings::setting(&state.config.env, "okta", "ISSUER"))?;
            let document = discovery(state, &issuer).await?;
            let field = |name: &str| document[name].as_str().map(str::to_owned);
            Endpoints {
                authorization: field("authorization_endpoint")
                    .ok_or(Failure("oidc_endpoint_invalid"))?,
                token: field("token_endpoint").ok_or(Failure("oidc_endpoint_invalid"))?,
                userinfo: field("userinfo_endpoint"),
                jwks_uri: field("jwks_uri"),
                issuer: Some(issuer),
            }
        }
    })
}

// --- OIDC discovery (Okta) --------------------------------------------------

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const CACHE_TTL: StdDuration = StdDuration::from_secs(3600);

/// An HTTPS issuer without userinfo, query, or fragment, no trailing slash.
/// (Debug builds also accept loopback http for tests.)
fn normalized_issuer(raw: Option<&str>) -> Result<String, Failure> {
    let invalid = Failure("oidc_issuer_invalid");
    let url = url::Url::parse(
        raw.ok_or(Failure("oidc_issuer_invalid"))?
            .trim()
            .trim_end_matches('/'),
    )
    .map_err(|_| Failure("oidc_issuer_invalid"))?;
    let loopback =
        cfg!(debug_assertions) && matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    if !(url.scheme() == "https" || (loopback && url.scheme() == "http"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// An endpoint the discovery document names must stay on the issuer's origin.
fn same_origin(endpoint: &str, issuer: &str) -> bool {
    match (url::Url::parse(endpoint), url::Url::parse(issuer)) {
        (Ok(endpoint), Ok(issuer)) => {
            endpoint.scheme() == issuer.scheme()
                && endpoint.host() == issuer.host()
                && endpoint.port_or_known_default() == issuer.port_or_known_default()
                && endpoint.username().is_empty()
                && endpoint.fragment().is_none()
        }
        _ => false,
    }
}

async fn fetch_json(state: &AppState, url: &str, bearer: Option<&str>) -> Result<Value, Failure> {
    let mut request = state
        .http
        .get(url)
        .header(header::ACCEPT, "application/json");
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|error| {
        tracing::warn!(error = %without_url(error), "OIDC request failed");
        Failure("oidc_request_failed")
    })?;
    if !response.status().is_success() {
        return Err(Failure("oidc_request_failed"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| Failure("oidc_request_failed"))?;
    if bytes.len() > MAX_BODY {
        return Err(Failure("oidc_document_invalid"));
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or(Failure("oidc_document_invalid"))
}

async fn cached(
    state: &AppState,
    key: &str,
    force: bool,
    fetch: impl std::future::Future<Output = Result<Value, Failure>>,
) -> Result<Value, Failure> {
    if !force
        && let Some((at, value)) = CACHE.lock().expect("discovery cache").get(key)
        && at.elapsed() < CACHE_TTL
    {
        return Ok(value.clone());
    }
    let _ = state;
    let value = fetch.await?;
    CACHE
        .lock()
        .expect("discovery cache")
        .insert(key.to_owned(), (Instant::now(), value.clone()));
    Ok(value)
}

async fn discovery(state: &AppState, issuer: &str) -> Result<Value, Failure> {
    cached(state, &format!("metadata/{issuer}"), false, async {
        let document = fetch_json(
            state,
            &format!("{issuer}/.well-known/openid-configuration"),
            None,
        )
        .await?;
        if document["issuer"].as_str() != Some(issuer) {
            return Err(Failure("oidc_issuer_mismatch"));
        }
        for field in [
            "authorization_endpoint",
            "token_endpoint",
            "userinfo_endpoint",
            "jwks_uri",
        ] {
            if !document[field]
                .as_str()
                .is_some_and(|url| same_origin(url, issuer))
            {
                return Err(Failure("oidc_endpoint_invalid"));
            }
        }
        Ok(document)
    })
    .await
}

// --- start ------------------------------------------------------------------

fn to_login(session: &Session, alert: &str) -> Response {
    session.lock().set_alert(alert);
    redirect("/login")
}

fn urlsafe<const N: usize>() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<N>())
}

fn configured_provider(state: &AppState, key: &str) -> Option<Provider> {
    settings::configured(&state.config.env, key)
        .then(|| Provider::parse(key))
        .flatten()
}

fn with_cookie(mut response: Response, cookie: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn redirect_uri(state: &AppState, headers: &HeaderMap, key: &str) -> String {
    format!("{}/auth/{key}/callback", request_base(state, headers))
}

/// `GET /auth/{provider}/start`.
pub async fn start(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> ApiResult {
    let Some(provider) = configured_provider(&state, &key) else {
        return Ok(to_login(&session, "That sign-in method is not available."));
    };
    let env = &state.config.env;
    let endpoints = match endpoints(&state, provider).await {
        Ok(endpoints) => endpoints,
        Err(Failure(code)) => {
            tracing::error!(provider = key, code, "console login start failed");
            return Ok(to_login(
                &session,
                "Sign in is temporarily unavailable. Please try again.",
            ));
        }
    };
    let now = state.now();
    let nonce = urlsafe::<32>();
    let verifier = provider.pkce().then(urlsafe::<64>);
    let signed = state.signer.sign(
        STATE_PURPOSE,
        &StateData {
            provider: key.clone(),
            nonce: nonce.clone(),
        },
        FLOW_TTL,
        now,
    );
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair(
            "client_id",
            settings::setting(env, &key, "CLIENT_ID").unwrap_or_default(),
        )
        .append_pair("redirect_uri", &redirect_uri(&state, &headers, &key))
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPES)
        .append_pair("state", &signed)
        .append_pair("nonce", &nonce);
    if let Some(verifier) = &verifier {
        query
            .append_pair(
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            )
            .append_pair("code_challenge_method", "S256");
    }
    let mut location = url::Url::parse(&endpoints.authorization)
        .map_err(|_| crate::ApiError::internal("authorization endpoint is not a URL"))?;
    location.set_query(Some(&query.finish()));
    let cookie = state.flow_cookies.set(
        FLOW_COOKIE,
        &Flow {
            nonce,
            code_verifier: verifier,
        },
        FLOW_TTL,
        is_https(&state, &headers),
    );
    Ok(with_cookie(redirect(location.as_str()), &cookie))
}

// --- callback ---------------------------------------------------------------

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// `GET /auth/{provider}/callback?code=&state=` (or `?error=`).
pub async fn callback(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(key): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> ApiResult {
    let clear = state.flow_cookies.clear(FLOW_COOKIE);
    let Some(provider) = configured_provider(&state, &key) else {
        return Ok(with_cookie(
            to_login(&session, "That sign-in method is not available."),
            &clear,
        ));
    };
    let response = finish(&state, &session, &headers, provider, &key, query).await;
    Ok(with_cookie(response, &clear))
}

async fn finish(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    provider: Provider,
    key: &str,
    query: CallbackQuery,
) -> Response {
    let invalid = || {
        to_login(
            session,
            "This sign-in link is invalid or expired. Start again.",
        )
    };
    let flow_state: Option<StateData> = query
        .state
        .as_deref()
        .and_then(|token| state.signer.verify(STATE_PURPOSE, token, state.now()));
    let Some(flow_state) = flow_state.filter(|data| data.provider == key) else {
        return invalid();
    };
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok());
    let flow: Option<Flow> = state.flow_cookies.read(FLOW_COOKIE, cookie);
    let Some(flow) = flow.filter(|flow| csrf::eq(&flow.nonce, &flow_state.nonce)) else {
        return invalid();
    };
    if query
        .error
        .as_deref()
        .is_some_and(|error| !error.is_empty())
    {
        return to_login(session, "Sign in was canceled.");
    }
    let identity = match identify(state, headers, provider, key, &query, &flow).await {
        Ok(identity) => identity,
        Err(Failure(code)) => {
            tracing::error!(provider = key, code, "console login exchange failed");
            return to_login(session, "Sign in failed. Please try again.");
        }
    };
    let policy = SsoPolicy::from_env(&state.config.env);
    let provisioned = match link_or_provision(&state.db, state.now(), key, &identity, &policy).await
    {
        Ok(user) => user,
        Err(ProvisionError::DomainNotAllowed) => {
            tracing::warn!(
                provider = key,
                "console login rejected by the SSO email domain list"
            );
            return to_login(
                session,
                "That email domain is not allowed to access the console.",
            );
        }
        Err(ProvisionError::Invalid(field)) => {
            tracing::error!(provider = key, field, "console login provisioning failed");
            return to_login(session, "Sign in failed while setting up your account.");
        }
        // Retried inside `link_or_provision`; only a second conflict reaches
        // here, which the retry reports as `Invalid`.
        Err(ProvisionError::Conflict) => {
            tracing::error!(
                provider = key,
                "console login provisioning conflicted twice"
            );
            return to_login(session, "Sign in failed while setting up your account.");
        }
        Err(ProvisionError::Database(error)) => {
            tracing::error!(%error, "console login provisioning failed");
            return to_login(session, "Sign in failed while setting up your account.");
        }
    };
    after_commit(state, key, provisioned.id, provisioned.slack_changed).await;
    if provisioned.status == "disabled" {
        return to_login(session, centaur_console_auth::denial::DISABLED_MESSAGE);
    }
    sign_in(
        state,
        session,
        provisioned.id,
        provisioned.admin,
        provisioned.status == "active",
        &provisioned.email,
    )
}

/// Work that follows a committed sign-in; failures never block it.
async fn after_commit(state: &AppState, provider: &str, user_id: i64, slack_changed: bool) {
    if provider != "slack" || !slack_changed {
        return;
    }
    crate::principals::effects::slack_identity_linked(state, user_id).await;
    if let Err(error) =
        centaur_console_db::personal_ssh_keys::apply_for_user(&state.db, user_id, state.now()).await
    {
        tracing::warn!(%error, "personal SSH key grant after sign-in failed");
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
}

async fn identify(
    state: &AppState,
    headers: &HeaderMap,
    provider: Provider,
    key: &str,
    query: &CallbackQuery,
    flow: &Flow,
) -> Result<SsoIdentity, Failure> {
    let env = &state.config.env;
    let client_id = settings::setting(env, key, "CLIENT_ID").unwrap_or_default();
    let client_secret = settings::setting(env, key, "CLIENT_SECRET").unwrap_or_default();
    let code = query
        .code
        .as_deref()
        .filter(|code| !code.is_empty())
        .ok_or(Failure("missing_code"))?;
    let endpoints = endpoints(state, provider).await?;

    let basic = provider == Provider::Okta
        && settings::setting(env, key, "TOKEN_ENDPOINT_AUTH_METHOD")
            .unwrap_or("client_secret_basic")
            == "client_secret_basic";
    if provider == Provider::Okta
        && !basic
        && settings::setting(env, key, "TOKEN_ENDPOINT_AUTH_METHOD") != Some("client_secret_post")
    {
        return Err(Failure("token_endpoint_auth_method_invalid"));
    }
    let redirect = redirect_uri(state, headers, key);
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect.as_str()),
    ];
    if let Some(verifier) = flow.code_verifier.as_deref() {
        form.push(("code_verifier", verifier));
    }
    let mut request = state
        .http
        .post(&endpoints.token)
        .header(header::ACCEPT, "application/json");
    if basic {
        request = request.basic_auth(client_id, Some(client_secret));
    } else {
        form.push(("client_id", client_id));
        form.push(("client_secret", client_secret));
    }
    let response = request.form(&form).send().await.map_err(|error| {
        tracing::warn!(error = %without_url(error), "token request failed");
        Failure("token_request_failed")
    })?;
    if !response.status().is_success() {
        return Err(Failure("token_request_failed"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| Failure("token_request_failed"))?;
    let tokens: TokenResponse = (bytes.len() <= MAX_BODY)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
        .ok_or(Failure("token_response_invalid"))?;
    let id_token = tokens
        .id_token
        .filter(|token| !token.is_empty())
        .ok_or(Failure("missing_id_token"))?;
    let now = state.now();
    match provider {
        Provider::Google | Provider::Slack => {
            let claims = unverified_claims(&id_token)?;
            let mut identity = check_claims(
                &claims,
                client_id,
                provider.valid_issuers(),
                &flow.nonce,
                now,
            )?;
            if provider == Provider::Slack {
                identity.team_id = claims["https://slack.com/team_id"]
                    .as_str()
                    .map(|team| team.trim().to_owned())
                    .filter(|team| !team.is_empty());
            }
            Ok(identity)
        }
        Provider::Okta => {
            let issuer = endpoints
                .issuer
                .as_deref()
                .ok_or(Failure("oidc_issuer_invalid"))?;
            let jwks_uri = endpoints
                .jwks_uri
                .as_deref()
                .ok_or(Failure("oidc_endpoint_invalid"))?;
            let jwks: JwkSet = serde_json::from_value(
                cached(
                    state,
                    &format!("jwks/{jwks_uri}"),
                    false,
                    fetch_json(state, jwks_uri, None),
                )
                .await?,
            )
            .map_err(|_| Failure("oidc_document_invalid"))?;
            let claims = verify_okta_token(&id_token, &jwks, issuer, client_id, now)?;
            let access_token = tokens
                .access_token
                .filter(|token| !token.is_empty())
                .ok_or(Failure("missing_access_token"))?;
            let userinfo = endpoints
                .userinfo
                .as_deref()
                .ok_or(Failure("oidc_endpoint_invalid"))?;
            let profile = fetch_json(state, userinfo, Some(&access_token))
                .await
                .map_err(|_| Failure("userinfo_request_failed"))?;
            okta_identity(&claims, &profile, &flow.nonce)
        }
    }
}

// --- id_token handling (pure, unit-tested) -------------------------------------

/// The payload of an id_token received over TLS from the token endpoint.
fn unverified_claims(id_token: &str) -> Result<Value, Failure> {
    let segment = id_token.split('.').nth(1).unwrap_or_default();
    URL_SAFE_NO_PAD
        .decode(segment.trim_end_matches('='))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .ok_or(Failure("parse"))
}

fn audience_matches(claims: &Value, client_id: &str) -> bool {
    match &claims["aud"] {
        Value::String(aud) => aud == client_id,
        Value::Array(auds) => auds.iter().any(|aud| aud.as_str() == Some(client_id)),
        _ => false,
    }
}

fn string_claim(claims: &Value, name: &str) -> Option<String> {
    claims[name]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Checks audience, issuer, expiry, and (when the IdP echoes one) nonce.
fn check_claims(
    claims: &Value,
    client_id: &str,
    issuers: &[&str],
    nonce: &str,
    now: DateTime<Utc>,
) -> Result<SsoIdentity, Failure> {
    if !audience_matches(claims, client_id) {
        return Err(Failure("id_token_aud_mismatch"));
    }
    if !claims["iss"]
        .as_str()
        .is_some_and(|iss| issuers.contains(&iss))
    {
        return Err(Failure("id_token_iss_invalid"));
    }
    if !claims["exp"]
        .as_i64()
        .is_some_and(|exp| exp > now.timestamp())
    {
        return Err(Failure("id_token_expired"));
    }
    if claims["nonce"]
        .as_str()
        .is_some_and(|echoed| !csrf::eq(echoed, nonce))
    {
        return Err(Failure("id_token_nonce_mismatch"));
    }
    Ok(SsoIdentity {
        subject: string_claim(claims, "sub").ok_or(Failure("id_token_missing_sub"))?,
        email: string_claim(claims, "email"),
        email_verified: claims["email_verified"] == Value::Bool(true),
        name: string_claim(claims, "name"),
        team_id: None,
    })
}

/// Verifies an Okta id_token: RS256 against the issuer's JWKS, with issuer,
/// audience, expiry, issue time, and the required claims.
fn verify_okta_token(
    id_token: &str,
    jwks: &JwkSet,
    issuer: &str,
    client_id: &str,
    now: DateTime<Utc>,
) -> Result<Value, Failure> {
    let bad = Failure("id_token_invalid");
    let header = jsonwebtoken::decode_header(id_token).map_err(|_| Failure("id_token_invalid"))?;
    if header.alg != Algorithm::RS256 {
        return Err(bad);
    }
    let jwk = header
        .kid
        .as_deref()
        .and_then(|kid| jwks.find(kid))
        .ok_or(Failure("id_token_invalid"))?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| Failure("id_token_invalid"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[client_id]);
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    let claims = jsonwebtoken::decode::<Value>(id_token, &key, &validation)
        .map_err(|_| Failure("id_token_invalid"))?
        .claims;
    let ok = claims["exp"]
        .as_i64()
        .is_some_and(|exp| exp > now.timestamp())
        && claims["iat"]
            .as_i64()
            .is_some_and(|iat| iat <= now.timestamp() + 60)
        && ["sub", "nonce"]
            .iter()
            .all(|name| claims[*name].is_string());
    if ok {
        Ok(claims)
    } else {
        Err(Failure("id_token_invalid"))
    }
}

/// The nonce must match the flow, and UserInfo must confirm the subject and
/// a verified email.
fn okta_identity(claims: &Value, profile: &Value, nonce: &str) -> Result<SsoIdentity, Failure> {
    if !claims["nonce"]
        .as_str()
        .is_some_and(|echoed| csrf::eq(echoed, nonce))
    {
        return Err(Failure("id_token_nonce_mismatch"));
    }
    let subject = string_claim(claims, "sub").ok_or(Failure("id_token_missing_sub"))?;
    if profile["sub"].as_str() != Some(subject.as_str()) {
        return Err(Failure("userinfo_subject_mismatch"));
    }
    let email = string_claim(profile, "email");
    if email.is_none() || profile["email_verified"] != Value::Bool(true) {
        return Err(Failure("userinfo_email_unverified"));
    }
    Ok(SsoIdentity {
        subject,
        email,
        email_verified: true,
        name: string_claim(profile, "name").or_else(|| string_claim(claims, "name")),
        team_id: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn claims(extra: Value) -> Value {
        let mut base = json!({
            "iss": "https://accounts.google.com", "aud": "client", "sub": "s-1",
            "exp": 1_800_000_600, "email": " A@Example.test ", "email_verified": true,
            "name": "Ada",
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    }

    #[test]
    fn google_claims_are_checked() {
        let issuers = Provider::Google.valid_issuers();
        let identity = check_claims(&claims(json!({})), "client", issuers, "n", now()).unwrap();
        assert_eq!(identity.subject, "s-1");
        assert_eq!(identity.email.as_deref(), Some("A@Example.test"));
        assert!(identity.email_verified);
        for (extra, code) in [
            (json!({"aud": "other"}), "id_token_aud_mismatch"),
            (json!({"iss": "https://evil.test"}), "id_token_iss_invalid"),
            (json!({"exp": 1_700_000_000}), "id_token_expired"),
            (json!({"nonce": "different"}), "id_token_nonce_mismatch"),
            (json!({"sub": " "}), "id_token_missing_sub"),
        ] {
            let error = check_claims(&claims(extra), "client", issuers, "n", now()).unwrap_err();
            assert_eq!(error.0, code);
        }
        // Only a boolean `true` verifies an email.
        let unverified = check_claims(
            &claims(json!({"email_verified": "true"})),
            "client",
            issuers,
            "n",
            now(),
        )
        .unwrap();
        assert!(!unverified.email_verified);
        assert!(
            check_claims(
                &claims(json!({"nonce": "n"})),
                "client",
                issuers,
                "n",
                now()
            )
            .is_ok()
        );
    }

    #[test]
    fn okta_identity_needs_matching_subject_nonce_and_verified_email() {
        let id_claims = json!({"sub": "u1", "nonce": "n", "name": "Token Name"});
        let profile = json!({"sub": "u1", "email": "a@b.test", "email_verified": true});
        let identity = okta_identity(&id_claims, &profile, "n").unwrap();
        assert_eq!(
            (identity.subject.as_str(), identity.name.as_deref()),
            ("u1", Some("Token Name"))
        );
        for (claims, profile, nonce, code) in [
            (
                id_claims.clone(),
                profile.clone(),
                "other",
                "id_token_nonce_mismatch",
            ),
            (
                id_claims.clone(),
                json!({"sub": "u2", "email": "a@b.test", "email_verified": true}),
                "n",
                "userinfo_subject_mismatch",
            ),
            (
                id_claims.clone(),
                json!({"sub": "u1", "email": "a@b.test", "email_verified": false}),
                "n",
                "userinfo_email_unverified",
            ),
            (
                id_claims.clone(),
                json!({"sub": "u1", "email_verified": true}),
                "n",
                "userinfo_email_unverified",
            ),
        ] {
            assert_eq!(okta_identity(&claims, &profile, nonce).unwrap_err().0, code);
        }
    }

    #[test]
    fn issuers_and_endpoints_must_be_https_and_same_origin() {
        assert_eq!(
            normalized_issuer(Some(" https://idp.example.test/ ")).unwrap(),
            "https://idp.example.test"
        );
        for bad in [
            "http://idp.example.test",
            "https://u:p@idp.example.test",
            "https://idp.example.test?x=1",
            "https://idp.example.test#f",
            "nope",
        ] {
            assert!(normalized_issuer(Some(bad)).is_err(), "{bad}");
        }
        assert!(normalized_issuer(None).is_err());
        assert!(same_origin(
            "https://idp.example.test/oauth2/v1/token",
            "https://idp.example.test"
        ));
        assert!(!same_origin(
            "https://evil.test/token",
            "https://idp.example.test"
        ));
        assert!(!same_origin(
            "https://idp.example.test:8443/token",
            "https://idp.example.test"
        ));
        assert!(!same_origin(
            "http://idp.example.test/token",
            "https://idp.example.test"
        ));
    }

    #[test]
    fn parses_unverified_payloads() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"sub":"x"}"#);
        assert_eq!(
            unverified_claims(&format!("h.{payload}.s")).unwrap()["sub"],
            "x"
        );
        assert!(unverified_claims("nonsense").is_err());
        assert!(unverified_claims("h.!!.s").is_err());
    }

    // A throwaway RSA key generated for these tests only.
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7pHDVqZI6pLW9\nhCmW0EMs2wIRGlriJew0VCQKQi1uTKZrB4KDwhlFiyEPRT2OyKJEUSsWYiz1j7Rk\n+KR8+Sr1m3oK/VEZBhi8sA6cdi+ZMroyBg/1UWgPaUi1+9YGMMkw67DmeEoIs1e8\n4FkRNNaelvr/5FmT+JSYpzJtQ74oHGZS1s+VerG8/H1gjHI3dA9eqfIb97a11S+m\nZfxbvsJ4gtZpzl5oDlDFpmQZkPpKOJAY73/7Rqs+iWtoZDPmiqeibhLCGp6YHPAu\nzebvh6LLR/zGQQQrwjPdmyaPMPRNlG0zKt9fUKrE+wozO8C9M9k5DKcEOAp8423X\noY0u/RZTAgMBAAECggEAApTE/pNqEQdx/oziW5I60lOi6CRSKrk10hR0OHXnlRRV\n17QPPgu9+EBtMH4V1jB5VvI84v5vuuMCqr7m2Fc/GYjNeXg5lNwKHBJZ6RoRFozl\nlcmSj5/SjB+5IdO4gnZKVjNvyAPQlOUOqGj3oK5241CVXE7CUrBBLwZhwbIEXB3d\n0jRuF6ttmcvk46IW/M1dK8UfU1r68Y2vtyd1R9z6cGvAEo1qrweLvieSMvNc43If\nT29taxY4erADRcZgM1mI7Siv0LTaaHqTtxqzDQiG8MZ885ALPucB3q1b7eGz/3w8\nS1PSMTx00tei7CNzRHCPSBUCzjqKOuxKKdq3s5WfrQKBgQD2dOGvGLmOiUQjcieP\nxyFt2uCdIPY+x1KLQXOt6vjxfFwU5xeff3uvTeKNVH+dcWDKKpXfDG+TDqNgL6n2\n1vyDovIRlU6QcwESCmP7TIuegBrr/sQHe0A2e464pcX//VwhIEwXoPb6dwGiTX/m\nZ0rlgmvKfRZ28YCMIsmmH0SHLwKBgQDC6Ij90IUkjuBWqGayMb/TUE3rWXs1DAGk\nlONqmroWcc27mm2ytodWKJcDWNt0L+CA6OMcvYghuektyMz80bmCgV56mly0veO6\nc87UAD8u8geEV1IpbiDvszME6oQetqrM1jWreOCO2mjKEseFvlGIVQBOwhlaVbXG\n1Io4ebEaHQKBgQCAgo/PmoffIApMpVClswLmYnQu2qosMAkFnfpw3I40dVEagSmb\nPIFE9srEQSRN93mt0SjW5ogjWUWteQ0aLh37kfLBYmZWAfGz84EQ32gKHOhs2Cpl\ncEoaXrEb7Szif70TdODYjVFbZsSOWfTwMjomlKwaKMGXAfSync0h9znndwKBgADC\n58YWCm8aH8Y24i/QAe/RYHyK3PBYG6mI61p/QDrWNveEoukPzXeIsQ5SSokpprba\nMgjqtJ1tHHwRTLuwFglsd/ICqFoMY9dWnHg3iAem6mCZ9rgyte45xPk/8UQtcn1a\nWNtSnY3xTHd9m0nqdGcT7exg1biEnTEpo2CVGRwBAoGAN/0CvTpGsBdX9QJUD7aW\nOwOuHHvmq4qYeOMOUWzIXa3BIkcI8N+Oqwf2PhqT8auF8sK4r3ygA7nDEnf3GLF4\n0IpQbxcQf1f0cvcCiHfZbdtD/250y8lcalFIokasW6cYTYjPVKd7hR4t+/oClT9J\n1yxJYV+7QIn/4W7SS4oOMM8=\n-----END PRIVATE KEY-----\n";
    const TEST_N: &str = "u6Rw1amSOqS1vYQpltBDLNsCERpa4iXsNFQkCkItbkymaweCg8IZRYshD0U9jsiiRFErFmIs9Y-0ZPikfPkq9Zt6Cv1RGQYYvLAOnHYvmTK6MgYP9VFoD2lItfvWBjDJMOuw5nhKCLNXvOBZETTWnpb6_-RZk_iUmKcybUO-KBxmUtbPlXqxvPx9YIxyN3QPXqnyG_e2tdUvpmX8W77CeILWac5eaA5QxaZkGZD6SjiQGO9_-0arPolraGQz5oqnom4SwhqemBzwLs3m74eiy0f8xkEEK8Iz3ZsmjzD0TZRtMyrfX1CqxPsKMzvAvTPZOQynBDgKfONt16GNLv0WUw";

    fn jwks() -> JwkSet {
        serde_json::from_value(json!({"keys": [
            {"kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig", "n": TEST_N, "e": "AQAB"},
        ]}))
        .unwrap()
    }

    fn okta_token(kid: Option<&str>, extra: Value) -> String {
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_owned);
        let mut claims = json!({
            "iss": "https://idp.example.test", "aud": "client", "sub": "u1", "nonce": "n",
            "iat": 1_800_000_000 - 10, "exp": 1_800_000_000 + 600,
        });
        claims
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        jsonwebtoken::encode(
            &header,
            &claims,
            &jsonwebtoken::EncodingKey::from_rsa_pem(TEST_KEY.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn okta_id_tokens_are_verified_against_the_jwks() {
        let verify = |token: &str| {
            verify_okta_token(token, &jwks(), "https://idp.example.test", "client", now())
        };
        assert_eq!(
            verify(&okta_token(Some("k1"), json!({}))).unwrap()["sub"],
            "u1"
        );
        for (token, why) in [
            (okta_token(Some("k1"), json!({"aud": "other"})), "audience"),
            (
                okta_token(Some("k1"), json!({"iss": "https://evil.test"})),
                "issuer",
            ),
            (
                okta_token(Some("k1"), json!({"exp": 1_800_000_000 - 1})),
                "expired",
            ),
            (
                okta_token(Some("k1"), json!({"iat": 1_800_000_000 + 3600})),
                "issued in the future",
            ),
            (okta_token(Some("unknown"), json!({})), "unknown key id"),
            (okta_token(None, json!({})), "no key id"),
            (okta_token(Some("k1"), json!({"nonce": null})), "no nonce"),
        ] {
            assert_eq!(verify(&token).unwrap_err().0, "id_token_invalid", "{why}");
        }
        // Signed by another key, or with a symmetric algorithm: refused.
        let forged = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &json!({"iss": "https://idp.example.test", "aud": "client", "sub": "u1", "nonce": "n", "exp": 1_800_000_600}),
            &jsonwebtoken::EncodingKey::from_secret(TEST_N.as_bytes()),
        )
        .unwrap();
        assert!(verify(&forged).is_err());
        let mut tampered = okta_token(Some("k1"), json!({}));
        tampered.push('A');
        assert!(verify(&tampered).is_err());
    }
}
