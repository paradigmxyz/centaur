//! Console's session cookie and the authorization guard for
//! `/api/v1/console/*`.
//!
//! Console's Rails application is being moved into api-rs one area at a time.
//! Routes are added through [`ConsoleRoutes`], which requires an access level
//! for every route and defaults to [`ConsoleAccess::Admin`], so a ported route
//! cannot accidentally be less protected than Console's default.
//!
//! The account, origin, and admin checks follow
//! `services/console/app/controllers/concerns/console_api_authentication.rb`:
//! authorization header, session/user, disabled, pending, exact Origin, admin.
//! Unlike Rails, the port uses no CSRF token: an unsafe request must carry the
//! configured Console public origin verbatim, and a missing or `null` Origin is
//! refused.

use std::fmt;

use axum::{
    Extension, Json, Router,
    extract::{FromRequestParts, OriginalUri, Request, State},
    http::{
        HeaderMap, Method, StatusCode,
        header::{AUTHORIZATION, SET_COOKIE},
        request::Parts,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{MethodRouter, delete, get},
};
use centaur_console_db::{ConsoleUser, ConsoleUserStatus, find_console_user, oid};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;

use crate::{
    ApiError, AppState,
    console::{
        http::{render_error, request_origin},
        session_cookie::{ConsoleSession, SessionCookieConfig},
    },
};

/// Everything needed to verify a Console session and the Origin of a write.
///
/// `Debug` shows the public origin and cookie name but not the secret or the
/// derived key.
#[derive(Clone)]
pub struct ConsoleSessionConfig {
    cookie: SessionCookieConfig,
    public_origin: String,
    sandbox_jwt: Option<crate::console::sandbox::SandboxJwtConfig>,
}

impl fmt::Debug for ConsoleSessionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsoleSessionConfig")
            .field("cookie", &self.cookie)
            .field("public_origin", &self.public_origin)
            .field("sandbox_jwt", &self.sandbox_jwt)
            .finish()
    }
}

impl ConsoleSessionConfig {
    /// Builds the console session layer when a secret is configured.
    ///
    /// `CENTAUR_CONSOLE_PUBLIC_URL` already exists on api-rs for other purposes,
    /// so an origin without a secret simply leaves this layer disabled. A
    /// secret without an origin cannot check unsafe requests and is a startup
    /// error, as is an invalid origin once the layer is enabled.
    pub fn from_settings(
        session_secret: Option<&str>,
        console_public_url: Option<&str>,
    ) -> Result<Option<Self>, ConsoleSessionConfigError> {
        let session_secret = session_secret
            .map(str::trim)
            .filter(|secret| !secret.is_empty());
        let console_public_url = console_public_url
            .map(str::trim)
            .filter(|url| !url.is_empty());

        match (session_secret, console_public_url) {
            (None, _) => Ok(None),
            (Some(_), None) => Err(ConsoleSessionConfigError::MissingPublicUrl),
            (Some(session_secret), Some(console_public_url)) => {
                let public_origin = normalize_console_origin(console_public_url)
                    .ok_or(ConsoleSessionConfigError::InvalidPublicUrl)?;
                let secure = public_origin.starts_with("https://");
                Ok(Some(Self {
                    cookie: SessionCookieConfig::new(session_secret, secure),
                    public_origin,
                    sandbox_jwt: None,
                }))
            }
        }
    }

    pub(crate) fn public_origin(&self) -> &str {
        &self.public_origin
    }

    #[allow(dead_code)]
    pub(crate) fn cookie_name(&self) -> &str {
        self.cookie.cookie_name()
    }

    pub(crate) fn cookie(&self) -> &SessionCookieConfig {
        &self.cookie
    }

    pub(crate) fn session_from_headers(&self, headers: &HeaderMap) -> Option<ConsoleSession> {
        self.cookie.session_from_headers(headers)
    }

    /// Test-only override for the sandbox JWT settings, which production reads
    /// from the environment on each request.
    #[cfg(test)]
    pub(crate) fn with_sandbox_jwt(
        mut self,
        config: crate::console::sandbox::SandboxJwtConfig,
    ) -> Self {
        self.sandbox_jwt = Some(config);
        self
    }

    fn session_cookie_header(&self, session: &ConsoleSession) -> Option<axum::http::HeaderValue> {
        self.cookie.set_cookie_header(session)
    }
}

