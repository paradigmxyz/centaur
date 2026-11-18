//! Calls to identity providers: the RFC 6749 authorization-code exchange and
//! RFC 7591 client registration, plus the capped-size JSON fetch the
//! provider strategies share.
//!
//! SECURITY: nothing here logs a code, token, client secret, request URL, or
//! response body. Only OAuth error codes and stages leave this module.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use centaur_console_db::redact::{self, Redacted};
use serde_json::{Value, json};

use super::upstream_url;
use crate::AppState;

pub const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 64 * 1024;

/// How the client authenticates to the token endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    ClientSecretPost,
    ClientSecretBasic,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientSecretPost => "client_secret_post",
            Self::ClientSecretBasic => "client_secret_basic",
        }
    }
}

/// Why an exchange, registration, or identity lookup failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeError {
    pub message: String,
    /// `network`, `http`, `oauth`, or `parse`.
    pub stage: &'static str,
    pub code: Option<String>,
    pub status: Option<u16>,
}

impl ExchangeError {
    pub fn new(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            stage,
            code: None,
            status: None,
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// The OAuth error code when there is one, else the stage. Shown to the
    /// user.
    pub fn reason(&self) -> &str {
        self.code
            .as_deref()
            .filter(|code| !code.trim().is_empty())
            .unwrap_or(self.stage)
    }
}

/// A normalized token response. `Debug` redacts the tokens and the raw body.
#[derive(Clone, PartialEq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    /// The granted scopes, split on commas and whitespace; empty when the
    /// provider did not say.
    pub scopes: Vec<String>,
    pub id_token: Option<String>,
    pub response: Value,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &Redacted)
            .field("refresh_token", &redact::option(&self.refresh_token))
            .field("expires_in", &self.expires_in)
            .field("scopes", &self.scopes)
            .field("id_token", &redact::option(&self.id_token))
            .field("response", &Redacted)
            .finish()
    }
}

/// The arguments of [`exchange`]. `Debug` redacts the secret, the code, and
/// the PKCE verifier.
pub struct Exchange<'a> {
    pub token_endpoint: &'a str,
    pub client_id: &'a str,
    pub client_secret: Option<&'a str>,
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub code_verifier: Option<&'a str>,
    pub auth_method: AuthMethod,
    pub require_refresh_token: bool,
    /// Provider-specific form fields; they never replace the standard ones.
    pub extra_params: &'a [(String, String)],
}

impl std::fmt::Debug for Exchange<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exchange")
            .field("token_endpoint", &self.token_endpoint)
            .field("client_id", &self.client_id)
            .field("client_secret", &redact::option(&self.client_secret))
            .field("code", &Redacted)
            .field("redirect_uri", &self.redirect_uri)
            .field("code_verifier", &redact::option(&self.code_verifier))
            .field("auth_method", &self.auth_method)
            .field("require_refresh_token", &self.require_refresh_token)
            .field(
                "extra_params",
                &redact::Keys(self.extra_params.iter().map(|(key, _)| key.as_str())),
            )
            .finish()
    }
}

/// A capped upstream response.
pub struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: bytes::Bytes,
}

