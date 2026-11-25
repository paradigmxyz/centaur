//! Console's session cookie configuration and the two shell endpoints that
//! every signed-in browser (and delegated sandbox) calls.
//!
//! The authorization decision itself lives in [`crate::console::guard`]. This
//! module owns the cookie settings, the `/me` and `/session` handlers, and the
//! `ConsoleUser` request extension. Routes are added through the unified
//! `GuardedRoutes` builder, which requires a capability for every route.

use std::fmt;

use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode, header::SET_COOKIE},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use centaur_console_db::{ConsoleUser, ConsoleUserStatus, oid};
use serde_json::json;
use thiserror::Error;
use url::Url;

use crate::{
    ApiError, AppState,
    console::{
        guard::{GuardedRoutes, SandboxDelegated},
        sandbox::SandboxJwtConfig,
        session_cookie::{ConsoleSession, SessionCookieConfig},
    },
};

/// The Console layer's settings: the cookie, the exact Origin writes must
/// carry, and the sandbox-token verifier (an explicit test override; production
/// reads the environment).
///
/// `Debug` shows the public origin and cookie name but not the secret or the
/// derived key.
#[derive(Clone)]
pub struct ConsoleSessionConfig {
    cookie: SessionCookieConfig,
    public_origin: String,
    sandbox_jwt: Option<SandboxJwtConfig>,
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

    pub(crate) fn cookie(&self) -> &SessionCookieConfig {
        &self.cookie
    }

    pub(crate) fn session_from_headers(&self, headers: &HeaderMap) -> Option<ConsoleSession> {
        self.cookie.session_from_headers(headers)
    }

    /// The sandbox-token verifier. `None` means the guard reads the environment.
    pub(crate) fn sandbox_jwt(&self) -> Option<SandboxJwtConfig> {
        self.sandbox_jwt.clone()
    }

    /// Test-only override for the sandbox JWT settings, which production reads
    /// from the environment on each request.
    #[cfg(test)]
    pub(crate) fn with_sandbox_jwt(mut self, config: SandboxJwtConfig) -> Self {
        self.sandbox_jwt = Some(config);
        self
    }

    pub(crate) fn session_cookie_header(
        &self,
        session: &ConsoleSession,
    ) -> Option<axum::http::HeaderValue> {
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

/// The console API router, mounted at `/api/v1/console` only when configured.
pub(crate) fn console_router(state: AppState) -> axum::Router<AppState> {
    let routes = GuardedRoutes::new(state)
        .console_pending("/me", get(console_me))
        .console_pending("/session", delete(console_session_destroy));
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
                "api_keys": admin && !delegated,
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
fn test_console_routes(routes: GuardedRoutes) -> GuardedRoutes {
    use axum::routing::{get, post};

    routes
        .console_admin("/probe/admin", get(probe_admin))
        .console_user("/probe/active", get(probe_active))
        .console_pending("/probe/pending", get(probe_pending).post(probe_pending))
        .console_admin("/probe/admin-write", post(probe_admin))
}

#[cfg(test)]
async fn probe_admin(user: ConsoleUser) -> Json<serde_json::Value> {
    probe_user(user)
}

#[cfg(test)]
async fn probe_active(user: ConsoleUser) -> Json<serde_json::Value> {
    probe_user(user)
}

#[cfg(test)]
async fn probe_pending(user: ConsoleUser) -> Json<serde_json::Value> {
    probe_user(user)
}

#[cfg(test)]
fn probe_user(user: ConsoleUser) -> Json<serde_json::Value> {
    Json(json!({
        "user_id": user.id,
        "email": user.email,
        "admin": user.admin,
        "status": crate::console::status_name(user.status),
    }))
}

impl axum::extract::FromRequestParts<AppState> for ConsoleUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
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
    use axum::http::{HeaderMap, header::ORIGIN};

    use crate::console::http::request_origin;

    const PUBLIC_URL: &str = "https://console.example.test/some/path?ignored=yes";
    const SECRET: &str =
        "test-only-console-session-secret-0000000000000000000000000000000000000000000000000000";

    fn config() -> ConsoleSessionConfig {
        ConsoleSessionConfig::from_settings(Some(SECRET), Some(PUBLIC_URL))
            .unwrap()
            .unwrap()
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
    fn console_session_origin_requires_the_matching_origin_header() {
        let config = config();
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, "https://console.example.test".parse().unwrap());
        assert!(request_origin(&headers) == Some(config.public_origin()));
        headers.remove(ORIGIN);
        headers.insert(
            axum::http::header::REFERER,
            "https://console.example.test/console".parse().unwrap(),
        );
        assert!(request_origin(&headers).is_none());
        headers.insert(ORIGIN, "null".parse().unwrap());
        assert!(request_origin(&headers) != Some(config.public_origin()));
    }

    #[test]
    fn sandbox_jwt_override_is_debug_hidden() {
        let config = config()
            .with_sandbox_jwt(SandboxJwtConfig::new("sandbox-secret", "aud", "iss").unwrap());
        assert!(!format!("{config:?}").contains("sandbox-secret"));
        assert!(config.sandbox_jwt().is_some());
    }
}
