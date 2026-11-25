//! Console area: browser-facing authentication routes.
//!
//! SSO sign-in and the MCP OAuth authorization server's metadata, registration,
//! consent, and token endpoints. The SPA shell and its assets are static and
//! live in `console::static_files`.

mod mcp_oauth;
mod sso;

use crate::{
    AppState,
    console::{
        guard::GuardedRoutes,
        http::{ConsoleError, json_not_found},
        pages, return_path,
        session_cookie::ConsoleSession,
        static_files::APP_CSP,
    },
    console_session::ConsoleSessionConfig,
};
use axum::{
    Router,
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
};
use centaur_console_db::ConsoleUserStatus;

/// Routes under `/api/v1/console` (paths relative to that prefix).
pub(crate) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    routes
}

/// Routes under `/api/v1` (bearer API; paths relative to that prefix).
pub(crate) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    routes
}

/// Routes under `/api/v1/sandbox` (paths relative to that prefix).
pub(crate) fn sandbox(routes: GuardedRoutes) -> GuardedRoutes {
    routes
}

/// Server-rendered pages and other top-level routes (absolute paths).
pub(crate) fn pages(router: Router<AppState>) -> Router<AppState> {
    router
        .route(
            "/auth/{provider}/start",
            axum::routing::get(sso::start).fallback(not_found_for_method),
        )
        .route(
            "/auth/{provider}/callback",
            axum::routing::get(sso::callback).fallback(not_found_for_method),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            axum::routing::get(mcp_oauth::metadata).fallback(not_found_for_method),
        )
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(mcp_oauth::metadata).fallback(not_found_for_method),
        )
        .route(
            "/mcp/oauth/register",
            axum::routing::post(mcp_oauth::register).fallback(not_found_for_method),
        )
        .route(
            "/mcp/oauth/authorize",
            axum::routing::get(mcp_oauth::authorize_page)
                .post(mcp_oauth::approve)
                .fallback(not_found_for_method),
        )
        .route(
            "/mcp/oauth/token",
            axum::routing::post(mcp_oauth::token).fallback(not_found_for_method),
        )
}

async fn not_found_for_method() -> Response {
    json_not_found()
}

/// The configured SSO providers, in Console's display order.
pub(crate) fn providers() -> Vec<&'static str> {
    sso::providers()
}

/// The Console session config. The Console router is mounted only when this is
/// configured, so every route inside it can rely on the value.
pub(crate) fn console_config(state: &AppState) -> &ConsoleSessionConfig {
    state
        .console_session()
        .expect("the Console router is mounted only with a session config")
}

pub(super) fn public_base_url(config: &ConsoleSessionConfig) -> String {
    config.public_origin().to_owned()
}

pub(super) fn console_env(key: &str) -> Option<String> {
    crate::console::console_env(key)
}

pub(super) fn login_redirect_with_csp(
    config: &ConsoleSessionConfig,
    headers: &HeaderMap,
    error: &str,
    extra_cookies: Vec<HeaderValue>,
    csp: &'static str,
) -> Response {
    let session = config
        .session_from_headers(headers)
        .unwrap_or_else(ConsoleSession::anonymous);
    let mut cookies = extra_cookies;
    if let Some(cookie) = config.cookie().set_cookie_header(&session) {
        cookies.push(cookie);
    }
    pages::redirect(&format!("/login?error={error}"), &cookies, Some(csp))
}

/// Resolve the signed-in user for the authorization endpoint, applying the
/// status gate. Returns `Err(response)` when the
/// request has already been answered with a redirect.
#[allow(clippy::result_large_err)]
pub(super) async fn require_active_user(
    state: &AppState,
    config: &ConsoleSessionConfig,
    headers: &HeaderMap,
    return_to: Option<&str>,
    csp: &'static str,
) -> Result<centaur_console_db::ConsoleUser, Response> {
    let Some(mut session) = config.session_from_headers(headers) else {
        let mut anonymous = ConsoleSession::anonymous();
        if let Some(safe) = return_to.and_then(return_path::safe) {
            anonymous.set_return_to(safe);
        }
        let cookie = config.cookie().set_cookie_header(&anonymous);
        return Err(pages::redirect("/login", cookie.as_slice(), Some(csp)));
    };
    let Some(user_id) = session.user_id() else {
        if let Some(safe) = return_to.and_then(return_path::safe) {
            session.set_return_to(safe);
        }
        let cookie = config.cookie().set_cookie_header(&session);
        return Err(pages::redirect("/login", cookie.as_slice(), Some(csp)));
    };
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return Err(error.into_response()),
    };
    let user = match centaur_console_db::find_console_user(&pool, user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            if let Some(safe) = return_to.and_then(return_path::safe) {
                session.set_return_to(safe);
            }
            let cookie = config.cookie().set_cookie_header(&session);
            return Err(pages::redirect("/login", cookie.as_slice(), Some(csp)));
        }
        Err(error) => return Err(ConsoleError::internal(error).into_response()),
    };
    match user.status {
        ConsoleUserStatus::Active => Ok(user),
        ConsoleUserStatus::Pending => {
            let cookie = config.cookie().set_cookie_header(&session);
            Err(pages::redirect("/pending", cookie.as_slice(), Some(csp)))
        }
        ConsoleUserStatus::Disabled => {
            let signed_out = ConsoleSession::anonymous();
            let cookie = config.cookie().set_cookie_header(&signed_out);
            Err(pages::redirect(
                &format!(
                    "/login?error={}",
                    crate::console::pages::login_error::ACCOUNT_DISABLED
                ),
                cookie.as_slice(),
                Some(csp),
            ))
        }
    }
}

pub(super) fn callback_url(base: &str, provider: &str) -> String {
    absolute_url(base, &format!("/auth/{provider}/callback"))
}

/// An absolute path replaces any base path.
pub(super) fn absolute_url(base: &str, path: &str) -> String {
    let Ok(mut url) = url::Url::parse(base) else {
        return format!("{}{}", base.trim_end_matches('/'), path);
    };
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}
