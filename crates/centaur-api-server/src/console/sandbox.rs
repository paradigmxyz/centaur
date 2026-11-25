//! The single sandbox token: verification, live binding checks, and the
//! sandbox request context.
//!
//! proxy-sync mints one token per proxy (audience `centaur-sandbox`) and
//! iron-proxy injects it host-only for every api-rs host, including the Console
//! host. The token replaces both the old `centaur-api` principal JWT and the
//! sandbox entitlement JWT.
//!
//! The signature is not the whole authorization story. Every request re-reads
//! the proxy -> principal binding from the database, so reassigning a proxy
//! revokes access immediately. A delegated Console credential (`console.delegate`)
//! additionally re-checks that the principal is still a Slack DM in the bot's
//! home workspace linked to an active Console admin.
//!
//! The tenancy iron-proxy, which serves Code Mode turns for every principal,
//! has no per-principal proxy row. proxy-sync gives it the same token with
//! `binding: "codemode"`, an empty `proxy_id`, and at most a 15-minute
//! lifetime. Such a token skips the proxy lookup but still requires a live
//! Console principal and the same delegate re-check.
//!
//! Routes are added through the unified `console::guard::GuardedRoutes`
//! builder; this module only authenticates the credential and exposes the
//! claims handlers need.

use std::fmt;

use axum::Router;
use axum::http::{HeaderMap, header::AUTHORIZATION};
use centaur_console_db::{
    ConsoleProxy, ConsoleUser, ConsoleUserStatus, Principal, decode_oid, find_console_proxy,
    find_console_user, find_principal, oid, slack_bot_home_team,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    ApiError,
    api_jwt::jwt_signing_secret,
    auth::{
        AuthenticatedCaller, CallerClass, Capability, SANDBOX_TOKEN_AUDIENCE, SANDBOX_TOKEN_ISSUER,
    },
    console::guard::GuardedRoutes,
};

/// The JWT settings for the sandbox token. Debug hides the signing secret.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SandboxJwtConfig {
    secret: String,
    audience: String,
    issuer: String,
}

impl fmt::Debug for SandboxJwtConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SandboxJwtConfig")
            .field("audience", &self.audience)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl SandboxJwtConfig {
    /// `CENTAUR_JWT_SIGNING_SECRET` with the single sandbox audience/issuer.
    /// No secret means the sandbox surface cannot authenticate anything.
    pub(crate) fn from_env() -> Option<Self> {
        Self::new(
            jwt_signing_secret()?,
            SANDBOX_TOKEN_AUDIENCE,
            SANDBOX_TOKEN_ISSUER,
        )
    }

    pub(crate) fn new(
        secret: impl Into<String>,
        audience: impl Into<String>,
        issuer: impl Into<String>,
    ) -> Option<Self> {
        let secret = secret.into();
        (!secret.trim().is_empty()).then(|| Self {
            secret,
            audience: audience.into(),
            issuer: issuer.into(),
        })
    }

    #[cfg(test)]
    pub(crate) fn audience(&self) -> &str {
        &self.audience
    }

    #[cfg(test)]
    pub(crate) fn issuer(&self) -> &str {
        &self.issuer
    }
}

/// The `binding` claim of a Code Mode tool-host token, which has no proxy.
const CODEMODE_BINDING: &str = "codemode";
/// The longest lifetime a `codemode` binding is accepted for.
const CODEMODE_TOKEN_MAX_LIFETIME: i64 = 900;

/// The claims proxy-sync signs into the single sandbox token.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SandboxClaims {
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) sub: String,
    // The JWT layer validates the standard claims; a missing or non-string
    // binding claim reaches the proxy lookup and becomes "invalid sandbox
    // token". Only the payload values are compared, so a numeric claim is a
    // mismatch rather than a decode error.
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) sandbox_id: String,
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) proxy_id: String,
    /// `codemode` for a tool-host token; empty for a proxy token.
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) binding: String,
    #[serde(default)]
    pub(crate) capabilities: Vec<String>,
    #[serde(default)]
    pub(crate) slack: SandboxSlackClaims,
    #[serde(default)]
    pub(crate) console_delegate: bool,
    /// A numeric `iat` in the future is rejected by the JWT layer. Not required.
    #[serde(default)]
    pub(crate) iat: Option<serde_json::Value>,
    /// The JWT layer validates `exp`; a `codemode` binding also bounds its
    /// lifetime, so the claim is kept here.
    #[serde(default)]
    pub(crate) exp: Option<i64>,
}

