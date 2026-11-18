//! The consent-flow provider strategies. A strategy owns the IdP-specific
//! parts of a flow (endpoints, extra parameters, scope handling, how a token
//! response maps to a stable account identity); everything else is
//! provider-agnostic. Keep the registry in step with
//! `crates/centaur-token-broker/providers.json`.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    exchange::{AuthMethod, ExchangeError, TokenSet, send, text},
    upstream_url,
};
use crate::AppState;

/// The registered providers, in registry order (the first is the form
/// default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKey {
    Attio,
    Custom,
    Github,
    Google,
    Granola,
    Linear,
    Mercator,
    Microsoft,
    Slack,
    Zoom,
}

pub const REGISTRY: [ProviderKey; 10] = [
    ProviderKey::Attio,
    ProviderKey::Custom,
    ProviderKey::Github,
    ProviderKey::Google,
    ProviderKey::Granola,
    ProviderKey::Linear,
    ProviderKey::Mercator,
    ProviderKey::Microsoft,
    ProviderKey::Slack,
    ProviderKey::Zoom,
];

impl ProviderKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attio => "attio",
            Self::Custom => "custom",
            Self::Github => "github",
            Self::Google => "google",
            Self::Granola => "granola",
            Self::Linear => "linear",
            Self::Mercator => "mercator",
            Self::Microsoft => "microsoft",
            Self::Slack => "slack",
            Self::Zoom => "zoom",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        REGISTRY
            .into_iter()
            .find(|candidate| candidate.as_str() == key)
    }
}

/// The keys of a custom provider's `provider_config`.
pub const CUSTOM_CONFIG_KEYS: [&str; 4] = [
    "authorization_endpoint",
    "token_endpoint",
    "userinfo_endpoint",
    "resource",
];
pub const CUSTOM_OPTIONAL_CONFIG_KEYS: [&str; 1] = ["userinfo_endpoint"];

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct CustomConfig {
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    userinfo_endpoint: Option<String>,
    resource: Option<String>,
}

/// One request rule for the wrapping static secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleSpec {
    pub host: String,
    pub http_methods: Vec<String>,
    pub paths: Vec<String>,
}

impl RuleSpec {
    fn host(host: &str) -> Self {
        Self {
            host: host.to_owned(),
            http_methods: Vec::new(),
            paths: Vec::new(),
        }
    }
}

/// A provider preset: an app that can be created with one click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub slug: &'static str,
    pub description: &'static str,
    pub allowed_scopes: Vec<String>,
    pub shared: bool,
}

/// The provider account a token response belongs to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identity {
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub team_id: Option<String>,
    pub labels: Map<String, Value>,
}

/// A strategy: the registry's instance, or a custom provider configured from
/// an app.
#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    pub key: ProviderKey,
    /// The custom provider's app slug (its display name).
    custom_slug: Option<String>,
    config: CustomConfig,
}

const ATTIO_SELF: &str = "https://api.attio.com/v2/self";
const GITHUB_USER: &str = "https://api.github.com/user";
const LINEAR_GRAPHQL: &str = "https://api.linear.app/graphql";
const LINEAR_VIEWER_QUERY: &str = "{ viewer { id name email } }";
const ZOOM_SELF: &str = "https://api.zoom.us/v2/users/me";
pub const MERCATOR_ORIGIN: &str = "https://mercator.sh";
pub const MERCATOR_SCOPE: &str = "mercator:tools";
const SLACK_TOKEN_ENDPOINT: &str = "https://slack.com/api/oauth.v2.access";

/// The timeout of the provider-details call the console page waits on.
pub const DETAILS_TIMEOUT: Duration = Duration::from_secs(2);

impl Provider {
    pub fn fetch(key: &str) -> Option<Self> {
        ProviderKey::parse(key).map(Self::registered)
    }

    pub fn registered(key: ProviderKey) -> Self {
        Self {
            key,
            custom_slug: None,
            config: CustomConfig::default(),
        }
    }

    /// The strategy of an app row (`None` for an unknown provider key).
    pub fn for_app(provider: &str, slug: &str, provider_config: &Value) -> Option<Self> {
        let key = ProviderKey::parse(provider)?;
        if key == ProviderKey::Custom {
            return Some(Self {
                key,
                custom_slug: Some(slug.to_owned()),
                config: serde_json::from_value(provider_config.clone()).unwrap_or_default(),
            });
        }
        Some(Self::registered(key))
    }

