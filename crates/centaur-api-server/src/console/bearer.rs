//! Bearer authentication for the operator API under `/api/v1`.
//!
//! Mirrors `services/console/app/controllers/api/base_controller.rb`: a
//! `Bearer ` token is looked up by SHA-256 `api_keys.token_hash`; its owner
//! must be active or the request is a 401 `invalid or missing API key`, and
//! must be an admin or it is a 403 `API key owner is not an admin`.
//!
//! Routes are added through [`BearerRoutes`] so every route carries the guard.
//! The console and bearer namespaces are separate nested routers, so a console
//! route can never reach this guard (and vice versa); a router test pins that.

use axum::{
    Router,
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};
use centaur_console_db::{ConsoleUser, find_console_api_key};
use sha2::{Digest, Sha256};

use crate::{
    ApiError, AppState,
    console::http::{ConsoleError, render_error},
};

/// A caller authorized by an active admin API key.
#[derive(Clone, Debug)]
pub(crate) struct BearerUser(pub(crate) ConsoleUser);

/// The `ApiKey` row that authenticated the request. Handlers that need the
/// key's own id (Console's "cannot revoke the key used for this request"
/// guard) read it here instead of re-hashing the `Authorization` header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BearerApiKey(pub(crate) i64);

/// A route builder that cannot add a bearer route without the API-key check.
pub(crate) struct BearerRoutes {
    state: AppState,
    router: Router<AppState>,
}

impl BearerRoutes {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            state,
            router: Router::new(),
        }
    }

    pub(crate) fn route(mut self, path: &str, route: MethodRouter<AppState>) -> Self {
        let guard = BearerGuard {
            state: self.state.clone(),
        };
        let route = route.route_layer(middleware::from_fn_with_state(
            guard,
            authorize_bearer_request,
        ));
        self.router = self.router.route(path, route);
        self
    }

    /// Every bearer route requires an active admin owner, so this is just the
    /// explicit spelling of [`Self::route`].
    pub(crate) fn admin(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.route(path, route)
    }

    pub(crate) fn into_router(self) -> Router<AppState> {
        self.router
    }
}

/// The bearer router, nested at `/api/v1` while Console is enabled. Ported
/// bearer areas add their routes here.
pub(crate) fn bearer_router(state: AppState) -> Router<AppState> {
    let routes = super::areas::bearer(BearerRoutes::new(state));
    #[cfg(test)]
    let routes = test_bearer_routes(routes);
    routes.into_router()
}

#[derive(Clone)]
struct BearerGuard {
    state: AppState,
}

async fn authorize_bearer_request(
    State(guard): State<BearerGuard>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = bearer_token(request.headers());
    let authenticated = match token {
        Some(token) => {
            let pool = match guard.state.console_db() {
                Ok(pool) => pool,
                Err(error) => return error.into_response(),
            };
            let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
            match find_console_api_key(&pool, &token_hash).await {
                Ok(authenticated) => authenticated,
                Err(error) => return ApiError::Internal(error.to_string()).into_response(),
            }
        }
        None => None,
    };
    let owner = authenticated.as_ref().map(|(_, owner)| owner);

    match decide_bearer_access(token.is_some(), owner) {
        Ok(owner) => {
            request.extensions_mut().insert(BearerUser(owner));
            if let Some((key_id, _)) = authenticated {
                request.extensions_mut().insert(BearerApiKey(key_id));
            }
            next.run(request).await
        }
        Err(rejection) => rejection.into_response(),
    }
}

/// Console's `bearer_token`: only a case-sensitive `Bearer ` scheme is
/// accepted, and whitespace between it and the token is removed.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = header
        .strip_prefix("Bearer ")?
        .trim_start_matches(char::is_whitespace);
    (!token.is_empty()).then_some(token)
}