/// The Slack channel lists embedded in the sandbox token.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SandboxSlackClaims {
    #[serde(default)]
    pub(crate) upload_channels: Vec<String>,
    #[serde(default)]
    pub(crate) download_channels: Vec<String>,
    #[serde(default)]
    pub(crate) history_channels: Vec<String>,
}

fn string_claim<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().map(str::to_owned).unwrap_or_default())
}

/// What a successfully authenticated sandbox request carries to its handler.
#[derive(Clone, Debug)]
pub(crate) struct SandboxBinding {
    /// The proxy this token is bound to, or `None` for a Code Mode tool-host
    /// token.
    pub(crate) proxy: Option<ConsoleProxy>,
    pub(crate) claims: SandboxClaims,
}

impl SandboxBinding {
    /// The Console principal this credential acts for.
    pub(crate) fn principal_id(&self) -> Option<i64> {
        match &self.proxy {
            Some(proxy) => proxy.principal_id,
            None => decode_oid("prn", &self.claims.sub),
        }
    }
}

/// The historical name of [`SandboxBinding`] used by the area handlers.
pub(crate) type SandboxContext = SandboxBinding;

impl axum::extract::FromRequestParts<crate::AppState> for SandboxBinding {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            ApiError::Internal("sandbox authorization middleware did not attach context".to_owned())
        })
    }
}

/// The outcome of looking for a sandbox token in the request headers.
pub(crate) enum SandboxAuthentication {
    /// No Authorization header, or one that is not a sandbox JWT.
    NotPresent,
    /// A sandbox JWT that does not verify (shape, signature, audience, issuer,
    /// expiry).
    InvalidOrMissing,
    /// A sandbox JWT that verifies but whose live proxy binding no longer
    /// matches.
    Invalid,
    Authenticated(Box<AuthenticatedCaller>),
}

