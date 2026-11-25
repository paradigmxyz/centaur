//! The unified authorization layer.
//!
//! Every credential maps to an [`AuthenticatedCaller`] (an identity plus a set
//! of [`Capability`] values), and every route declares the capability it
//! requires. The builder in this module is the only way to add a route, so a
//! route cannot be registered without a declaration.
//!
//! The old `ConsoleRoutes`, `BearerRoutes`, and `SandboxRoutes` guards are
//! gone. A path no longer decides which credential type is accepted: the guard
//! tries the sandbox token, a Console API key, api-rs's service credentials,
//! and (on Console routes, when no `Authorization` header is present) the
//! Console session cookie. The route's required capability's [`Surface`] only
//! selects the wording of an unauthenticated/forbidden response, which keeps
//! the browser and operator contracts stable.
//!
//! Browser rules that stay Console-specific:
//! - a disabled account is rejected and its cookie reset;
//! - an unsafe cookie-authenticated request must carry the exact Console
//!   Origin;
//! - an `Authorization` header is never mixed with a cookie session.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Router,
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{
        HeaderMap, Method, StatusCode,
        header::{AUTHORIZATION, SET_COOKIE},
        request::Parts,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};
use centaur_console_db::{ConsoleUser, ConsoleUserStatus, find_console_api_key, find_console_user};
use sha2::{Digest, Sha256};

use crate::{
    ApiError, AppState,
    auth::{AuthenticatedCaller, CallerClass, Capability, Surface},
    console::{
        http::{ConsoleError, request_origin},
        sandbox::{
            SandboxAuthentication, SandboxContext, SandboxJwtConfig, authenticate_sandbox,
            bearer_token,
        },
        session_cookie::ConsoleSession,
    },
    console_session::ConsoleSessionConfig,
};

/// The effective Console user a cookie session or delegated sandbox token
/// authenticated. Kept as its own extension so handlers keep extracting
/// `ConsoleUser`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SandboxDelegated;

impl FromRequestParts<AppState> for SandboxDelegated {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().copied().ok_or_else(|| {
            ApiError::Internal("console authorization did not mark a delegated caller".to_owned())
        })
    }
}

/// A caller authorized by a Console API key whose owner is an active admin.
#[derive(Clone, Debug)]
pub(crate) struct BearerUser(pub(crate) ConsoleUser);

/// The `ApiKey` row that authenticated the request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BearerApiKey(pub(crate) i64);

impl FromRequestParts<AppState> for BearerUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            ApiError::Internal("bearer authorization did not attach a user".to_owned())
        })
    }
}

impl FromRequestParts<AppState> for BearerApiKey {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().copied().ok_or_else(|| {
            ApiError::Internal("bearer authorization did not attach an API key".to_owned())
        })
    }
}

/// What a route requires. Console routes use one capability for every method; a
/// path whose methods differ (api-rs's session and workflow routes) declares
/// them method by method.
#[derive(Clone)]
enum RoutePolicy {
    Any(Capability),
    Methods(Arc<BTreeMap<Method, Capability>>),
}

impl RoutePolicy {
    fn capability(&self, method: &Method) -> Option<Capability> {
        match self {
            Self::Any(capability) => Some(*capability),
            Self::Methods(capabilities) => capabilities.get(method).copied(),
        }
    }
}

#[derive(Clone)]
struct RouteGuard {
    state: AppState,
    policy: RoutePolicy,
}

/// The only route builder. Every method names the capability the route
/// requires; the guard is attached with it.
pub(crate) struct GuardedRoutes {
    state: AppState,
    router: Router<AppState>,
}

impl GuardedRoutes {
    pub(crate) fn new(state: AppState) -> Self {
        Self {
            state,
            router: Router::new(),
        }
    }

    /// Adds a route with one capability for every method.
    pub(crate) fn capability(
        self,
        path: &str,
        route: MethodRouter<AppState>,
        capability: Capability,
    ) -> Self {
        self.with_policy(path, route, RoutePolicy::Any(capability))
    }

