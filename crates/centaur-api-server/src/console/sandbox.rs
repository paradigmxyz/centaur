//! Sandbox entitlement JWT authentication for `/api/v1/sandbox/*`.
//!
//! Mirrors `SandboxTokenAuthentication`: HS256 with Console's issuer/audience
//! defaults, then the proxy -> principal binding is re-read from the database
//! so reassigning a proxy revokes access immediately. The console guard reuses
//! [`sandbox_admin_user`] for its second credential: a Slack DM principal in
//! the bot's own workspace linked to an active Console admin.
//!
//! Routes are added through [`SandboxRoutes`] so every route carries the token
//! check.

use std::{env, fmt};

use axum::{
    Router,
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};
use centaur_console_db::{
    ConsoleProxy, ConsoleUser, ConsoleUserStatus, Principal, find_console_proxy, find_console_user,
    find_principal, oid, slack_bot_home_team,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    ApiError, AppState,
    api_jwt::jwt_signing_secret,
    console::http::{ConsoleError, render_error},
};

const DEFAULT_AUDIENCE: &str = "centaur-console-sandbox-entitlements";
const DEFAULT_ISSUER: &str = "centaur-console";
/// The JWT settings Console reads from the environment. Debug hides the
/// signing secret.
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
    /// `CENTAUR_JWT_SIGNING_SECRET` plus the entitlement audience/issuer
    /// overrides, with Console's defaults. No secret means the sandbox surface
    /// cannot authenticate anything.
    pub(crate) fn from_env() -> Option<Self> {
        Self::new(
            jwt_signing_secret()?,
            env::var("CENTAUR_SANDBOX_ENTITLEMENTS_JWT_AUDIENCE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_AUDIENCE.to_owned()),
            env::var("CENTAUR_SANDBOX_ENTITLEMENTS_JWT_ISSUER")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ISSUER.to_owned()),
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
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SandboxClaims {
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) sub: String,
    // The JWT layer validates the standard claims; a missing or non-string
    // binding claim reaches the proxy lookup and becomes "invalid sandbox
    // token", matching `SandboxTokenAuthentication` (Ruby only compares the
    // payload values, so a numeric claim is a mismatch rather than a decode
    // error).
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) sandbox_id: String,
    #[serde(default, deserialize_with = "string_claim")]
    pub(crate) proxy_id: String,
    #[serde(default, deserialize_with = "optional_string_claim")]
    pub(crate) principal_id: Option<String>,
    /// Ruby's JWT rejects a numeric `iat` in the future. Not required.
    #[serde(default)]
    pub(crate) iat: Option<serde_json::Value>,
}

fn string_claim<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().map(str::to_owned).unwrap_or_default())
}

fn optional_string_claim<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().map(str::to_owned))
}

/// What a successfully authenticated sandbox request carries to its handler.
#[derive(Clone, Debug)]
pub(crate) struct SandboxContext {
    pub(crate) proxy: ConsoleProxy,
    pub(crate) claims: SandboxClaims,
}

/// A route builder that cannot add a sandbox route without the JWT check.
pub(crate) struct SandboxRoutes {
    state: AppState,
    config: Option<SandboxJwtConfig>,
    router: Router<AppState>,
}

impl SandboxRoutes {
    pub(crate) fn new(state: AppState, config: Option<SandboxJwtConfig>) -> Self {
        Self {
            state,
            config,
            router: Router::new(),
        }
    }

    /// A sandbox route available to any valid entitlement token.
    pub(crate) fn route(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.with_access(path, route, SandboxAccess::Sandbox)
    }

    /// A route that only validates the entitlement JWT, without the
    /// proxy -> principal binding. Rails' sandbox `oauth_apps` index does this
    /// because it exposes only public OAuth start URLs; use it only there.
    pub(crate) fn jwt_only(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.with_access(path, route, SandboxAccess::JwtOnly)
    }

    fn with_access(
        mut self,
        path: &str,
        route: MethodRouter<AppState>,
        access: SandboxAccess,
    ) -> Self {
        let guard = SandboxGuard {
            state: self.state.clone(),
            config: self.config.clone(),
            access,
        };
        let route = route.route_layer(middleware::from_fn_with_state(
            guard,
            authorize_sandbox_request,
        ));
        self.router = self.router.route(path, route);
        self
    }

    pub(crate) fn into_router(self) -> Router<AppState> {
        self.router
    }
}

pub(crate) fn sandbox_router(
    state: AppState,
    config: Option<SandboxJwtConfig>,
) -> Router<AppState> {
    let routes = super::areas::sandbox(SandboxRoutes::new(state, config));
    #[cfg(test)]
    let routes = test_sandbox_routes(routes);
    routes.into_router()
}