impl Reply {
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Sends `request` and reads at most [`MAX_BODY`] bytes of the answer.
pub async fn send(request: reqwest::RequestBuilder) -> Result<Reply, ExchangeError> {
    let network = || ExchangeError::new("network", "upstream request failed");
    let response = request.send().await.map_err(|error| {
        tracing::debug!(error = %error.without_url(), "upstream request failed");
        network()
    })?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.bytes().await.map_err(|_| network())?;
    if body.len() > MAX_BODY {
        return Err(
            ExchangeError::new("parse", "upstream response is too large").with_status(status),
        );
    }
    Ok(Reply {
        status,
        content_type,
        body,
    })
}

/// A non-blank string, or a number as its decimal text.
pub fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn integer(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f as i64)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Splits a scope list (string or array) on commas and whitespace.
pub fn split_scopes(value: Option<&Value>) -> Vec<String> {
    let split = |text: &str| -> Vec<String> {
        text.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect()
    };
    match value {
        Some(Value::String(text)) => split(text),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .flat_map(split)
            .collect(),
        _ => Vec::new(),
    }
}

/// The authorization-code grant (with PKCE when `code_verifier` is set).
pub async fn exchange(state: &AppState, request: &Exchange<'_>) -> Result<TokenSet, ExchangeError> {
    let blank = |value: &str| value.trim().is_empty();
    if blank(request.code) {
        return Err(ExchangeError::new("oauth", "no authorization code").with_code("missing_code"));
    }
    if blank(request.token_endpoint) || blank(request.client_id) || blank(request.redirect_uri) {
        return Err(
            ExchangeError::new("oauth", "the app is not configured").with_code("misconfigured")
        );
    }
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", request.code),
        ("redirect_uri", request.redirect_uri),
    ];
    let secret = request.client_secret.filter(|secret| !blank(secret));
    let mut http_request = state
        .http
        .post(upstream_url(state, request.token_endpoint))
        .header(http::header::ACCEPT, "application/json")
        .timeout(EXCHANGE_TIMEOUT);
    match request.auth_method {
        AuthMethod::ClientSecretPost => {
            form.push(("client_id", request.client_id));
            if let Some(secret) = secret {
                form.push(("client_secret", secret));
            }
        }
        AuthMethod::ClientSecretBasic => {
            let Some(secret) = secret else {
                return Err(ExchangeError::new("oauth", "the app has no client secret")
                    .with_code("misconfigured"));
            };
            let encode = |value: &str| -> String {
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
            };
            let credentials = format!("{}:{}", encode(request.client_id), encode(secret));
            http_request = http_request.header(
                http::header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode(credentials)),
            );
        }
    }
    if let Some(verifier) = request.code_verifier.filter(|verifier| !blank(verifier)) {
        form.push(("code_verifier", verifier));
    }
    for (key, value) in request.extra_params {
        if !form.iter().any(|(existing, _)| existing == key) {
            form.push((key, value));
        }
    }
    let reply = send(http_request.form(&form)).await?;
    parse_token_reply(&reply, request.require_refresh_token)
}

fn parse_token_reply(
    reply: &Reply,
    require_refresh_token: bool,
) -> Result<TokenSet, ExchangeError> {
    let status = reply.status;
    let parsed = reply.json();
    if !reply.success() {
        let code = parsed.as_ref().and_then(|body| text(body.get("error")));
        let mut error = ExchangeError::new(
            if code.is_some() { "oauth" } else { "http" },
            format!("token endpoint http {status}"),
        )
        .with_status(status);
        error.code = code;
        return Err(error);
    }
    let parse_error =
        || ExchangeError::new("parse", "parsing token response failed").with_status(status);
    let body = parsed.filter(Value::is_object).ok_or_else(parse_error)?;
    if body.get("ok") == Some(&Value::Bool(false))
        && let Some(code) = text(body.get("error"))
    {
        return Err(
            ExchangeError::new("oauth", format!("token endpoint rejected code: {code}"))
                .with_code(code)
                .with_status(status),
        );
    }
    // Slack nests the user token under `authed_user`.
    let payload = match body.get("authed_user") {
        Some(user) if text(user.get("access_token")).is_some() => user,
        _ => &body,
    };
    let access_token = text(payload.get("access_token")).ok_or_else(|| {
        ExchangeError::new("parse", "token endpoint returned an empty access_token")
            .with_status(status)
    })?;
    let refresh_token = text(payload.get("refresh_token"));
    if require_refresh_token && refresh_token.is_none() {
        return Err(
            ExchangeError::new("oauth", "token endpoint returned no refresh_token")
                .with_code("missing_refresh_token")
                .with_status(status),
        );
    }
    let expires_in = match payload.get("expires_in") {
        None | Some(Value::Null) => None,
        other => Some(integer(other).ok_or_else(parse_error)?),
    };
    let scope = payload.get("scope").or_else(|| body.get("scope"));
    let id_token = text(payload.get("id_token")).or_else(|| text(body.get("id_token")));
    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_in,
        scopes: split_scopes(scope),
        id_token,
        response: body,
    })
}

