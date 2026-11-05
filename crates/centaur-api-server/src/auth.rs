use std::{collections::BTreeSet, env, sync::Arc};

use axum::http::HeaderMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::{
    ApiError,
    api_jwt::{bearer_token, decode_jwt_payload, verify_hs256_jwt},
};

const DEFAULT_API_JWT_AUDIENCE: &str = "centaur-api";
const DEFAULT_API_JWT_ISSUER: &str = "centaur-console";
const CONSOLE_SERVICE_SUBJECT: &str = "centaur-console";
const STATIC_ADMIN_IDENTITY: &str = "api-rs-admin";
/// Issuer of the access tokens api-rs mints for API clients. Distinct from
/// Console's issuer so neither kind of token can be read as the other.
pub(crate) const API_CLIENT_TOKEN_ISSUER: &str = "centaur-api-rs";
const API_CLIENT_TOKEN_USE: &str = "api_client";
pub(crate) const API_CLIENT_TOKEN_TTL_SECONDS: i64 = 15 * 60;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Capability {
    SessionsRead,
    SessionsWrite,
    SandboxesDrain,
    WorkflowsRead,
    WorkflowsWrite,
    WorkflowsEvents,
    WorkflowsActions,
    AdminSync,
    SlackExternalTeamsRead,
    SlackExternalTeamsWrite,
}

impl Capability {
    const ALL: [Self; 10] = [
        Self::SessionsRead,
        Self::SessionsWrite,
        Self::SandboxesDrain,
        Self::WorkflowsRead,
        Self::WorkflowsWrite,
        Self::WorkflowsEvents,
        Self::WorkflowsActions,
        Self::AdminSync,
        Self::SlackExternalTeamsRead,
        Self::SlackExternalTeamsWrite,
    ];

    /// The capability an API client OAuth scope grants.
    pub(crate) fn from_api_client_scope(scope: &str) -> Option<Self> {
        match scope {
            "sessions:read" => Some(Self::SessionsRead),
            "sessions:write" => Some(Self::SessionsWrite),
            _ => None,
        }
    }
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
}

impl AuthenticatedCaller {
    /// Drops every capability `keep` rejects.
    pub(crate) fn retain_capabilities(&mut self, keep: impl Fn(Capability) -> bool) {
        self.capabilities.retain(|capability| keep(*capability));
    }

    pub(crate) fn has_capability(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
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
    jwt_audience: Arc<str>,
    jwt_issuer: Arc<str>,
}

impl ApiAuthConfig {
    pub fn from_env() -> Result<Self, ApiAuthConfigError> {
        let jwt_secret = required_env("CENTAUR_JWT_SIGNING_SECRET")?;
        let jwt_audience = optional_env("CENTAUR_API_JWT_AUDIENCE")
            .unwrap_or_else(|| DEFAULT_API_JWT_AUDIENCE.to_owned());
        let jwt_issuer = optional_env("CENTAUR_API_JWT_ISSUER")
            .unwrap_or_else(|| DEFAULT_API_JWT_ISSUER.to_owned());

        let mut callers = Vec::new();
        if let Some(token) = optional_env("CENTAUR_APIRS_ADMIN_API_KEY") {
            callers.push(static_caller(
                STATIC_ADMIN_IDENTITY,
                CallerClass::Admin,
                token,
                Capability::ALL,
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
            jwt_audience: jwt_audience.into(),
            jwt_issuer: jwt_issuer.into(),
        })
    }

    pub fn testing(jwt_secret: impl Into<String>) -> Self {
        Self {
            static_callers: Arc::new(Vec::new()),
            jwt_secret: Arc::from(jwt_secret.into()),
            jwt_audience: Arc::from(DEFAULT_API_JWT_AUDIENCE),
            jwt_issuer: Arc::from(DEFAULT_API_JWT_ISSUER),
        }
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
            jwt_audience: Arc::from(DEFAULT_API_JWT_AUDIENCE),
            jwt_issuer: Arc::from(DEFAULT_API_JWT_ISSUER),
        }
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
            return self.authenticate_api_client(token);
        }
        let claims = verify_hs256_jwt::<ApiJwtClaims>(
            token,
            self.jwt_secret.as_bytes(),
            &self.jwt_audience,
            &self.jwt_issuer,
        )?;
        let subject = claims.sub.trim().to_owned();
        match claims.token_use {
            Some(ApiJwtTokenUse::ConsoleService) if subject == CONSOLE_SERVICE_SUBJECT => {
                Ok(AuthenticatedCaller {
                    class: CallerClass::Console,
                    identity: subject,
                    capabilities: Capability::ALL.into_iter().collect(),
                    platform_prefixes: None,
                    principal_subject: None,
                })
            }
            Some(ApiJwtTokenUse::ConsoleService) => Err(ApiError::Unauthorized(
                "invalid Console service token subject".to_owned(),
            )),
            None => {
                let mut capabilities = BTreeSet::new();
                match claims.capabilities {
                    Some(jwt_capabilities) => {
                        if jwt_capabilities.sessions_read {
                            capabilities.insert(Capability::SessionsRead);
                        }
                        if jwt_capabilities.workflows_read {
                            capabilities.insert(Capability::WorkflowsRead);
                        }
                        if jwt_capabilities.workflows_write {
                            capabilities.insert(Capability::WorkflowsWrite);
                        }
                    }
                    // Tokens minted before capability claims were deployed had
                    // session read access. Preserve that access during rolling
                    // upgrades; new tokens always carry the explicit object.
                    None => {
                        capabilities.insert(Capability::SessionsRead);
                    }
                }
                Ok(AuthenticatedCaller {
                    class: CallerClass::Principal,
                    identity: subject.clone(),
                    capabilities,
                    platform_prefixes: None,
                    principal_subject: Some(subject),
                })
            }
        }
    }
}