#[derive(Clone)]
struct SandboxGuard {
    state: AppState,
    config: Option<SandboxJwtConfig>,
    access: SandboxAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SandboxAccess {
    Sandbox,
    JwtOnly,
}

async fn authorize_sandbox_request(
    State(guard): State<SandboxGuard>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = bearer_token(request.headers()) else {
        return SandboxRejection::InvalidOrMissing.into_response();
    };
    let Some(config) = guard.config.as_ref() else {
        return SandboxRejection::Invalid.into_response();
    };
    let Some(claims) = verify_sandbox_jwt(token, config) else {
        // `SandboxTokenAuthentication` rescues JWT verification failures with
        // the same "invalid or missing" body as an absent token.
        return SandboxRejection::InvalidOrMissing.into_response();
    };

    if guard.access == SandboxAccess::JwtOnly {
        // Matching Rails' sandbox `oauth_apps` index: a valid signature,
        // audience, and issuer are enough for this public metadata endpoint.
        return next.run(request).await;
    }

    let pool = match guard.state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let Some(proxy) = (match find_console_proxy(&pool, &claims.proxy_id).await {
        Ok(proxy) => proxy,
        Err(error) => return ApiError::Internal(error.to_string()).into_response(),
    }) else {
        return SandboxRejection::Invalid.into_response();
    };
    if !valid_proxy_binding(&proxy, &claims) {
        return SandboxRejection::Invalid.into_response();
    }

    request
        .extensions_mut()
        .insert(SandboxContext { proxy, claims });
    next.run(request).await
}

/// The console API's second credential: a sandbox entitlement JWT from a Slack
/// DM in the bot's workspace linked to an active Console admin, acting as that
/// admin.
///
/// Returns `Ok(None)` for anything that is not such a credential, so the
/// console guard can keep its existing rejection for API keys and invalid
/// sandbox tokens. A valid entitlement whose binding or admin link fails is
/// also `None`; only a database failure is an error.
pub(crate) async fn sandbox_admin_user(
    state: &AppState,
    headers: &HeaderMap,
    config: Option<SandboxJwtConfig>,
) -> Result<Option<ConsoleUser>, ApiError> {
    let Some(config) = config.or_else(SandboxJwtConfig::from_env) else {
        return Ok(None);
    };
    let Some(token) = bearer_token(headers) else {
        return Ok(None);
    };
    let Some(claims) = verify_sandbox_jwt(token, &config) else {
        return Ok(None);
    };
    let pool = state.console_session_db()?;
    let Some(proxy) = find_console_proxy(&pool, &claims.proxy_id)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?
    else {
        return Ok(None);
    };
    if !valid_proxy_binding(&proxy, &claims) {
        return Ok(None);
    }
    let Some(principal_id) = proxy.principal_id else {
        return Ok(None);
    };
    let Some(principal) = find_principal(&pool, principal_id)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?
    else {
        return Ok(None);
    };
    let home_team = match principal.slack_team_id.as_deref() {
        Some(team_id) => slack_bot_home_team(&pool, team_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?,
        None => false,
    };
    let user = match principal.console_user_id {
        Some(user_id) => find_console_user(&pool, user_id)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?,
        None => None,
    };
    Ok(sandbox_admin_allowed(&principal, home_team, user.as_ref())
        .then_some(user)
        .flatten())
}

/// `CentaurJwt::Hs256.decode`: HS256 signature, expiry, audience, and issuer.
/// Proxy-sync always sets `exp` (`centaur-proxy-sync/src/tokens.rs`), so the
/// port requires it instead of accepting a token Ruby's JWT would verify
/// without an expiry check.
fn verify_sandbox_jwt(token: &str, config: &SandboxJwtConfig) -> Option<SandboxClaims> {
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
    // `JWT.decode` verifies `iat` when present (jsonwebtoken 11 does not).
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

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = header
        .strip_prefix("Bearer ")?
        .trim_start_matches(char::is_whitespace);
    (!token.is_empty()).then_some(token)
}

fn valid_proxy_binding(proxy: &ConsoleProxy, claims: &SandboxClaims) -> bool {
    let expected_principal = proxy.principal_id.map(|id| oid("prn", id));
    proxy.assigned()
        && claims.principal_id.as_ref() == expected_principal.as_ref()
        && proxy.name == claims.sandbox_id
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SandboxRejection {
    InvalidOrMissing,
    Invalid,
}

impl SandboxRejection {
    fn error(self) -> ConsoleError {
        match self {
            Self::InvalidOrMissing => {
                ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid or missing sandbox token")
            }
            Self::Invalid => ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid sandbox token"),
        }
    }
}

impl IntoResponse for SandboxRejection {
    fn into_response(self) -> Response {
        let error = self.error();
        render_error(error.status, &error.message, error.details)
    }
}

impl FromRequestParts<AppState> for SandboxContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            ApiError::Internal("sandbox authorization middleware did not attach context".to_owned())
        })
    }
}