/// Configuration failures are intentionally free of secret and URL contents.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ConsoleSessionConfigError {
    #[error(
        "CONSOLE_SESSION_SECRET is set but CENTAUR_CONSOLE_PUBLIC_URL is not; \
         set both to enable the console session API"
    )]
    MissingPublicUrl,
    #[error(
        "CENTAUR_CONSOLE_PUBLIC_URL must be an absolute http(s) URL with a host; \
         the console session API uses its scheme://host[:port] as the exact Origin"
    )]
    InvalidPublicUrl,
}

/// Reduces the configured Console URL to the exact required Origin.
fn normalize_console_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// Marks a console request authenticated by the sandbox-delegated credential
/// rather than the session cookie.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SandboxDelegated;

/// The minimum account level a console route requires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsoleAccess {
    /// Active Console admin. This is the default for console routes.
    Admin,
    /// Any active Console user.
    #[allow(dead_code)]
    ActiveUser,
    /// Any enabled Console user, including one pending approval.
    PendingUser,
}

/// A router builder that cannot add a console route without a session check.
pub(crate) struct ConsoleRoutes {
    state: AppState,
    router: Router<AppState>,
}

impl ConsoleRoutes {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            state,
            router: Router::new(),
        }
    }

    /// Adds an admin-only route. This is the default access level, so a route
    /// added without an explicit policy is never less protected than Console's
    /// default.
    #[allow(dead_code)]
    pub(crate) fn route(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.with_access(path, route, ConsoleAccess::Admin)
    }

    /// Adds an admin-only route, named explicitly for readability at call sites.
    #[allow(dead_code)]
    pub(crate) fn admin(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.route(path, route)
    }

    /// Adds a route available to any active user.
    #[allow(dead_code)]
    pub(crate) fn active(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.with_access(path, route, ConsoleAccess::ActiveUser)
    }

    /// Adds a route available to active and pending users.
    pub(crate) fn pending(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.with_access(path, route, ConsoleAccess::PendingUser)
    }

    fn with_access(
        mut self,
        path: &str,
        route: MethodRouter<AppState>,
        access: ConsoleAccess,
    ) -> Self {
        let guard = ConsoleGuard {
            state: self.state.clone(),
            access,
        };
        let route = route.route_layer(middleware::from_fn_with_state(
            guard,
            authorize_console_request,
        ));
        self.router = self.router.route(path, route);
        self
    }

    pub(crate) fn into_router(self) -> Router<AppState> {
        self.router
    }
}

#[derive(Clone)]
struct ConsoleGuard {
    state: AppState,
    access: ConsoleAccess,
}

/// The console API router, mounted at `/api/v1/console` only when configured.
pub(crate) fn console_router(state: AppState) -> Router<AppState> {
    let routes = ConsoleRoutes::new(state)
        .pending("/me", get(console_me))
        .pending("/session", delete(console_session_destroy));
    let routes = crate::console::areas::console(routes);
    #[cfg(test)]
    let routes = test_console_routes(routes);
    routes.into_router()
}