impl ApiAuthConfig {
    /// Mints the access token for API client `client_id` (its oid), granting
    /// `scopes`, valid from `now` (Unix seconds).
    pub(crate) fn mint_api_client_token(
        &self,
        client_id: &str,
        scopes: &[String],
        now: i64,
    ) -> Result<String, ApiError> {
        let claims = serde_json::json!({
            "iss": API_CLIENT_TOKEN_ISSUER,
            "aud": &*self.jwt_audience,
            "sub": client_id,
            "iat": now,
            "nbf": now,
            "exp": now + API_CLIENT_TOKEN_TTL_SECONDS,
            "jti": uuid::Uuid::new_v4().to_string(),
            "token_use": API_CLIENT_TOKEN_USE,
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
            &self.jwt_audience,
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
                .filter_map(Capability::from_api_client_scope)
                .collect(),
            platform_prefixes: None,
            principal_subject: None,
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

#[derive(Deserialize)]
struct ApiJwtClaims {
    sub: String,
    token_use: Option<ApiJwtTokenUse>,
    capabilities: Option<ApiJwtCapabilities>,
}

#[derive(Deserialize)]
struct ApiJwtCapabilities {
    #[serde(default)]
    sessions_read: bool,
    #[serde(default)]
    workflows_read: bool,
    #[serde(default)]
    workflows_write: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApiJwtTokenUse {
    ConsoleService,
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
    fn authenticates_console_service_jwt() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "centaur-console",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "token_use": "console_service",
            }),
            &EncodingKey::from_secret(b"jwt-secret"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        let caller = auth.authenticate(&headers).unwrap();
        assert_eq!(caller.class(), CallerClass::Console);
        assert!(caller.has_capability(Capability::AdminSync));
    }

    #[test]
    fn authenticates_static_admin_with_every_capability() {
        let auth = ApiAuthConfig {
            static_callers: Arc::new(vec![static_caller(
                STATIC_ADMIN_IDENTITY,
                CallerClass::Admin,
                "admin-key".to_owned(),
                Capability::ALL,
                None,
            )]),
            jwt_secret: Arc::from("jwt-secret"),
            jwt_audience: Arc::from(DEFAULT_API_JWT_AUDIENCE),
            jwt_issuer: Arc::from(DEFAULT_API_JWT_ISSUER),
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer admin-key".parse().unwrap());

        let caller = auth.authenticate(&headers).unwrap();
        assert_eq!(caller.class(), CallerClass::Admin);
        assert_eq!(caller.identity(), STATIC_ADMIN_IDENTITY);
        assert!(
            Capability::ALL
                .into_iter()
                .all(|capability| caller.has_capability(capability))
        );
        assert_eq!(caller.platform_prefixes(), None);
        assert_eq!(caller.principal_subject(), None);
    }

    #[test]
    fn console_service_jwt_requires_the_exact_service_subject() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "prn_test",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "token_use": "console_service",
            }),
            &EncodingKey::from_secret(b"jwt-secret"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        assert!(matches!(
            auth.authenticate(&headers),
            Err(ApiError::Unauthorized(_))
        ));
    }

    #[test]
    fn authenticates_legacy_principal_jwt_with_session_read_capability() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "prn_test",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
            }),
            &EncodingKey::from_secret(b"jwt-secret"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        let caller = auth.authenticate(&headers).unwrap();
        assert_eq!(caller.principal_subject(), Some("prn_test"));
        assert!(caller.has_capability(Capability::SessionsRead));
        assert!(!caller.has_capability(Capability::SessionsWrite));
    }

    #[test]
    fn authenticates_principal_jwt_with_explicit_capabilities() {
        let auth = ApiAuthConfig::testing("jwt-secret");
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "prn_test",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "capabilities": {
                    "sessions_read": false,
                    "workflows_read": true,
                    "workflows_write": true,
                },
            }),
            &EncodingKey::from_secret(b"jwt-secret"),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );

        let caller = auth.authenticate(&headers).unwrap();
        assert!(!caller.has_capability(Capability::SessionsRead));
        assert!(caller.has_capability(Capability::WorkflowsRead));
        assert!(caller.has_capability(Capability::WorkflowsWrite));
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
            .mint_api_client_token("acl_test", &["sessions:read".to_owned()], now)
            .unwrap();

        let caller = auth.authenticate(&bearer(&token)).unwrap();
        assert_eq!(caller.class(), CallerClass::ApiClient);
        assert_eq!(caller.identity(), "acl_test");
        assert!(caller.has_capability(Capability::SessionsRead));
        assert!(!caller.has_capability(Capability::SessionsWrite));
        assert_eq!(caller.principal_subject(), None);
        assert_eq!(caller.platform_prefixes(), None);

        let expired = auth
            .mint_api_client_token(
                "acl_test",
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
