//! Console area: browser-facing authentication, PWA, and SPA routes.
//!
//! This ports the browser surface around the JSON console API: SSO sign-in,
//! the MCP OAuth authorization server's metadata/registration endpoints, the
//! server-rendered consent page, the PWA manifest and service worker, the
//! protocol launch handler, the health endpoint, and the SPA shell fallback.
//! The form and redirect contracts are shared with Rails during a rolling
//! deploy, so fields, messages, and paths must stay byte-compatible. Rails'
//! format-suffix route aliases are not ported: only the two paths a page or
//! client actually requests, `/manifest.json` (the SPA's manifest link) and
//! `/service-worker.js` (the SPA's service-worker registration), are routed.

mod mcp_oauth;
mod sso;

use std::{env, path::PathBuf};

use crate::{
    AppState,
    console::{
        guard::GuardedRoutes,
        http::{ConsoleError, json_not_found},
        return_path,
        session_cookie::ConsoleSession,
    },
    console_session::ConsoleSessionConfig,
};
use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, LOCATION, SET_COOKIE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use centaur_console_db::ConsoleUserStatus;
use serde::Deserialize;

use crate::console::request::ConsoleQuery;

/// `ServerRenderedPagePolicy`: the app-wide policy narrowed for pages with no
/// JavaScript and no inline styles. No `form-action`, because the MCP consent
/// flow continues into an arbitrary registered client redirect URI.
pub(super) const SERVER_PAGE_CSP: &str = "default-src 'self'; script-src 'none'; style-src 'self'; \
     img-src 'self' data:; font-src 'self'; connect-src 'self'; worker-src 'self'; \
     manifest-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// Rails' app-wide policy for the SPA shell.
const APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     style-src-elem 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; \
     connect-src 'self'; worker-src 'self'; manifest-src 'self'; object-src 'none'; \
     base-uri 'none'; frame-ancestors 'none'";

const NOT_BUILT_MESSAGE: &str = "The console web app has not been built.\n\
Run `pnpm -C services/console/web build`, or use the Vite dev server\n\
(`pnpm -C services/console/web dev`) during development.\n";

/// `Lib::SpaRequest::EXCLUDED_PREFIXES`: protocol, API, and login namespaces
/// keep the JSON 404 instead of the SPA.
const NON_SPA_PREFIXES: &[&str] = &[
    "/api",
    "/rails",
    "/spa",
    "/assets",
    "/.well-known",
    "/up",
    "/mcp",
    "/oauth",
    "/auth",
    "/login",
    "/logout",
    "/pending",
];

const MANIFEST_JSON: &str =
    include_str!("../../../../../services/console/config/pwa/manifest.json");
const SERVICE_WORKER_JS: &str =
    include_str!("../../../../../services/console/config/pwa/service-worker.js");

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
            get(sso::start).fallback(not_found_for_method),
        )
        .route(
            "/auth/{provider}/callback",
            get(sso::callback).fallback(not_found_for_method),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(mcp_oauth::metadata).fallback(not_found_for_method),
        )
        .route(
            "/.well-known/openid-configuration",
            get(mcp_oauth::metadata).fallback(not_found_for_method),
        )
        .route(
            "/mcp/oauth/register",
            axum::routing::post(mcp_oauth::register).fallback(not_found_for_method),
        )
        .route(
            "/mcp/oauth/authorize",
            get(mcp_oauth::authorize_page)
                .post(mcp_oauth::approve)
                .fallback(not_found_for_method),
        )
        .route(
            "/mcp/oauth/token",
            axum::routing::post(mcp_oauth::token).fallback(not_found_for_method),
        )
        .route("/manifest", get(manifest).fallback(not_found_for_method))
        // `services/console/web/index.html` links `/manifest.json`, so that one
        // Rails format suffix is part of the SPA's contract.
        .route(
            "/manifest.json",
            get(manifest).fallback(not_found_for_method),
        )
        .route(
            "/service-worker",
            get(service_worker).fallback(not_found_for_method),
        )
        // `services/console/web/src/features/auth-pages/pwa.ts` registers
        // `/service-worker.js`.
        .route(
            "/service-worker.js",
            get(service_worker).fallback(not_found_for_method),
        )
        .route("/launch", get(launch).fallback(not_found_for_method))
        .route("/", get(spa_shell).fallback(not_found_for_method))
        .route(
            "/{*spa_path}",
            get(spa_fallback).fallback(not_found_for_method),
        )
}

async fn not_found_for_method() -> Response {
    json_not_found()
}

