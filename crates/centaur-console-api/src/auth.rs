//! Credentials per namespace.
//!
//! Each namespace (see [`super::routes`]) is mounted under its prefix with
//! exactly one guard layer, applied in [`super::app`], never by an area:
//!
//! | namespace | prefix | guard inserts |
//! | --- | --- | --- |
//! | bearer | `/api/v1` | [`BearerCaller`] |
//! | console | `/api/v1/console` | [`ConsoleCaller`] (session user; level per router) |
//! | sandbox | `/api/v1/sandbox` | [`SandboxCaller`] |
//! | sandbox claims | `/api/v1/sandbox` | [`SandboxClaimsCaller`] (token only, no DB) |
//! | sandbox admin | `/api/v1/sandbox/admin` | [`SandboxAdminCaller`] |
//! | internal | `/internal` | [`InternalCaller`] |
//! | public | none | nothing; pages use [`Session`] |
//!
//! A handler that takes one of those extractors but is mounted in another
//! namespace finds no credential and answers 401 `credential_namespace_mismatch`
//! instead of running.

use std::sync::{Arc, Mutex, MutexGuard};

use axum::{
    extract::{FromRequestParts, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use centaur_console_auth::denial::DISABLED_MESSAGE;
use centaur_console_auth::{
    AuthError, AuthUser, Denial,
    bearer::{self, BearerAuth},
    csrf::RequestOrigin,
    internal,
    sandbox::{self, SandboxAdminAuth, SandboxAuth, SandboxClaims},
    session::Session as SessionData,
};
use http::{HeaderMap, Method, StatusCode, header, request::Parts};

use super::{error::ApiError, state::AppState};

/// Who may call a console route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Active admins (the default).
    Admin,
    /// Any active account. Scope what you load to the caller.
    Active,
    /// Pending accounts too (`/me`, sign-out).
    Pending,
}

// --- the session ----------------------------------------------------------

/// The request's session. Lock it briefly; never hold the guard across an
/// `.await`.
#[derive(Clone)]
pub struct Session(Arc<Mutex<SessionData>>);

impl Session {
    pub fn lock(&self) -> MutexGuard<'_, SessionData> {
        self.0.lock().expect("session lock")
    }
}

impl FromRequestParts<AppState> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .ok_or_else(|| ApiError::internal("handler mounted without the session layer"))
    }
}

/// Whether the request reached us over HTTPS (directly configured, or
/// terminated by a proxy that says so).
pub fn is_https(state: &AppState, headers: &HeaderMap) -> bool {
    state.config.https_public_url()
        || headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

/// The browser-visible base URL of this request: `PUBLIC_URL`, else the
/// request's own origin.
pub fn request_base(state: &AppState, headers: &HeaderMap) -> String {
    let host = request_host(state, headers).unwrap_or("localhost");
    let scheme = if is_https(state, headers) {
        "https"
    } else {
        "http"
    };
    state.public_base_url(&format!("{scheme}://{host}"))
}

/// The request's host: `X-Forwarded-Host` only when the proxy's host is one
/// the console answers to, else the `Host` header.
fn request_host<'a>(state: &AppState, headers: &'a HeaderMap) -> Option<&'a str> {
    let forwarded = header_str(headers, "x-forwarded-host").filter(|host| state.hosts.allows(host));
    forwarded.or_else(|| header_str(headers, "host"))
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Loads the session from its cookie and writes it back when it changed.
pub async fn session_layer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let now = state.now();
    let cookie = header_str(request.headers(), "cookie");
    let session = Session(Arc::new(Mutex::new(state.sessions.load(cookie, now))));
    let secure = is_https(&state, request.headers());
    request.extensions_mut().insert(session.clone());
    let mut response = next.run(request).await;
    let set_cookie = state.sessions.set_cookie(&session.lock(), secure, now);
    if let Some(value) = set_cookie.and_then(|value| value.parse().ok()) {
        response.headers_mut().append(header::SET_COOKIE, value);
        // A response that sets the session must not be shared.
        response
            .headers_mut()
            .entry(header::CACHE_CONTROL)
            .or_insert(header::HeaderValue::from_static("no-store"));
    }
    response
}