    pub fn key_str(&self) -> &'static str {
        self.key.as_str()
    }

    pub fn display_name(&self) -> String {
        match self.key {
            ProviderKey::Attio => "Attio".into(),
            ProviderKey::Custom => self.custom_slug.clone().unwrap_or_else(|| "Custom".into()),
            ProviderKey::Github => "GitHub".into(),
            ProviderKey::Google => "Google".into(),
            ProviderKey::Granola => "Granola".into(),
            ProviderKey::Linear => "Linear".into(),
            ProviderKey::Mercator => "Mercator".into(),
            ProviderKey::Microsoft => "Microsoft".into(),
            ProviderKey::Slack => "Slack".into(),
            ProviderKey::Zoom => "Zoom".into(),
        }
    }

    pub fn authorization_endpoint(&self) -> Option<String> {
        Some(match self.key {
            ProviderKey::Attio => "https://app.attio.com/authorize".into(),
            ProviderKey::Custom => return self.config.authorization_endpoint.clone(),
            ProviderKey::Github => "https://github.com/login/oauth/authorize".into(),
            ProviderKey::Google => "https://accounts.google.com/o/oauth2/v2/auth".into(),
            ProviderKey::Granola => "https://mcp-auth.granola.ai/oauth2/authorize".into(),
            ProviderKey::Linear => "https://linear.app/oauth/authorize".into(),
            ProviderKey::Mercator => format!("{MERCATOR_ORIGIN}/authorize"),
            ProviderKey::Microsoft => {
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into()
            }
            ProviderKey::Slack => "https://slack.com/oauth/v2/authorize".into(),
            ProviderKey::Zoom => "https://zoom.us/oauth/authorize".into(),
        })
    }

    pub fn token_endpoint(&self) -> Option<String> {
        Some(match self.key {
            ProviderKey::Attio => "https://app.attio.com/oauth/token".into(),
            ProviderKey::Custom => return self.config.token_endpoint.clone(),
            ProviderKey::Github => "https://github.com/login/oauth/access_token".into(),
            ProviderKey::Google => "https://oauth2.googleapis.com/token".into(),
            ProviderKey::Granola => "https://mcp-auth.granola.ai/oauth2/token".into(),
            ProviderKey::Linear => "https://api.linear.app/oauth/token".into(),
            ProviderKey::Mercator => format!("{MERCATOR_ORIGIN}/oauth/token"),
            ProviderKey::Microsoft => {
                "https://login.microsoftonline.com/common/oauth2/v2.0/token".into()
            }
            ProviderKey::Slack => SLACK_TOKEN_ENDPOINT.into(),
            ProviderKey::Zoom => "https://zoom.us/oauth/token".into(),
        })
    }

    /// Scopes always requested so the account can be identified.
    pub fn identity_scopes(&self) -> Vec<&'static str> {
        match self.key {
            ProviderKey::Google => vec!["openid", "https://www.googleapis.com/auth/userinfo.email"],
            ProviderKey::Granola | ProviderKey::Microsoft => {
                vec!["openid", "email", "profile", "offline_access"]
            }
            _ => Vec::new(),
        }
    }

    /// Scopes a credential must hold to count as connected.
    pub fn required_scopes(&self) -> Vec<&'static str> {
        match self.key {
            ProviderKey::Mercator => vec![MERCATOR_SCOPE],
            ProviderKey::Slack => vec!["search:read"],
            ProviderKey::Zoom => vec!["user:read:user"],
            _ => Vec::new(),
        }
    }

    fn api_hosts(&self) -> Option<Vec<&'static str>> {
        Some(match self.key {
            ProviderKey::Attio => vec!["api.attio.com"],
            ProviderKey::Google => vec!["*.googleapis.com"],
            ProviderKey::Granola => vec!["mcp.granola.ai"],
            ProviderKey::Linear => vec!["api.linear.app", "uploads.linear.app", "mcp.linear.app"],
            ProviderKey::Mercator => vec!["mercator.sh"],
            ProviderKey::Microsoft => vec!["graph.microsoft.com"],
            ProviderKey::Slack => vec!["slack.com"],
            ProviderKey::Zoom => vec!["api.zoom.us"],
            ProviderKey::Custom | ProviderKey::Github => return None,
        })
    }

    /// The authorization query parameter that carries the scopes.
    pub fn scope_param(&self) -> &'static str {
        match self.key {
            ProviderKey::Slack => "user_scope",
            _ => "scope",
        }
    }

    pub fn scope_separator(&self) -> &'static str {
        match self.key {
            ProviderKey::Slack | ProviderKey::Linear => ",",
            _ => " ",
        }
    }

    pub fn extra_authorization_params(&self) -> Vec<(&'static str, String)> {
        match self.key {
            ProviderKey::Google => vec![
                ("access_type", "offline".into()),
                ("prompt", "consent".into()),
            ],
            ProviderKey::Microsoft => vec![("prompt", "select_account".into())],
            ProviderKey::Mercator => vec![("resource", format!("{MERCATOR_ORIGIN}/mcp/auth"))],
            ProviderKey::Custom => self
                .config
                .resource
                .clone()
                .map(|resource| ("resource", resource))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn extra_token_params(&self) -> Vec<(String, String)> {
        match (self.key, &self.config.resource) {
            (ProviderKey::Custom, Some(resource)) => vec![("resource".into(), resource.clone())],
            _ => Vec::new(),
        }
    }

    /// Whether token-broker can refresh the provider's access tokens.
    pub fn refreshable(&self) -> bool {
        !matches!(self.key, ProviderKey::Attio | ProviderKey::Github)
    }

    pub fn require_refresh_token(&self) -> bool {
        self.key != ProviderKey::Slack && self.refreshable()
    }

    /// Slack tokens refresh only when the workspace rotates them.
    pub fn refreshable_result(&self, tokens: &TokenSet) -> bool {
        match self.key {
            ProviderKey::Slack => tokens.refresh_token.is_some(),
            _ => self.refreshable(),
        }
    }

    /// Provider rules a token response must satisfy to be stored.
    pub fn validate_result(&self, tokens: &TokenSet) -> Result<(), ExchangeError> {
        match self.key {
            ProviderKey::Slack if tokens.refresh_token.is_none() && tokens.expires_in.is_some() => {
                Err(ExchangeError::new(
                    "oauth",
                    "token endpoint returned expiring Slack token without refresh_token",
                )
                .with_code("missing_refresh_token"))
            }
            ProviderKey::Mercator if !tokens.scopes.iter().any(|scope| scope == MERCATOR_SCOPE) => {
                Err(
                    ExchangeError::new("oauth", "Mercator scope was not granted")
                        .with_code("missing_scope"),
                )
            }
            _ => Ok(()),
        }
    }

    /// Whether a refresh re-requests the scopes (token-broker's table).
    pub fn refresh_scopes(&self) -> bool {
        matches!(
            self.key,
            ProviderKey::Google
                | ProviderKey::Granola
                | ProviderKey::Linear
                | ProviderKey::Mercator
                | ProviderKey::Microsoft
        )
    }

    pub fn auth_method(&self) -> AuthMethod {
        match self.key {
            ProviderKey::Zoom => AuthMethod::ClientSecretBasic,
            _ => AuthMethod::ClientSecretPost,
        }
    }

    /// The account part of a minted credential's `foreign_id`.
    pub fn foreign_id_subject(&self, subject: &str) -> String {
        match self.key {
            ProviderKey::Zoom => subject.to_owned(),
            _ => subject.to_lowercase(),
        }
    }

    pub fn wrapping_secret_kind(&self) -> &'static str {
        match self.key {
            ProviderKey::Github => "github_token",
            _ => "custom",
        }
    }

    /// The request rules of the wrapping secret: one per API host, or the
    /// provider's own. `None` when there is nothing to scope them to.
    pub fn credential_request_rules(&self) -> Option<Vec<RuleSpec>> {
        match self.key {
            ProviderKey::Mercator => Some(vec![RuleSpec {
                host: "mercator.sh".into(),
                http_methods: vec!["POST".into()],
                paths: vec!["/mcp/auth".into()],
            }]),
            ProviderKey::Custom => {
                let resource = url::Url::parse(self.config.resource.as_deref()?).ok()?;
                let path = resource.path();
                Some(vec![RuleSpec {
                    host: resource.host_str()?.to_owned(),
                    http_methods: Vec::new(),
                    paths: if path.is_empty() || path == "/" {
                        Vec::new()
                    } else {
                        vec![path.to_owned()]
                    },
                }])
            }
            _ => Some(self.api_hosts()?.into_iter().map(RuleSpec::host).collect()),
        }
    }

    pub fn preset(&self) -> Option<Preset> {
        (self.key == ProviderKey::Mercator).then(|| Preset {
            slug: "mercator",
            description: "Shared with your agents.",
            allowed_scopes: vec![MERCATOR_SCOPE.into()],
            shared: true,
        })
    }

    pub fn registration_endpoint(&self) -> Option<String> {
        (self.key == ProviderKey::Mercator).then(|| format!("{MERCATOR_ORIGIN}/oauth/register"))
    }

    /// Whether the console can set the provider up with one click.
    pub fn presettable(&self) -> bool {
        self.preset().is_some() && self.registration_endpoint().is_some()
    }

    /// Resolves the provider account of `tokens`.
    pub async fn identity(
        &self,
        state: &AppState,
        tokens: &TokenSet,
        client_id: &str,
    ) -> Result<Identity, ExchangeError> {
        match self.key {
            ProviderKey::Google => oidc_identity(tokens, client_id, &Oidc::GOOGLE),
            ProviderKey::Granola => oidc_identity(tokens, client_id, &Oidc::GRANOLA),
            ProviderKey::Microsoft => microsoft_identity(tokens, client_id),
            ProviderKey::Slack => slack_identity(tokens, client_id),
            ProviderKey::Github => github_identity(state, tokens).await,
            ProviderKey::Attio => attio_identity(state, tokens).await,
            ProviderKey::Linear => linear_identity(state, tokens).await,
            ProviderKey::Zoom => zoom_identity(state, tokens).await,
            ProviderKey::Mercator => mercator_identity(state, tokens).await,
            ProviderKey::Custom => self.custom_identity(state, tokens).await,
        }
    }

    async fn custom_identity(
        &self,
        state: &AppState,
        tokens: &TokenSet,
    ) -> Result<Identity, ExchangeError> {
        let name = self.display_name();
        let claims = match self
            .config
            .userinfo_endpoint
            .as_deref()
            .filter(|endpoint| !endpoint.trim().is_empty())
        {
            Some(endpoint) => {
                get_json(
                    &name,
                    state
                        .http
                        .get(upstream_url(state, endpoint))
                        .bearer_auth(&tokens.access_token)
                        .header(http::header::ACCEPT, "application/json"),
                )
                .await?
            }
            None => jwt_payload(&tokens.access_token)
                .filter(Value::is_object)
                .ok_or_else(|| {
                    ExchangeError::new(
                        "parse",
                        "access token is not a JWT and no userinfo endpoint is configured",
                    )
                    .with_code("invalid_identity_response")
                })?,
        };
        let subject = required(&claims, "sub", &name)?;
        Ok(Identity {
            subject,
            email: text(claims.get("email")),
            name: text(claims.get("name")),
            ..Identity::default()
        })
    }

    /// Mercator's wallet state for the console page: `{"claimed": bool|null}`
    /// (null when unknown), or `{}` for providers without details.
    pub async fn details(
        &self,
        state: &AppState,
        access_token: &str,
        provider_subject: Option<&str>,
    ) -> Value {
        if self.key != ProviderKey::Mercator {
            return json!({});
        }
        let unknown = json!({"claimed": null});
        let request = state
            .http
            .get(upstream_url(
                state,
                &format!("{MERCATOR_ORIGIN}/v1/claims/config?provider=slack"),
            ))
            .bearer_auth(access_token)
            .timeout(DETAILS_TIMEOUT);
        let Ok(reply) = send(request).await else {
            return unknown;
        };
        let Some(body) = reply.success().then(|| reply.json()).flatten() else {
            return unknown;
        };
        let wallet_matches = body
            .get("wallet_address")
            .and_then(Value::as_str)
            .is_some_and(|wallet| {
                wallet.eq_ignore_ascii_case(provider_subject.unwrap_or_default())
            });
        match body.get("claimed") {
            Some(Value::Bool(claimed)) if wallet_matches => json!({"claimed": claimed}),
            _ => unknown,
        }
    }
}

