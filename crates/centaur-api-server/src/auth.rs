use std::{collections::BTreeSet, env, sync::Arc};

use axum::http::HeaderMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::{
    ApiError,
    api_jwt::{API_JWT_AUDIENCE, bearer_token, decode_jwt_payload, verify_hs256_jwt},
    console::sandbox::SandboxBinding,
    console::session_cookie::ConsoleSession,
};
use centaur_console_db::ConsoleUser;

const STATIC_ADMIN_IDENTITY: &str = "api-rs-admin";
/// Audience of the single sandbox token proxy-sync mints. Distinct from
/// [`API_JWT_AUDIENCE`] so neither kind of token can be read as the other.
pub(crate) const SANDBOX_TOKEN_AUDIENCE: &str = "centaur-sandbox";
/// Issuer of the sandbox token. Proxy-sync signs it as Console, matching the
/// credentials it replaced.
pub(crate) const SANDBOX_TOKEN_ISSUER: &str = "centaur-console";
/// Issuer of the access tokens api-rs mints for API clients. Distinct from
/// Console's issuer so neither kind of token can be read as the other.
pub(crate) const API_CLIENT_TOKEN_ISSUER: &str = "centaur-api-rs";
const API_CLIENT_TOKEN_USE: &str = "api_client";
pub(crate) const API_CLIENT_TOKEN_TTL_SECONDS: i64 = 60 * 60;

/// The capability catalog. Every credential resolves to a set of these and
/// every route declares the one it requires (see `console::guard`).
///
/// Some capabilities imply weaker ones on the same surface: a Console admin
/// also satisfies an active-user route, an active user also satisfies a
/// pending-user route, and a sandbox-delegated admin satisfies every Console
/// level. Ordering and names are the contract used by token claims and the
/// documentation table in `console/README.md`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Capability {
    // api-rs session, sandbox, workflow, and Slack routes.
    SessionsRead,
    SessionsWrite,
    /// Releasing a session's sandbox. API clients and the static admin key
    /// hold it; ingress keys do not.
    SessionsSandboxRelease,
    SandboxesDrain,
    WorkflowsRead,
    WorkflowsWrite,
    WorkflowsEvents,
    WorkflowsActions,
    /// The Slack proxy. The sandbox token's channel lists scope it.
    SlackProxy,
    SlackExternalTeamsRead,
    SlackExternalTeamsWrite,
    // Console session API.
    ConsoleRead,
    ConsolePending,
    ConsoleUser,
    ConsoleAdmin,
    // The bearer operator API under `/api/v1`.
    OperatorApi,
    // The sandbox API under `/api/v1/sandbox`.
    SandboxPermissions,
    SandboxSkills,
    SandboxTasks,
    SandboxOauthApps,
    // A sandbox credential that may act as the linked active Console admin.
    ConsoleDelegate,
}

impl Capability {
    /// Every capability the static admin key holds. It deliberately excludes
    /// the Console session, operator, sandbox, and Slack proxy surfaces.
    pub(crate) const API_RS: [Self; 10] = [
        Self::SessionsRead,
        Self::SessionsWrite,
        Self::SessionsSandboxRelease,
        Self::SandboxesDrain,
        Self::WorkflowsRead,
        Self::WorkflowsWrite,
        Self::WorkflowsEvents,
        Self::WorkflowsActions,
        Self::SlackExternalTeamsRead,
        Self::SlackExternalTeamsWrite,
    ];

