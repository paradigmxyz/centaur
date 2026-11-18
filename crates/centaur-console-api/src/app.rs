//! The console service: every area's [`Routes`] under their namespaces'
//! prefixes and guards, static files and the SPA shell, and the middleware
//! every response passes through.

use std::time::Instant;

use axum::{
    Router,
    extract::Request,
    http::{HeaderValue, header},
    middleware::{self, Next, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::{
    catch_panic::CatchPanicLayer, services::ServeDir, set_header::SetResponseHeaderLayer,
};

use super::{
    assets,
    auth::{self, Access},
    error::ApiError,
    hosts, pages,
    routes::Routes,
    state::AppState,
};

pub const BEARER_PREFIX: &str = "/api/v1";
pub const CONSOLE_PREFIX: &str = "/api/v1/console";
pub const SANDBOX_PREFIX: &str = "/api/v1/sandbox";
pub const SANDBOX_ADMIN_PREFIX: &str = "/api/v1/sandbox/admin";
pub const INTERNAL_PREFIX: &str = "/internal";

/// The full app for `routes`.
pub fn router(state: AppState, routes: Routes) -> Router {
    let session = || from_fn_with_state(state.clone(), auth::session_layer);
    let console = |access| from_fn_with_state((state.clone(), access), auth::console_guard);

    let console_api = Router::new()
        .merge(routes.console.layer(console(Access::Admin)))
        .merge(routes.console_active.layer(console(Access::Active)))
        .merge(routes.console_pending.layer(console(Access::Pending)))
        .layer(session());
    let sandbox = Router::new()
        .merge(
            routes
                .sandbox
                .layer(from_fn_with_state(state.clone(), auth::sandbox_guard)),
        )
        .merge(routes.sandbox_claims.layer(from_fn_with_state(
            state.clone(),
            auth::sandbox_claims_guard,
        )));
    let sandbox_admin = routes
        .sandbox_admin
        .layer(from_fn_with_state(state.clone(), auth::sandbox_admin_guard));
    let bearer = routes
        .bearer
        .layer(from_fn_with_state(state.clone(), auth::bearer_guard));
    let internal = routes
        .internal
        .layer(from_fn_with_state(state.clone(), auth::internal_guard));

    let fallback = ServeDir::new(&state.config.public_dir)
        .call_fallback_on_method_not_allowed(true)
        .fallback(
            Router::new()
                .fallback(assets::fallback)
                .with_state(state.clone()),
        );

    Router::new()
        .route("/up", get(|| async { "ok" }))
        .nest(CONSOLE_PREFIX, console_api)
        .nest(SANDBOX_ADMIN_PREFIX, sandbox_admin)
        .nest(SANDBOX_PREFIX, sandbox)
        .nest(INTERNAL_PREFIX, internal)
        .nest(BEARER_PREFIX, bearer)
        .merge(routes.public.layer(session()))
        .method_not_allowed_fallback(|| async { ApiError::MethodNotAllowed.into_response() })
        .fallback_service(fallback)
        .with_state(state.clone())
        .layer(from_fn_with_state(state.hosts.clone(), hosts::guard))
        .layer(middleware::from_fn(record_request))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(pages::DEFAULT_CSP),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
        .layer(CatchPanicLayer::custom(|_| {
            ApiError::internal("handler panicked").into_response()
        }))
}

/// Request metrics and one log line per request (`/up` is quiet).
async fn record_request(request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    centaur_telemetry::record_http_request_finished(
        method.as_str(),
        "console",
        status,
        started.elapsed(),
    );
    if path != "/up" {
        tracing::info!(%method, path, status, "request");
    }
    response
}