/// Problems with a custom provider's `provider_config`.
pub fn custom_config_errors(config: &Map<String, Value>) -> Vec<String> {
    let unknown: Vec<&str> = config
        .keys()
        .map(String::as_str)
        .filter(|key| !CUSTOM_CONFIG_KEYS.contains(key))
        .collect();
    let mut errors = Vec::new();
    if !unknown.is_empty() {
        errors.push(format!("has unknown keys: {}", unknown.join(", ")));
    }
    for key in CUSTOM_CONFIG_KEYS {
        let value = config.get(key);
        let blank = value.is_none_or(|value| {
            value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty())
        });
        if CUSTOM_OPTIONAL_CONFIG_KEYS.contains(&key) && blank {
            continue;
        }
        if !value.and_then(Value::as_str).is_some_and(https_url) {
            errors.push(format!("{key} must be an https URL"));
        }
    }
    errors
}

/// An `https` URL with a host and no embedded credentials.
pub fn https_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

// --- identities -----------------------------------------------------------------

fn failure(stage: &'static str, message: &str, code: &str) -> ExchangeError {
    ExchangeError::new(stage, message).with_code(code)
}

/// The payload of a JWT (not verified: it came straight from the token
/// endpoint over TLS).
fn jwt_payload(token: &str) -> Option<Value> {
    let segment = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(segment.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn id_token_claims(tokens: &TokenSet) -> Result<Value, ExchangeError> {
    let id_token = tokens.id_token.as_deref().ok_or_else(|| {
        failure(
            "oauth",
            "token response carried no id_token",
            "missing_id_token",
        )
    })?;
    jwt_payload(id_token)
        .filter(Value::is_object)
        .ok_or_else(|| ExchangeError::new("parse", "id_token payload did not decode"))
}

struct Oidc {
    issuers: &'static [&'static str],
    issuer_message: &'static str,
}

impl Oidc {
    const GOOGLE: Self = Self {
        issuers: &["https://accounts.google.com", "accounts.google.com"],
        issuer_message: "id_token iss was not a Google issuer",
    };
    const GRANOLA: Self = Self {
        issuers: &["https://mcp-auth.granola.ai"],
        issuer_message: "id_token iss was not a Granola issuer",
    };
}

fn check_audience(claims: &Value, client_id: &str) -> Result<(), ExchangeError> {
    if claims.get("aud").and_then(Value::as_str) == Some(client_id) {
        Ok(())
    } else {
        Err(failure(
            "oauth",
            "id_token aud did not match client_id",
            "id_token_aud_mismatch",
        ))
    }
}

fn oidc_identity(
    tokens: &TokenSet,
    client_id: &str,
    rules: &Oidc,
) -> Result<Identity, ExchangeError> {
    let claims = id_token_claims(tokens)?;
    check_audience(&claims, client_id)?;
    let issuer = claims.get("iss").and_then(Value::as_str);
    if !issuer.is_some_and(|issuer| rules.issuers.contains(&issuer)) {
        return Err(failure(
            "oauth",
            rules.issuer_message,
            "id_token_iss_invalid",
        ));
    }
    let subject = text(claims.get("sub"))
        .ok_or_else(|| failure("oauth", "id_token carried no sub", "id_token_missing_sub"))?;
    Ok(Identity {
        subject,
        email: text(claims.get("email")),
        ..Identity::default()
    })
}

fn microsoft_identity(tokens: &TokenSet, client_id: &str) -> Result<Identity, ExchangeError> {
    let claims = id_token_claims(tokens)?;
    check_audience(&claims, client_id)?;
    if !claims
        .get("iss")
        .and_then(Value::as_str)
        .is_some_and(microsoft_issuer)
    {
        return Err(failure(
            "oauth",
            "id_token iss was not a Microsoft issuer",
            "id_token_iss_invalid",
        ));
    }
    let subject = text(claims.get("oid"))
        .ok_or_else(|| failure("oauth", "id_token carried no oid", "id_token_missing_oid"))?;
    let verified = match claims.get("xms_edov") {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(flag)) => flag == "true" || flag == "1",
        Some(Value::Number(flag)) => flag.as_i64() == Some(1),
        _ => false,
    };
    Ok(Identity {
        subject,
        email: verified.then(|| text(claims.get("email"))).flatten(),
        name: text(claims.get("name")),
        ..Identity::default()
    })
}