async fn manifest(headers: HeaderMap) -> Response {
    let _ = headers;
    let mut response = (StatusCode::OK, MANIFEST_JSON).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/manifest+json; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static(
            "inline; filename=\"manifest.json\"; filename*=UTF-8''manifest.json",
        ),
    );
    response.headers_mut().insert(
        "content-transfer-encoding",
        HeaderValue::from_static("binary"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("content-security-policy", HeaderValue::from_static(APP_CSP));
    response
}

async fn service_worker() -> Response {
    let mut response = (StatusCode::OK, SERVICE_WORKER_JS).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static(
            "inline; filename=\"service-worker.js\"; filename*=UTF-8''service-worker.js",
        ),
    );
    response.headers_mut().insert(
        "content-transfer-encoding",
        HeaderValue::from_static("binary"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("content-security-policy", HeaderValue::from_static(APP_CSP));
    response
}

/// `GET /launch`: route a `web+centaur://` URL into the SPA. The custom-scheme
/// target is reduced to a strictly path-shaped `/console/...` location, so a
/// crafted link can never bounce off-origin or onto a server endpoint.
async fn launch(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsoleQuery(query): ConsoleQuery<LaunchQuery>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let Some(config) = console_config(&state).cloned() else {
        return console_unavailable();
    };
    let full_path = match uri.query() {
        Some(query) if !query.is_empty() => format!("{}?{query}", uri.path()),
        _ => uri.path().to_owned(),
    };
    if let Err(response) =
        require_active_user(&state, &config, &headers, Some(&full_path), APP_CSP).await
    {
        return response;
    }
    redirect(
        &launch_path_for(query.target.as_deref().unwrap_or_default()),
        Vec::new(),
        Some(APP_CSP),
    )
}

#[derive(Debug, Deserialize)]
struct LaunchQuery {
    #[serde(default)]
    target: Option<String>,
}

fn launch_path_for(target: &str) -> String {
    const SCHEME_PREFIX: &str = "web+centaur://";
    let Some(rest) = target.strip_prefix(SCHEME_PREFIX) else {
        return "/".to_owned();
    };
    if rest.is_empty() {
        return "/".to_owned();
    }
    let path = rest
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if !safe_launch_path(&path) {
        return "/".to_owned();
    }
    format!("/{path}")
}

fn safe_launch_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("console") else {
        return false;
    };
    rest.is_empty()
        || (rest.starts_with('/')
            && rest.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '/' | '-')
            }))
}

async fn spa_shell(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    serve_spa(state, headers, uri.path(), uri.query()).await
}

async fn spa_fallback(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    serve_spa(state, headers, uri.path(), uri.query()).await
}

async fn serve_spa(
    state: AppState,
    headers: HeaderMap,
    path: &str,
    query: Option<&str>,
) -> Response {
    if !spa_request(path) {
        return non_spa_not_found(path);
    }
    let Some(config) = state.console_session().cloned() else {
        return console_unavailable();
    };
    let mut session = config
        .session_from_headers(&headers)
        .unwrap_or_else(ConsoleSession::anonymous);
    if session.user_id().is_none() {
        let full_path = match query {
            Some(query) if !query.is_empty() => format!("{path}?{query}"),
            _ => path.to_owned(),
        };
        if let Some(safe) = return_path::safe(&full_path) {
            session.set_return_to(safe);
        }
    }
    let cookie = config.cookie().set_cookie_header(&session);
    match std::fs::read(spa_shell_path()) {
        Ok(body) => {
            let mut response = Response::new(Body::from(body));
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response.headers_mut().insert(
                "content-disposition",
                HeaderValue::from_static(
                    "inline; filename=\"index.html\"; filename*=UTF-8''index.html",
                ),
            );
            response.headers_mut().insert(
                "content-transfer-encoding",
                HeaderValue::from_static("binary"),
            );
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
                .headers_mut()
                .insert("content-security-policy", HeaderValue::from_static(APP_CSP));
            if let Some(cookie) = cookie {
                response.headers_mut().append(SET_COOKIE, cookie);
            }
            response
        }
        Err(_) => {
            let mut response = (StatusCode::SERVICE_UNAVAILABLE, NOT_BUILT_MESSAGE).into_response();
            response.headers_mut().insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
                .headers_mut()
                .insert("content-security-policy", HeaderValue::from_static(APP_CSP));
            if let Some(cookie) = cookie {
                response.headers_mut().append(SET_COOKIE, cookie);
            }
            response
        }
    }
}

/// `Lib::SpaRequest`'s path allowlist. Rails also required `request.format.html?`;
/// the port dropped Accept/MIME negotiation, so a page route is HTML and the SPA
/// shell is selected by path alone. The SPA shell's other rules (only when the
/// build output exists, the return-path cookie, the app CSP) are unchanged.
fn spa_request(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    !NON_SPA_PREFIXES
        .iter()
        .any(|prefix| matches_prefix(path, prefix))
}