    /// Adds a route whose methods require different capabilities. Every method
    /// in the method router must appear in `capabilities`; an undeclared method
    /// is denied.
    pub(crate) fn methods(
        self,
        path: &str,
        route: MethodRouter<AppState>,
        capabilities: impl IntoIterator<Item = (Method, Capability)>,
    ) -> Self {
        let policy = RoutePolicy::Methods(Arc::new(capabilities.into_iter().collect()));
        self.with_policy(path, route, policy)
    }

    /// A `/api/v1/console` route available to any active Console user.
    pub(crate) fn console_user(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.capability(path, route, Capability::ConsoleUser)
    }

    /// A `/api/v1/console` route available to active and pending users.
    pub(crate) fn console_pending(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.capability(path, route, Capability::ConsolePending)
    }

    /// A `/api/v1/console` admin route. This is the default access level, so a
    /// route added without an explicit policy is never less protected than
    /// Console's default.
    pub(crate) fn console_admin(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.capability(path, route, Capability::ConsoleAdmin)
    }

    /// A bearer operator route under `/api/v1`.
    pub(crate) fn operator(self, path: &str, route: MethodRouter<AppState>) -> Self {
        self.capability(path, route, Capability::OperatorApi)
    }

    /// A sandbox route under `/api/v1/sandbox`.
    pub(crate) fn sandbox(
        self,
        path: &str,
        route: MethodRouter<AppState>,
        capability: Capability,
    ) -> Self {
        self.capability(path, route, capability)
    }

    /// Merges an already-built nested router and applies one capability to
    /// every route in it.
    pub(crate) fn merge_guarded(mut self, other: Router<AppState>, capability: Capability) -> Self {
        let guard = RouteGuard {
            state: self.state.clone(),
            policy: RoutePolicy::Any(capability),
        };
        self.router = self
            .router
            .merge(other.route_layer(middleware::from_fn_with_state(guard, authorize)));
        self
    }

    fn with_policy(
        mut self,
        path: &str,
        route: MethodRouter<AppState>,
        policy: RoutePolicy,
    ) -> Self {
        let guard = RouteGuard {
            state: self.state.clone(),
            policy,
        };
        let route = route.route_layer(middleware::from_fn_with_state(guard, authorize));
        self.router = self.router.route(path, route);
        self
    }

    pub(crate) fn into_router(self) -> Router<AppState> {
        self.router
    }
}

async fn authorize(State(guard): State<RouteGuard>, request: Request, next: Next) -> Response {
    let Some(required) = guard.policy.capability(request.method()) else {
        tracing::warn!(
            http_method = request.method().as_str(),
            http_route = matched_route(&request).as_str(),
            "authenticated caller denied because route has no authorization policy"
        );
        return ApiError::Forbidden("caller is not authorized for this route".to_owned())
            .into_response();
    };
    let surface = required.surface();
    let has_authorization = authorization_present(request.headers());

    if has_authorization {
        return authorize_bearer(guard, request, next, required, surface).await;
    }

    if surface == Surface::Console
        && let Some(config) = guard.state.console_session().cloned()
        && let Some(session) = config.session_from_headers(request.headers())
    {
        return authorize_console_cookie(guard, request, next, required, &config, session).await;
    }

    record_outcome("unknown", "unauthorized");
    unauthenticated(surface)
}