/// A client registered with an IdP. `Debug` hides the secret.
pub struct Registration {
    pub client_id: String,
    pub client_secret: String,
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// RFC 7591 registration of a confidential client with `redirect_uri`.
pub async fn register(
    state: &AppState,
    endpoint: &str,
    redirect_uri: &str,
    scope: &str,
    auth_method: AuthMethod,
) -> Result<Registration, ExchangeError> {
    let body = json!({
        "client_name": "Centaur",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": auth_method.as_str(),
        "scope": scope,
    });
    let reply = send(state.http.post(upstream_url(state, endpoint)).json(&body)).await?;
    let failed = || ExchangeError::new("parse", "Client registration failed");
    if !reply.success() {
        return Err(failed());
    }
    let parsed = reply.json().ok_or_else(failed)?;
    let field = |key: &str| {
        parsed
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    match (field("client_id"), field("client_secret")) {
        (Some(client_id), Some(client_secret)) => Ok(Registration {
            client_id,
            client_secret,
        }),
        _ => Err(failed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> Reply {
        Reply {
            status,
            content_type: None,
            body: bytes::Bytes::from(body.to_owned()),
        }
    }

    #[test]
    fn parses_a_token_response() {
        let tokens = parse_token_reply(
            &reply(
                200,
                r#"{"access_token":"AT","refresh_token":"RT","expires_in":"3600","scope":"a,b c","id_token":"h.p.s"}"#,
            ),
            true,
        )
        .unwrap();
        assert_eq!(tokens.access_token, "AT");
        assert_eq!(tokens.refresh_token.as_deref(), Some("RT"));
        assert_eq!(tokens.expires_in, Some(3600));
        assert_eq!(tokens.scopes, ["a", "b", "c"]);
        assert!(!format!("{tokens:?}").contains("AT\""));
    }

    #[test]
    fn slack_user_tokens_are_nested() {
        let tokens = parse_token_reply(
            &reply(
                200,
                r#"{"ok":true,"access_token":"BOT","scope":"commands","authed_user":{"id":"U1","access_token":"USER","refresh_token":"USER-RT","expires_in":43200,"scope":"im:history,channels:history"}}"#,
            ),
            false,
        )
        .unwrap();
        assert_eq!(tokens.access_token, "USER");
        assert_eq!(tokens.scopes, ["im:history", "channels:history"]);
        assert_eq!(tokens.response["authed_user"]["id"], "U1");
    }

    #[test]
    fn classifies_failures() {
        let error =
            parse_token_reply(&reply(400, r#"{"error":"invalid_grant"}"#), false).unwrap_err();
        assert_eq!(
            (error.stage, error.reason(), error.status),
            ("oauth", "invalid_grant", Some(400))
        );
        let error = parse_token_reply(&reply(503, "down"), false).unwrap_err();
        assert_eq!((error.stage, error.reason()), ("http", "http"));
        let error = parse_token_reply(&reply(200, r#"{"ok":false,"error":"invalid_code"}"#), false)
            .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("invalid_code"));
        for body in [
            "not json",
            "[]",
            r#"{"access_token":""}"#,
            r#"{"access_token":"x","expires_in":"soon"}"#,
        ] {
            assert_eq!(
                parse_token_reply(&reply(200, body), false)
                    .unwrap_err()
                    .stage,
                "parse",
                "{body}"
            );
        }
        let error = parse_token_reply(&reply(200, r#"{"access_token":"x"}"#), true).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("missing_refresh_token"));
    }

    #[test]
    fn debug_hides_request_secrets() {
        let request = Exchange {
            token_endpoint: "https://idp.example/token",
            client_id: "cid",
            client_secret: Some("SECRET-VALUE"),
            code: "CODE-VALUE",
            redirect_uri: "https://c.example/cb",
            code_verifier: Some("VERIFIER-VALUE"),
            auth_method: AuthMethod::ClientSecretPost,
            require_refresh_token: false,
            extra_params: &[("resource".into(), "RESOURCE-VALUE".into())],
        };
        let debug = format!("{request:?}");
        for secret in [
            "SECRET-VALUE",
            "CODE-VALUE",
            "VERIFIER-VALUE",
            "RESOURCE-VALUE",
        ] {
            assert!(!debug.contains(secret), "{debug}");
        }
        let registration = Registration {
            client_id: "id".into(),
            client_secret: "SECRET-VALUE".into(),
        };
        assert!(!format!("{registration:?}").contains("SECRET-VALUE"));
    }
}