    /// The capabilities an API client OAuth scope grants.
    pub(crate) fn from_api_client_scope(scope: &str) -> &'static [Self] {
        match scope {
            "sessions:read" => &[Self::SessionsRead],
            "sessions:write" => &[Self::SessionsWrite, Self::SessionsSandboxRelease],
            _ => &[],
        }
    }

    /// The wire name used in the sandbox token's `capabilities` claim.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SessionsRead => "sessions.read",
            Self::SessionsWrite => "sessions.write",
            Self::SessionsSandboxRelease => "sessions.sandbox_release",
            Self::SandboxesDrain => "sandboxes.drain",
            Self::WorkflowsRead => "workflows.read",
            Self::WorkflowsWrite => "workflows.write",
            Self::WorkflowsEvents => "workflows.events",
            Self::WorkflowsActions => "workflows.actions",
            Self::SlackProxy => "slack.proxy",
            Self::SlackExternalTeamsRead => "slack_external_teams.read",
            Self::SlackExternalTeamsWrite => "slack_external_teams.write",
            Self::ConsoleRead => "console.read",
            Self::ConsolePending => "console.pending",
            Self::ConsoleUser => "console.user",
            Self::ConsoleAdmin => "console.admin",
            Self::OperatorApi => "operator.api",
            Self::SandboxPermissions => "sandbox.permissions",
            Self::SandboxSkills => "sandbox.skills",
            Self::SandboxTasks => "sandbox.tasks",
            Self::SandboxOauthApps => "sandbox.oauth_apps",
            Self::ConsoleDelegate => "console.delegate",
        }
    }

    /// Parses a sandbox token capability name. Unknown names are ignored so a
    /// newer proxy-sync can add one without breaking an older api-rs.
    pub(crate) fn from_str(name: &str) -> Option<Self> {
        const ALL: [Capability; 21] = [
            Capability::SessionsRead,
            Capability::SessionsWrite,
            Capability::SessionsSandboxRelease,
            Capability::SandboxesDrain,
            Capability::WorkflowsRead,
            Capability::WorkflowsWrite,
            Capability::WorkflowsEvents,
            Capability::WorkflowsActions,
            Capability::SlackProxy,
            Capability::SlackExternalTeamsRead,
            Capability::SlackExternalTeamsWrite,
            Capability::ConsoleRead,
            Capability::ConsolePending,
            Capability::ConsoleUser,
            Capability::ConsoleAdmin,
            Capability::OperatorApi,
            Capability::SandboxPermissions,
            Capability::SandboxSkills,
            Capability::SandboxTasks,
            Capability::SandboxOauthApps,
            Capability::ConsoleDelegate,
        ];
        ALL.into_iter()
            .find(|capability| capability.as_str() == name)
    }

    /// Whether holding `self` also grants `other`.
    pub(crate) const fn implies(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::ConsoleDelegate, Self::ConsoleAdmin)
                | (Self::ConsoleDelegate, Self::ConsoleUser)
                | (Self::ConsoleDelegate, Self::ConsolePending)
                | (Self::ConsoleDelegate, Self::ConsoleRead)
                | (Self::ConsoleAdmin, Self::ConsoleUser)
                | (Self::ConsoleAdmin, Self::ConsolePending)
                | (Self::ConsoleAdmin, Self::ConsoleRead)
                | (Self::ConsoleUser, Self::ConsolePending)
                | (Self::ConsoleUser, Self::ConsoleRead)
                | (Self::ConsolePending, Self::ConsoleRead)
        )
    }

    /// Whether a caller holding `self` satisfies a route that requires
    /// `required`.
    pub(crate) fn satisfies(self, required: Self) -> bool {
        self == required || self.implies(required)
    }

    /// The surface whose error envelope a route requiring this capability
    /// answers. The surface only selects unauthenticated/forbidden response
    /// wording; every surface accepts every credential the caller holds.
    pub(crate) const fn surface(self) -> Surface {
        match self {
            Self::ConsoleRead
            | Self::ConsolePending
            | Self::ConsoleUser
            | Self::ConsoleAdmin
            | Self::ConsoleDelegate => Surface::Console,
            Self::OperatorApi => Surface::Bearer,
            Self::SandboxPermissions
            | Self::SandboxSkills
            | Self::SandboxTasks
            | Self::SandboxOauthApps => Surface::Sandbox,
            Self::SessionsRead
            | Self::SessionsWrite
            | Self::SessionsSandboxRelease
            | Self::SandboxesDrain
            | Self::WorkflowsRead
            | Self::WorkflowsWrite
            | Self::WorkflowsEvents
            | Self::WorkflowsActions
            | Self::SlackProxy
            | Self::SlackExternalTeamsRead
            | Self::SlackExternalTeamsWrite => Surface::ApiRs,
        }
    }
}

/// The credential surface whose rejection body a route answers. It is derived
/// from the route's required capability, not from its path: a path no longer
/// implies which credential type is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Surface {
    ApiRs,
    Console,
    Bearer,
    Sandbox,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallerClass {
    Admin,
    Console,
    Ingress,
    Principal,
    ApiClient,
}