/// Browsers never bring an `Authorization` header; everything here is a
/// machine credential. The sandbox token, a Console-issued JWT with its own
/// audience, is tried first.
async fn authorize_bearer(
    guard: RouteGuard,
    mut request: Request,
    next: Next,
    required: Capability,
    surface: Surface,
) -> Response {
    let sandbox_config = guard
        .state
        .console_session()
        .and_then(ConsoleSessionConfig::sandbox_jwt)
        .or_else(SandboxJwtConfig::from_env);
    match authenticate_sandbox(&guard.state, request.headers(), sandbox_config).await {
        Ok(SandboxAuthentication::Authenticated(caller)) => {
            return finish(request, next, required, *caller).await;
        }
        Ok(SandboxAuthentication::Invalid) => {
            record_outcome("principal", "unauthorized");
            return authorization_failure(surface, AuthorizationFailure::SandboxInvalid);
        }
        Ok(SandboxAuthentication::InvalidOrMissing) => {
            record_outcome("principal", "unauthorized");
            return authorization_failure(surface, AuthorizationFailure::SandboxInvalidOrMissing);
        }
        Ok(SandboxAuthentication::NotPresent) => {}
        Err(error) => return error.into_response(),
    }

    // A Console API key is an opaque token, so its lookup is independent of the
    // JWT checks.
    if let Some(pool) = guard.state.console_db_optional() {
        let token = bearer_token(request.headers()).unwrap_or_default();
        if !token.is_empty() {
            let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
            match find_console_api_key(&pool, &token_hash).await {
                Ok(Some((key_id, owner))) => {
                    if !owner.admin {
                        record_outcome("bearer", "forbidden");
                        return authorization_failure(
                            surface,
                            AuthorizationFailure::BearerNotAdmin,
                        );
                    }
                    let mut caller = AuthenticatedCaller::new(
                        CallerClass::ApiClient,
                        owner.email.clone(),
                        [Capability::OperatorApi],
                        None,
                        None,
                    );
                    caller.attach_bearer_api_key(key_id, owner);
                    return finish(request, next, required, caller).await;
                }
                Ok(None) => {}
                Err(error) => return ApiError::Internal(error.to_string()).into_response(),
            }
        }
    }

    // api-rs's static admin key and API client tokens.
    match guard.state.auth().authenticate(request.headers()) {
        Ok(mut caller) => {
            if caller.class() == CallerClass::ApiClient {
                let pool = match guard.state.console_db() {
                    Ok(pool) => pool,
                    Err(error) => return error.into_response(),
                };
                match crate::api_clients::refresh_api_client(&pool, &mut caller).await {
                    Ok(client) => {
                        request.extensions_mut().insert(client);
                    }
                    Err(error) => {
                        record_outcome(caller.class().as_str(), "unauthorized");
                        return error.into_response();
                    }
                }
            }
            finish(request, next, required, caller).await
        }
        Err(ApiError::Internal(message)) => ApiError::Internal(message).into_response(),
        Err(_) => {
            record_outcome("unknown", "unauthorized");
            authorization_failure(surface, AuthorizationFailure::SessionForbidden)
        }
    }
}

async fn authorize_console_cookie(
    guard: RouteGuard,
    mut request: Request,
    next: Next,
    required: Capability,
    config: &ConsoleSessionConfig,
    session: ConsoleSession,
) -> Response {
    let Some(user_id) = session.user_id() else {
        record_outcome("console", "unauthorized");
        return authorization_failure(Surface::Console, AuthorizationFailure::NoCredential);
    };
    let pool = match guard.state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let user = match find_console_user(&pool, user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            record_outcome("console", "unauthorized");
            return authorization_failure(Surface::Console, AuthorizationFailure::NoCredential);
        }
        Err(error) => return ApiError::Internal(error.to_string()).into_response(),
    };

    // The order is Console's: disabled, pending, Origin, admin.
    if user.status == ConsoleUserStatus::Disabled {
        record_outcome("console", "unauthorized");
        let mut response =
            authorization_failure(Surface::Console, AuthorizationFailure::AccountDisabled);
        let cleared = ConsoleSession::anonymous();
        if let Some(cookie) = config.session_cookie_header(&cleared) {
            response.headers_mut().append(SET_COOKIE, cookie);
        }
        return response;
    }
    let mut caller = AuthenticatedCaller::new(
        CallerClass::Console,
        user.email.clone(),
        console_capabilities(&user),
        None,
        None,
    );
    caller.attach_console_user(user.clone(), Some(session.clone()));
    if user.status == ConsoleUserStatus::Pending && user.admin {
        // A pending admin never receives admin access, matching the old guard.
        caller.retain_capabilities(|capability| capability != Capability::ConsoleAdmin);
    }

    // Console's order: pending before Origin, Origin before admin.
    if user.status == ConsoleUserStatus::Pending && !caller.has_capability(required) {
        record_outcome("console", "forbidden");
        return authorization_failure(Surface::Console, AuthorizationFailure::AccountPending);
    }
    let is_safe_method = matches!(request.method(), &Method::GET | &Method::HEAD);
    if !is_safe_method && !origin_matches(request.headers(), config) {
        record_outcome("console", "forbidden");
        return authorization_failure(Surface::Console, AuthorizationFailure::Csrf);
    }
    if !caller.has_capability(required) {
        record_outcome("console", "forbidden");
        return authorization_failure(Surface::Console, AuthorizationFailure::AdminRequired);
    }

    request.extensions_mut().insert(user);
    request.extensions_mut().insert(session);
    record_outcome("console", "authorized");
    next.run(request).await
}