/// `https://login.microsoftonline.com/<tenant guid>/v2.0`.
fn microsoft_issuer(issuer: &str) -> bool {
    issuer
        .strip_prefix("https://login.microsoftonline.com/")
        .and_then(|rest| rest.strip_suffix("/v2.0"))
        .is_some_and(|tenant| {
            tenant.len() == 36
                && tenant
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || c == '-')
        })
}

fn dig<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

fn slack_identity(tokens: &TokenSet, client_id: &str) -> Result<Identity, ExchangeError> {
    let response = &tokens.response;
    let field = |path: &[&str]| text(dig(response, path));
    let team_id = field(&["team", "id"]).or_else(|| field(&["authed_user", "team_id"]));
    if let Some(user_id) = field(&["authed_user", "id"]) {
        return Ok(Identity {
            subject: user_id,
            email: field(&["authed_user", "email"]),
            name: field(&["authed_user", "name"]).or_else(|| field(&["authed_user", "user"])),
            team_id,
            labels: Map::new(),
        });
    }
    if let Some(bot_user_id) = field(&["bot_user_id"]) {
        return Ok(Identity {
            name: field(&["team", "name"]).or_else(|| Some(bot_user_id.clone())),
            subject: bot_user_id,
            email: None,
            team_id,
            labels: Map::new(),
        });
    }
    // Sign in with Slack: an OIDC id_token from slack.com.
    let claims = id_token_claims(tokens)?;
    check_audience(&claims, client_id)?;
    if claims.get("iss").and_then(Value::as_str) != Some("https://slack.com") {
        return Err(failure(
            "oauth",
            "id_token iss was not an expected issuer",
            "id_token_iss_invalid",
        ));
    }
    let subject = text(claims.get("sub"))
        .ok_or_else(|| failure("oauth", "id_token carried no sub", "id_token_missing_sub"))?;
    Ok(Identity {
        subject,
        email: text(claims.get("email")),
        name: text(claims.get("name")),
        ..Identity::default()
    })
}

