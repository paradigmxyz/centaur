//! Console SSO: `/auth/:provider/start` and `/auth/:provider/callback`.
//!
//! A signed state parameter, an encrypted same-site flow cookie binding the
//! callback to the browser, optional PKCE, and provider-specific identity
//! extraction. Provisioning SQL lives in `centaur-console-db`.

use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_db::{
    ConsoleUserStatus,
    areas::auth_pages::{SsoIdentity, link_or_provision_user},
};
use hmac::{Hmac, KeyInit, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use super::{
    APP_CSP, callback_url, console_config, console_env, login_redirect_with_csp, public_base_url,
};
use crate::{
    AppState,
    console::{
        pages::{landing_path, login_error, redirect},
        request::ConsoleQuery,
        session_cookie::{self, ConsoleSession},
    },
    console_session::ConsoleSessionConfig,
};

const STATE_TTL_SECONDS: i64 = 600;
const FLOW_COOKIE: &str = "console_login_flow";
const UNAVAILABLE_ALERT: &str = login_error::SSO_UNAVAILABLE;
const START_FAILED_ALERT: &str = login_error::SSO_START_FAILED;
const INVALID_FLOW_ALERT: &str = login_error::SSO_INVALID_FLOW;
const CANCELED_ALERT: &str = login_error::SSO_CANCELED;
const EXCHANGE_FAILED_ALERT: &str = login_error::SSO_EXCHANGE_FAILED;
const DOMAIN_ALERT: &str = login_error::SSO_DOMAIN_NOT_ALLOWED;
const PROVISION_FAILED_ALERT: &str = login_error::SSO_PROVISION_FAILED;
const DISABLED_ALERT: &str = login_error::ACCOUNT_DISABLED;

#[derive(Clone, Debug)]
struct Provider {
    key: String,
    client_id: String,
    client_secret: String,
    kind: ProviderKind,
}

#[derive(Clone, Debug)]
enum ProviderKind {
    Google,
    Slack,
    Okta { issuer: String },
}

impl Provider {
    fn load(key: &str) -> Option<Self> {
        let upper = key.to_ascii_uppercase();
        let client_id = console_env(&format!("{upper}_CLIENT_ID"))?;
        let client_secret = console_env(&format!("{upper}_CLIENT_SECRET"))?;
        if client_id.trim().is_empty() || client_secret.trim().is_empty() {
            return None;
        }
        let kind = match key {
            "google" => ProviderKind::Google,
            "slack" => ProviderKind::Slack,
            "okta" => {
                let issuer = console_env("OKTA_ISSUER")?;
                if issuer.trim().is_empty() {
                    return None;
                }
                ProviderKind::Okta { issuer }
            }
            _ => return None,
        };
        Some(Self {
            key: key.to_owned(),
            client_id,
            client_secret,
            kind,
        })
    }

    fn scopes(&self) -> &'static str {
        "openid email profile"
    }

    fn pkce(&self) -> bool {
        !matches!(self.kind, ProviderKind::Slack)
    }

    async fn endpoints(&self) -> Result<(String, String), ()> {
        match &self.kind {
            ProviderKind::Google => Ok((
                "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
                "https://oauth2.googleapis.com/token".to_owned(),
            )),
            ProviderKind::Slack => Ok((
                "https://slack.com/openid/connect/authorize".to_owned(),
                "https://slack.com/api/openid.connect.token".to_owned(),
            )),
            ProviderKind::Okta { issuer } => {
                let metadata = okta_metadata(issuer).await?;
                Ok((metadata.authorization_endpoint, metadata.token_endpoint))
            }
        }
    }

    fn client_auth_method(&self) -> Option<&'static str> {
        match self.kind {
            ProviderKind::Google | ProviderKind::Slack => Some("client_secret_post"),
            ProviderKind::Okta { .. } => match console_env("OKTA_TOKEN_ENDPOINT_AUTH_METHOD")
                .filter(|value| !value.trim().is_empty())
                .as_deref()
            {
                None | Some("client_secret_basic") => Some("client_secret_basic"),
                Some("client_secret_post") => Some("client_secret_post"),
                Some(_) => None,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StatePayload {
    provider: String,
    nonce: String,
    exp: i64,
}

#[derive(Serialize, Deserialize)]
struct FlowPayload {
    nonce: String,
    #[serde(default)]
    code_verifier: Option<String>,
}

#[derive(Debug)]
struct TokenResult {
    access_token: String,
    id_token: Option<String>,
}

/// The provider's callback query string.
#[derive(Debug, Deserialize)]
pub(super) struct CallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// GET /auth/:provider/start
pub(super) async fn start(
    State(state): State<AppState>,
    Path(provider_key): Path<String>,
    headers: HeaderMap,
) -> Response {
    let config = console_config(&state).clone();
    let Some(provider) = Provider::load(&provider_key) else {
        return login_redirect_with_csp(&config, &headers, UNAVAILABLE_ALERT, Vec::new(), APP_CSP);
    };

    let nonce = random_token(32);
    let code_verifier = provider.pkce().then(|| random_token(64));
    let state_value = match sign_state(&config, &provider, &nonce) {
        Some(value) => value,
        None => {
            return login_redirect_with_csp(
                &config,
                &headers,
                START_FAILED_ALERT,
                Vec::new(),
                APP_CSP,
            );
        }
    };
    let (authorization_endpoint, _) = match provider.endpoints().await {
        Ok(endpoints) => endpoints,
        Err(()) => {
            return login_redirect_with_csp(
                &config,
                &headers,
                START_FAILED_ALERT,
                vec![clear_flow_cookie(&config)],
                APP_CSP,
            );
        }
    };

    let mut query = vec![
        ("client_id", provider.client_id.clone()),
        (
            "redirect_uri",
            callback_url(&public_base_url(&config), &provider.key),
        ),
        ("response_type", "code".to_owned()),
        ("scope", provider.scopes().to_owned()),
        ("state", state_value),
        ("nonce", nonce.clone()),
    ];
    if let Some(verifier) = code_verifier.as_deref() {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        query.push(("code_challenge", challenge));
        query.push(("code_challenge_method", "S256".to_owned()));
    }
    let location = append_query(&authorization_endpoint, &query);

    let mut cookies = Vec::new();
    if let Some(cookie) = flow_cookie(
        &config,
        &FlowPayload {
            nonce,
            code_verifier,
        },
    ) {
        cookies.push(cookie);
    }
    redirect(&location, &cookies, Some(APP_CSP))
}

/// GET /auth/:provider/callback
pub(super) async fn callback(
    State(state): State<AppState>,
    Path(provider_key): Path<String>,
    headers: HeaderMap,
    ConsoleQuery(params): ConsoleQuery<CallbackQuery>,
) -> Response {
    let config = console_config(&state).clone();
    let Some(provider) = Provider::load(&provider_key) else {
        return failed(&config, &headers, UNAVAILABLE_ALERT);
    };

    let state_payload = verify_state(&config, params.state.as_deref().unwrap_or_default());
    let Some(state_payload) = state_payload else {
        return failed(&config, &headers, INVALID_FLOW_ALERT);
    };
    if state_payload.provider != provider.key {
        return failed(&config, &headers, INVALID_FLOW_ALERT);
    }
    let Some(flow) = read_flow_cookie(&config, &headers) else {
        return failed(&config, &headers, INVALID_FLOW_ALERT);
    };
    if state_payload.nonce.as_bytes() != flow.nonce.as_bytes() {
        return failed(&config, &headers, INVALID_FLOW_ALERT);
    }

    if params
        .error
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return failed(&config, &headers, CANCELED_ALERT);
    }
    let code = params.code.clone().unwrap_or_default();
    let redirect_uri = callback_url(&public_base_url(&config), &provider.key);
    let Some(auth_method) = provider.client_auth_method() else {
        return failed(&config, &headers, EXCHANGE_FAILED_ALERT);
    };
    let (_, token_endpoint) = match provider.endpoints().await {
        Ok(endpoints) => endpoints,
        Err(()) => return failed(&config, &headers, EXCHANGE_FAILED_ALERT),
    };
    let result = match exchange_code(
        &provider,
        &token_endpoint,
        auth_method,
        &code,
        &redirect_uri,
        flow.code_verifier.as_deref().unwrap_or_default(),
    )
    .await
    {
        Ok(result) => result,
        Err(()) => return failed(&config, &headers, EXCHANGE_FAILED_ALERT),
    };
    let identity = match provider.identity(&result, &flow.nonce).await {
        Ok(identity) => identity,
        Err(()) => return failed(&config, &headers, EXCHANGE_FAILED_ALERT),
    };

    if !sso_email_allowed(identity.email.as_deref()) {
        return failed(&config, &headers, DOMAIN_ALERT);
    }
    let bootstrap =
        identity.email_verified && identity.email.as_deref().is_some_and(bootstrap_admin);
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let user = match link_or_provision_user(&pool, &provider.key, &identity, bootstrap).await {
        Ok(user) => user,
        Err(error) => {
            tracing::error!("console login provisioning failed: {error}");
            return failed(&config, &headers, PROVISION_FAILED_ALERT);
        }
    };
    if user.status == ConsoleUserStatus::Disabled {
        return failed(&config, &headers, DISABLED_ALERT);
    }

    let previous = config.session_from_headers(&headers);
    let remembered = previous
        .as_ref()
        .and_then(|session| session.return_to())
        .and_then(crate::console::return_path::safe);
    let mut session = ConsoleSession::signed_in(user.id);
    let location = if user.status == ConsoleUserStatus::Active {
        remembered.unwrap_or_else(|| landing_path(&user))
    } else {
        if let Some(remembered) = remembered {
            session.set_return_to(remembered);
        }
        "/pending".to_owned()
    };
    let mut cookies = vec![clear_flow_cookie(&config)];
    if let Some(cookie) = config.cookie().set_cookie_header(&session) {
        cookies.push(cookie);
    }
    redirect(&location, &cookies, Some(APP_CSP))
}

/// A callback failure: the flow cookie is always cleared and the login page
/// renders the alert.
fn failed(config: &ConsoleSessionConfig, headers: &HeaderMap, alert: &str) -> Response {
    login_redirect_with_csp(
        config,
        headers,
        alert,
        vec![clear_flow_cookie(config)],
        APP_CSP,
    )
}

impl Provider {
    async fn identity(&self, result: &TokenResult, nonce: &str) -> Result<SsoIdentity, ()> {
        match &self.kind {
            ProviderKind::Google => id_token_identity(
                result.id_token.as_deref(),
                &self.client_id,
                &["https://accounts.google.com", "accounts.google.com"],
            ),
            ProviderKind::Slack => {
                let mut identity = id_token_identity(
                    result.id_token.as_deref(),
                    &self.client_id,
                    &["https://slack.com"],
                )?;
                let claims = decode_claims(result.id_token.as_deref().ok_or(())?)?;
                identity.team_id = claims
                    .get("https://slack.com/team_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned);
                Ok(identity)
            }
            ProviderKind::Okta { issuer } => {
                okta_identity(result, nonce, &self.client_id, &normalize_issuer(issuer)?).await
            }
        }
    }
}

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0; bytes];
    getrandom::fill(&mut buffer).expect("operating system randomness");
    URL_SAFE_NO_PAD.encode(buffer)
}

const STATE_KEY_LABEL: &[u8] = b"console-login-state-v1";
const FLOW_KEY_LABEL: &[u8] = b"console-login-flow-v1";
const FLOW_AAD: &[u8] = b"console_login_flow:v1";

fn sign_state(config: &ConsoleSessionConfig, provider: &Provider, nonce: &str) -> Option<String> {
    let payload = StatePayload {
        provider: provider.key.clone(),
        nonce: nonce.to_owned(),
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + STATE_TTL_SECONDS,
    };
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).ok()?);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&config.cookie().derive_key(STATE_KEY_LABEL)).ok()?;
    mac.update(encoded.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Some(format!("v1.{encoded}.{signature}"))
}