/// The capabilities a Console user's session grants.
pub(crate) fn console_capabilities(user: &ConsoleUser) -> Vec<Capability> {
    match user.status {
        ConsoleUserStatus::Pending | ConsoleUserStatus::Disabled => {
            vec![Capability::ConsoleRead, Capability::ConsolePending]
        }
        ConsoleUserStatus::Active if user.admin => vec![
            Capability::ConsoleRead,
            Capability::ConsoleAdmin,
            Capability::ConsoleUser,
        ],
        ConsoleUserStatus::Active => vec![Capability::ConsoleRead, Capability::ConsoleUser],
    }
}

/// Runs the caller-specific restrictions that are not a route capability, then
/// enforces the route's capability.
async fn finish(
    mut request: Request,
    next: Next,
    required: Capability,
    caller: AuthenticatedCaller,
) -> Response {
    if !caller.has_capability(required) {
        record_outcome(caller.class().as_str(), "forbidden");
        return capability_failure(&caller, required);
    }

    let route = matched_route(&request);
    let thread_key = crate::routes::session_thread_key_from_request(&request);

    if let Some(prefixes) = caller.platform_prefixes()
        && route.starts_with("/api/session/")
        && let Some(thread_key) = thread_key.clone()
        && !crate::routes::thread_key_matches_platform(prefixes, thread_key.as_str())
    {
        record_outcome(caller.class().as_str(), "forbidden");
        return ApiError::Forbidden("caller is not authorized for this session".to_owned())
            .into_response();
    }
    if caller.class() == CallerClass::ApiClient
        && let Some(client) = request
            .extensions()
            .get::<crate::api_clients::ApiClientContext>()
        && !crate::routes::api_client_may_touch(client, &route, thread_key)
    {
        record_outcome("api_client", "forbidden");
        return ApiError::Forbidden("caller is not authorized for this session".to_owned())
            .into_response();
    }

    record_outcome(caller.class().as_str(), "authorized");
    if let Some(user) = caller.console_user().cloned() {
        request.extensions_mut().insert(user);
    }
    if let Some(session) = caller.console_session().cloned() {
        request.extensions_mut().insert(session);
    }
    if let Some(key_id) = caller.bearer_api_key() {
        let owner = caller.console_user().cloned().expect("bearer owner");
        request.extensions_mut().insert(BearerUser(owner));
        request.extensions_mut().insert(BearerApiKey(key_id));
    }
    if caller.is_sandbox_delegated() {
        request.extensions_mut().insert(SandboxDelegated);
    }
    if let Some(binding) = caller.sandbox().cloned() {
        request.extensions_mut().insert(SandboxContext {
            proxy: binding.proxy,
            claims: binding.claims,
        });
    }
    request.extensions_mut().insert(caller);
    next.run(request).await
}