fn non_spa_not_found(path: &str) -> Response {
    if path == "/"
        || NON_SPA_PREFIXES
            .iter()
            .any(|prefix| matches_prefix(path, prefix))
        || matches_prefix(path, "/console")
    {
        let mut response = json_not_found();
        response
            .headers_mut()
            .insert("content-security-policy", HeaderValue::from_static(APP_CSP));
        return response;
    }
    // Beyond the Console-owned prefixes the app-wide fallback keeps axum's
    // empty 404 (see `console::fallback`), so an unknown non-HTML path is not
    // Console's JSON body.
    StatusCode::NOT_FOUND.into_response()
}

fn matches_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path.starts_with(&format!("{prefix}/"))
        || path.starts_with(&format!("{prefix}."))
}

fn spa_shell_path() -> PathBuf {
    crate::console::spa_files::spa_shell_path()
}

pub(super) fn console_config(state: &AppState) -> Option<&ConsoleSessionConfig> {
    state.console_session()
}

/// The response every Console page renders while `CONSOLE_SESSION_SECRET` is
/// unset, in Console's envelope like the rest of this area's failures.
pub(super) fn console_unavailable() -> Response {
    ConsoleError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "the console session API is not configured",
    )
    .into_response()
}

pub(super) fn public_base_url(headers: &HeaderMap) -> String {
    if let Some(value) = console_env("PUBLIC_URL").filter(|value| !value.trim().is_empty()) {
        return value.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

pub(super) fn console_env(key: &str) -> Option<String> {
    crate::console::console_env(key)
}

pub(super) fn env_any(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| env::var(key).ok().filter(|value| !value.trim().is_empty()))
}

pub(super) fn redirect(
    location: &str,
    cookies: Vec<HeaderValue>,
    csp: Option<&'static str>,
) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    match HeaderValue::from_str(location) {
        Ok(value) => {
            response.headers_mut().insert(LOCATION, value);
        }
        Err(_) => {
            response
                .headers_mut()
                .insert(LOCATION, HeaderValue::from_static("/login"));
        }
    }
    for cookie in cookies {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    if let Some(csp) = csp {
        response
            .headers_mut()
            .insert("content-security-policy", HeaderValue::from_static(csp));
    }
    response
}

pub(super) fn server_page(mut response: Response) -> Response {
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(SERVER_PAGE_CSP),
    );
    response
}

pub(super) fn html_page(status: StatusCode, html: String) -> Response {
    let mut response = (status, html).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    server_page(response)
}

/// Redirect to `/login?error=<code>`, which the login page maps to the same
/// message text the session flash used to carry.
pub(super) fn login_redirect(
    config: &ConsoleSessionConfig,
    headers: &HeaderMap,
    error: &str,
    extra_cookies: Vec<HeaderValue>,
) -> Response {
    login_redirect_with_csp(config, headers, error, extra_cookies, SERVER_PAGE_CSP)
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
    redirect(&format!("/login?error={error}"), cookies, Some(csp))
}

/// Resolve the signed-in user for the authorization endpoint, applying the same
/// status gate as `ApplicationController`. Returns `Err(response)` when the
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
        return Err(redirect(
            "/login",
            config
                .cookie()
                .set_cookie_header(&anonymous)
                .into_iter()
                .collect(),
            Some(csp),
        ));
    };
    let Some(user_id) = session.user_id() else {
        if let Some(safe) = return_to.and_then(return_path::safe) {
            session.set_return_to(safe);
        }
        return Err(redirect(
            "/login",
            config
                .cookie()
                .set_cookie_header(&session)
                .into_iter()
                .collect(),
            Some(csp),
        ));
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
            return Err(redirect(
                "/login",
                config
                    .cookie()
                    .set_cookie_header(&session)
                    .into_iter()
                    .collect(),
                Some(csp),
            ));
        }
        Err(error) => return Err(ConsoleError::internal(error).into_response()),
    };
    match user.status {
        ConsoleUserStatus::Active => Ok(user),
        ConsoleUserStatus::Pending => Err(redirect(
            "/pending",
            config
                .cookie()
                .set_cookie_header(&session)
                .into_iter()
                .collect(),
            Some(csp),
        )),
        ConsoleUserStatus::Disabled => {
            let signed_out = ConsoleSession::anonymous();
            Err(redirect(
                &format!(
                    "/login?error={}",
                    crate::console::pages::login_error::ACCOUNT_DISABLED
                ),
                config
                    .cookie()
                    .set_cookie_header(&signed_out)
                    .into_iter()
                    .collect(),
                Some(csp),
            ))
        }
    }
}

pub(super) fn callback_url(base: &str, provider: &str) -> String {
    absolute_url(base, &format!("/auth/{provider}/callback"))
}

/// `URI.join(base, path).to_s`: an absolute path replaces any base path.
pub(super) fn absolute_url(base: &str, path: &str) -> String {
    let Ok(mut url) = url::Url::parse(base) else {
        return format!("{}{}", base.trim_end_matches('/'), path);
    };
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}