/// Verifies the request's sandbox token and performs the live checks.
///
/// `NotPresent` is returned for a token that is clearly not a sandbox JWT, so
/// the unified guard can try the other bearer credential types. `Invalid` means
/// the token verified but its binding is stale.
pub(crate) async fn authenticate_sandbox(
    state: &crate::AppState,
    headers: &HeaderMap,
    config: Option<SandboxJwtConfig>,
) -> Result<SandboxAuthentication, ApiError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(SandboxAuthentication::NotPresent);
    };
    if unverified_audience(token).as_deref() != Some(SANDBOX_TOKEN_AUDIENCE) {
        return Ok(SandboxAuthentication::NotPresent);
    }
    let Some(config) = config.or_else(SandboxJwtConfig::from_env) else {
        return Ok(SandboxAuthentication::InvalidOrMissing);
    };
    let Some(claims) = verify_sandbox_token(token, &config) else {
        return Ok(SandboxAuthentication::InvalidOrMissing);
    };
    let pool = state.console_session_db()?;
    let (proxy, principal_id) = if claims.binding == CODEMODE_BINDING {
        if !valid_codemode_claims(&claims) {
            return Ok(SandboxAuthentication::Invalid);
        }
        let Some(principal_id) = decode_oid("prn", &claims.sub) else {
            return Ok(SandboxAuthentication::Invalid);
        };
        // A tool-host token has no proxy row, so the live check is that its
        // principal still exists.
        if find_principal(&pool, principal_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
            .is_none()
        {
            return Ok(SandboxAuthentication::Invalid);
        }
        (None, Some(principal_id))
    } else {
        let Some(proxy) = find_console_proxy(&pool, &claims.proxy_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
        else {
            return Ok(SandboxAuthentication::Invalid);
        };
        if !valid_proxy_binding(&proxy, &claims) {
            return Ok(SandboxAuthentication::Invalid);
        }
        let principal_id = proxy.principal_id;
        (Some(proxy), principal_id)
    };

    let capabilities: Vec<Capability> = claims
        .capabilities
        .iter()
        .filter_map(|name| Capability::from_str(name))
        .filter(|capability| *capability != Capability::ConsoleDelegate)
        .collect();
    let identity = if claims.proxy_id.is_empty() {
        claims.sandbox_id.clone()
    } else {
        claims.proxy_id.clone()
    };
    let mut caller = AuthenticatedCaller::new(
        CallerClass::Principal,
        identity,
        capabilities,
        None,
        Some(claims.sub.clone()),
    );
    caller.attach_sandbox(SandboxBinding {
        proxy,
        claims: claims.clone(),
    });

    if claims.console_delegate {
        // api-rs never trusts the token's delegate claim alone: re-check the
        // linked user and the DM/home-team rule on every request.
        if let Some(user) = delegated_admin(&pool, principal_id).await? {
            caller.grant([
                Capability::ConsoleDelegate,
                Capability::ConsoleAdmin,
                Capability::ConsoleUser,
                Capability::ConsolePending,
                Capability::ConsoleRead,
            ]);
            caller.attach_console_user(user, None);
        }
    }
    Ok(SandboxAuthentication::Authenticated(Box::new(caller)))
}

/// The linked active Console admin a delegated sandbox token may act as, if the
/// principal still passes the sandbox-admin rule.
async fn delegated_admin(
    pool: &sqlx::PgPool,
    principal_id: Option<i64>,
) -> Result<Option<ConsoleUser>, ApiError> {
    let Some(principal_id) = principal_id else {
        return Ok(None);
    };
    let Some(principal) = find_principal(pool, principal_id)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?
    else {
        return Ok(None);
    };
    let home_team = match principal.slack_team_id.as_deref() {
        Some(team_id) => slack_bot_home_team(pool, team_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?,
        None => false,
    };
    let user = match principal.console_user_id {
        Some(user_id) => find_console_user(pool, user_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?,
        None => None,
    };
    Ok(sandbox_admin_allowed(&principal, home_team, user.as_ref())
        .then_some(user)
        .flatten())
}

/// HS256 signature, expiry, audience, and issuer checks.
pub(crate) fn verify_sandbox_token(
    token: &str,
    config: &SandboxJwtConfig,
) -> Option<SandboxClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    validation.validate_nbf = true;
    validation.set_audience(&[&config.audience]);
    validation.set_issuer(&[&config.issuer]);
    validation.set_required_spec_claims(&["iss", "aud", "exp"]);
    let claims = decode::<SandboxClaims>(
        token,
        &DecodingKey::from_secret(config.secret.as_bytes()),
        &validation,
    )
    .ok()?
    .claims;
    // `iat` is verified when present (jsonwebtoken 11 does not).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    if let Some(iat) = claims.iat.as_ref()
        && iat.as_f64()? > now
    {
        return None;
    }
    Some(claims)
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = header
        .strip_prefix("Bearer ")?
        .trim_start_matches(char::is_whitespace);
    (!token.is_empty()).then_some(token)
}

/// The `aud` claim of a JWT, before its signature is checked. Used only to pick
/// which verifier to apply.
fn unverified_audience(token: &str) -> Option<String> {
    crate::api_jwt::decode_jwt_payload(token)
        .ok()?
        .get("aud")?
        .as_str()
        .map(str::to_owned)
}

fn valid_proxy_binding(proxy: &ConsoleProxy, claims: &SandboxClaims) -> bool {
    let expected_principal = proxy.principal_id.map(|id| oid("prn", id));
    proxy.assigned()
        && claims.sub == expected_principal.unwrap_or_default()
        && proxy.name == claims.sandbox_id
}

/// A tool-host token has no proxy to bind. Its replacement checks are an empty
/// `proxy_id` and a lifetime no longer than the one proxy-sync mints.
fn valid_codemode_claims(claims: &SandboxClaims) -> bool {
    let Some(iat) = claims.iat.as_ref().and_then(serde_json::Value::as_i64) else {
        return false;
    };
    let Some(exp) = claims.exp else {
        return false;
    };
    let Some(lifetime) = exp.checked_sub(iat) else {
        return false;
    };
    claims.proxy_id.is_empty() && (0..=CODEMODE_TOKEN_MAX_LIFETIME).contains(&lifetime)
}

/// The delegated-credential facts, kept pure so the complete rule is testable
/// without a database.
fn sandbox_admin_allowed(
    principal: &Principal,
    home_slack_team: bool,
    user: Option<&ConsoleUser>,
) -> bool {
    principal.kind == "slack_dm"
        && home_slack_team
        && user.is_some_and(|user| user.status == ConsoleUserStatus::Active && user.admin)
}

pub(crate) fn sandbox_router(state: crate::AppState) -> Router<crate::AppState> {
    let routes = super::areas::sandbox(GuardedRoutes::new(state));
    #[cfg(test)]
    let routes = test_sandbox_routes(routes);
    routes.into_router()
}

#[cfg(test)]
pub(crate) fn test_sandbox_routes(routes: GuardedRoutes) -> GuardedRoutes {
    use axum::routing::get;

    routes.sandbox(
        "/probe/sandbox",
        get(probe_sandbox),
        Capability::SandboxTasks,
    )
}

#[cfg(test)]
async fn probe_sandbox(context: SandboxContext) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "sandbox_id": context.claims.sandbox_id,
        "proxy_id": context.claims.proxy_id,
        "principal_id": context.claims.sub,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{SANDBOX_TOKEN_AUDIENCE, SANDBOX_TOKEN_ISSUER};
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    #[test]
    fn sandbox_jwt_checks_claims_like_ruby_jwt() {
        let config = SandboxJwtConfig::new(
            "test-only-sandbox-jwt-secret",
            SANDBOX_TOKEN_AUDIENCE,
            SANDBOX_TOKEN_ISSUER,
        )
        .unwrap();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let token = |claims: serde_json::Value| {
            encode(
                &Header::new(Algorithm::HS256),
                &claims,
                &EncodingKey::from_secret(config.secret.as_bytes()),
            )
            .unwrap()
        };
        let minimal = token(json!({
            "iss": SANDBOX_TOKEN_ISSUER,
            "aud": SANDBOX_TOKEN_AUDIENCE,
            "exp": now + 60,
            "sub": "prn_1",
            "proxy_id": "prx_1",
            "sandbox_id": "sandbox",
        }));
        assert_eq!(
            verify_sandbox_token(&minimal, &config).unwrap().proxy_id,
            "prx_1"
        );
        let no_expiry = token(json!({
            "iss": SANDBOX_TOKEN_ISSUER,
            "aud": SANDBOX_TOKEN_AUDIENCE,
            "sub": "prn_1",
        }));
        assert!(verify_sandbox_token(&no_expiry, &config).is_none());
        let missing_audience = token(json!({"iss": SANDBOX_TOKEN_ISSUER, "exp": now + 60}));
        assert!(verify_sandbox_token(&missing_audience, &config).is_none());
        let missing_issuer = token(json!({
            "aud": SANDBOX_TOKEN_AUDIENCE,
            "exp": now + 60,
        }));
        assert!(verify_sandbox_token(&missing_issuer, &config).is_none());
        let future_iat = token(json!({
            "iss": SANDBOX_TOKEN_ISSUER,
            "aud": SANDBOX_TOKEN_AUDIENCE,
            "exp": now + 60,
            "iat": now + 60,
        }));
        assert!(verify_sandbox_token(&future_iat, &config).is_none());
        let malformed_bindings = token(json!({
            "iss": SANDBOX_TOKEN_ISSUER,
            "aud": SANDBOX_TOKEN_AUDIENCE,
            "exp": now + 60,
            "sub": 7,
            "sandbox_id": false,
            "proxy_id": 9,
        }));
        let claims = verify_sandbox_token(&malformed_bindings, &config).unwrap();
        assert!(claims.sub.is_empty());
        assert!(claims.sandbox_id.is_empty());
        assert!(claims.proxy_id.is_empty());
        let expired = token(json!({
            "iss": SANDBOX_TOKEN_ISSUER,
            "aud": SANDBOX_TOKEN_AUDIENCE,
            "exp": now - 1,
        }));
        assert!(verify_sandbox_token(&expired, &config).is_none());
        let wrong_audience = token(json!({
            "iss": SANDBOX_TOKEN_ISSUER,
            "aud": "another-audience",
            "exp": now + 60,
        }));
        assert!(verify_sandbox_token(&wrong_audience, &config).is_none());
    }

    fn proxy(principal_id: Option<i64>) -> ConsoleProxy {
        ConsoleProxy {
            id: 9,
            name: "sandbox-name".to_owned(),
            principal_id,
        }
    }

    fn claims(principal_id: Option<&str>) -> SandboxClaims {
        SandboxClaims {
            sub: principal_id.unwrap_or_default().to_owned(),
            sandbox_id: "sandbox-name".to_owned(),
            proxy_id: oid("prx", 9),
            binding: String::new(),
            capabilities: Vec::new(),
            slack: Default::default(),
            console_delegate: false,
            iat: None,
            exp: None,
        }
    }

    fn principal(kind: &str) -> Principal {
        Principal {
            id: 3,
            foreign_id: None,
            name: None,
            kind: kind.to_owned(),
            labels: json!({}),
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: Some("T123".to_owned()),
            slack_email: None,
            console_user_id: Some(7),
            sandbox_repo_cache: "none".to_owned(),
            sandbox_observability_enabled: false,
        }
    }

    fn user(status: ConsoleUserStatus, admin: bool) -> ConsoleUser {
        ConsoleUser {
            id: 7,
            email: "admin@example.test".to_owned(),
            name: None,
            admin,
            status,
        }
    }

    #[test]
    fn sandbox_binding_requires_an_assigned_proxy_matching_name_and_principal() {
        assert!(valid_proxy_binding(
            &proxy(Some(3)),
            &claims(Some(&oid("prn", 3)))
        ));
        assert!(!valid_proxy_binding(
            &proxy(None),
            &claims(Some(&oid("prn", 3)))
        ));
        assert!(!valid_proxy_binding(
            &proxy(Some(4)),
            &claims(Some(&oid("prn", 3)))
        ));
        assert!(!valid_proxy_binding(&proxy(Some(3)), &claims(None)));
        let mut wrong_name = claims(Some(&oid("prn", 3)));
        wrong_name.sandbox_id = "other".to_owned();
        assert!(!valid_proxy_binding(&proxy(Some(3)), &wrong_name));
    }

    #[test]
    fn codemode_binding_requires_an_empty_proxy_and_a_short_lifetime() {
        let mut claims = claims(Some(&oid("prn", 3)));
        claims.binding = CODEMODE_BINDING.to_owned();
        claims.proxy_id.clear();
        claims.sandbox_id = "codemode".to_owned();
        claims.iat = Some(json!(1_700_000_000));
        claims.exp = Some(1_700_000_900);
        assert!(valid_codemode_claims(&claims));
        assert_eq!(
            SandboxBinding {
                proxy: None,
                claims: claims.clone(),
            }
            .principal_id(),
            Some(3)
        );

        // At most 15 minutes.
        claims.exp = Some(1_700_000_901);
        assert!(!valid_codemode_claims(&claims));
        claims.exp = None;
        assert!(!valid_codemode_claims(&claims));
        claims.exp = Some(1_699_999_999);
        assert!(!valid_codemode_claims(&claims));

        // A `codemode` token carries no proxy binding.
        claims.exp = Some(1_700_000_900);
        claims.proxy_id = oid("prx", 9);
        assert!(!valid_codemode_claims(&claims));

        // The lifetime difference needs a numeric `iat`.
        claims.proxy_id.clear();
        claims.iat = Some(json!("yesterday"));
        assert!(!valid_codemode_claims(&claims));
    }

    #[test]
    fn sandbox_admin_requires_slack_dm_home_team_and_active_admin() {
        let active_admin = user(ConsoleUserStatus::Active, true);
        assert!(sandbox_admin_allowed(
            &principal("slack_dm"),
            true,
            Some(&active_admin)
        ));
        assert!(!sandbox_admin_allowed(
            &principal("slack_channel"),
            true,
            Some(&active_admin)
        ));
        assert!(!sandbox_admin_allowed(
            &principal("slack_dm"),
            false,
            Some(&active_admin)
        ));
        assert!(!sandbox_admin_allowed(
            &principal("slack_dm"),
            true,
            Some(&user(ConsoleUserStatus::Pending, true))
        ));
        assert!(!sandbox_admin_allowed(
            &principal("slack_dm"),
            true,
            Some(&user(ConsoleUserStatus::Active, false))
        ));
        assert!(!sandbox_admin_allowed(&principal("slack_dm"), true, None));
    }

    #[test]
    fn sandbox_config_uses_the_single_audience_and_hides_the_secret() {
        let config =
            SandboxJwtConfig::new("secret", SANDBOX_TOKEN_AUDIENCE, SANDBOX_TOKEN_ISSUER).unwrap();
        assert_eq!(config.audience(), SANDBOX_TOKEN_AUDIENCE);
        assert_eq!(config.issuer(), SANDBOX_TOKEN_ISSUER);
        assert!(!format!("{config:?}").contains("secret"));
        assert!(
            SandboxJwtConfig::new("  ", SANDBOX_TOKEN_AUDIENCE, SANDBOX_TOKEN_ISSUER).is_none()
        );
    }
}