impl CallerClass {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Console => "console",
            Self::Ingress => "ingress",
            Self::Principal => "principal",
            Self::ApiClient => "api_client",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedCaller {
    class: CallerClass,
    identity: String,
    capabilities: BTreeSet<Capability>,
    platform_prefixes: Option<&'static [&'static str]>,
    principal_subject: Option<String>,
    api_client_token: Option<ApiClientToken>,
    /// The effective Console user: a cookie session's user or the linked admin
    /// a delegated sandbox token acts as.
    console_user: Option<ConsoleUser>,
    /// The cookie session a handler may rotate in its response.
    console_session: Option<ConsoleSession>,
    /// The `api_keys` row a bearer credential resolved.
    bearer_api_key: Option<i64>,
    /// The proxy binding and sandbox claims of a sandbox credential.
    sandbox: Option<SandboxBinding>,
}

/// What an API client access token says about its validity.
#[derive(Clone, Debug)]
pub(crate) struct ApiClientToken {
    /// Fingerprint of the secret the token was issued for. Rotating the
    /// secret changes it, revoking the token.
    pub(crate) secret_fingerprint: String,
    /// Unix seconds.
    pub(crate) expires_at: i64,
}

impl AuthenticatedCaller {
    /// Builds a caller from a verified credential's identity and capabilities.
    pub(crate) fn new(
        class: CallerClass,
        identity: impl Into<String>,
        capabilities: impl IntoIterator<Item = Capability>,
        platform_prefixes: Option<&'static [&'static str]>,
        principal_subject: Option<String>,
    ) -> Self {
        Self {
            class,
            identity: identity.into(),
            capabilities: capabilities.into_iter().collect(),
            platform_prefixes,
            principal_subject,
            api_client_token: None,
            console_user: None,
            console_session: None,
            bearer_api_key: None,
            sandbox: None,
        }
    }

    pub(crate) fn api_client_token(&self) -> Option<&ApiClientToken> {
        self.api_client_token.as_ref()
    }

    /// Drops every capability `keep` rejects.
    pub(crate) fn retain_capabilities(&mut self, keep: impl Fn(Capability) -> bool) {
        self.capabilities.retain(|capability| keep(*capability));
    }

    pub(crate) fn has_capability(&self, capability: Capability) -> bool {
        self.capabilities
            .iter()
            .any(|granted| granted.satisfies(capability))
    }

    /// Adds capabilities a credential's non-token context grants.
    pub(crate) fn grant(&mut self, capabilities: impl IntoIterator<Item = Capability>) {
        self.capabilities.extend(capabilities);
    }

    pub(crate) fn attach_console_user(
        &mut self,
        user: ConsoleUser,
        session: Option<ConsoleSession>,
    ) {
        self.identity = user.email.clone();
        self.console_user = Some(user);
        self.console_session = session;
    }

    pub(crate) fn attach_bearer_api_key(&mut self, id: i64, owner: ConsoleUser) {
        self.identity = owner.email.clone();
        self.console_user = Some(owner);
        self.bearer_api_key = Some(id);
    }

    pub(crate) fn attach_sandbox(&mut self, binding: SandboxBinding) {
        self.sandbox = Some(binding);
    }

    pub(crate) fn console_user(&self) -> Option<&ConsoleUser> {
        self.console_user.as_ref()
    }

    pub(crate) fn console_session(&self) -> Option<&ConsoleSession> {
        self.console_session.as_ref()
    }

    pub(crate) fn bearer_api_key(&self) -> Option<i64> {
        self.bearer_api_key
    }

    pub(crate) fn sandbox(&self) -> Option<&SandboxBinding> {
        self.sandbox.as_ref()
    }

    pub(crate) fn is_sandbox_delegated(&self) -> bool {
        self.has_capability(Capability::ConsoleDelegate)
    }

    pub(crate) const fn class(&self) -> CallerClass {
        self.class
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    pub(crate) const fn platform_prefixes(&self) -> Option<&'static [&'static str]> {
        self.platform_prefixes
    }

    pub(crate) fn principal_subject(&self) -> Option<&str> {
        self.principal_subject.as_deref()
    }
}

#[derive(Clone, Debug)]
struct StaticCaller {
    token_digest: [u8; 32],
    caller: AuthenticatedCaller,
}

#[derive(Clone)]
pub struct ApiAuthConfig {
    static_callers: Arc<Vec<StaticCaller>>,
    jwt_secret: Arc<str>,
    /// Whether API clients may get and use access tokens
    /// (`CENTAUR_API_CLIENTS_ENABLED`).
    api_clients_enabled: bool,
}