#[cfg(test)]
pub(crate) fn test_sandbox_routes(routes: SandboxRoutes) -> SandboxRoutes {
    use axum::routing::get;

    routes.route("/probe/sandbox", get(probe_sandbox))
}

#[cfg(test)]
async fn probe_sandbox(context: SandboxContext) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "sandbox_id": context.claims.sandbox_id,
        "proxy_id": context.claims.proxy_id,
        "principal_id": context.claims.principal_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    #[test]
    fn sandbox_jwt_checks_claims_like_ruby_jwt() {
        let config = SandboxJwtConfig::new(
            "test-only-sandbox-jwt-secret",
            "centaur-console-sandbox-entitlements",
            "centaur-console",
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
            "iss": "centaur-console",
            "aud": "centaur-console-sandbox-entitlements",
            "exp": now + 60,
            "proxy_id": "prx_1",
            "sandbox_id": "sandbox",
        }));
        assert_eq!(
            verify_sandbox_jwt(&minimal, &config).unwrap().proxy_id,
            "prx_1"
        );
        // Proxy-sync always includes `exp`; an unsigned-lifetime token is now
        // refused instead of passing Ruby's present-only expiry check.
        let no_expiry = token(json!({
            "iss": "centaur-console",
            "aud": "centaur-console-sandbox-entitlements",
        }));
        assert!(verify_sandbox_jwt(&no_expiry, &config).is_none());
        let missing_audience = token(json!({"iss": "centaur-console", "exp": now + 60}));
        assert!(verify_sandbox_jwt(&missing_audience, &config).is_none());
        let missing_issuer = token(json!({
            "aud": "centaur-console-sandbox-entitlements",
            "exp": now + 60,
        }));
        assert!(verify_sandbox_jwt(&missing_issuer, &config).is_none());
        let future_iat = token(json!({
            "iss": "centaur-console",
            "aud": "centaur-console-sandbox-entitlements",
            "exp": now + 60,
            "iat": now + 60,
        }));
        assert!(verify_sandbox_jwt(&future_iat, &config).is_none());
        // Ruby compares payload values to database values, so a non-string
        // binding claim is a mismatch ("invalid sandbox token"), not a decode
        // error ("invalid or missing sandbox token").
        let malformed_bindings = token(json!({
            "iss": "centaur-console",
            "aud": "centaur-console-sandbox-entitlements",
            "exp": now + 60,
            "sub": 7,
            "sandbox_id": false,
            "proxy_id": 9,
            "principal_id": {"unexpected": true},
        }));
        let claims = verify_sandbox_jwt(&malformed_bindings, &config).unwrap();
        assert!(claims.sub.is_empty());
        assert!(claims.sandbox_id.is_empty());
        assert!(claims.proxy_id.is_empty());
        assert_eq!(claims.principal_id, None);
        let expired = token(json!({
            "iss": "centaur-console",
            "aud": "centaur-console-sandbox-entitlements",
            "exp": now - 1,
        }));
        assert!(verify_sandbox_jwt(&expired, &config).is_none());
        let wrong_audience = token(json!({
            "iss": "centaur-console",
            "aud": "another-audience",
            "exp": now + 60,
        }));
        assert!(verify_sandbox_jwt(&wrong_audience, &config).is_none());
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
            sub: "centaur-sandbox".to_owned(),
            sandbox_id: "sandbox-name".to_owned(),
            proxy_id: oid("prx", 9),
            principal_id: principal_id.map(str::to_owned),
            iat: None,
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
    fn sandbox_error_bodies_match_console() {
        assert_eq!(
            SandboxRejection::InvalidOrMissing.error().message,
            "invalid or missing sandbox token"
        );
        assert_eq!(
            SandboxRejection::Invalid.error().message,
            "invalid sandbox token"
        );
    }

    #[test]
    fn sandbox_config_uses_console_defaults_and_hides_the_secret() {
        let config = SandboxJwtConfig::new("secret", DEFAULT_AUDIENCE, DEFAULT_ISSUER).unwrap();
        assert_eq!(config.audience, DEFAULT_AUDIENCE);
        assert_eq!(config.issuer, DEFAULT_ISSUER);
        assert!(!format!("{config:?}").contains("secret"));
        assert!(SandboxJwtConfig::new("  ", DEFAULT_AUDIENCE, DEFAULT_ISSUER).is_none());
    }
}
