//! The Rust Console surface.
//!
//! Each ported area adds its routes through the single
//! [`guard::GuardedRoutes`] builder: it declares the capability a route
//! requires and attaches the unified authorization layer. The console, bearer,
//! and sandbox mounts use the same builder; a path no longer selects which
//! credential type is accepted.
//!
//! HTML pages remain server-rendered with the static `public/assets/oauth-page`
//! files embedded in the binary. The built SPA is served from disk by
//! `static_files`.
//!
pub(crate) mod areas;
mod assets;
pub(crate) mod guard;
pub(crate) mod host;
pub(crate) mod http;
mod pages;
pub(crate) mod request;
pub(crate) mod return_path;
pub(crate) mod sandbox;
pub(crate) mod session_cookie;
mod static_files;
#[cfg(test)]
mod tests_db;

use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_console_db::ConsoleUserStatus;

use crate::{AppState, console::guard::GuardedRoutes, console_session::console_router};

pub(crate) const DISABLED_MESSAGE: &str = "Your account is disabled.";
pub(crate) const PENDING_PATH: &str = "/pending";
pub(crate) const LOGIN_PATH: &str = "/login";

/// Every enabled Console route. Compiled only when `CONSOLE_SESSION_SECRET`
/// and `CENTAUR_CONSOLE_PUBLIC_URL` are configured.
pub(crate) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .merge(areas::pages(pages::router()))
        .merge(assets::router())
        .merge(static_files::router())
        .nest("/api/v1/console", console_router(state.clone()))
        .nest("/api/v1/sandbox", sandbox::sandbox_router(state.clone()))
        .nest("/api/v1", bearer_router(state))
}

/// The bearer operator API, nested at `/api/v1` while Console is enabled.
pub(crate) fn bearer_router(state: AppState) -> Router<AppState> {
    let routes = areas::bearer(GuardedRoutes::new(state));
    #[cfg(test)]
    let routes = test_bearer_routes(routes);
    routes.into_router()
}

#[cfg(test)]
fn test_bearer_routes(routes: GuardedRoutes) -> GuardedRoutes {
    use axum::routing::get;

    routes.operator("/probe/bearer/admin", get(probe_bearer_admin))
}

#[cfg(test)]
async fn probe_bearer_admin(
    user: crate::console::guard::BearerUser,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "user_id": user.0.id,
        "email": user.0.email,
        "admin": user.0.admin,
    }))
}