/// A JSON object from an identity endpoint, or why not.
async fn get_json(
    provider: &str,
    request: reqwest::RequestBuilder,
) -> Result<Value, ExchangeError> {
    let reply = send(request).await.map_err(|_| {
        ExchangeError::new(
            "network",
            format!("{provider} identity endpoint request failed"),
        )
        .with_code("identity_lookup_failed")
    })?;
    identity_json(provider, &reply)
}

fn identity_json(provider: &str, reply: &super::exchange::Reply) -> Result<Value, ExchangeError> {
    if !reply.success() {
        return Err(ExchangeError::new(
            "http",
            format!("{provider} identity endpoint http {}", reply.status),
        )
        .with_code("identity_lookup_failed")
        .with_status(reply.status));
    }
    reply.json().filter(Value::is_object).ok_or_else(|| {
        ExchangeError::new(
            "parse",
            format!("{provider} identity endpoint returned an invalid payload"),
        )
        .with_code("invalid_identity_response")
        .with_status(reply.status)
    })
}

/// A required, non-blank identity field.
fn required(value: &Value, key: &str, provider: &str) -> Result<String, ExchangeError> {
    text(value.get(key)).ok_or_else(|| {
        ExchangeError::new(
            "parse",
            format!("{provider} identity endpoint returned no stable subject"),
        )
        .with_code("missing_identity")
    })
}