fn verify_state(config: &ConsoleSessionConfig, value: &str) -> Option<StatePayload> {
    let rest = value.strip_prefix("v1.")?;
    let (encoded, signature) = rest.split_once('.')?;
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&config.cookie().derive_key(STATE_KEY_LABEL)).ok()?;
    mac.update(encoded.as_bytes());
    let expected = mac.finalize().into_bytes();
    if signature.len() != expected.len()
        || signature.as_slice().ct_eq(expected.as_slice()).unwrap_u8() != 1
    {
        return None;
    }
    let payload: StatePayload =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()?;
    if payload.exp < time::OffsetDateTime::now_utc().unix_timestamp() {
        return None;
    }
    Some(payload)
}

fn flow_cookie(config: &ConsoleSessionConfig, payload: &FlowPayload) -> Option<HeaderValue> {
    let plaintext = serde_json::to_vec(payload).ok()?;
    let value = session_cookie::seal(
        &config.cookie().derive_key(FLOW_KEY_LABEL),
        FLOW_AAD,
        &plaintext,
    )?;
    let secure = if config.public_origin().starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{FLOW_COOKIE}={value}; Path=/; Max-Age={STATE_TTL_SECONDS}; HttpOnly; SameSite=Lax{secure}"
    ))
    .ok()
}