/// The app-wide fallback: Console-owned prefixes get Console's JSON 404 body;
/// everything else keeps axum's empty 404.
pub(crate) async fn fallback(request: Request) -> Response {
    let path = request.uri().path();
    let in_namespace = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}/"));
    if in_namespace("/api/v1")
        || path == LOGIN_PATH
        || path == PENDING_PATH
        || path == "/logout"
        || in_namespace("/console")
        || in_namespace("/mcp/oauth")
        || in_namespace("/oauth")
        || in_namespace("/assets/oauth-page")
    {
        http::json_not_found()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Reads a Console environment setting from `CENTAUR_CONSOLE_<suffix>`. A
/// variable explicitly set to the empty string is returned as-is; the legacy
/// `IRON_CONTROL_` prefix is no longer consulted.
pub(crate) fn console_env(suffix: &str) -> Option<String> {
    std::env::var(format!("CENTAUR_CONSOLE_{suffix}")).ok()
}

/// The boolean Console settings. These are the only values
/// [`validate_boolean_settings`] accepts, and the only ones for which
/// [`boolean_setting`] returns a value.
pub(crate) const BOOLEAN_SETTINGS: &[&str] = &["PASSWORD_LOGIN_ENABLED", "API_CLIENTS_ENABLED"];

/// Unset means enabled; otherwise the strict boolean cast, so an explicitly
/// empty value is `None` (falsy, and `null` in `/me`) rather than `false`.
pub(crate) fn password_login_setting() -> Option<bool> {
    match console_env("PASSWORD_LOGIN_ENABLED") {
        None => Some(true),
        Some(raw) => boolean_setting(&raw),
    }
}

pub(crate) fn password_login_enabled() -> bool {
    password_login_setting().unwrap_or(false)
}

/// Only an explicit true enables it.
pub(crate) fn api_clients_enabled() -> bool {
    console_env("API_CLIENTS_ENABLED")
        .as_deref()
        .and_then(boolean_setting)
        .unwrap_or(false)
}

/// A Console boolean setting accepts only `true`/`false` (case-insensitive)
/// and `1`/`0`. An explicitly empty value is a third state: for
/// `PASSWORD_LOGIN_ENABLED` it disables password login and reports `null`.
fn boolean_setting(raw: &str) -> Option<bool> {
    if raw.is_empty() {
        return None;
    }
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Rejects an invalid boolean setting at startup instead of silently treating
/// a typo as a value. An unset or explicitly empty variable keeps Console's
/// existing default (`PASSWORD_LOGIN_ENABLED` enabled, `API_CLIENTS_ENABLED`
/// disabled). `api-rs`'s startup calls this through the library re-export.
pub fn validate_boolean_settings() -> Result<(), String> {
    for suffix in BOOLEAN_SETTINGS {
        let Some(raw) = console_env(suffix) else {
            continue;
        };
        if !raw.is_empty() && boolean_setting(&raw).is_none() {
            return Err(format!(
                "CENTAUR_CONSOLE_{suffix} must be true, false, 1, or 0 \
                 (case-insensitive); got '{raw}'"
            ));
        }
    }
    Ok(())
}

pub(crate) fn status_name(status: ConsoleUserStatus) -> &'static str {
    match status {
        ConsoleUserStatus::Pending => "pending",
        ConsoleUserStatus::Active => "active",
        ConsoleUserStatus::Disabled => "disabled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn console_router_fallback_is_json_inside_every_nested_namespace() {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;

        let config = crate::ConsoleSessionConfig::from_settings(
            Some("test-only-console-session-secret"),
            Some("https://console.example.test"),
        )
        .unwrap()
        .unwrap();
        let app = crate::build_router_with_app_state(
            AppState::unready(crate::ApiAuthConfig::testing("test-secret"))
                .with_console_session(config),
        );
        for path in [
            "/api/v1/console/missing",
            "/api/v1/sandbox/missing",
            "/api/v1/missing",
            "/mcp/oauth/missing",
            "/oauth/missing",
            "/assets/oauth-page/missing",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::HOST, "console.example.test")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("application/json"),
                "{path}"
            );
        }

        // `/console/...` is a browser navigation and gets the SPA shell (a 404
        // when this checkout has no build). Other paths are not SPA routes.
        // `static_files` tests the shell itself against a temporary build.
        let shell = static_files::spa_shell_path();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/console/missing")
                    .header(header::HOST, "console.example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let expected = if shell.is_file() {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(response.status(), expected);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/not-console")
                    .header(header::HOST, "console.example.test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn console_fallback_matches_namespaces_on_a_path_boundary() {
        let request = |path: &str| {
            Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert_eq!(
            fallback(request("/console/api/v1/missing")).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            fallback(request("/console-api")).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            fallback(request("/api/v1/missing")).await.status(),
            StatusCode::NOT_FOUND
        );
        let response = fallback(request("/api/v1/missing")).await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["message"],
            "not found"
        );
        // Outside Console's prefixes the app keeps axum's empty 404.
        let response = fallback(request("/console-api")).await;
        assert!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn host_routing_separates_the_console_and_internal_surfaces() {
        use axum::body::Body;
        use axum::http::{Method, Request, header};
        use tower::ServiceExt;

        const CONSOLE_HOST: &str = "console.example.test";
        const INTERNAL_HOST: &str = "centaur-api.centaur.svc.cluster.local";

        async fn send(
            app: &Router,
            host: &str,
            method: Method,
            path: &str,
            headers: &[(&str, &str)],
        ) -> Response {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, host);
            for (name, value) in headers {
                builder = builder.header(*name, *value);
            }
            app.clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap()
        }

        let config = crate::ConsoleSessionConfig::from_settings(
            Some("test-only-console-session-secret"),
            Some("https://console.example.test"),
        )
        .unwrap()
        .unwrap();
        let app = crate::build_router_with_app_state(
            AppState::unready(crate::ApiAuthConfig::testing("test-secret"))
                .with_console_session(config),
        );

        // api-rs health and metrics routes are not on the Console host.
        for (method, path) in [
            (Method::GET, "/metrics"),
            (Method::GET, "/healthz"),
            (Method::GET, "/readyz"),
            (Method::GET, "/mcp"),
            (Method::POST, "/oauth/token"),
            (Method::GET, "/.well-known/oauth-protected-resource"),
            (Method::GET, "/api/session/slack:example"),
            (Method::POST, "/api/webhooks/example"),
        ] {
            let response = send(&app, CONSOLE_HOST, method, path, &[]).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(
            send(&app, INTERNAL_HOST, Method::GET, "/metrics", &[])
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&app, INTERNAL_HOST, Method::GET, "/healthz", &[])
                .await
                .status(),
            StatusCode::OK
        );
        // `AppState::unready` is exactly what a starting pod looks like.
        assert_eq!(
            send(&app, INTERNAL_HOST, Method::GET, "/readyz", &[])
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        // The bearer API under `/api/v1` is reachable from both hosts; the
        // guard answers 401 without credentials on either side.
        for host in [CONSOLE_HOST, INTERNAL_HOST] {
            let response = send(&app, host, Method::GET, "/api/v1/api_keys", &[]).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{host}");
        }

        // The cookie console API belongs to the public host only. An internal
        // request without an Authorization header never reaches its guard.
        let response = send(&app, CONSOLE_HOST, Method::GET, "/api/v1/console/me", &[]).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = send(&app, INTERNAL_HOST, Method::GET, "/api/v1/console/me", &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        // A delegated (Authorization) request does reach the guard internally
        // and fails its sandbox-token check instead of the host check.
        let response = send(
            &app,
            INTERNAL_HOST,
            Method::GET,
            "/api/v1/console/me",
            &[("authorization", "Bearer not-a-sandbox-token")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // The browser surface: server pages on the public host, 404 elsewhere.
        assert_eq!(
            send(&app, CONSOLE_HOST, Method::GET, "/login", &[])
                .await
                .status(),
            StatusCode::OK
        );
        for path in [
            "/login",
            "/logout",
            "/pending",
            "/auth/github/start",
            "/oauth/example/start",
            "/mcp/oauth/authorize",
            "/.well-known/oauth-authorization-server",
            "/assets/oauth-page/oauth.css",
        ] {
            let response = send(&app, INTERNAL_HOST, Method::GET, path, &[]).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }

        // The SPA fallback exists only on the public host. The harness does
        // not always build the shell, so the public host is either the shell
        // or the not-built page.
        let shell = static_files::spa_shell_path();
        let response = send(&app, CONSOLE_HOST, Method::GET, "/console/principals", &[]).await;
        if shell.is_file() {
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response = send(&app, INTERNAL_HOST, Method::GET, "/console/principals", &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .is_none_or(|value| !value.to_str().unwrap_or("").starts_with("text/html")),
        );
        let response = send(&app, INTERNAL_HOST, Method::GET, "/", &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn console_boolean_settings_accept_only_true_false_one_zero() {
        for raw in ["1", "true", "TRUE", "True"] {
            assert_eq!(boolean_setting(raw), Some(true), "{raw:?}");
        }
        for raw in ["0", "false", "FALSE", "False"] {
            assert_eq!(boolean_setting(raw), Some(false), "{raw:?}");
        }
        // A wide cast is not allowed: these are invalid, not true.
        for raw in ["t", "T", "on", "ON", "yes", "no", "enabled", "2", " "] {
            assert_eq!(boolean_setting(raw), None, "{raw:?}");
        }
        // An explicitly empty value is the third state.
        assert_eq!(boolean_setting(""), None);
    }
}