fn capability_failure(caller: &AuthenticatedCaller, required: Capability) -> Response {
    match required.surface() {
        Surface::Console => {
            // A caller without any Console capability (an API key that is not
            // an operator credential for this route, or a sandbox token that is
            // not delegated) keeps Console's rejection.
            match caller.console_user() {
                Some(user) if user.status == ConsoleUserStatus::Pending => {
                    authorization_failure(Surface::Console, AuthorizationFailure::AccountPending)
                }
                Some(user) if user.status == ConsoleUserStatus::Active && !user.admin => {
                    authorization_failure(Surface::Console, AuthorizationFailure::AdminRequired)
                }
                _ => {
                    authorization_failure(Surface::Console, AuthorizationFailure::SessionForbidden)
                }
            }
        }
        Surface::Bearer => {
            ApiError::Forbidden("caller is not authorized for this operator route".to_owned())
                .into_response()
        }
        Surface::Sandbox => ConsoleError::new(
            StatusCode::FORBIDDEN,
            "sandbox token is not authorized for this route",
        )
        .into_response(),
        Surface::ApiRs => ApiError::Forbidden("caller is not authorized for this route".to_owned())
            .into_response(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationFailure {
    /// No credential at all, or one that did not verify.
    NoCredential,
    /// A sandbox JWT whose signature/claims do not verify.
    SandboxInvalidOrMissing,
    /// A sandbox JWT that verifies but whose live binding failed.
    SandboxInvalid,
    /// A Console API key owner who is not an admin.
    BearerNotAdmin,
    /// A Console cookie for a disabled account.
    AccountDisabled,
    /// A Console cookie for a pending account.
    AccountPending,
    /// An unsafe Console cookie request without the exact Origin.
    Csrf,
    /// A Console caller that does not hold the route's admin level.
    AdminRequired,
    /// An Authorization header that is not an accepted Console credential.
    SessionForbidden,
}

fn authorization_failure(surface: Surface, failure: AuthorizationFailure) -> Response {
    match surface {
        Surface::Console => console_failure(failure),
        Surface::Bearer => match failure {
            AuthorizationFailure::BearerNotAdmin => {
                ConsoleError::new(StatusCode::FORBIDDEN, "API key owner is not an admin")
                    .into_response()
            }
            _ => ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid or missing API key")
                .into_response(),
        },
        Surface::Sandbox => match failure {
            AuthorizationFailure::SandboxInvalid => {
                ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid sandbox token").into_response()
            }
            _ => ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid or missing sandbox token")
                .into_response(),
        },
        Surface::ApiRs => match failure {
            AuthorizationFailure::BearerNotAdmin => {
                ApiError::Forbidden("API key owner is not an admin".to_owned()).into_response()
            }
            _ => ApiError::Unauthorized("missing bearer token".to_owned()).into_response(),
        },
    }
}

fn unauthenticated(surface: Surface) -> Response {
    match surface {
        Surface::ApiRs => ApiError::Unauthorized("missing bearer token".to_owned()).into_response(),
        Surface::Console => ConsoleError::new(StatusCode::UNAUTHORIZED, "console session required")
            .with_reason("session_required")
            .into_response(),
        Surface::Bearer => {
            ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid or missing API key")
                .into_response()
        }
        Surface::Sandbox => {
            ConsoleError::new(StatusCode::UNAUTHORIZED, "invalid or missing sandbox token")
                .into_response()
        }
    }
}

fn console_failure(failure: AuthorizationFailure) -> Response {
    let (status, message, reason) = match failure {
        AuthorizationFailure::NoCredential => (
            StatusCode::UNAUTHORIZED,
            "console session required",
            "session_required",
        ),
        AuthorizationFailure::SandboxInvalidOrMissing | AuthorizationFailure::SandboxInvalid => (
            StatusCode::FORBIDDEN,
            "the console API requires a console session; API keys are not accepted \
             (the bearer API is /api/v1 outside /api/v1/console)",
            "session_required",
        ),
        AuthorizationFailure::SessionForbidden => (
            StatusCode::FORBIDDEN,
            "the console API requires a console session; API keys are not accepted \
             (the bearer API is /api/v1 outside /api/v1/console)",
            "session_required",
        ),
        AuthorizationFailure::BearerNotAdmin => (
            StatusCode::FORBIDDEN,
            "the console API requires a console session; API keys are not accepted \
             (the bearer API is /api/v1 outside /api/v1/console)",
            "session_required",
        ),
        AuthorizationFailure::AccountDisabled => (
            StatusCode::UNAUTHORIZED,
            "account disabled",
            "account_disabled",
        ),
        AuthorizationFailure::AccountPending => (
            StatusCode::FORBIDDEN,
            "account pending approval",
            "account_pending",
        ),
        AuthorizationFailure::Csrf => (
            StatusCode::FORBIDDEN,
            "invalid or missing CSRF token",
            "csrf",
        ),
        AuthorizationFailure::AdminRequired => (
            StatusCode::FORBIDDEN,
            "admin access required",
            "admin_required",
        ),
    };
    ConsoleError::new(status, message)
        .with_reason(reason)
        .into_response()
}

fn authorization_present(headers: &HeaderMap) -> bool {
    headers.get(AUTHORIZATION).is_some_and(|value| {
        value
            .to_str()
            .map_or(true, |value| !value.trim().is_empty())
    })
}

fn origin_matches(headers: &HeaderMap, config: &ConsoleSessionConfig) -> bool {
    request_origin(headers) == Some(config.public_origin())
}

fn matched_route<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| "__unmatched__".to_owned())
}

fn record_outcome(class: &'static str, outcome: &'static str) {
    centaur_telemetry::record_api_authentication(class, outcome);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(status: ConsoleUserStatus, admin: bool) -> ConsoleUser {
        ConsoleUser {
            id: 42,
            email: "user@example.test".to_owned(),
            name: Some("User".to_owned()),
            admin,
            status,
        }
    }

    #[test]
    fn console_capabilities_follow_the_account_level() {
        let admin = console_capabilities(&user(ConsoleUserStatus::Active, true));
        assert!(admin.contains(&Capability::ConsoleAdmin));
        assert!(admin.contains(&Capability::ConsoleUser));
        assert!(admin.contains(&Capability::ConsoleRead));

        let active = console_capabilities(&user(ConsoleUserStatus::Active, false));
        assert!(!active.contains(&Capability::ConsoleAdmin));
        assert!(active.contains(&Capability::ConsoleUser));

        let pending = console_capabilities(&user(ConsoleUserStatus::Pending, false));
        assert!(!pending.contains(&Capability::ConsoleUser));
        assert!(pending.contains(&Capability::ConsolePending));
    }

    #[test]
    fn console_levels_imply_weaker_levels_but_not_the_reverse() {
        assert!(Capability::ConsoleAdmin.satisfies(Capability::ConsoleUser));
        assert!(Capability::ConsoleAdmin.satisfies(Capability::ConsolePending));
        assert!(Capability::ConsoleAdmin.satisfies(Capability::ConsoleRead));
        assert!(Capability::ConsoleUser.satisfies(Capability::ConsolePending));
        assert!(!Capability::ConsolePending.satisfies(Capability::ConsoleUser));
        assert!(!Capability::ConsoleUser.satisfies(Capability::ConsoleAdmin));
        assert!(Capability::ConsoleDelegate.satisfies(Capability::ConsoleAdmin));
        assert!(!Capability::OperatorApi.satisfies(Capability::ConsoleAdmin));
    }

    #[test]
    fn capability_names_round_trip_and_cover_the_catalog() {
        let catalog = [
            Capability::SessionsRead,
            Capability::SessionsWrite,
            Capability::SandboxesDrain,
            Capability::WorkflowsRead,
            Capability::WorkflowsWrite,
            Capability::WorkflowsEvents,
            Capability::WorkflowsActions,
            Capability::SlackProxy,
            Capability::SlackExternalTeamsRead,
            Capability::SlackExternalTeamsWrite,
            Capability::ConsoleRead,
            Capability::ConsolePending,
            Capability::ConsoleUser,
            Capability::ConsoleAdmin,
            Capability::OperatorApi,
            Capability::SandboxPermissions,
            Capability::SandboxSkills,
            Capability::SandboxTasks,
            Capability::SandboxOauthApps,
            Capability::ConsoleDelegate,
        ];
        for capability in catalog {
            assert_eq!(
                Capability::from_str(capability.as_str()),
                Some(capability),
                "{}",
                capability.as_str()
            );
        }
        assert_eq!(Capability::from_str("unknown"), None);
    }

    #[test]
    fn api_rs_service_credentials_do_not_gain_console_or_operator_access() {
        for capability in Capability::API_RS {
            assert!(!matches!(
                capability.surface(),
                Surface::Console | Surface::Bearer | Surface::Sandbox
            ));
        }
        assert!(!Capability::API_RS.contains(&Capability::OperatorApi));
        assert!(!Capability::API_RS.contains(&Capability::SlackProxy));
    }

    #[test]
    fn surfaces_follow_the_required_capability() {
        assert_eq!(Capability::ConsoleAdmin.surface(), Surface::Console);
        assert_eq!(Capability::OperatorApi.surface(), Surface::Bearer);
        assert_eq!(Capability::SandboxSkills.surface(), Surface::Sandbox);
        assert_eq!(Capability::SessionsRead.surface(), Surface::ApiRs);
    }
}