fn read_flow_cookie(config: &ConsoleSessionConfig, headers: &HeaderMap) -> Option<FlowPayload> {
    let raw = session_cookie::cookie_value(headers, FLOW_COOKIE)?;
    let plaintext =
        session_cookie::open(&config.cookie().derive_key(FLOW_KEY_LABEL), FLOW_AAD, raw)?;
    serde_json::from_slice(&plaintext).ok()
}

fn clear_flow_cookie(config: &ConsoleSessionConfig) -> HeaderValue {
    let secure = if config.public_origin().starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{FLOW_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{secure}"
    ))
    .expect("static cookie header")
}

fn append_query(endpoint: &str, query: &[(&str, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in query {
        serializer.append_pair(key, value);
    }
    let query = serializer.finish();
    // The query replaces any pre-existing query rather than appending to it.
    if let Ok(mut url) = Url::parse(endpoint) {
        url.set_query(Some(&query));
        return url.to_string();
    }
    let mut url = endpoint.to_owned();
    if url.contains('?') {
        url.push('&');
    } else {
        url.push('?');
    }
    url.push_str(&query);
    url
}

/// Encodes spaces as `+` rather than `%20`.
fn form_component(value: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("value", value)
        .finish()
        .strip_prefix("value=")
        .unwrap_or_default()
        .to_owned()
}

async fn exchange_code(
    provider: &Provider,
    token_endpoint: &str,
    auth_method: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResult, ()> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code.to_owned()),
        ("redirect_uri", redirect_uri.to_owned()),
    ];
    let mut headers = Vec::new();
    match auth_method {
        "client_secret_basic" => {
            if provider.client_secret.is_empty() {
                return Err(());
            }
            let credentials = format!(
                "{}:{}",
                form_component(&provider.client_id),
                form_component(&provider.client_secret)
            );
            headers.push((
                "authorization".to_owned(),
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(credentials)
                ),
            ));
        }
        _ => {
            form.push(("client_id", provider.client_id.clone()));
            form.push(("client_secret", provider.client_secret.clone()));
        }
    }
    if !code_verifier.is_empty() {
        form.push(("code_verifier", code_verifier.to_owned()));
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| ())?;
    let mut request = client.post(token_endpoint);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.form(&form).send().await.map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let bytes = response.bytes().await.map_err(|_| ())?;
    if bytes.len() > 64 * 1024 {
        return Err(());
    }
    let parsed: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    let payload = parsed
        .get("authed_user")
        .filter(|value| {
            value
                .get("access_token")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        .unwrap_or(&parsed);
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let id_token = payload
        .get("id_token")
        .and_then(Value::as_str)
        .or_else(|| parsed.get("id_token").and_then(Value::as_str))
        .map(str::to_owned);
    Ok(TokenResult {
        access_token: access_token.to_owned(),
        id_token,
    })
}

fn id_token_identity(
    id_token: Option<&str>,
    client_id: &str,
    valid_issuers: &[&str],
) -> Result<SsoIdentity, ()> {
    let id_token = id_token.filter(|value| !value.is_empty()).ok_or(())?;
    let claims = decode_claims(id_token)?;
    if claims.get("aud").and_then(Value::as_str) != Some(client_id) {
        return Err(());
    }
    if !claims
        .get("iss")
        .and_then(Value::as_str)
        .is_some_and(|issuer| valid_issuers.contains(&issuer))
    {
        return Err(());
    }
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(())?
        .to_owned();
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let email_verified = claims.get("email_verified") == Some(&Value::Bool(true));
    let name = claims
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    Ok(SsoIdentity {
        subject,
        email,
        email_verified,
        name,
        team_id: None,
    })
}

fn decode_claims(id_token: &str) -> Result<Map<String, Value>, ()> {
    let segment = id_token.split('.').nth(1).ok_or(())?;
    // JWT base64url segments are usually unpadded, but padding is tolerated.
    let segment = segment.trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD.decode(segment).map_err(|_| ())?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or(())
}

fn sso_email_allowed(email: Option<&str>) -> bool {
    let domains = env_list("SSO_EMAIL_DOMAINS");
    if domains.is_empty() {
        return true;
    }
    // An address without `@` is compared verbatim and a domain list can admit
    // it.
    let email = email.unwrap_or_default().trim().to_ascii_lowercase();
    let domain = email
        .split_once('@')
        .map_or(email.as_str(), |(_, domain)| domain);
    domains.iter().any(|allowed| allowed == domain)
}

fn bootstrap_admin(email: &str) -> bool {
    let email = email.trim().to_ascii_lowercase();
    env_list("BOOTSTRAP_ADMINS")
        .iter()
        .any(|value| value == &email)
}

/// A comma/whitespace-separated environment list, trimmed and lowercased.
fn env_list(name: &str) -> Vec<String> {
    console_env(name)
        .unwrap_or_default()
        .split(|character: char| character == ',' || character.is_whitespace())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

/// Configured SSO providers in Console's display order.
pub(super) fn providers() -> Vec<&'static str> {
    ["google", "slack", "okta"]
        .into_iter()
        .filter(|key| Provider::load(key).is_some())
        .collect()
}

#[derive(Debug)]
struct OktaMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    jwks_uri: String,
}