impl ApiAuthConfig {
    pub fn from_env() -> Result<Self, ApiAuthConfigError> {
        let jwt_secret = required_env("CENTAUR_JWT_SIGNING_SECRET")?;

        let mut callers = Vec::new();
        if let Some(token) = optional_env("CENTAUR_APIRS_ADMIN_API_KEY") {
            callers.push(static_caller(
                STATIC_ADMIN_IDENTITY,
                CallerClass::Admin,
                token,
                Capability::API_RS,
                None,
            ));
        }
        for spec in INGRESS_SPECS {
            let Some(token) = optional_env(spec.env_var) else {
                continue;
            };
            let mut capabilities = vec![Capability::SessionsRead, Capability::SessionsWrite];
            if spec.workflow_events {
                capabilities.push(Capability::WorkflowsEvents);
            }
            if spec.identity == "slackbot" {
                capabilities.push(Capability::WorkflowsActions);
                capabilities.push(Capability::SlackExternalTeamsRead);
            }
            callers.push(static_caller(
                spec.identity,
                CallerClass::Ingress,
                token,
                capabilities,
                Some(spec.platform_prefixes),
            ));
        }

        validate_unique_tokens(&callers)?;
        Ok(Self {
            static_callers: Arc::new(callers),
            jwt_secret: jwt_secret.into(),
            api_clients_enabled: optional_env("CENTAUR_API_CLIENTS_ENABLED")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
        })
    }

    pub fn testing(jwt_secret: impl Into<String>) -> Self {
        Self {
            static_callers: Arc::new(Vec::new()),
            jwt_secret: Arc::from(jwt_secret.into()),
            api_clients_enabled: true,
        }
    }

    /// Adds the static admin caller with `key`, as `CENTAUR_APIRS_ADMIN_API_KEY`.
    #[cfg(test)]
    pub(crate) fn with_test_admin(mut self, key: &str) -> Self {
        let mut callers = self.static_callers.as_ref().clone();
        callers.push(static_caller(
            STATIC_ADMIN_IDENTITY,
            CallerClass::Admin,
            key.to_owned(),
            Capability::API_RS,
            None,
        ));
        self.static_callers = Arc::new(callers);
        self
    }

    #[cfg(test)]
    pub(crate) fn testing_with_slack_ingress(
        slack_key: impl Into<String>,
        jwt_secret: impl Into<String>,
    ) -> Self {
        let callers = vec![static_caller(
            "slackbot",
            CallerClass::Ingress,
            slack_key.into(),
            [
                Capability::SessionsRead,
                Capability::SessionsWrite,
                Capability::WorkflowsEvents,
                Capability::WorkflowsActions,
                Capability::SlackExternalTeamsRead,
            ],
            Some(&["slack:"]),
        )];
        Self {
            static_callers: Arc::new(callers),
            jwt_secret: Arc::from(jwt_secret.into()),
            api_clients_enabled: true,
        }
    }

    /// Enables or disables API clients, overriding the environment.
    pub fn with_api_clients_enabled(mut self, enabled: bool) -> Self {
        self.api_clients_enabled = enabled;
        self
    }

    pub(crate) const fn api_clients_enabled(&self) -> bool {
        self.api_clients_enabled
    }

    pub(crate) fn verify_workflow_button(
        &self,
        request: centaur_workflows::slack_buttons::Invocation,
    ) -> Result<centaur_workflows::CreateWorkflowRunRequest, ApiError> {
        centaur_workflows::slack_buttons::verify(request, self.jwt_secret.as_bytes())
            .map_err(|error| ApiError::Forbidden(error.into()))
    }

    pub(crate) fn authenticate(
        &self,
        headers: &HeaderMap,
    ) -> Result<AuthenticatedCaller, ApiError> {
        let token = bearer_token(headers)?;
        let candidate_digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut matched = None;
        for configured in self.static_callers.iter() {
            let matches = bool::from(candidate_digest.ct_eq(&configured.token_digest));
            if matches {
                matched = Some(configured.caller.clone());
            }
        }
        if let Some(caller) = matched {
            return Ok(caller);
        }

        if token.matches('.').count() != 2 {
            return Err(ApiError::Unauthorized("invalid bearer token".to_owned()));
        }
        if unverified_issuer(token).as_deref() == Some(API_CLIENT_TOKEN_ISSUER) {
            if !self.api_clients_enabled {
                return Err(ApiError::Unauthorized(
                    "API clients are disabled".to_owned(),
                ));
            }
            return self.authenticate_api_client(token);
        }
        // The only JWTs api-rs accepts are its own API client tokens; the
        // Console runs in-process and mints none.
        Err(ApiError::Unauthorized("invalid bearer token".to_owned()))
    }
}