async fn console_me(
    State(state): State<AppState>,
    session: Option<Extension<ConsoleSession>>,
    delegated: Option<Extension<SandboxDelegated>>,
    user: ConsoleUser,
) -> Response {
    let Some(config) = state.console_session() else {
        return ApiError::ServiceUnavailable(
            "the console session API is not configured".to_owned(),
        )
        .into_response();
    };
    let delegated = delegated.is_some();
    let session = session.map(|Extension(session)| session);
    let admin = user.admin;
    let active = user.status == ConsoleUserStatus::Active;
    let api_clients_enabled = crate::console::api_clients_enabled();
    let body = json!({
        "data": {
            "user": {
                "id": oid("usr", user.id),
                "email": user.email,
                "name": user.name,
                "status": crate::console::status_name(user.status),
                "admin": admin,
            },
            "auth_method": if delegated { "sandbox" } else { "session" },
            "acting_admin": admin,
            "features": {
                "api_clients": api_clients_enabled,
                "password_login": crate::console::password_login_setting(),
            },
            "nav": {
                "control": admin,
                "principals": admin,
                "roles": admin,
                "secrets": admin,
                "credentials": admin,
                "oauth_apps": admin,
                "users": admin,
                "api_clients": admin && api_clients_enabled,
                "settings": admin,
                "workflows": admin,
                "integrations": active,
                "skills": active,
                "scheduled_tasks": active,
            },
        }
    });

    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().expect("valid header"));
    if let Some(session) = session.as_ref()
        && let Some(cookie) = config.session_cookie_header(session)
    {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

async fn console_session_destroy(
    State(state): State<AppState>,
    delegated: Option<Extension<SandboxDelegated>>,
) -> Response {
    let Some(config) = state.console_session() else {
        return ApiError::ServiceUnavailable(
            "the console session API is not configured".to_owned(),
        )
        .into_response();
    };
    // A sandbox-delegated caller has no cookie to reset.
    if delegated.is_some() {
        return StatusCode::NO_CONTENT.into_response();
    }
    // Reset the session; the login page renders its own signed-out state.
    let next = ConsoleSession::anonymous();
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Some(cookie) = config.session_cookie_header(&next) {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

#[cfg(test)]
fn test_console_routes(routes: ConsoleRoutes) -> ConsoleRoutes {
    use axum::routing::post;

    routes
        .admin("/probe/admin", get(probe_admin))
        .active("/probe/active", get(probe_active))
        .pending("/probe/pending", get(probe_pending).post(probe_pending))
        .admin("/probe/admin-write", post(probe_admin))
}

#[cfg(test)]
async fn probe_admin(user: ConsoleUser) -> Json<Value> {
    probe_user(user)
}

#[cfg(test)]
async fn probe_active(user: ConsoleUser) -> Json<Value> {
    probe_user(user)
}

#[cfg(test)]
async fn probe_pending(user: ConsoleUser) -> Json<Value> {
    probe_user(user)
}

#[cfg(test)]
fn probe_user(user: ConsoleUser) -> Json<Value> {
    Json(json!({
        "user_id": user.id,
        "email": user.email,
        "admin": user.admin,
        "status": crate::console::status_name(user.status),
    }))
}

async fn authorize_console_request(
    State(guard): State<ConsoleGuard>,
    mut request: Request,
    next: Next,
) -> Response {
    if !console_namespace_request(console_request_path(&request)) {
        return ConsoleRejection::NamespaceMismatch.into_response();
    }
    let Some(config) = guard.state.console_session().cloned() else {
        return ApiError::ServiceUnavailable(
            "the console session API is not configured".to_owned(),
        )
        .into_response();
    };

    if authorization_present(request.headers()) {
        // A sandbox entitlement JWT from an active Console admin's Slack DM is
        // the console API's second credential. Anything else that sent an
        // Authorization header, including an API key, keeps the rejection
        // below. The delegated credential has no cookie, so it has no CSRF
        // token or Origin to check.
        return match crate::console::sandbox::sandbox_admin_user(
            &guard.state,
            request.headers(),
            config.sandbox_jwt.clone(),
        )
        .await
        {
            Ok(Some(user)) => match decide_sandbox_access(&user, guard.access) {
                Ok(user) => {
                    request.extensions_mut().insert(user);
                    request.extensions_mut().insert(SandboxDelegated);
                    next.run(request).await
                }
                Err(rejection) => rejection.into_response(),
            },
            Ok(None) => ConsoleRejection::SessionRequiredForbidden.into_response(),
            Err(error) => error.into_response(),
        };
    }
    let session = config.session_from_headers(request.headers());
    let Some(user_id) = session.as_ref().and_then(ConsoleSession::user_id) else {
        // Rails checks the session before touching the database.
        return ConsoleRejection::SessionRequired.into_response();
    };
    let pool = match guard.state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let user = match find_console_user(&pool, user_id).await {
        Ok(user) => user,
        Err(error) => return ApiError::Internal(error.to_string()).into_response(),
    };

    let is_safe_method = matches!(request.method(), &Method::GET | &Method::HEAD);
    let origin_matches = !is_safe_method && origin_matches(request.headers(), &config);

    match decide_console_access(&ConsoleDecision {
        has_authorization: false,
        user: user.as_ref(),
        is_safe_method,
        origin_matches,
        access: guard.access,
    }) {
        Ok(user) => {
            request.extensions_mut().insert(user);
            if let Some(session) = session {
                request.extensions_mut().insert(session);
            }
            next.run(request).await
        }
        Err(ConsoleRejection::AccountDisabled) => {
            let mut response = ConsoleRejection::AccountDisabled.into_response();
            let cleared = ConsoleSession::anonymous();
            if let Some(cookie) = config.session_cookie_header(&cleared) {
                response.headers_mut().append(SET_COOKIE, cookie);
            }
            response
        }
        Err(rejection) => rejection.into_response(),
    }
}

/// Rails' `request.headers["Authorization"].present?`: an absent, empty, or
/// whitespace-only header does not count as a bearer credential.
fn authorization_present(headers: &HeaderMap) -> bool {
    headers.get(AUTHORIZATION).is_some_and(|value| {
        // A non-UTF-8 header cannot be a bearer credential, but it is present;
        // fail closed rather than treating it as absent.
        value
            .to_str()
            .map_or(true, |value| !value.trim().is_empty())
    })
}

/// `ConsoleApiAuthentication` only serves `/api/v1/console/`; a route mounted
/// elsewhere fails closed rather than accepting a session cookie.
fn console_namespace_request(path: &str) -> bool {
    path.starts_with("/api/v1/console/")
}

/// Nested axum routers strip their prefix from `request.uri()`; Rails checks
/// the full request path, which axum keeps in `OriginalUri`.
fn console_request_path(request: &Request) -> &str {
    request
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| request.uri().path(), |original| original.0.path())
}

/// The request facts the authorization decision depends on, kept separate from
/// HTTP and the database so the complete check order is unit-testable.
struct ConsoleDecision<'a> {
    has_authorization: bool,
    user: Option<&'a ConsoleUser>,
    is_safe_method: bool,
    origin_matches: bool,
    access: ConsoleAccess,
}

/// Applies Console's checks in order: authorization header, session/user,
/// disabled, pending, exact Origin, admin.
fn decide_console_access(decision: &ConsoleDecision<'_>) -> Result<ConsoleUser, ConsoleRejection> {
    if decision.has_authorization {
        return Err(ConsoleRejection::SessionRequiredForbidden);
    }
    let Some(user) = decision.user else {
        return Err(ConsoleRejection::SessionRequired);
    };
    check_account(user, decision.access)?;
    if !decision.is_safe_method && !decision.origin_matches {
        return Err(ConsoleRejection::Csrf);
    }
    check_admin(user, decision.access)?;
    Ok(user.clone())
}

/// A sandbox-delegated credential has already passed the sandbox-admin rule,
/// so it only needs the account and access-level checks a session gets.
fn decide_sandbox_access(
    user: &ConsoleUser,
    access: ConsoleAccess,
) -> Result<ConsoleUser, ConsoleRejection> {
    check_account(user, access)?;
    check_admin(user, access)?;
    Ok(user.clone())
}

fn check_account(user: &ConsoleUser, access: ConsoleAccess) -> Result<(), ConsoleRejection> {
    match user.status {
        ConsoleUserStatus::Disabled => Err(ConsoleRejection::AccountDisabled),
        ConsoleUserStatus::Pending if access != ConsoleAccess::PendingUser => {
            Err(ConsoleRejection::AccountPending)
        }
        ConsoleUserStatus::Pending | ConsoleUserStatus::Active => Ok(()),
    }
}

fn check_admin(user: &ConsoleUser, access: ConsoleAccess) -> Result<(), ConsoleRejection> {
    if access == ConsoleAccess::Admin && !user.admin {
        return Err(ConsoleRejection::AdminRequired);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsoleRejection {
    NamespaceMismatch,
    SessionRequiredForbidden,
    SessionRequired,
    AccountDisabled,
    AccountPending,
    Csrf,
    AdminRequired,
}

impl ConsoleRejection {
    fn response_parts(self) -> (StatusCode, Value) {
        let (status, message, reason) = match self {
            Self::NamespaceMismatch => (
                StatusCode::UNAUTHORIZED,
                "endpoint credential misconfigured",
                "credential_namespace_mismatch",
            ),
            Self::SessionRequiredForbidden => (
                StatusCode::FORBIDDEN,
                "the console API requires a console session; API keys are not accepted \
                 (the bearer API is /api/v1 outside /api/v1/console)",
                "session_required",
            ),
            Self::SessionRequired => (
                StatusCode::UNAUTHORIZED,
                "console session required",
                "session_required",
            ),
            Self::AccountDisabled => (
                StatusCode::UNAUTHORIZED,
                "account disabled",
                "account_disabled",
            ),
            Self::AccountPending => (
                StatusCode::FORBIDDEN,
                "account pending approval",
                "account_pending",
            ),
            Self::Csrf => (
                StatusCode::FORBIDDEN,
                "invalid or missing CSRF token",
                "csrf",
            ),
            Self::AdminRequired => (
                StatusCode::FORBIDDEN,
                "admin access required",
                "admin_required",
            ),
        };
        (
            status,
            json!({
                "error": {
                    "message": message,
                    "details": {"reason": reason},
                }
            }),
        )
    }
}

impl IntoResponse for ConsoleRejection {
    fn into_response(self) -> Response {
        let (status, body) = self.response_parts();
        render_error(
            status,
            body["error"]["message"].as_str().expect("message"),
            body["error"].get("details").cloned(),
        )
    }
}

fn origin_matches(headers: &HeaderMap, config: &ConsoleSessionConfig) -> bool {
    request_origin(headers).as_deref() == Some(config.public_origin())
}

impl FromRequestParts<AppState> for ConsoleUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            ApiError::Internal("console authorization middleware did not attach a user".to_owned())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLIC_URL: &str = "https://console.example.test/some/path?ignored=yes";
    const SECRET: &str =
        "test-only-console-session-secret-0000000000000000000000000000000000000000000000000000";

    fn config() -> ConsoleSessionConfig {
        ConsoleSessionConfig::from_settings(Some(SECRET), Some(PUBLIC_URL))
            .unwrap()
            .unwrap()
    }

    fn user(status: ConsoleUserStatus, admin: bool) -> ConsoleUser {
        ConsoleUser {
            id: 42,
            email: "user@example.test".to_owned(),
            name: Some("User".to_owned()),
            admin,
            status,
        }
    }

    fn decision(access: ConsoleAccess, user: Option<&ConsoleUser>) -> ConsoleDecision<'_> {
        ConsoleDecision {
            has_authorization: false,
            user,
            is_safe_method: true,
            origin_matches: true,
            access,
        }
    }

    fn assert_rejection(
        decision: &ConsoleDecision<'_>,
        status: StatusCode,
        reason: &str,
        message: &str,
    ) {
        let (actual_status, body) = decide_console_access(decision)
            .expect_err("decision should be rejected")
            .response_parts();
        assert_eq!(actual_status, status);
        assert_eq!(
            body,
            json!({
                "error": {
                    "message": message,
                    "details": {"reason": reason},
                }
            })
        );
    }

    #[test]
    fn console_session_config_requires_a_secret_and_hides_it() {
        for (secret, public_url) in [
            (None, None),
            (None, Some("https://console.example.test")),
            (Some("  "), Some("https://console.example.test")),
        ] {
            assert!(
                ConsoleSessionConfig::from_settings(secret, public_url)
                    .unwrap()
                    .is_none()
            );
        }
        for (secret, public_url) in [(Some(SECRET), None), (Some(SECRET), Some("  "))] {
            assert!(ConsoleSessionConfig::from_settings(secret, public_url).is_err());
        }

        let config = config();
        assert_eq!(config.public_origin(), "https://console.example.test");
        assert_eq!(config.cookie_name(), "_centaur_console_session");
        assert!(!format!("{config:?}").contains(SECRET));

        for invalid in [
            "console.example.test",
            "/relative",
            "ftp://console.example.test",
            "mailto:ops@example.test",
        ] {
            assert!(
                ConsoleSessionConfig::from_settings(Some(SECRET), Some(invalid)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn console_session_authorization_header_is_rejected_before_everything_else() {
        let admin = user(ConsoleUserStatus::Active, true);
        let mut input = decision(ConsoleAccess::Admin, Some(&admin));
        input.has_authorization = true;
        input.is_safe_method = false;
        input.origin_matches = false;
        assert_rejection(
            &input,
            StatusCode::FORBIDDEN,
            "session_required",
            "the console API requires a console session; API keys are not accepted \
             (the bearer API is /api/v1 outside /api/v1/console)",
        );
    }

    #[test]
    fn console_session_account_checks_follow_the_console_order() {
        assert_rejection(
            &decision(ConsoleAccess::Admin, None),
            StatusCode::UNAUTHORIZED,
            "session_required",
            "console session required",
        );

        let disabled = user(ConsoleUserStatus::Disabled, true);
        let mut input = decision(ConsoleAccess::PendingUser, Some(&disabled));
        input.is_safe_method = false;
        input.origin_matches = false;
        assert_rejection(
            &input,
            StatusCode::UNAUTHORIZED,
            "account_disabled",
            "account disabled",
        );

        let pending = user(ConsoleUserStatus::Pending, true);
        let mut input = decision(ConsoleAccess::ActiveUser, Some(&pending));
        input.is_safe_method = false;
        input.origin_matches = false;
        assert_rejection(
            &input,
            StatusCode::FORBIDDEN,
            "account_pending",
            "account pending approval",
        );

        // The Origin check precedes the admin check.
        let active_non_admin = user(ConsoleUserStatus::Active, false);
        let mut input = decision(ConsoleAccess::Admin, Some(&active_non_admin));
        input.is_safe_method = false;
        input.origin_matches = false;
        assert_rejection(
            &input,
            StatusCode::FORBIDDEN,
            "csrf",
            "invalid or missing CSRF token",
        );
        assert_rejection(
            &decision(ConsoleAccess::Admin, Some(&active_non_admin)),
            StatusCode::FORBIDDEN,
            "admin_required",
            "admin access required",
        );
    }

    #[test]
    fn console_session_sandbox_access_reruns_the_account_and_admin_checks() {
        let admin = user(ConsoleUserStatus::Active, true);
        assert!(decide_sandbox_access(&admin, ConsoleAccess::Admin).is_ok());
        assert!(decide_sandbox_access(&admin, ConsoleAccess::ActiveUser).is_ok());
        assert!(decide_sandbox_access(&admin, ConsoleAccess::PendingUser).is_ok());

        let non_admin = user(ConsoleUserStatus::Active, false);
        assert_rejection(
            &decision(ConsoleAccess::Admin, Some(&non_admin)),
            StatusCode::FORBIDDEN,
            "admin_required",
            "admin access required",
        );
        assert!(matches!(
            decide_sandbox_access(&non_admin, ConsoleAccess::Admin),
            Err(ConsoleRejection::AdminRequired)
        ));
        assert!(decide_sandbox_access(&non_admin, ConsoleAccess::ActiveUser).is_ok());

        let disabled = user(ConsoleUserStatus::Disabled, true);
        assert!(matches!(
            decide_sandbox_access(&disabled, ConsoleAccess::PendingUser),
            Err(ConsoleRejection::AccountDisabled)
        ));
        let pending = user(ConsoleUserStatus::Pending, true);
        assert!(matches!(
            decide_sandbox_access(&pending, ConsoleAccess::ActiveUser),
            Err(ConsoleRejection::AccountPending)
        ));
    }

    #[test]
    fn console_session_origin_applies_only_to_unsafe_methods() {
        let admin = user(ConsoleUserStatus::Active, true);
        let mut input = decision(ConsoleAccess::Admin, Some(&admin));
        input.is_safe_method = false;
        input.origin_matches = false;
        assert_rejection(
            &input,
            StatusCode::FORBIDDEN,
            "csrf",
            "invalid or missing CSRF token",
        );

        input.origin_matches = true;
        assert!(decide_console_access(&input).is_ok());

        let mut safe = decision(ConsoleAccess::Admin, Some(&admin));
        safe.is_safe_method = true;
        safe.origin_matches = false;
        assert!(decide_console_access(&safe).is_ok());
    }

    #[test]
    fn console_session_origin_requires_the_matching_origin_header() {
        let config = config();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "https://console.example.test".parse().unwrap(),
        );
        assert!(origin_matches(&headers, &config));
        headers.remove(axum::http::header::ORIGIN);
        headers.insert(
            axum::http::header::REFERER,
            "https://console.example.test/console".parse().unwrap(),
        );
        assert!(!origin_matches(&headers, &config));
        headers.remove(axum::http::header::REFERER);
        assert!(!origin_matches(&headers, &config));
        headers.insert(axum::http::header::ORIGIN, "null".parse().unwrap());
        assert!(!origin_matches(&headers, &config));
    }

    #[tokio::test]
    async fn console_session_namespace_check_uses_the_full_nested_path() {
        use axum::body::Body;
        use tower::ServiceExt;

        // A session cookie carries a user id, so after the namespace check the
        // guard needs the (unconfigured) database and answers 503. A stripped
        // path would fail the namespace guard (401) first.
        let config = config();
        let cookie = config
            .cookie()
            .set_cookie_header(&ConsoleSession::signed_in(1))
            .expect("session cookie");
        let state = AppState::unready(crate::ApiAuthConfig::testing("test-secret"))
            .with_console_session(config);
        let response = crate::build_router_with_app_state(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/console/probe/admin")
                    .header(axum::http::header::HOST, "console.example.test")
                    .header(axum::http::header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