async fn okta_metadata(issuer: &str) -> Result<OktaMetadata, ()> {
    let issuer = normalize_issuer(issuer)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| ())?;
    let url = format!("{issuer}/.well-known/openid-configuration");
    let response = client.get(&url).send().await.map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let bytes = response.bytes().await.map_err(|_| ())?;
    if bytes.len() > 256 * 1024 {
        return Err(());
    }
    let metadata: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    let text = |name: &str| {
        metadata
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(())
    };
    let discovered_issuer = text("issuer")?;
    if discovered_issuer != issuer {
        return Err(());
    }
    let authorization_endpoint = secure_endpoint(&text("authorization_endpoint")?, &issuer)?;
    let token_endpoint = secure_endpoint(&text("token_endpoint")?, &issuer)?;
    let userinfo_endpoint = secure_endpoint(&text("userinfo_endpoint")?, &issuer)?;
    let jwks_uri = secure_endpoint(&text("jwks_uri")?, &issuer)?;
    Ok(OktaMetadata {
        issuer,
        authorization_endpoint,
        token_endpoint,
        userinfo_endpoint,
        jwks_uri,
    })
}

fn normalize_issuer(value: &str) -> Result<String, ()> {
    let value = value.trim().strip_suffix('/').unwrap_or(value.trim());
    let url = Url::parse(value).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(value.to_owned())
}

