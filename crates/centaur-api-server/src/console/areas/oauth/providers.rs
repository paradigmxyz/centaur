//! Port of Console's OAuth provider strategies (`services/console/lib/oauth`)
//! and of the code-exchange error type they raise.
//!
//! A provider owns the IdP-specific parts of the consent flow: endpoints, the
//! extra authorization parameters, how to parse the granted scopes, and how to
//! resolve a stable account identity after the exchange. Everything else
//! (state signing, PKCE, the upsert) is provider-agnostic.

use std::collections::BTreeMap;
use std::time::Duration;

use centaur_console_db::areas::oauth::{OauthApp, WrappingRule};
use regex::Regex;
use serde_json::{Map, Value, json};

use super::http::{self, HttpResponse};

/// `Broker::ExchangeError`: a consent-flow failure that is shown as the result
/// page's reason (the OAuth error code when present, else the stage).
#[derive(Clone, Debug)]
pub(crate) struct ExchangeError {
    pub(crate) stage: &'static str,
    pub(crate) code: Option<String>,
    pub(crate) status: Option<u16>,
    pub(crate) message: String,
}

impl ExchangeError {
    pub(crate) fn new(message: impl Into<String>, stage: &'static str) -> Self {
        Self {
            stage,
            code: None,
            status: None,
            message: message.into(),
        }
    }

    pub(crate) fn with_code(
        message: impl Into<String>,
        stage: &'static str,
        code: impl Into<String>,
    ) -> Self {
        Self {
            code: Some(code.into()),
            ..Self::new(message, stage)
        }
    }

    pub(crate) fn reason(&self) -> String {
        self.code
            .clone()
            .filter(|code| !code.is_empty())
            .unwrap_or_else(|| self.stage.to_owned())
    }
}

/// The normalized success result of `Broker::AuthorizationCodeClient#exchange`.
#[derive(Clone, Debug)]
pub(crate) struct CodeExchangeResult {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) expires_in: Option<i64>,
    /// The provider's native granted-scope value (string or, for a few IdPs, an
    /// array); `None` when the token response omitted it.
    pub(crate) scope: Option<Value>,
    pub(crate) id_token: Option<String>,
    /// The parsed provider response, for provider-specific metadata (Slack's
    /// `authed_user`, for example).
    pub(crate) response: Value,
}

/// The account identity a consent upserts a credential by.
#[derive(Clone, Debug)]
pub(crate) struct Identity {
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) labels: Map<String, Value>,
    pub(crate) team_id: Option<String>,
}

impl Identity {
    fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            email: None,
            name: None,
            labels: Map::new(),
            team_id: None,
        }
    }
}

/// The registry order is the console provider list; the first entry is the
/// form default. Matches `Oauth::Providers.registry`.
pub(crate) const PROVIDER_KEYS: [&str; 10] = [
    "attio",
    "custom",
    "github",
    "google",
    "granola",
    "linear",
    "mercator",
    "microsoft",
    "slack",
    "zoom",
];

pub(crate) const CUSTOM_KEY: &str = "custom";
pub(crate) const SLACK_KEY: &str = "slack";
pub(crate) const MERCATOR_KEY: &str = "mercator";
pub(crate) const GITHUB_KEY: &str = "github";

pub(crate) const CUSTOM_CONFIG_KEYS: [&str; 5] = [
    "authorization_endpoint",
    "token_endpoint",
    "userinfo_endpoint",
    "revocation_endpoint",
    "resource",
];
pub(crate) const CUSTOM_OPTIONAL_CONFIG_KEYS: [&str; 2] =
    ["userinfo_endpoint", "revocation_endpoint"];

const MERCATOR_ORIGIN: &str = "https://mercator.sh";
const MERCATOR_SCOPE: &str = "mercator:tools";

/// A configured provider strategy. The custom provider carries the app's
/// `provider_config`; every other strategy is stateless.
#[derive(Clone, Debug)]
pub(crate) struct Provider {
    kind: &'static str,
    custom: Option<CustomProvider>,
}

#[derive(Clone, Debug, Default)]
struct CustomProvider {
    slug: String,
    config: Map<String, Value>,
}

impl Provider {
    /// The strategy for an app, or `None` for an unknown provider key.
    pub(crate) fn from_app(app: &OauthApp) -> Option<Self> {
        if app.provider == CUSTOM_KEY {
            return Some(Self {
                kind: CUSTOM_KEY,
                custom: Some(CustomProvider {
                    slug: app.slug.clone(),
                    config: app.provider_config.clone(),
                }),
            });
        }
        let kind = PROVIDER_KEYS
            .into_iter()
            .find(|key| *key == app.provider.as_str())?;
        Some(Self { kind, custom: None })
    }

    /// The unconfigured registry entry, for the provider list and presets.
    pub(crate) fn registry(key: &str) -> Option<Self> {
        let kind = PROVIDER_KEYS
            .into_iter()
            .find(|candidate| *candidate == key)?;
        Some(Self { kind, custom: None })
    }

    pub(crate) fn registry_all() -> Vec<Self> {
        PROVIDER_KEYS
            .into_iter()
            .filter_map(Self::registry)
            .collect()
    }