/// `Api::BaseController`'s decision order, minus the never-reachable
/// credential-namespace branch: token present, owner resolved, owner admin.
fn decide_bearer_access(
    token_present: bool,
    owner: Option<&ConsoleUser>,
) -> Result<ConsoleUser, BearerRejection> {
    if !token_present {
        return Err(BearerRejection::InvalidKey);
    }
    let Some(owner) = owner else {
        return Err(BearerRejection::InvalidKey);
    };
    if !owner.admin {
        return Err(BearerRejection::NotAdmin);
    }
    Ok(owner.clone())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BearerRejection {
    InvalidKey,
    NotAdmin,
}

impl BearerRejection {
    fn error(self) -> ConsoleError {
        match self {
            Self::InvalidKey => {
                ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid or missing API key")
            }
            Self::NotAdmin => {
                ConsoleError::new(StatusCode::FORBIDDEN, "API key owner is not an admin")
            }
        }
    }
}

impl IntoResponse for BearerRejection {
    fn into_response(self) -> Response {
        let error = self.error();
        render_error(error.status, &error.message, error.details)
    }
}

impl FromRequestParts<AppState> for BearerApiKey {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().copied().ok_or_else(|| {
            ApiError::Internal(
                "bearer authorization middleware did not attach an API key".to_owned(),
            )
        })
    }
}

impl FromRequestParts<AppState> for BearerUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            ApiError::Internal("bearer authorization middleware did not attach a user".to_owned())
        })
    }
}

#[cfg(test)]
fn test_bearer_routes(routes: BearerRoutes) -> BearerRoutes {
    use axum::routing::get;

    routes.route("/probe/bearer/admin", get(probe_bearer_admin))
}

#[cfg(test)]
async fn probe_bearer_admin(user: BearerUser) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "user_id": user.0.id,
        "email": user.0.email,
        "admin": user.0.admin,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_console_db::ConsoleUserStatus;

    fn user(admin: bool) -> ConsoleUser {
        ConsoleUser {
            id: 1,
            email: "admin@example.test".to_owned(),
            name: None,
            admin,
            status: ConsoleUserStatus::Active,
        }
    }

    fn error(rejection: BearerRejection) -> ConsoleError {
        rejection.error()
    }

    #[test]
    fn bearer_token_requires_the_exact_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer token".parse().unwrap());
        assert_eq!(bearer_token(&headers), Some("token"));
        headers.insert(AUTHORIZATION, "Bearer  token".parse().unwrap());
        assert_eq!(bearer_token(&headers), Some("token"));
        headers.insert(AUTHORIZATION, "bearer token".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
        headers.insert(AUTHORIZATION, "Basic token".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
        headers.insert(AUTHORIZATION, "Bearer ".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn bearer_guard_order_is_token_owner_then_admin() {
        let admin = user(true);
        assert_eq!(
            decide_bearer_access(false, Some(&admin)),
            Err(BearerRejection::InvalidKey)
        );
        assert_eq!(
            decide_bearer_access(true, None),
            Err(BearerRejection::InvalidKey)
        );
        assert_eq!(
            decide_bearer_access(true, Some(&user(false))),
            Err(BearerRejection::NotAdmin)
        );
        assert_eq!(decide_bearer_access(true, Some(&admin)).unwrap(), admin);
    }

    #[test]
    fn bearer_error_bodies_match_console() {
        let invalid = error(BearerRejection::InvalidKey);
        assert_eq!(invalid.status, StatusCode::UNAUTHORIZED);
        assert_eq!(invalid.message, "invalid or missing API key");
        assert_eq!(invalid.details, None);
        let forbidden = error(BearerRejection::NotAdmin);
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
        assert_eq!(forbidden.message, "API key owner is not an admin");
        assert_eq!(forbidden.details, None);
    }

    /// A console route and a bearer route are separate nested routers, so a
    /// request cannot reach the other namespace's guard even when both test
    /// probe routes exist. This replaces the deleted runtime namespace check.
    #[tokio::test]
    async fn console_and_bearer_routes_cannot_be_mixed() {
        use axum::{
            body::Body,
            http::{Request, header},
        };
        use tower::ServiceExt;

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