async fn github_identity(state: &AppState, tokens: &TokenSet) -> Result<Identity, ExchangeError> {
    let profile = get_json(
        "GitHub",
        state
            .http
            .get(upstream_url(state, GITHUB_USER))
            .bearer_auth(&tokens.access_token)
            .header(http::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(http::header::USER_AGENT, "centaur-console"),
    )
    .await?;
    let subject = required(&profile, "id", "GitHub")?;
    let login = required(&profile, "login", "GitHub")?;
    let mut labels = Map::new();
    labels.insert("github_login".into(), login.clone().into());
    Ok(Identity {
        subject,
        email: text(profile.get("email")),
        name: text(profile.get("name")).or(Some(login)),
        team_id: None,
        labels,
    })
}

async fn attio_identity(state: &AppState, tokens: &TokenSet) -> Result<Identity, ExchangeError> {
    let workspace = get_json(
        "Attio",
        state
            .http
            .get(upstream_url(state, ATTIO_SELF))
            .bearer_auth(&tokens.access_token)
            .header(http::header::USER_AGENT, "centaur-console"),
    )
    .await?;
    let subject = required(&workspace, "workspace_id", "Attio")?;
    let name = text(workspace.get("workspace_name"))
        .or_else(|| text(workspace.get("workspace_slug")))
        .unwrap_or_else(|| subject.clone());
    Ok(Identity {
        subject,
        name: Some(name),
        ..Identity::default()
    })
}

async fn linear_identity(state: &AppState, tokens: &TokenSet) -> Result<Identity, ExchangeError> {
    let payload = get_json(
        "Linear",
        state
            .http
            .post(upstream_url(state, LINEAR_GRAPHQL))
            .bearer_auth(&tokens.access_token)
            .header(http::header::USER_AGENT, "centaur-console")
            .json(&json!({"query": LINEAR_VIEWER_QUERY})),
    )
    .await?;
    let viewer = dig(&payload, &["data", "viewer"])
        .filter(|viewer| viewer.is_object())
        .ok_or_else(|| {
            failure(
                "parse",
                "Linear identity endpoint returned no viewer",
                "missing_identity",
            )
        })?;
    let subject = required(viewer, "id", "Linear")?;
    let email = text(viewer.get("email"));
    let name = text(viewer.get("name"))
        .or_else(|| email.clone())
        .unwrap_or_else(|| subject.clone());
    Ok(Identity {
        subject,
        email,
        name: Some(name),
        ..Identity::default()
    })
}

async fn zoom_identity(state: &AppState, tokens: &TokenSet) -> Result<Identity, ExchangeError> {
    let user = get_json(
        "Zoom",
        state
            .http
            .get(upstream_url(state, ZOOM_SELF))
            .bearer_auth(&tokens.access_token)
            .header(http::header::USER_AGENT, "centaur-console"),
    )
    .await?;
    let subject = required(&user, "id", "Zoom")?;
    let email = text(user.get("email"));
    let name = text(user.get("display_name"))
        .or_else(|| email.clone())
        .unwrap_or_else(|| subject.clone());
    Ok(Identity {
        subject,
        email,
        name: Some(name),
        ..Identity::default()
    })
}

/// The data lines of the last server-sent event that has any.
fn sse_data(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n");
    normalized
        .split("\n\n")
        .filter(|event| event.lines().any(|line| line.starts_with("data:")))
        .last()
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.trim().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn wallet_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Mercator identifies an account by wallet address, reported by its MCP
/// `get_connection_status` tool (as JSON or as a server-sent event).
async fn mercator_identity(state: &AppState, tokens: &TokenSet) -> Result<Identity, ExchangeError> {
    let request = state
        .http
        .post(upstream_url(state, &format!("{MERCATOR_ORIGIN}/mcp/auth")))
        .bearer_auth(&tokens.access_token)
        .header(http::header::ACCEPT, "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "get_connection_status", "arguments": {}}
        }));
    let mut reply = send(request).await.map_err(|_| {
        failure(
            "network",
            "Mercator identity endpoint request failed",
            "identity_lookup_failed",
        )
    })?;
    if reply
        .content_type
        .as_deref()
        .is_some_and(|value| value.starts_with("text/event-stream"))
    {
        reply.body = sse_data(&String::from_utf8_lossy(&reply.body)).into();
    }
    let payload = identity_json("Mercator", &reply)?;
    let missing = || {
        failure(
            "oauth",
            "Mercator wallet identity unavailable",
            "missing_wallet",
        )
    };
    let status = payload
        .get("result")
        .filter(|result| {
            result
                .get("isError")
                .is_none_or(|flag| flag == &Value::Bool(false))
                && payload.get("error").is_none_or(Value::is_null)
        })
        .and_then(|result| result.get("structuredContent"))
        .filter(|status| status.is_object())
        .ok_or_else(missing)?;
    let address = dig(status, &["account", "walletAddress"]).and_then(Value::as_str);
    match address {
        Some(address)
            if status.get("oauthAuthenticated") == Some(&Value::Bool(true))
                && wallet_address(address) =>
        {
            Ok(Identity {
                subject: address.to_lowercase(),
                name: Some(address.to_owned()),
                ..Identity::default()
            })
        }
        _ => Err(missing()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(id_token: Option<&Value>, response: Value) -> TokenSet {
        TokenSet {
            access_token: "AT".into(),
            refresh_token: Some("RT".into()),
            expires_in: Some(3600),
            scopes: Vec::new(),
            id_token: id_token.map(|claims| {
                format!(
                    "h.{}.s",
                    URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap())
                )
            }),
            response,
        }
    }

    #[test]
    fn token_broker_knows_how_to_refresh_every_providers_credentials() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../centaur-token-broker/providers.json");
        let table: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let table = table.as_object().unwrap();
        let mut expected: Vec<&str> = REGISTRY.iter().map(|key| key.as_str()).collect();
        expected.sort();
        let mut actual: Vec<&str> = table.keys().map(String::as_str).collect();
        actual.sort();
        assert_eq!(expected, actual);
        for key in REGISTRY {
            let provider = Provider::registered(key);
            let row = &table[key.as_str()];
            assert_eq!(
                row["token_endpoint_auth_method"],
                provider.auth_method().as_str(),
                "{key:?}"
            );
            assert_eq!(row["refresh_scopes"], provider.refresh_scopes(), "{key:?}");
        }
    }

    #[test]
    fn oidc_identities_check_audience_issuer_and_subject() {
        let claims = json!({"aud": "cid", "iss": "https://accounts.google.com", "sub": "123", "email": "u@example.com"});
        let identity =
            oidc_identity(&tokens(Some(&claims), json!({})), "cid", &Oidc::GOOGLE).unwrap();
        assert_eq!(identity.subject, "123");
        assert_eq!(identity.email.as_deref(), Some("u@example.com"));
        for (claims, code) in [
            (
                json!({"aud": "other", "iss": "https://accounts.google.com", "sub": "1"}),
                "id_token_aud_mismatch",
            ),
            (
                json!({"aud": "cid", "iss": "https://evil.example", "sub": "1"}),
                "id_token_iss_invalid",
            ),
            (
                json!({"aud": "cid", "iss": "accounts.google.com", "sub": " "}),
                "id_token_missing_sub",
            ),
        ] {
            let error =
                oidc_identity(&tokens(Some(&claims), json!({})), "cid", &Oidc::GOOGLE).unwrap_err();
            assert_eq!(error.code.as_deref(), Some(code));
        }
        let error = oidc_identity(&tokens(None, json!({})), "cid", &Oidc::GOOGLE).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("missing_id_token"));
    }

    #[test]
    fn microsoft_identities_use_oid_and_verified_email_only() {
        let tenant = "9188040d-6c67-4c5b-b112-36a304b66dad";
        let mut claims = json!({"aud": "cid", "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"), "oid": "oid-1", "email": "u@example.com", "name": "U"});
        let identity = microsoft_identity(&tokens(Some(&claims), json!({})), "cid").unwrap();
        assert_eq!((identity.subject.as_str(), identity.email), ("oid-1", None));
        claims["xms_edov"] = json!(true);
        let identity = microsoft_identity(&tokens(Some(&claims), json!({})), "cid").unwrap();
        assert_eq!(identity.email.as_deref(), Some("u@example.com"));
        claims["iss"] = json!("https://login.microsoftonline.com/common/v2.0");
        assert!(microsoft_identity(&tokens(Some(&claims), json!({})), "cid").is_err());
    }

    #[test]
    fn slack_identities() {
        let user = tokens(
            None,
            json!({"team": {"id": "T1"}, "authed_user": {"id": "U1", "name": "ada"}}),
        );
        let identity = slack_identity(&user, "cid").unwrap();
        assert_eq!(
            (
                identity.subject.as_str(),
                identity.team_id.as_deref(),
                identity.name.as_deref()
            ),
            ("U1", Some("T1"), Some("ada"))
        );
        let bot = tokens(
            None,
            json!({"team": {"id": "T1", "name": "Acme"}, "bot_user_id": "UB"}),
        );
        let identity = slack_identity(&bot, "cid").unwrap();
        assert_eq!(
            (identity.subject.as_str(), identity.name.as_deref()),
            ("UB", Some("Acme"))
        );
        let claims =
            json!({"aud": "cid", "iss": "https://slack.com", "sub": "U9", "email": "a@b.test"});
        let oidc = slack_identity(&tokens(Some(&claims), json!({})), "cid").unwrap();
        assert_eq!(
            (oidc.subject.as_str(), oidc.email.as_deref()),
            ("U9", Some("a@b.test"))
        );
        assert!(slack_identity(&tokens(None, json!({})), "cid").is_err());

        let slack = Provider::registered(ProviderKey::Slack);
        let mut static_token = tokens(None, json!({}));
        static_token.refresh_token = None;
        assert!(!slack.require_refresh_token());
        assert!(!slack.refreshable_result(&static_token));
        assert_eq!(
            slack
                .validate_result(&static_token)
                .unwrap_err()
                .code
                .as_deref(),
            Some("missing_refresh_token")
        );
        static_token.expires_in = None;
        assert!(slack.validate_result(&static_token).is_ok());
    }

    #[test]
    fn mercator_requires_its_scope() {
        let mercator = Provider::registered(ProviderKey::Mercator);
        let mut set = tokens(None, json!({}));
        assert_eq!(
            mercator.validate_result(&set).unwrap_err().code.as_deref(),
            Some("missing_scope")
        );
        set.scopes = vec![MERCATOR_SCOPE.into()];
        assert!(mercator.validate_result(&set).is_ok());
        assert!(mercator.presettable());
        assert_eq!(
            mercator.credential_request_rules().unwrap()[0].paths,
            ["/mcp/auth"]
        );
    }

    #[test]
    fn parses_server_sent_events_and_wallets() {
        assert_eq!(sse_data("event: message\ndata: {\"a\":1}\n\n"), "{\"a\":1}");
        assert_eq!(sse_data("data: first\n\ndata: last\n\n"), "last");
        assert!(wallet_address("0xAbCdEf0123456789aBcDeF0123456789abcdef01"));
        assert!(!wallet_address("0x123"));
    }

    #[test]
    fn custom_config_validation_and_rules() {
        let config = |value: Value| value.as_object().unwrap().clone();
        let valid = config(json!({
            "authorization_endpoint": "https://idp.example/authorize",
            "token_endpoint": "https://idp.example/token",
            "resource": "https://api.example/v1/mcp"
        }));
        assert!(custom_config_errors(&valid).is_empty());
        let errors = custom_config_errors(&config(json!({
            "authorization_endpoint": "http://idp.example/authorize",
            "token_endpoint": "https://user:pw@idp.example/token",
            "userinfo_endpoint": "ftp://x",
            "extra": 1
        })));
        assert_eq!(errors.len(), 5, "{errors:?}");
        let provider = Provider::for_app("custom", "acme", &Value::Object(valid)).unwrap();
        assert_eq!(provider.display_name(), "acme");
        let rules = provider.credential_request_rules().unwrap();
        assert_eq!(
            (rules[0].host.as_str(), rules[0].paths.clone()),
            ("api.example", vec!["/v1/mcp".to_owned()])
        );
        assert_eq!(
            provider.extra_authorization_params(),
            [("resource", "https://api.example/v1/mcp".to_owned())]
        );
    }
}