fn secure_endpoint(value: &str, issuer: &str) -> Result<String, ()> {
    let endpoint = Url::parse(value).map_err(|_| ())?;
    let issuer = Url::parse(issuer).map_err(|_| ())?;
    if endpoint.scheme() != "https"
        || endpoint.host_str() != issuer.host_str()
        || endpoint.port_or_known_default() != issuer.port_or_known_default()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(());
    }
    Ok(value.to_owned())
}

async fn okta_identity(
    result: &TokenResult,
    nonce: &str,
    client_id: &str,
    issuer: &str,
) -> Result<SsoIdentity, ()> {
    let metadata = okta_metadata(issuer).await?;
    let id_token = result
        .id_token
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let header = decode_header(id_token).map_err(|_| ())?;
    if header.alg != Algorithm::RS256 {
        return Err(());
    }
    let kid = header.kid.ok_or(())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| ())?;
    let response = client
        .get(&metadata.jwks_uri)
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let bytes = response.bytes().await.map_err(|_| ())?;
    if bytes.len() > 256 * 1024 {
        return Err(());
    }
    let jwks: JwkSet = serde_json::from_slice(&bytes).map_err(|_| ())?;
    let jwk = jwks.find(&kid).ok_or(())?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| ())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[metadata.issuer.as_str()]);
    validation.set_audience(&[client_id]);
    validation.required_spec_claims = ["exp", "iss", "aud", "sub"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let token = decode::<Value>(id_token, &key, &validation).map_err(|_| ())?;
    let claims = token.claims.as_object().ok_or(())?;
    if claims.get("iat").and_then(Value::as_i64).is_none() {
        return Err(());
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
        return Err(());
    }
    let subject = claims.get("sub").and_then(Value::as_str).ok_or(())?;

    let response = client
        .get(&metadata.userinfo_endpoint)
        .header("authorization", format!("Bearer {}", result.access_token))
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let bytes = response.bytes().await.map_err(|_| ())?;
    if bytes.len() > 64 * 1024 {
        return Err(());
    }
    let profile: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    let profile = profile.as_object().ok_or(())?;
    if profile.get("sub").and_then(Value::as_str) != Some(subject) {
        return Err(());
    }
    if profile.get("email_verified") != Some(&Value::Bool(true)) {
        return Err(());
    }
    let email = profile
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(())?
        .to_owned();
    let name = profile
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            claims
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .map(str::to_owned);
    Ok(SsoIdentity {
        subject: subject.to_owned(),
        email: Some(email),
        email_verified: true,
        name,
        team_id: None,
    })
}