impl ApiAuthConfig {
    /// Mints the access token for API client `client_id` (its oid), granting
    /// `scopes`, valid from `now` (Unix seconds). `secret_fingerprint`
    /// identifies the client's current secret.
    pub(crate) fn mint_api_client_token(
        &self,
        client_id: &str,
        secret_fingerprint: &str,
        scopes: &[String],
        now: i64,
    ) -> Result<String, ApiError> {
        let claims = serde_json::json!({
            "iss": API_CLIENT_TOKEN_ISSUER,
            "aud": API_JWT_AUDIENCE,
            "sub": client_id,
            "iat": now,
            "nbf": now,
            "exp": now + API_CLIENT_TOKEN_TTL_SECONDS,
            "jti": uuid::Uuid::new_v4().to_string(),
            "token_use": API_CLIENT_TOKEN_USE,
            "secret_fingerprint": secret_fingerprint,
            "scope": scopes.join(" "),
        });
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )
        .map_err(|error| ApiError::Internal(format!("failed to sign API client token: {error}")))
    }

    /// Verifies an API client access token. The caller's capabilities are its
    /// token scopes; the request path narrows them to the client's live scopes.
    fn authenticate_api_client(&self, token: &str) -> Result<AuthenticatedCaller, ApiError> {
        let claims = verify_hs256_jwt::<ApiClientClaims>(
            token,
            self.jwt_secret.as_bytes(),
            API_JWT_AUDIENCE,
            API_CLIENT_TOKEN_ISSUER,
        )?;
        if claims.token_use != API_CLIENT_TOKEN_USE {
            return Err(ApiError::Unauthorized(
                "invalid API client token use".to_owned(),
            ));
        }
        Ok(AuthenticatedCaller {
            class: CallerClass::ApiClient,
            identity: claims.sub.trim().to_owned(),
            capabilities: claims
                .scope
                .split_whitespace()
                .flat_map(Capability::from_api_client_scope)
                .copied()
                .collect(),
            platform_prefixes: None,
            principal_subject: None,
            api_client_token: Some(ApiClientToken {
                secret_fingerprint: claims.secret_fingerprint,
                expires_at: claims.exp,
            }),
            console_user: None,
            console_session: None,
            bearer_api_key: None,
            sandbox: None,
        })
    }
}

/// The `iss` claim of a JWT, before its signature is checked. Used only to
/// pick which verifier to apply.
fn unverified_issuer(token: &str) -> Option<String> {
    decode_jwt_payload(token)
        .ok()?
        .get("iss")?
        .as_str()
        .map(str::to_owned)
}

#[derive(Deserialize)]
struct ApiClientClaims {
    sub: String,
    token_use: String,
    secret_fingerprint: String,
    exp: i64,
    #[serde(default)]
    scope: String,
}

#[derive(Debug, Error)]
pub enum ApiAuthConfigError {
    #[error("{0} is required for api-rs authentication")]
    MissingEnvironment(&'static str),
    #[error("api-rs authentication keys for {first} and {second} must be distinct")]
    DuplicateToken { first: String, second: String },
}

/// Every ingress key and the session thread-key families it is allowed to
/// touch. The platform scoping in `authorize_api_request` denies any
/// `/api/session/*` call outside them, so a family missing here locks a bot
/// out of its own sessions.
const INGRESS_SPECS: &[IngressSpec] = &[
    IngressSpec {
        env_var: "SLACKBOT_API_KEY",
        identity: "slackbot",
        platform_prefixes: &["slack:"],
        workflow_events: true,
    },
    IngressSpec {
        env_var: "DISCORDBOT_API_KEY",
        identity: "discordbot",
        platform_prefixes: &["discord:"],
        workflow_events: false,
    },
    IngressSpec {
        env_var: "GITHUBBOT_API_KEY",
        identity: "githubbot",
        platform_prefixes: &[
            "github:",
            "github-issue:",
            "github-manage:",
            "github-review:",
        ],
        workflow_events: true,
    },
    IngressSpec {
        env_var: "LINEARBOT_API_KEY",
        identity: "linearbot",
        platform_prefixes: &["linear:"],
        workflow_events: false,
    },
    IngressSpec {
        env_var: "TEAMSBOT_API_KEY",
        identity: "teamsbot",
        platform_prefixes: &["teams:"],
        workflow_events: false,
    },
];

struct IngressSpec {
    env_var: &'static str,
    identity: &'static str,
    /// Every session thread-key prefix this ingress mints.
    platform_prefixes: &'static [&'static str],
    workflow_events: bool,
}

