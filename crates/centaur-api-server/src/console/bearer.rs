//! The bearer operator API under `/api/v1`.
//!
//! Authentication is the unified guard: a Console API key whose owner is an
//! active admin resolves to a caller holding `operator.api`. The credential
//! types and their error bodies live in [`crate::console::guard`]; this module
//! only keeps the bearer area's request extensions and router entry point so
//! the area modules have a stable import path.

use axum::Router;

use crate::{AppState, console::guard::GuardedRoutes};

pub(crate) use crate::console::guard::BearerUser;

/// The bearer router, nested at `/api/v1` while Console is enabled. Ported
/// bearer areas add their routes here.
pub(crate) fn bearer_router(state: AppState) -> Router<AppState> {
    let routes = crate::console::areas::bearer(GuardedRoutes::new(state));
    #[cfg(test)]
    let routes = test_bearer_routes(routes);
    routes.into_router()
}

#[cfg(test)]
pub(crate) fn test_bearer_routes(routes: GuardedRoutes) -> GuardedRoutes {
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;

    /// A Console route and a bearer route are separate mounts of the same
    /// unified guard. A request cannot reach a probe that the other namespace
    /// did not register.
    #[tokio::test]
    async fn console_and_bearer_routes_cannot_be_mixed() {
        let config = crate::ConsoleSessionConfig::from_settings(
            Some("test-only-console-session-secret"),
            Some("https://console.example.test"),
        )
        .unwrap()
        .unwrap();
        let app = crate::build_router_with_app_state(
            crate::AppState::unready(crate::ApiAuthConfig::testing("test-secret"))
                .with_console_session(config),
        );
        for path in [
            "/api/v1/console/probe/bearer/admin",
            "/api/v1/probe/admin",
            "/api/v1/console/probe/bearer/admin",
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
        }
    }
}