/// The signed-in user of a page request, if any. A disabled user's session
/// is cleared (with an explanation for the sign-in page).
pub async fn session_user(
    state: &AppState,
    session: &Session,
) -> Result<Option<AuthUser>, ApiError> {
    let Some(id) = session.lock().user_id() else {
        return Ok(None);
    };
    let user = AuthUser::find(&state.db, id).await?;
    match user {
        Some(user) if user.is_disabled() => {
            let mut session = session.lock();
            session.sign_out(state.now());
            session.set_alert(DISABLED_MESSAGE);
            Ok(None)
        }
        other => Ok(other),
    }
}

/// Form-post protection for server-rendered pages: same-origin plus the
/// form's `authenticity_token` (or `X-CSRF-Token`) against the session.
pub fn verify_form_csrf(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
    token: Option<&str>,
) -> Result<(), ApiError> {
    verify_origin(state, headers)?;
    let presented = token.or_else(|| header_str(headers, "x-csrf-token"));
    if session.lock().csrf_matches(presented) {
        Ok(())
    } else {
        Err(Denial::CSRF.into())
    }
}

fn verify_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = RequestOrigin {
        origin: header_str(headers, "origin"),
        host: request_host(state, headers),
        public_url: state.config.public_url.as_deref(),
    };
    if origin.same_origin() {
        Ok(())
    } else {
        Err(Denial::CSRF.into())
    }
}

// --- console ----------------------------------------------------------------

/// The signed-in console user (see [`Access`]).
#[derive(Clone, Debug)]
pub struct ConsoleCaller(pub AuthUser);