fn static_caller(
    identity: &str,
    class: CallerClass,
    token: String,
    capabilities: impl IntoIterator<Item = Capability>,
    platform_prefixes: Option<&'static [&'static str]>,
) -> StaticCaller {
    StaticCaller {
        token_digest: Sha256::digest(token.as_bytes()).into(),
        caller: AuthenticatedCaller {
            class,
            identity: identity.to_owned(),
            capabilities: capabilities.into_iter().collect(),
            platform_prefixes,
            principal_subject: None,
            api_client_token: None,
            console_user: None,
            console_session: None,
            bearer_api_key: None,
            sandbox: None,
        },
    }
}

fn validate_unique_tokens(callers: &[StaticCaller]) -> Result<(), ApiAuthConfigError> {
    for (index, caller) in callers.iter().enumerate() {
        for other in callers.iter().skip(index + 1) {
            if bool::from(caller.token_digest.ct_eq(&other.token_digest)) {
                return Err(ApiAuthConfigError::DuplicateToken {
                    first: caller.caller.identity.clone(),
                    second: other.caller.identity.clone(),
                });
            }
        }
    }
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, ApiAuthConfigError> {
    optional_env(name).ok_or(ApiAuthConfigError::MissingEnvironment(name))
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, header};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::json;

    use super::*;
    use crate::routes::thread_key_matches_platform;

    #[test]
    fn authenticates_static_admin_with_every_capability() {
        let auth = ApiAuthConfig {
            static_callers: Arc::new(vec![static_caller(
                STATIC_ADMIN_IDENTITY,
                CallerClass::Admin,
                "admin-key".to_owned(),
                Capability::API_RS,
                None,
            )]),
            jwt_secret: Arc::from("jwt-secret"),
            api_clients_enabled: true,
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer admin-key".parse().unwrap());

        let caller = auth.authenticate(&headers).unwrap();
        assert_eq!(caller.class(), CallerClass::Admin);
        assert_eq!(caller.identity(), STATIC_ADMIN_IDENTITY);
        assert!(
            Capability::API_RS
                .into_iter()
                .all(|capability| caller.has_capability(capability))
        );
        assert_eq!(caller.platform_prefixes(), None);
        assert_eq!(caller.principal_subject(), None);
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn minted_api_client_tokens_authenticate_with_their_scopes() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let token = auth
            .mint_api_client_token("acl_test", "fp", &["sessions:read".to_owned()], now)
            .unwrap();

        let caller = auth.authenticate(&bearer(&token)).unwrap();
        assert_eq!(caller.class(), CallerClass::ApiClient);
        assert_eq!(caller.identity(), "acl_test");
        assert!(caller.has_capability(Capability::SessionsRead));
        assert!(!caller.has_capability(Capability::SessionsWrite));
        assert_eq!(caller.principal_subject(), None);
        assert_eq!(caller.platform_prefixes(), None);
        let token_claims = caller.api_client_token().unwrap();
        assert_eq!(token_claims.secret_fingerprint, "fp");
        assert_eq!(token_claims.expires_at, now + API_CLIENT_TOKEN_TTL_SECONDS);

        let expired = auth
            .mint_api_client_token(
                "acl_test",
                "fp",
                &["sessions:read".to_owned()],
                now - API_CLIENT_TOKEN_TTL_SECONDS - 60,
            )
            .unwrap();
        assert!(matches!(
            auth.authenticate(&bearer(&expired)),
            Err(ApiError::Unauthorized(_))
        ));
    }

    #[test]
    fn api_client_tokens_are_rejected_while_api_clients_are_disabled() {
        let enabled = ApiAuthConfig::testing("jwt-secret");
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let token = enabled
            .mint_api_client_token("acl_test", "fp", &["sessions:read".to_owned()], now)
            .unwrap();

        let disabled = enabled.with_api_clients_enabled(false);
        assert!(matches!(
            disabled.authenticate(&bearer(&token)),
            Err(ApiError::Unauthorized(_))
        ));
    }

    #[test]
    fn api_client_and_console_tokens_cannot_pose_as_each_other() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        let sign = |claims: serde_json::Value| {
            encode(
                &Header::new(Algorithm::HS256),
                &claims,
                &EncodingKey::from_secret(b"jwt-secret"),
            )
            .unwrap()
        };
        for claims in [
            // A Console-issued token claiming to be an API client token.
            json!({
                "iss": "centaur-console", "sub": "acl_test", "aud": "centaur-api",
                "iat": 1_700_000_000i64, "exp": 4_102_444_800i64,
                "token_use": "api_client", "scope": "sessions:write",
            }),
            // An api-rs-issued token without the API client token use.
            json!({
                "iss": API_CLIENT_TOKEN_ISSUER, "sub": "centaur-console", "aud": "centaur-api",
                "iat": 1_700_000_000i64, "exp": 4_102_444_800i64,
                "token_use": "console_service",
            }),
            // A retired Console service token.
            json!({
                "iss": "centaur-console", "sub": "centaur-console", "aud": "centaur-api",
                "iat": 1_700_000_000i64, "exp": 4_102_444_800i64,
                "token_use": "console_service",
            }),
            json!({
                "iss": API_CLIENT_TOKEN_ISSUER, "sub": "prn_test", "aud": "centaur-api",
                "iat": 1_700_000_000i64, "exp": 4_102_444_800i64,
                "capabilities": { "sessions_read": true },
            }),
        ] {
            assert!(
                matches!(
                    auth.authenticate(&bearer(&sign(claims.clone()))),
                    Err(ApiError::Unauthorized(_))
                ),
                "{claims}"
            );
        }
    }

    #[test]
    fn githubbot_ingress_admits_every_thread_key_family_it_mints() {
        // The same keys services/githubbot/test/thread-keys.test.ts pins on
        // the producer side: chat threads (body-mention.ts), issue work
        // (issue-manager.ts), owned-PR management (pr-manager.ts), and review
        // runs (review.ts). Dropping a family 403s the bot out of its sessions.
        let githubbot = INGRESS_SPECS
            .iter()
            .find(|spec| spec.identity == "githubbot")
            .expect("githubbot ingress spec");

        for thread_key in [
            "github:acme/repo:7",
            "github-issue:acme/repo:7",
            "github-manage:acme/repo:7",
            "github-review:acme/repo:7",
        ] {
            assert!(
                thread_key_matches_platform(githubbot.platform_prefixes, thread_key),
                "{thread_key}"
            );
        }
        for thread_key in ["slack:C123:1.2", "githubx:acme/repo:7"] {
            assert!(
                !thread_key_matches_platform(githubbot.platform_prefixes, thread_key),
                "{thread_key}"
            );
        }
    }

    #[test]
    fn every_ingress_scopes_itself_to_at_least_one_prefix() {
        for spec in INGRESS_SPECS {
            assert!(
                !spec.platform_prefixes.is_empty(),
                "{} would be scoped to no session at all",
                spec.identity
            );
        }
    }

    #[test]
    fn duplicate_tokens_are_rejected() {
        let callers = vec![
            static_caller(
                "slackbot",
                CallerClass::Ingress,
                "same".to_owned(),
                [Capability::SessionsWrite],
                Some(&["slack:"]),
            ),
            static_caller(
                "githubbot",
                CallerClass::Ingress,
                "same".to_owned(),
                [Capability::SessionsWrite],
                Some(&["github:"]),
            ),
        ];

        assert!(matches!(
            validate_unique_tokens(&callers),
            Err(ApiAuthConfigError::DuplicateToken { .. })
        ));
    }

    #[test]
    fn missing_malformed_unknown_and_expired_credentials_are_unauthorized() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        for value in [None, Some("Basic value"), Some("Bearer unknown")] {
            let mut headers = HeaderMap::new();
            if let Some(value) = value {
                headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            }
            assert!(matches!(
                auth.authenticate(&headers),
                Err(ApiError::Unauthorized(_))
            ));
        }

        let expired = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "prn_test",
                "aud": "centaur-api",
                "iat": 1_600_000_000i64,
                "exp": 1_600_000_100i64,
            }),
            &EncodingKey::from_secret(b"jwt-secret"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {expired}").parse().unwrap(),
        );
        assert!(matches!(
            auth.authenticate(&headers),
            Err(ApiError::Unauthorized(_))
        ));
    }
}