    pub(crate) fn key(&self) -> &'static str {
        self.kind
    }

    pub(crate) fn display_name(&self) -> String {
        match self.kind {
            CUSTOM_KEY => self
                .custom
                .as_ref()
                .map(|custom| custom.slug.clone())
                .filter(|slug| !slug.is_empty())
                .unwrap_or_else(|| "Custom".to_owned()),
            "attio" => "Attio".to_owned(),
            "github" => "GitHub".to_owned(),
            "google" => "Google".to_owned(),
            "granola" => "Granola".to_owned(),
            "linear" => "Linear".to_owned(),
            "mercator" => "Mercator".to_owned(),
            "microsoft" => "Microsoft".to_owned(),
            "slack" => "Slack".to_owned(),
            "zoom" => "Zoom".to_owned(),
            _ => self.kind.to_owned(),
        }
    }

    fn config_str(&self, key: &str) -> Option<String> {
        self.custom
            .as_ref()
            .and_then(|custom| custom.config.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    pub(crate) fn authorization_endpoint(&self) -> Option<String> {
        match self.kind {
            "attio" => Some("https://app.attio.com/authorize".to_owned()),
            "github" => Some("https://github.com/login/oauth/authorize".to_owned()),
            "google" => Some("https://accounts.google.com/o/oauth2/v2/auth".to_owned()),
            "granola" => Some("https://mcp-auth.granola.ai/oauth2/authorize".to_owned()),
            "linear" => Some("https://linear.app/oauth/authorize".to_owned()),
            "mercator" => Some(format!("{MERCATOR_ORIGIN}/authorize")),
            "microsoft" => {
                Some("https://login.microsoftonline.com/common/oauth2/v2.0/authorize".to_owned())
            }
            "slack" => Some("https://slack.com/oauth/v2/authorize".to_owned()),
            "zoom" => Some("https://zoom.us/oauth/authorize".to_owned()),
            CUSTOM_KEY => self.config_str("authorization_endpoint"),
            _ => None,
        }
    }

    pub(crate) fn token_endpoint(&self) -> Option<String> {
        match self.kind {
            "attio" => Some("https://app.attio.com/oauth/token".to_owned()),
            "github" => Some("https://github.com/login/oauth/access_token".to_owned()),
            "google" => Some("https://oauth2.googleapis.com/token".to_owned()),
            "granola" => Some("https://mcp-auth.granola.ai/oauth2/token".to_owned()),
            "linear" => Some("https://api.linear.app/oauth/token".to_owned()),
            "mercator" => Some(format!("{MERCATOR_ORIGIN}/oauth/token")),
            "microsoft" => {
                Some("https://login.microsoftonline.com/common/oauth2/v2.0/token".to_owned())
            }
            "slack" => Some(super::slack_api_endpoint("oauth.v2.access")),
            "zoom" => Some("https://zoom.us/oauth/token".to_owned()),
            CUSTOM_KEY => self.config_str("token_endpoint"),
            _ => None,
        }
    }

    pub(crate) fn revocation_endpoint(&self) -> Option<String> {
        if self.kind == CUSTOM_KEY {
            self.config_str("revocation_endpoint")
        } else {
            None
        }
    }

    pub(crate) fn identity_scopes(&self) -> Vec<String> {
        match self.kind {
            "google" => vec![
                "openid".to_owned(),
                "https://www.googleapis.com/auth/userinfo.email".to_owned(),
            ],
            "granola" => vec![
                "openid".to_owned(),
                "email".to_owned(),
                "profile".to_owned(),
                "offline_access".to_owned(),
            ],
            "microsoft" => vec![
                "openid".to_owned(),
                "email".to_owned(),
                "profile".to_owned(),
                "offline_access".to_owned(),
            ],
            _ => Vec::new(),
        }
    }

    pub(crate) fn required_scopes(&self) -> Vec<String> {
        match self.kind {
            "mercator" => vec![MERCATOR_SCOPE.to_owned()],
            "slack" => vec!["search:read".to_owned()],
            "zoom" => vec!["user:read:user".to_owned()],
            _ => Vec::new(),
        }
    }

    pub(crate) fn api_hosts(&self) -> Vec<String> {
        match self.kind {
            "attio" => vec!["api.attio.com".to_owned()],
            "google" => vec!["*.googleapis.com".to_owned()],
            "granola" => vec!["mcp.granola.ai".to_owned()],
            "linear" => vec![
                "api.linear.app".to_owned(),
                "uploads.linear.app".to_owned(),
                "mcp.linear.app".to_owned(),
            ],
            "microsoft" => vec!["graph.microsoft.com".to_owned()],
            "slack" => vec!["slack.com".to_owned()],
            "zoom" => vec!["api.zoom.us".to_owned()],
            "mercator" => vec!["mercator.sh".to_owned()],
            _ => Vec::new(),
        }
    }

    pub(crate) fn authorization_scope_param(&self) -> &'static str {
        if self.kind == SLACK_KEY {
            "user_scope"
        } else {
            "scope"
        }
    }

    pub(crate) fn scope_separator(&self) -> &'static str {
        match self.kind {
            "linear" | "slack" => ",",
            _ => " ",
        }
    }

    pub(crate) fn extra_authorization_params(&self) -> Vec<(String, String)> {
        match self.kind {
            "google" => vec![
                ("access_type".to_owned(), "offline".to_owned()),
                ("prompt".to_owned(), "consent".to_owned()),
            ],
            "microsoft" => vec![("prompt".to_owned(), "select_account".to_owned())],
            "mercator" => vec![("resource".to_owned(), format!("{MERCATOR_ORIGIN}/mcp/auth"))],
            CUSTOM_KEY => self
                .config_str("resource")
                .map(|resource| vec![("resource".to_owned(), resource)])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn extra_token_params(&self) -> Vec<(String, String)> {
        if self.kind == CUSTOM_KEY {
            self.config_str("resource")
                .map(|resource| vec![("resource".to_owned(), resource)])
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn refreshable(&self) -> bool {
        !matches!(self.kind, "attio" | "github")
    }

    pub(crate) fn require_refresh_token(&self) -> bool {
        if self.kind == SLACK_KEY {
            false
        } else {
            self.refreshable()
        }
    }

    pub(crate) fn refreshable_result(&self, result: &CodeExchangeResult) -> bool {
        if self.kind == SLACK_KEY {
            result
                .refresh_token
                .as_deref()
                .is_some_and(|token| !token.is_empty())
        } else {
            self.refreshable()
        }
    }

    pub(crate) fn token_endpoint_auth_method(&self) -> &'static str {
        if self.kind == "zoom" {
            "client_secret_basic"
        } else {
            "client_secret_post"
        }
    }

    pub(crate) fn parse_granted_scopes(&self, scope: &Value) -> Vec<String> {
        match self.kind {
            "linear" => match scope {
                Value::Array(items) => items
                    .iter()
                    .map(|item| item.as_str().unwrap_or_default().to_owned())
                    .filter(|item| !item.is_empty())
                    .collect(),
                other => split_scopes(other.as_str().unwrap_or_default(), true),
            },
            "attio" | "github" | "slack" => split_scopes(scope.as_str().unwrap_or_default(), true),
            _ => scope
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        }
    }

    pub(crate) fn foreign_id_subject(&self, subject: &str) -> String {
        if self.kind == "zoom" {
            subject.to_owned()
        } else {
            subject.to_lowercase()
        }
    }

    pub(crate) fn wrapping_secret_kind(&self) -> &'static str {
        if self.kind == GITHUB_KEY {
            "github_token"
        } else {
            "custom"
        }
    }

    /// The canonical `custom` inject configuration, or the profile-owned
    /// replacement configuration for the GitHub profile.
    ///
    /// Keyed on the wrapping secret's *kind*, not the provider key: Rails
    /// (`Oauth::FlowsController#ensure_wrapping_secret`) gives every
    /// `custom`-kind wrapper the Bearer inject config, whichever provider
    /// minted it. Only profile kinds (GitHub's `github_token`) bring their own
    /// configuration, which the flow applies.
    pub(crate) fn wrapping_secret_config(&self) -> (Option<Value>, Option<Value>) {
        if self.wrapping_secret_kind() == "custom" {
            (
                Some(json!({"header": "Authorization", "formatter": "Bearer {{ .Value }}"})),
                None,
            )
        } else {
            (None, None)
        }
    }

    pub(crate) fn credential_request_rules(&self) -> Option<Vec<WrappingRule>> {
        match self.kind {
            "mercator" => Some(vec![WrappingRule {
                host: "mercator.sh".to_owned(),
                http_methods: vec!["POST".to_owned()],
                paths: vec!["/mcp/auth".to_owned()],
            }]),
            CUSTOM_KEY => {
                let resource = self.config_str("resource")?;
                let url = url::Url::parse(&resource).ok()?;
                let path = url.path();
                Some(vec![WrappingRule {
                    host: url.host_str()?.to_owned(),
                    http_methods: Vec::new(),
                    paths: if path.is_empty() || path == "/" {
                        Vec::new()
                    } else {
                        vec![path.to_owned()]
                    },
                }])
            }
            _ => None,
        }
    }

    pub(crate) fn validate_result(&self, result: &CodeExchangeResult) -> Result<(), ExchangeError> {
        match self.kind {
            "mercator" => {
                let scopes = result
                    .scope
                    .as_ref()
                    .map(|scope| self.parse_granted_scopes(scope))
                    .unwrap_or_default();
                if scopes.iter().any(|scope| scope == MERCATOR_SCOPE) {
                    Ok(())
                } else {
                    Err(ExchangeError::with_code(
                        "Mercator scope was not granted",
                        "oauth",
                        "missing_scope",
                    ))
                }
            }
            SLACK_KEY => {
                if result
                    .refresh_token
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
                    && result.expires_in.is_some()
                {
                    Err(ExchangeError::with_code(
                        "token endpoint returned expiring Slack token without refresh_token",
                        "oauth",
                        "missing_refresh_token",
                    ))
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    pub(crate) async fn identity_from(
        &self,
        result: &CodeExchangeResult,
        client_id: &str,
    ) -> Result<Identity, ExchangeError> {
        match self.kind {
            "attio" => self.attio_identity(result).await,
            "github" => self.github_identity(result).await,
            "google" => self.google_identity(result, client_id),
            "granola" => self.granola_identity(result, client_id),
            "linear" => self.linear_identity(result).await,
            "mercator" => self.mercator_identity(result).await,
            "microsoft" => self.microsoft_identity(result, client_id),
            "slack" => self.slack_identity(result, client_id),
            "zoom" => self.zoom_identity(result).await,
            CUSTOM_KEY => self.custom_identity(result).await,
            _ => Err(ExchangeError::with_code(
                "unknown provider",
                "parse",
                "invalid_identity_response",
            )),
        }
    }

    async fn get_identity_json(
        &self,
        response: Result<HttpResponse, reqwest::Error>,
    ) -> Result<Value, ExchangeError> {
        let response = response.map_err(|error| {
            ExchangeError::with_code(
                format!(
                    "{} identity endpoint request failed: {error}",
                    self.display_name()
                ),
                "network",
                "identity_lookup_failed",
            )
        })?;
        if !response.success() {
            return Err(ExchangeError {
                stage: "http",
                code: Some("identity_lookup_failed".to_owned()),
                status: Some(response.status),
                message: format!(
                    "{} identity endpoint http {}",
                    self.display_name(),
                    response.status
                ),
            });
        }
        match response.json() {
            Some(Value::Object(map)) => Ok(Value::Object(map)),
            Some(_) => Err(ExchangeError {
                stage: "parse",
                code: Some("invalid_identity_response".to_owned()),
                status: Some(response.status),
                message: format!(
                    "{} identity endpoint returned an invalid payload",
                    self.display_name()
                ),
            }),
            None => Err(ExchangeError {
                stage: "parse",
                code: Some("invalid_identity_response".to_owned()),
                status: Some(response.status),
                message: format!(
                    "{} identity endpoint returned invalid JSON",
                    self.display_name()
                ),
            }),
        }
    }

    fn require_identity(&self, value: Option<&Value>) -> Result<String, ExchangeError> {
        match value {
            Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
            Some(Value::Number(value)) => Ok(value.to_string()),
            _ => Err(ExchangeError::with_code(
                format!(
                    "{} identity endpoint returned no stable subject",
                    self.display_name()
                ),
                "parse",
                "missing_identity",
            )),
        }
    }

    async fn attio_identity(&self, result: &CodeExchangeResult) -> Result<Identity, ExchangeError> {
        if result.access_token.is_empty() {
            return Err(missing_access_token());
        }
        let response = http::send(
            reqwest::Method::GET,
            "https://api.attio.com/v2/self",
            &[
                http::bearer(&result.access_token),
                http::header("User-Agent", "centaur-console"),
            ],
            None,
            None,
            Duration::from_secs(30),
            64 * 1024,
        )
        .await;
        let workspace = self.get_identity_json(response).await?;
        let subject = self.require_identity(workspace.get("workspace_id"))?;
        let name = ["workspace_name", "workspace_slug"]
            .iter()
            .find_map(|key| workspace.get(*key).and_then(Value::as_str))
            .filter(|value| !value.is_empty())
            .unwrap_or(&subject)
            .to_owned();
        let mut identity = Identity::new(subject);
        identity.name = Some(name);
        Ok(identity)
    }

    async fn github_identity(
        &self,
        result: &CodeExchangeResult,
    ) -> Result<Identity, ExchangeError> {
        if result.access_token.is_empty() {
            return Err(missing_access_token());
        }
        let response = http::send(
            reqwest::Method::GET,
            "https://api.github.com/user",
            &[
                http::header("Accept", "application/vnd.github+json"),
                http::bearer(&result.access_token),
                http::header("X-GitHub-Api-Version", "2022-11-28"),
                http::header("User-Agent", "centaur-console"),
            ],
            None,
            None,
            Duration::from_secs(30),
            64 * 1024,
        )
        .await;
        let profile = self.get_identity_json(response).await?;
        let subject = self.require_identity(profile.get("id"))?;
        let login = self.require_identity(profile.get("login"))?;
        let mut identity = Identity::new(subject);
        identity.email = nonempty_string(profile.get("email"));
        identity.name = nonempty_string(profile.get("name")).or_else(|| Some(login.clone()));
        identity
            .labels
            .insert("github_login".to_owned(), Value::String(login));
        Ok(identity)
    }

    fn google_identity(
        &self,
        result: &CodeExchangeResult,
        client_id: &str,
    ) -> Result<Identity, ExchangeError> {
        let claims = id_token_claims(result)?;
        require_audience(&claims, client_id)?;
        let issuer = claims
            .get("iss")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(
            issuer,
            "https://accounts.google.com" | "accounts.google.com"
        ) {
            return Err(ExchangeError::with_code(
                "id_token iss was not a Google issuer",
                "oauth",
                "id_token_iss_invalid",
            ));
        }
        let subject = self.require_identity(claims.get("sub"))?;
        let mut identity = Identity::new(subject);
        identity.email = nonempty_string(claims.get("email"));
        Ok(identity)
    }

    fn granola_identity(
        &self,
        result: &CodeExchangeResult,
        client_id: &str,
    ) -> Result<Identity, ExchangeError> {
        let claims = id_token_claims(result)?;
        require_audience(&claims, client_id)?;
        if claims.get("iss").and_then(Value::as_str) != Some("https://mcp-auth.granola.ai") {
            return Err(ExchangeError::with_code(
                "id_token iss was not a Granola issuer",
                "oauth",
                "id_token_iss_invalid",
            ));
        }
        let subject = self.require_identity(claims.get("sub"))?;
        let mut identity = Identity::new(subject);
        identity.email = nonempty_string(claims.get("email"));
        Ok(identity)
    }

    fn microsoft_identity(
        &self,
        result: &CodeExchangeResult,
        client_id: &str,
    ) -> Result<Identity, ExchangeError> {
        let claims = id_token_claims(result)?;
        require_audience(&claims, client_id)?;
        if !microsoft_issuer(
            claims
                .get("iss")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ) {
            return Err(ExchangeError::with_code(
                "id_token iss was not a Microsoft issuer",
                "oauth",
                "id_token_iss_invalid",
            ));
        }
        let subject = self.require_identity(claims.get("oid"))?;
        let mut identity = Identity::new(subject);
        // Only verified domains are trusted: on the common endpoint any tenant
        // admin can set an arbitrary `email`.
        if boolean_claim(claims.get("xms_edov")) {
            identity.email = nonempty_string(claims.get("email"));
        }
        identity.name = nonempty_string(claims.get("name"));
        Ok(identity)
    }

    fn slack_identity(
        &self,
        result: &CodeExchangeResult,
        client_id: &str,
    ) -> Result<Identity, ExchangeError> {
        let response = &result.response;
        let authed_user = response.get("authed_user").and_then(Value::as_object);
        if let Some(subject) = authed_user
            .and_then(|user| user.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            let mut identity = Identity::new(subject);
            identity.email = authed_user
                .and_then(|user| user.get("email"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            identity.name = authed_user
                .and_then(|user| user.get("name"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    authed_user
                        .and_then(|user| user.get("user"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                })
                .map(str::to_owned);
            identity.team_id = response
                .get("team")
                .and_then(|team| team.get("id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    authed_user
                        .and_then(|user| user.get("team_id"))
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                })
                .map(str::to_owned);
            return Ok(identity);
        }
        if let Some(bot_user_id) = response
            .get("bot_user_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            let mut identity = Identity::new(bot_user_id);
            identity.name = response
                .get("team")
                .and_then(|team| team.get("name"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or(Some(bot_user_id))
                .map(str::to_owned);
            identity.team_id = response
                .get("team")
                .and_then(|team| team.get("id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            return Ok(identity);
        }
        let id_token = result.id_token.as_deref().filter(|value| !value.is_empty());
        let Some(id_token) = id_token else {
            return Err(ExchangeError::with_code(
                "token response carried no id_token",
                "oauth",
                "missing_id_token",
            ));
        };
        let claims = decode_id_token(id_token)?;
        require_audience(&claims, client_id)?;
        if claims.get("iss").and_then(Value::as_str) != Some("https://slack.com") {
            return Err(ExchangeError::with_code(
                "id_token iss was not a Slack issuer",
                "oauth",
                "id_token_iss_invalid",
            ));
        }
        let subject = self.require_identity(claims.get("sub"))?;
        let mut identity = Identity::new(subject);
        identity.email = nonempty_string(claims.get("email"));
        identity.name = nonempty_string(claims.get("name"));
        Ok(identity)
    }

    async fn linear_identity(
        &self,
        result: &CodeExchangeResult,
    ) -> Result<Identity, ExchangeError> {
        if result.access_token.is_empty() {
            return Err(missing_access_token());
        }
        let response = http::send(
            reqwest::Method::POST,
            "https://api.linear.app/graphql",
            &[
                http::bearer(&result.access_token),
                http::header("User-Agent", "centaur-console"),
            ],
            None,
            Some(&json!({"query": "{ viewer { id name email } }"})),
            Duration::from_secs(30),
            64 * 1024,
        )
        .await;
        let payload = self.get_identity_json(response).await?;
        let viewer = payload
            .get("data")
            .and_then(|data| data.get("viewer"))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ExchangeError::with_code(
                    "Linear identity endpoint returned no viewer",
                    "parse",
                    "missing_identity",
                )
            })?;
        let subject = self.require_identity(viewer.get("id"))?;
        let mut identity = Identity::new(subject.clone());
        identity.email = nonempty_string(viewer.get("email"));
        identity.name = nonempty_string(viewer.get("name"))
            .or_else(|| identity.email.clone())
            .or(Some(subject));
        Ok(identity)
    }

    async fn zoom_identity(&self, result: &CodeExchangeResult) -> Result<Identity, ExchangeError> {
        let response = http::send(
            reqwest::Method::GET,
            "https://api.zoom.us/v2/users/me",
            &[
                http::bearer(&result.access_token),
                http::header("User-Agent", "centaur-console"),
            ],
            None,
            None,
            Duration::from_secs(30),
            64 * 1024,
        )
        .await;
        let user = self.get_identity_json(response).await?;
        let subject = self.require_identity(user.get("id"))?;
        let mut identity = Identity::new(subject.clone());
        identity.email = nonempty_string(user.get("email"));
        identity.name = nonempty_string(user.get("display_name"))
            .or_else(|| identity.email.clone())
            .or(Some(subject));
        Ok(identity)
    }

    async fn custom_identity(
        &self,
        result: &CodeExchangeResult,
    ) -> Result<Identity, ExchangeError> {
        if result.access_token.is_empty() {
            return Err(missing_access_token());
        }
        let claims = if let Some(userinfo) = self.config_str("userinfo_endpoint") {
            let response = http::send(
                reqwest::Method::GET,
                &userinfo,
                &[
                    http::bearer(&result.access_token),
                    http::header("Accept", "application/json"),
                ],
                None,
                None,
                Duration::from_secs(30),
                64 * 1024,
            )
            .await;
            self.get_identity_json(response)
                .await?
                .as_object()
                .cloned()
                .unwrap_or_default()
        } else {
            access_token_claims(&result.access_token)?
        };
        let subject = self.require_identity(claims.get("sub"))?;
        let mut identity = Identity::new(subject);
        identity.email = nonempty_string(claims.get("email"));
        identity.name = nonempty_string(claims.get("name"));
        Ok(identity)
    }

    async fn mercator_identity(
        &self,
        result: &CodeExchangeResult,
    ) -> Result<Identity, ExchangeError> {
        if result.access_token.is_empty() {
            return Err(missing_access_token());
        }
        let response = http::send(
            reqwest::Method::POST,
            &format!("{MERCATOR_ORIGIN}/mcp/auth"),
            &[
                http::bearer(&result.access_token),
                http::header("Accept", "application/json, text/event-stream"),
            ],
            None,
            Some(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "get_connection_status", "arguments": {}}
            })),
            Duration::from_secs(30),
            128 * 1024,
        )
        .await;
        let response = response.map_err(|error| {
            ExchangeError::with_code(
                format!("Mercator identity endpoint request failed: {error}"),
                "network",
                "identity_lookup_failed",
            )
        })?;
        if !response.success() {
            return Err(ExchangeError {
                stage: "http",
                code: Some("identity_lookup_failed".to_owned()),
                status: Some(response.status),
                message: format!("Mercator identity endpoint http {}", response.status),
            });
        }
        let body = if response
            .content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/event-stream"))
        {
            let body = response.body.replace("\r\n", "\n");
            let events: Vec<&str> = body.split("\n\n").collect();
            let event = events
                .into_iter()
                .rev()
                .find(|item| item.lines().any(|line| line.starts_with("data:")))
                .unwrap_or_default()
                .to_owned();
            event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            response.body
        };
        let payload: Value = serde_json::from_str(&body).map_err(|_| {
            ExchangeError::with_code(
                "Mercator identity endpoint returned invalid JSON",
                "parse",
                "invalid_identity_response",
            )
        })?;
        let status = payload
            .get("result")
            .filter(|result| {
                result.get("isError").and_then(Value::as_bool) != Some(true)
                    && payload.get("error").is_none_or(Value::is_null)
            })
            .and_then(|result| result.get("structuredContent"));
        let address = status
            .and_then(|status| status.get("account"))
            .and_then(|account| account.get("walletAddress"))
            .and_then(Value::as_str)
            .filter(|address| wallet_address(address));
        if status.and_then(|status| status.get("oauthAuthenticated")) != Some(&Value::Bool(true))
            || address.is_none()
        {
            return Err(ExchangeError::with_code(
                "Mercator wallet identity unavailable",
                "oauth",
                "missing_wallet",
            ));
        }
        let address = address.expect("checked");
        let mut identity = Identity::new(address.to_lowercase());
        identity.name = Some(address.to_owned());
        Ok(identity)
    }

    /// The optional Mercator wallet panel. Any failure yields `{"claimed": null}`
    /// so credential management keeps working.
    pub(crate) async fn details_for(&self, credential: &OauthAppCredential) -> Value {
        if self.kind != MERCATOR_KEY {
            return json!({});
        }
        let response = http::send(
            reqwest::Method::GET,
            &format!("{MERCATOR_ORIGIN}/v1/claims/config?provider=slack"),
            &[http::bearer(&credential.access_token)],
            None,
            None,
            Duration::from_secs(2),
            128 * 1024,
        )
        .await;
        let Some(response) = response.ok() else {
            return json!({"claimed": null});
        };
        let body = response.json();
        let Some(Value::Object(body)) = body else {
            return json!({"claimed": null});
        };
        let wallet = body.get("wallet_address").and_then(Value::as_str);
        let claimed = body.get("claimed").and_then(Value::as_bool);
        if wallet.is_some_and(|wallet| wallet.eq_ignore_ascii_case(&credential.provider_subject))
            && claimed.is_some()
        {
            json!({"claimed": claimed})
        } else {
            json!({"claimed": null})
        }
    }
}

/// The credential fields the provider-details lookup needs.
#[derive(Clone, Debug)]
pub(crate) struct OauthAppCredential {
    pub(crate) access_token: String,
    pub(crate) provider_subject: String,
}

fn missing_access_token() -> ExchangeError {
    ExchangeError::with_code(
        "token response returned an empty access_token",
        "parse",
        "missing_access_token",
    )
}

fn split_scopes(raw: &str, comma: bool) -> Vec<String> {
    raw.split(|character: char| character.is_whitespace() || (comma && character == ','))
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect()
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn boolean_claim(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().is_some_and(|value| value != 0),
        Some(Value::String(value)) => !matches!(
            value.as_str(),
            "" | "0" | "f" | "F" | "false" | "FALSE" | "off" | "OFF"
        ),
        _ => false,
    }
}

fn microsoft_issuer(issuer: &str) -> bool {
    static ISSUER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    ISSUER
        .get_or_init(|| {
            Regex::new(r"^https://login\.microsoftonline\.com/[0-9a-f-]{36}/v2\.0$")
                .expect("valid Microsoft issuer pattern")
        })
        .is_match(issuer)
}

fn wallet_address(address: &str) -> bool {
    static ADDRESS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    ADDRESS
        .get_or_init(|| Regex::new(r"^0x[0-9a-fA-F]{40}$").expect("valid wallet pattern"))
        .is_match(address)
}

fn require_audience(claims: &Map<String, Value>, client_id: &str) -> Result<(), ExchangeError> {
    let audience = claims.get("aud");
    let matches = match audience {
        Some(Value::String(value)) => value == client_id,
        Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some(client_id)),
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(ExchangeError::with_code(
            "id_token aud did not match client_id",
            "oauth",
            "id_token_aud_mismatch",
        ))
    }
}

fn id_token_claims(result: &CodeExchangeResult) -> Result<Map<String, Value>, ExchangeError> {
    let Some(id_token) = result.id_token.as_deref().filter(|value| !value.is_empty()) else {
        return Err(ExchangeError::with_code(
            "token response carried no id_token",
            "oauth",
            "missing_id_token",
        ));
    };
    decode_id_token(id_token)
}

fn decode_id_token(id_token: &str) -> Result<Map<String, Value>, ExchangeError> {
    use base64::Engine;
    let segment = id_token.split('.').nth(1).unwrap_or_default();
    let decoded = base64::engine::general_purpose::URL_SAFE_PAD_INDIFFERENT
        .decode(segment)
        .map_err(|_| ExchangeError::new("id_token payload did not decode", "parse"))?;
    match serde_json::from_slice::<Value>(&decoded) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(ExchangeError::new(
            "id_token payload did not decode",
            "parse",
        )),
    }
}

fn access_token_claims(access_token: &str) -> Result<Map<String, Value>, ExchangeError> {
    decode_id_token(access_token).map_err(|_| {
        ExchangeError::with_code(
            "access token is not a JWT and no userinfo endpoint is configured",
            "parse",
            "invalid_identity_response",
        )
    })
}

/// The custom provider's config keys as `OauthApp#provider_config_valid` sees
/// them: an app's `provider_config` field. [`garde::Validate`] appends every
/// message the model adds, in its order, so this one field can report several
/// errors (garde's field rules otherwise return one).
#[derive(Clone, Debug)]
pub(crate) struct ProviderConfig {
    provider: String,
    config: Map<String, Value>,
}

impl ProviderConfig {
    pub(crate) fn new(provider: &str, config: &Map<String, Value>) -> Self {
        Self {
            provider: provider.to_owned(),
            config: config.clone(),
        }
    }
}

impl garde::Validate for ProviderConfig {
    type Context = ();

    fn validate_into(
        &self,
        _context: &Self::Context,
        parent: &mut dyn FnMut() -> garde::Path,
        report: &mut garde::Report,
    ) {
        if self.provider != CUSTOM_KEY {
            if !self.config.is_empty() {
                report.append(
                    parent(),
                    garde::Error::new("is only supported by the custom provider"),
                );
            }
            return;
        }
        let unknown: Vec<&str> = self
            .config
            .keys()
            .map(String::as_str)
            .filter(|key| !CUSTOM_CONFIG_KEYS.contains(key))
            .collect();
        if !unknown.is_empty() {
            report.append(
                parent(),
                garde::Error::new(format!("has unknown keys: {}", unknown.join(", "))),
            );
        }
        for key in CUSTOM_CONFIG_KEYS {
            let value = self.config.get(key);
            let optional = CUSTOM_OPTIONAL_CONFIG_KEYS.contains(&key);
            let blank = value.is_none_or(|value| match value {
                Value::Null => true,
                Value::String(value) => value.trim().is_empty(),
                _ => false,
            });
            if optional && blank {
                continue;
            }
            if !value.is_some_and(https_url) {
                report.append(
                    parent(),
                    garde::Error::new(format!("{key} must be an https URL")),
                );
            }
        }
    }
}

fn https_url(value: &Value) -> bool {
    let Some(raw) = value.as_str() else {
        return false;
    };
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
}

/// The registration request fields a preset builds (RFC 7591).
#[derive(Clone, Debug)]
pub(crate) struct RegistrationRequest {
    pub(crate) endpoint: String,
    pub(crate) redirect_uri: String,
    pub(crate) scope: String,
    pub(crate) auth_method: &'static str,
}

/// A preset the Integrations page can set up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Preset {
    Mercator,
    Runlayer,
}

#[derive(Clone, Debug)]
pub(crate) struct PresetAttributes {
    pub(crate) slug: String,
    pub(crate) description: String,
    pub(crate) allowed_scopes: Vec<String>,
    pub(crate) provider: &'static str,
    pub(crate) shared: bool,
    pub(crate) owner_only: bool,
    pub(crate) always_available: bool,
    pub(crate) labels: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct PresetSetup {
    pub(crate) provider_config: Map<String, Value>,
    pub(crate) registration_endpoint: String,
}

impl Preset {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Mercator => MERCATOR_KEY,
            Self::Runlayer => "runlayer",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Mercator => "Mercator",
            Self::Runlayer => "Runlayer",
        }
    }

    pub(crate) fn fetch(key: &str) -> Option<Self> {
        match key {
            "runlayer" => Some(Self::Runlayer),
            MERCATOR_KEY => Some(Self::Mercator),
            _ => None,
        }
    }

    pub(crate) fn all() -> Vec<Self> {
        vec![Self::Mercator, Self::Runlayer]
    }

    pub(crate) fn attributes(self) -> PresetAttributes {
        match self {
            Self::Mercator => PresetAttributes {
                slug: MERCATOR_KEY.to_owned(),
                description: "Shared with your agents.".to_owned(),
                allowed_scopes: vec![MERCATOR_SCOPE.to_owned()],
                provider: MERCATOR_KEY,
                shared: true,
                owner_only: false,
                always_available: false,
                labels: Map::new(),
            },
            Self::Runlayer => {
                let mut labels = Map::new();
                labels.insert(
                    "centaur-tool".to_owned(),
                    Value::String("runlayer".to_owned()),
                );
                PresetAttributes {
                    slug: "runlayer".to_owned(),
                    description: "Your Runlayer tools and permissions.".to_owned(),
                    allowed_scopes: vec!["mcp:proxy".to_owned()],
                    provider: CUSTOM_KEY,
                    shared: false,
                    owner_only: true,
                    always_available: true,
                    labels,
                }
            }
        }
    }

    pub(crate) fn token_endpoint_auth_method(self) -> &'static str {
        match self {
            Self::Mercator => "client_secret_post",
            Self::Runlayer => "client_secret_post",
        }
    }

    pub(crate) fn setup_inputs(self) -> Value {
        match self {
            Self::Mercator => json!([]),
            Self::Runlayer => json!([{
                "name": "resource_url",
                "label": "MCP URL",
                "type": "url",
                "placeholder": "https://<tenant>/mcp"
            }]),
        }
    }

    pub(crate) fn setup(self, resource_url: Option<&str>) -> Option<PresetSetup> {
        match self {
            Self::Mercator => Some(PresetSetup {
                provider_config: Map::new(),
                registration_endpoint: format!("{MERCATOR_ORIGIN}/oauth/register"),
            }),
            Self::Runlayer => runlayer_setup(resource_url.unwrap_or_default()),
        }
    }
}

fn runlayer_setup(raw: &str) -> Option<PresetSetup> {
    let raw = raw.trim();
    let url = url::Url::parse(raw).ok()?;
    let port = url.port().unwrap_or(443);
    if url.scheme() != "https"
        || url.host_str().is_none()
        || port != 443
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with("/mcp")
        || raw
            .chars()
            .any(|character| matches!(character, '*' | '[' | ']' | '{' | '}'))
    {
        return None;
    }
    let endpoint = |action: &str| {
        let mut endpoint = url.clone();
        endpoint.set_path(&format!("/api/v1/oauth/{action}"));
        endpoint.to_string()
    };
    let mut provider_config = Map::new();
    provider_config.insert(
        "authorization_endpoint".to_owned(),
        Value::String(endpoint("authorize")),
    );
    provider_config.insert(
        "token_endpoint".to_owned(),
        Value::String(endpoint("token")),
    );
    provider_config.insert(
        "revocation_endpoint".to_owned(),
        Value::String(endpoint("revoke")),
    );
    provider_config.insert("resource".to_owned(), Value::String(raw.to_owned()));
    Some(PresetSetup {
        provider_config,
        registration_endpoint: endpoint("register"),
    })
}

/// Fetches a preset and validates the registered client's response.
pub(crate) async fn register_client(
    request: &RegistrationRequest,
) -> Result<(String, String), ExchangeError> {
    let body = json!({
        "client_name": "Centaur",
        "redirect_uris": [request.redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": request.auth_method,
        "scope": request.scope
    });
    let response = http::send(
        reqwest::Method::POST,
        &request.endpoint,
        &[],
        None,
        Some(&body),
        Duration::from_secs(30),
        128 * 1024,
    )
    .await
    .map_err(|_| ExchangeError::new("Client registration unavailable", "network"))?;
    let payload = response
        .success()
        .then(|| response.json())
        .flatten()
        .and_then(|value| value.as_object().cloned());
    let client_id = payload
        .as_ref()
        .and_then(|body| body.get("client_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let client_secret = payload
        .as_ref()
        .and_then(|body| body.get("client_secret"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    match (client_id, client_secret) {
        (Some(client_id), Some(client_secret)) => {
            Ok((client_id.to_owned(), client_secret.to_owned()))
        }
        _ => Err(ExchangeError::new("Client registration failed", "parse")),
    }
}

/// A provider preset's allowed scopes as a registration `scope` string: RFC
/// 7591 always uses spaces, whatever the consent separator is.
pub(crate) fn registration_scope(scopes: &[String]) -> String {
    scopes.join(" ")
}

/// The label slice an app's wrapping secret copies (`centaur-tool`).
pub(crate) fn tool_label(app: &OauthApp) -> Map<String, Value> {
    let mut labels = Map::new();
    if let Some(Value::String(tool)) = app.labels.get("centaur-tool") {
        labels.insert("centaur-tool".to_owned(), Value::String(tool.clone()));
    }
    labels
}

/// The provider keys sorted the way `Presets.all` sorts preset keys.
pub(crate) fn sorted_preset_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = Preset::all().iter().map(|preset| preset.key()).collect();
    keys.sort_unstable();
    keys
}

/// The provider registry as `{key => provider}` for validation.
pub(crate) fn registry_map() -> BTreeMap<&'static str, Provider> {
    PROVIDER_KEYS
        .into_iter()
        .filter_map(|key| Provider::registry(key).map(|provider| (key, provider)))
        .collect()
}

/// A service-level (not provider-level) registration call for tests.
pub(crate) type RegisterFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(String, String), ExchangeError>>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use garde::Validate;

    fn custom(config: Map<String, Value>) -> Provider {
        Provider {
            kind: CUSTOM_KEY,
            custom: Some(CustomProvider {
                slug: "custom-app".to_owned(),
                config,
            }),
        }
    }

    /// Rails' `ensure_wrapping_secret`: every `custom`-kind wrapper (any provider
    /// but GitHub) injects `Authorization: Bearer`; GitHub's `github_token`
    /// profile brings its own replace config, applied by the flow.
    #[test]
    fn every_custom_kind_wrapper_injects_a_bearer_token() {
        let bearer = json!({"header": "Authorization", "formatter": "Bearer {{ .Value }}"});
        for key in PROVIDER_KEYS {
            let provider = if key == CUSTOM_KEY {
                custom(Map::new())
            } else {
                Provider::registry(key).unwrap()
            };
            if key == GITHUB_KEY {
                assert_eq!(provider.wrapping_secret_kind(), "github_token");
                assert_eq!(provider.wrapping_secret_config(), (None, None), "{key}");
            } else {
                assert_eq!(provider.wrapping_secret_kind(), "custom", "{key}");
                assert_eq!(
                    provider.wrapping_secret_config(),
                    (Some(bearer.clone()), None),
                    "{key}"
                );
            }
        }
    }

    fn custom_config_errors(config: &Map<String, Value>) -> Vec<String> {
        let validation = ProviderConfig::new(CUSTOM_KEY, config);
        let Err(report) = validation.validate() else {
            return Vec::new();
        };
        report
            .into_inner()
            .into_iter()
            .map(|(_, error)| error.message().to_owned())
            .collect()
    }

    #[test]
    fn custom_config_validation_accepts_a_valid_config_and_rejects_bad_keys() {
        let mut config = Map::new();
        config.insert(
            "authorization_endpoint".to_owned(),
            json!("https://auth.example.test/authorize"),
        );
        config.insert(
            "token_endpoint".to_owned(),
            json!("https://auth.example.test/token"),
        );
        config.insert("resource".to_owned(), json!("https://api.example.test/mcp"));
        assert!(custom_config_errors(&config).is_empty());

        config.insert("extra".to_owned(), json!("x"));
        assert!(!custom_config_errors(&config).is_empty());

        config.remove("token_endpoint");
        let errors = custom_config_errors(&config);
        assert_eq!(errors.len(), 2);

        config.insert(
            "userinfo_endpoint".to_owned(),
            json!("http://auth.example.test/userinfo"),
        );
        let errors = custom_config_errors(&config);
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn provider_config_is_only_for_the_custom_provider() {
        let mut config = Map::new();
        config.insert(
            "token_endpoint".to_owned(),
            json!("https://auth.example.test/token"),
        );
        let validation = ProviderConfig::new("google", &config);
        assert!(validation.validate().is_err());
    }

    #[test]
    fn grant_scope_parsing_matches_the_provider_shapes() {
        let google = Provider::registry("google").unwrap();
        assert_eq!(
            google.parse_granted_scopes(&json!("a b")),
            vec!["a".to_owned(), "b".to_owned()]
        );
        let linear = Provider::registry("linear").unwrap();
        assert_eq!(
            linear.parse_granted_scopes(&json!([])),
            Vec::<String>::new()
        );
        assert_eq!(
            linear.parse_granted_scopes(&json!("a,b c")),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
        let slack = Provider::registry("slack").unwrap();
        assert_eq!(slack.authorization_scope_param(), "user_scope");
        assert_eq!(slack.required_scopes(), vec!["search:read".to_owned()]);
    }

    #[test]
    fn id_tokens_check_the_audience_and_issuer() {
        use base64::Engine;
        let claims = json!({
            "aud": "client-id",
            "iss": "https://accounts.google.com",
            "sub": "sub-1",
            "email": "user@example.test"
        });
        let token = format!(
            "h.{}.s",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claims).unwrap())
        );
        let result = CodeExchangeResult {
            access_token: "at".to_owned(),
            refresh_token: Some("rt".to_owned()),
            expires_in: Some(60),
            scope: None,
            id_token: Some(token.clone()),
            response: json!({}),
        };
        let google = Provider::registry("google").unwrap();
        let identity = google.google_identity(&result, "client-id").unwrap();
        assert_eq!(identity.subject, "sub-1");
        assert_eq!(identity.email.as_deref(), Some("user@example.test"));
        let error = google.google_identity(&result, "other").unwrap_err();
        assert_eq!(error.reason(), "id_token_aud_mismatch");

        let microsoft = Provider::registry("microsoft").unwrap();
        let error = microsoft
            .microsoft_identity(&result, "client-id")
            .unwrap_err();
        assert_eq!(error.reason(), "id_token_iss_invalid");
    }

    #[test]
    fn mercator_requires_its_scope() {
        let mercator = Provider::registry("mercator").unwrap();
        let mut result = CodeExchangeResult {
            access_token: "at".to_owned(),
            refresh_token: Some("rt".to_owned()),
            expires_in: Some(60),
            scope: Some(json!("mercator:tools")),
            id_token: None,
            response: json!({}),
        };
        assert!(mercator.validate_result(&result).is_ok());
        result.scope = Some(json!("other"));
        assert_eq!(
            mercator.validate_result(&result).unwrap_err().reason(),
            "missing_scope"
        );
    }
}