/// The guard of `/api/v1/console/*`, in this order: no `Authorization`
/// header (401/403 `session_required`), a session user, a disabled user is
/// signed out (401), pending users only where allowed (403), writes need a
/// same-origin request with the session's CSRF token in `X-CSRF-Token`
/// (403 `csrf`), admin-only routes need an admin (403).
pub async fn console_guard(
    State((state, access)): State<(AppState, Access)>,
    mut request: Request,
    next: Next,
) -> Response {
    let session = request.extensions().get::<Session>().cloned();
    let (headers, method) = (request.headers().clone(), request.method().clone());
    match authenticate_console(&state, session, &headers, &method, access).await {
        Ok(user) => {
            request.extensions_mut().insert(ConsoleCaller(user));
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

async fn authenticate_console(
    state: &AppState,
    session: Option<Session>,
    headers: &HeaderMap,
    method: &Method,
    access: Access,
) -> Result<AuthUser, ApiError> {
    let session = session
        .ok_or_else(|| ApiError::internal("console routes mounted without the session layer"))?;
    if bearer::authorization_present(headers) {
        return Err(Denial::BEARER_ON_CONSOLE.into());
    }
    let Some(user_id) = session.lock().user_id() else {
        return Err(Denial::SESSION_REQUIRED.into());
    };
    let Some(user) = AuthUser::find(&state.db, user_id).await? else {
        return Err(Denial::SESSION_REQUIRED.into());
    };
    if user.is_disabled() {
        let mut session = session.lock();
        session.sign_out(state.now());
        session.set_alert(DISABLED_MESSAGE);
        return Err(Denial::ACCOUNT_DISABLED.into());
    }
    if access != Access::Pending && !user.is_active() {
        return Err(Denial::ACCOUNT_PENDING.into());
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        verify_origin(state, headers)?;
        if !session
            .lock()
            .csrf_matches(header_str(headers, "x-csrf-token"))
        {
            return Err(Denial::CSRF.into());
        }
    }
    if access == Access::Admin && !user.admin {
        return Err(Denial::ADMIN_REQUIRED.into());
    }
    Ok(user)
}

// --- bearer, sandbox, internal ---------------------------------------------

#[derive(Clone, Debug)]
pub struct BearerCaller(pub BearerAuth);
#[derive(Clone, Debug)]
pub struct SandboxCaller(pub SandboxAuth);
#[derive(Clone, Debug)]
pub struct SandboxClaimsCaller(pub SandboxClaims);
#[derive(Clone, Debug)]
pub struct SandboxAdminCaller(pub SandboxAdminAuth);
/// An api-rs service call on `/internal/*`.
#[derive(Clone, Copy, Debug)]
pub struct InternalCaller;

macro_rules! caller_extractor {
    ($($caller:ty),*) => {$(
        impl FromRequestParts<AppState> for $caller {
            type Rejection = ApiError;

            async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, ApiError> {
                parts
                    .extensions
                    .get::<$caller>()
                    .cloned()
                    .ok_or_else(|| Denial::NAMESPACE_MISMATCH.into())
            }
        }
    )*};
}
caller_extractor!(
    ConsoleCaller,
    BearerCaller,
    SandboxCaller,
    SandboxClaimsCaller,
    SandboxAdminCaller,
    InternalCaller
);

fn auth_error(error: AuthError) -> ApiError {
    match error {
        AuthError::NotConfigured(error) => ApiError::internal(error),
        AuthError::Database(error) => error.into(),
    }
}

pub async fn bearer_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    match bearer::authenticate(&state.db, request.headers()).await {
        Ok(Ok(auth)) => {
            request.extensions_mut().insert(BearerCaller(auth));
            next.run(request).await
        }
        Ok(Err(denial)) => ApiError::from(denial).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

pub async fn sandbox_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let result = sandbox::authenticate(
        &state.db,
        request.headers(),
        &state.config.jwt_signing_secret,
        &state.config.sandbox_jwt,
        state.now(),
    )
    .await;
    match result {
        Ok(Ok(auth)) => {
            request.extensions_mut().insert(SandboxCaller(auth));
            next.run(request).await
        }
        Ok(Err(denial)) => ApiError::from(denial).into_response(),
        Err(error) => auth_error(error).into_response(),
    }
}

/// Token-only sandbox authentication (no database binding).
pub async fn sandbox_claims_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = bearer::bearer_token(request.headers()) else {
        return ApiError::from(Denial::SANDBOX_TOKEN_MISSING).into_response();
    };
    let verified = sandbox::verify_token(
        token,
        &state.config.jwt_signing_secret,
        &state.config.sandbox_jwt,
        state.now(),
    );
    match verified {
        Ok(Some(claims)) => {
            request.extensions_mut().insert(SandboxClaimsCaller(claims));
            next.run(request).await
        }
        Ok(None) => ApiError::from(Denial::SANDBOX_TOKEN_MISSING).into_response(),
        Err(error) => ApiError::internal(error).into_response(),
    }
}

pub async fn sandbox_admin_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let result = sandbox::authenticate_admin(
        &state.db,
        request.headers(),
        &state.config.jwt_signing_secret,
        &state.config.sandbox_jwt,
        state.now(),
    )
    .await;
    match result {
        Ok(Ok(auth)) => {
            tracing::info!(
                admin = auth.admin.id,
                principal = auth.sandbox.principal_id,
                method = %request.method(),
                path = request.uri().path(),
                "sandbox_admin_request"
            );
            request.extensions_mut().insert(SandboxAdminCaller(auth));
            next.run(request).await
        }
        Ok(Err(denial)) => ApiError::from(denial).into_response(),
        Err(error) => auth_error(error).into_response(),
    }
}

/// `/internal/*`: api-rs service tokens. Error bodies are flat,
/// `{"error":"<message>"}` (RFC 0006a).
pub async fn internal_guard(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let valid = bearer::bearer_token(request.headers()).is_some_and(|token| {
        internal::verify(token, &state.config.jwt_signing_secret, state.now())
    });
    if valid {
        request.extensions_mut().insert(InternalCaller);
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({ "error": Denial::SERVICE_TOKEN_INVALID.message })),
    )
        .into_response()
}
