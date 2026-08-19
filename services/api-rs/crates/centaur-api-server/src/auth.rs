use std::{collections::BTreeSet, env, sync::Arc};

use axum::http::HeaderMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::{
    ApiError,
    api_jwt::{bearer_token, verify_hs256_jwt},
};

const DEFAULT_API_JWT_AUDIENCE: &str = "centaur-api";
const DEFAULT_API_JWT_ISSUER: &str = "centaur-console";
const CONSOLE_SERVICE_SUBJECT: &str = "centaur-console";
const STATIC_ADMIN_IDENTITY: &str = "api-rs-admin";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Capability {
    SessionsRead,
    SessionsWrite,
    SandboxesDrain,
    WorkflowsRead,
    WorkflowsWrite,
    WorkflowsEvents,
    AdminArchive,
    AdminSync,
}

impl Capability {
    const ALL: [Self; 8] = [
        Self::SessionsRead,
        Self::SessionsWrite,
        Self::SandboxesDrain,
        Self::WorkflowsRead,
        Self::WorkflowsWrite,
        Self::WorkflowsEvents,
        Self::AdminArchive,
        Self::AdminSync,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallerClass {
    Admin,
    Console,
    Ingress,
    Principal,
}

impl CallerClass {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Console => "console",
            Self::Ingress => "ingress",
            Self::Principal => "principal",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedCaller {
    class: CallerClass,
    identity: String,
    capabilities: BTreeSet<Capability>,
    platform_prefix: Option<&'static str>,
    principal_subject: Option<String>,
}

impl AuthenticatedCaller {
    pub(crate) fn has_capability(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    pub(crate) const fn class(&self) -> CallerClass {
        self.class
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    pub(crate) const fn platform_prefix(&self) -> Option<&'static str> {
        self.platform_prefix
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
        for spec in [
            IngressSpec {
                env_var: "SLACKBOT_API_KEY",
                identity: "slackbot",
                platform_prefix: "slack:",
                workflow_events: true,
            },
            IngressSpec {
                env_var: "DISCORDBOT_API_KEY",
                identity: "discordbot",
                platform_prefix: "discord:",
                workflow_events: false,
            },
            IngressSpec {
                env_var: "GITHUBBOT_API_KEY",
                identity: "githubbot",
                platform_prefix: "github:",
                workflow_events: true,
            },
            IngressSpec {
                env_var: "LINEARBOT_API_KEY",
                identity: "linearbot",
                platform_prefix: "linear:",
                workflow_events: false,
            },
            IngressSpec {
                env_var: "TEAMSBOT_API_KEY",
                identity: "teamsbot",
                platform_prefix: "teams:",
                workflow_events: false,
            },
        ] {
            let Some(token) = optional_env(spec.env_var) else {
                continue;
            };
            let mut capabilities = vec![Capability::SessionsRead, Capability::SessionsWrite];
            if spec.workflow_events {
                capabilities.push(Capability::WorkflowsEvents);
            }
            callers.push(static_caller(
                spec.identity,
                CallerClass::Ingress,
                token,
                capabilities,
                Some(spec.platform_prefix),
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
            ],
            Some("slack:"),
        )];
        Self {
            static_callers: Arc::new(callers),
            jwt_secret: Arc::from(jwt_secret.into()),
            jwt_audience: Arc::from(DEFAULT_API_JWT_AUDIENCE),
            jwt_issuer: Arc::from(DEFAULT_API_JWT_ISSUER),
        }
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
                    platform_prefix: None,
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
                    platform_prefix: None,
                    principal_subject: Some(subject),
                })
            }
        }
    }
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

struct IngressSpec {
    env_var: &'static str,
    identity: &'static str,
    platform_prefix: &'static str,
    workflow_events: bool,
}

fn static_caller(
    identity: &str,
    class: CallerClass,
    token: String,
    capabilities: impl IntoIterator<Item = Capability>,
    platform_prefix: Option<&'static str>,
) -> StaticCaller {
    StaticCaller {
        token_digest: Sha256::digest(token.as_bytes()).into(),
        caller: AuthenticatedCaller {
            class,
            identity: identity.to_owned(),
            capabilities: capabilities.into_iter().collect(),
            platform_prefix,
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
        assert_eq!(caller.platform_prefix(), None);
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

    #[test]
    fn duplicate_tokens_are_rejected() {
        let callers = vec![
            static_caller(
                "slackbot",
                CallerClass::Ingress,
                "same".to_owned(),
                [Capability::SessionsWrite],
                Some("slack:"),
            ),
            static_caller(
                "githubbot",
                CallerClass::Ingress,
                "same".to_owned(),
                [Capability::SessionsWrite],
                Some("github:"),
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
