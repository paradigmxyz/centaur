//! The server-rendered Console sign-in pages.
//!
//! They intentionally have no JavaScript: sign-in is a step of the MCP OAuth
//! flow. The form field contract (`email`, `password`; the pending page's
//! `POST /logout`) is unchanged, but the forms carry no authenticity token and
//! there is no `_method` override: every unsafe request must name the Console
//! public origin exactly, and a missing or `null` Origin is refused. Forms and
//! query parameters are parsed into typed structs, so a nested `return_to[]` key
//! is simply an unknown field.
//!
//! SSO buttons render from the same enabled-provider set the SSO routes use;
//! see `src/console/README.md`.

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION, SET_COOKIE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use bcrypt::verify;
use centaur_console_db::{
    ConsoleUser, ConsoleUserStatus, find_console_password_user_by_email, find_console_user,
};
use serde::Deserialize;

use crate::{
    AppState,
    console::{
        DISABLED_MESSAGE, LOGIN_PATH, PENDING_PATH,
        areas::auth_pages::console_config,
        assets,
        http::{ConsoleError, request_origin},
        request::{ConsoleForm, ConsoleQuery},
        return_path,
        session_cookie::ConsoleSession,
    },
    console_session::ConsoleSessionConfig,
};

const DISABLED_PASSWORD_MESSAGE: &str = "Email and password sign in is disabled.";
const INVALID_CREDENTIALS_MESSAGE: &str = "Invalid email or password.";

/// Codes carried by `GET /login?error=<code>` after a redirect. The login page
/// maps each to the message text it used to carry through the session flash, so
/// server-rendered error pages still work without a mutable server-side flash.
pub(crate) mod login_error {
    pub(crate) const ACCOUNT_DISABLED: &str = "account_disabled";
    pub(crate) const SESSION_EXPIRED: &str = "session_expired";
    pub(crate) const SSO_UNAVAILABLE: &str = "sso_unavailable";
    pub(crate) const SSO_START_FAILED: &str = "sso_start_failed";
    pub(crate) const SSO_INVALID_FLOW: &str = "sso_invalid_flow";
    pub(crate) const SSO_CANCELED: &str = "sso_canceled";
    pub(crate) const SSO_EXCHANGE_FAILED: &str = "sso_exchange_failed";
    pub(crate) const SSO_DOMAIN_NOT_ALLOWED: &str = "sso_domain_not_allowed";
    pub(crate) const SSO_PROVISION_FAILED: &str = "sso_provision_failed";

    pub(crate) fn message(code: &str) -> Option<&'static str> {
        Some(match code {
            ACCOUNT_DISABLED => "Your account is disabled.",
            SESSION_EXPIRED => "Please sign in again.",
            SSO_UNAVAILABLE => "That sign-in method is not available.",
            SSO_START_FAILED => "Sign in is temporarily unavailable. Please try again.",
            SSO_INVALID_FLOW => "This sign-in link is invalid or expired. Start again.",
            SSO_CANCELED => "Sign in was canceled.",
            SSO_EXCHANGE_FAILED => "Sign in failed. Please try again.",
            SSO_DOMAIN_NOT_ALLOWED => "That email domain is not allowed to access the console.",
            SSO_PROVISION_FAILED => "Sign in failed while setting up your account.",
            _ => return None,
        })
    }
}
/// `ServerRenderedPagePolicy`: the app-wide policy narrowed for pages with no
/// JavaScript and no inline styles. No `form-action`, because the login flow
/// continues into MCP consent and its redirect to an arbitrary client URI.
pub(crate) const SERVER_PAGE_CSP: &str = "default-src 'self'; script-src 'none'; style-src 'self'; \
     img-src 'self' data:; font-src 'self'; connect-src 'self'; worker-src 'self'; \
     manifest-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login_page).post(login_submit))
        .route("/pending", get(pending_page))
        .route("/logout", axum::routing::post(logout))
        .route("/icon.svg", get(icon))
}

/// `GET /login` query parameters. A nested `return_to[]` is an unknown field,
/// so `return_to` is absent (there is no array sentinel).
#[derive(Debug, Default, Deserialize)]
struct LoginQuery {
    #[serde(default)]
    return_to: Option<String>,
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// `POST /login` form fields; any other field is ignored.
#[derive(Debug, Default, Deserialize)]
struct LoginForm {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

async fn login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsoleQuery(query): ConsoleQuery<LoginQuery>,
) -> Response {
    let config = console_config(&state).clone();
    let (mut session, current) = match current(&state, &config, &headers).await {
        Ok(current) => current,
        Err(response) => return response,
    };
    let return_to = present(query.return_to);
    let next = present(query.next);

    let query_return_to = return_to.as_deref().or(next.as_deref());
    if let Some(user) = current
        .as_ref()
        .filter(|user| user.status == ConsoleUserStatus::Active)
    {
        let location = query_return_to
            .and_then(return_path::safe)
            .unwrap_or_else(|| landing_path(user));
        return redirect_with_session(&location, &config, &session);
    }
    if current.is_none() {
        // A cookie for a deleted user still keeps its remembered return path,
        // exactly like an absent current user; the id is only replaced on
        // the next successful sign-in.
        // `new` remembers only `?return_to=`; `?next=` is used just for the
        // redirect of an already signed-in active user.
        if let Some(raw) = return_to.as_deref()
            && let Some(safe) = return_path::safe(raw)
        {
            session.set_return_to(safe);
        }
    }

    let alert = query.error.as_deref().and_then(login_error::message);
    render_login(&config, &session, alert, StatusCode::OK)
}

/// The cookie -> user id -> database-user ladder three page handlers share.
#[allow(clippy::result_large_err)]
async fn current(
    state: &AppState,
    config: &ConsoleSessionConfig,
    headers: &HeaderMap,
) -> Result<(ConsoleSession, Option<ConsoleUser>), Response> {
    let session = config
        .session_from_headers(headers)
        .unwrap_or_else(ConsoleSession::anonymous);
    let user = match session.user_id() {
        Some(user_id) => {
            let pool = state
                .console_session_db()
                .map_err(|error| error.into_response())?;
            find_console_user(&pool, user_id)
                .await
                .map_err(|error| ConsoleError::internal(error).into_response())?
        }
        None => None,
    };
    Ok((session, user))
}

/// A blank-string filter for an optional query value.
fn present(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

async fn pending_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let config = console_config(&state).clone();
    let (session, user) = match current(&state, &config, &headers).await {
        Ok(current) => current,
        Err(response) => return response,
    };
    if session.user_id().is_none() {
        return redirect(LOGIN_PATH, &[], Some(SERVER_PAGE_CSP));
    }
    let Some(user) = user else {
        return redirect(LOGIN_PATH, &[], Some(SERVER_PAGE_CSP));
    };
    match user.status {
        ConsoleUserStatus::Active => redirect_with_session(&landing_path(&user), &config, &session),
        // The holding page is also what a disabled session sees; the API guard
        // is what signs it out.
        ConsoleUserStatus::Pending | ConsoleUserStatus::Disabled => {
            let html = pending_html(&user.email);
            page_response(StatusCode::OK, html, &config, &session)
        }
    }
}

async fn login_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsoleForm(form): ConsoleForm<LoginForm>,
) -> Response {
    let config = console_config(&state).clone();
    let session = config
        .session_from_headers(&headers)
        .unwrap_or_else(ConsoleSession::anonymous);
    if !origin_allowed(&headers, &config) {
        return csrf_rejection();
    }

    if !crate::console::password_login_enabled() {
        return render_login(
            &config,
            &session,
            Some(DISABLED_PASSWORD_MESSAGE),
            StatusCode::NOT_FOUND,
        );
    }

    let email = form
        .email
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let password = form.password.as_deref().unwrap_or_default();
    let pool = match state.console_session_db() {
        Ok(pool) => pool,
        Err(error) => return error.into_response(),
    };
    let user = match find_console_password_user_by_email(&pool, &email).await {
        Ok(user) => user,
        Err(error) => return crate::ApiError::Internal(error.to_string()).into_response(),
    };
    let authenticated = user.as_ref().is_some_and(|(_, digest)| {
        digest
            .as_deref()
            .is_some_and(|digest| verify(password, digest).unwrap_or(false))
    });
    let Some((user, _)) = user.filter(|_| authenticated) else {
        return render_login(
            &config,
            &session,
            Some(INVALID_CREDENTIALS_MESSAGE),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    };

    if user.status == ConsoleUserStatus::Disabled {
        return render_login(
            &config,
            &session,
            Some(DISABLED_MESSAGE),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }

    let return_to = session.return_to().and_then(return_path::safe);
    let mut next = ConsoleSession::signed_in(user.id);
    let location = if user.status == ConsoleUserStatus::Active {
        return_to.unwrap_or_else(|| landing_path(&user))
    } else {
        if let Some(return_to) = return_to {
            next.set_return_to(return_to);
        }
        PENDING_PATH.to_owned()
    };
    redirect_with_session(&location, &config, &next)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let config = console_config(&state).clone();
    let (session, user) = match current(&state, &config, &headers).await {
        Ok(current) => current,
        Err(response) => return response,
    };
    // Logout keeps the login gate: without a signed-in user (including a stale
    // cookie for a deleted user) the request redirects instead of rejecting the
    // missing Origin.
    if session.user_id().is_none() || user.is_none() {
        return redirect(LOGIN_PATH, &[], Some(SERVER_PAGE_CSP));
    }
    // The login gate resolves the current user from the database before the
    // origin check runs, so a cookie for a deleted user redirects even without
    // an Origin.
    if !origin_allowed(&headers, &config) {
        return csrf_rejection();
    }
    logout_with_session(&config)
}

fn logout_with_session(config: &ConsoleSessionConfig) -> Response {
    redirect_with_session(LOGIN_PATH, config, &ConsoleSession::anonymous())
}

async fn icon() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "image/svg+xml")
        .header(CACHE_CONTROL, assets::PUBLIC_ASSET_CACHE_CONTROL)
        .body(Body::from(
            &include_bytes!("../../../../services/console/public/icon.svg")[..],
        ))
        .expect("valid icon response")
}

/// Every server-rendered form post must carry the configured Console public
/// origin verbatim. A missing Origin and `Origin: null` are both refused.
fn origin_allowed(headers: &HeaderMap, config: &ConsoleSessionConfig) -> bool {
    request_origin(headers) == Some(config.public_origin())
}

/// A sign-in or sign-out form post without the Console origin. The JSON console
/// API answers the same failure with 403 and a `csrf` reason instead (see
/// `console_session.rs`); a browser form gets an HTML page.
fn csrf_rejection() -> Response {
    server_error_page(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Your request was rejected",
        "The form could not be verified. Go back and try again.",
        None,
    )
}

/// A server-rendered error page in the OAuth layout, with an optional retry
/// link.
pub(crate) fn server_error_page(
    status: StatusCode,
    heading: &str,
    message: &str,
    retry_href: Option<&str>,
) -> Response {
    let retry = retry_href
        .map(|href| {
            format!(
                r#"<div class="oauth-result-retry"><a href="{}" class="oauth-button oauth-button-primary">Try again</a></div>"#,
                escape_html(href)
            )
        })
        .unwrap_or_default();
    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <title>{heading} · Centaur Console</title>
    <meta name="color-scheme" content="dark light">
    <link rel="icon" href="/icon.svg" type="image/svg+xml">
    <link rel="stylesheet" href="{stylesheet}">
  </head>

  <body>
    <main class="oauth-main">
      <div class="oauth-column">
<section class="oauth-card" aria-labelledby="oauth-result-heading">
  <img src="{logo}" alt="Centaur" class="oauth-logo-small" width="497" height="127">
  <h1 id="oauth-result-heading" class="oauth-result-heading is-error">{heading}</h1>
  <p class="oauth-result-message">{message}</p>{retry}
</section>
      </div>
    </main>
  </body>
</html>
"##,
        stylesheet = escape_html(&assets::path("oauth.css").expect("bundled oauth.css")),
        logo =
            escape_html(&assets::path("centaur-lockup-white.svg").expect("bundled lockup asset")),
        heading = escape_html(heading),
        message = escape_html(message),
    );
    let response = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .expect("valid error page response");
    server_page_csp(response)
}

pub(crate) fn server_page_csp(mut response: Response) -> Response {
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(SERVER_PAGE_CSP),
    );
    response
}

pub(crate) fn redirect(
    location: &str,
    cookies: &[HeaderValue],
    csp: Option<&'static str>,
) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    let location =
        HeaderValue::from_str(location).unwrap_or_else(|_| HeaderValue::from_static("/login"));
    response.headers_mut().insert(LOCATION, location);
    for cookie in cookies {
        response.headers_mut().append(SET_COOKIE, cookie.clone());
    }
    if let Some(csp) = csp {
        response
            .headers_mut()
            .insert("content-security-policy", HeaderValue::from_static(csp));
    }
    response
}

fn redirect_with_session(
    location: &str,
    config: &ConsoleSessionConfig,
    session: &ConsoleSession,
) -> Response {
    let cookie = config.cookie().set_cookie_header(session);
    redirect(location, cookie.as_slice(), Some(SERVER_PAGE_CSP))
}

fn page_response(
    status: StatusCode,
    html: String,
    config: &ConsoleSessionConfig,
    session: &ConsoleSession,
) -> Response {
    let mut response = html_response(status, html);
    if let Some(cookie) = config.cookie().set_cookie_header(session) {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

pub(crate) fn html_response(status: StatusCode, html: String) -> Response {
    let response = Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .expect("valid html response");
    server_page_csp(response)
}

fn render_login(
    config: &ConsoleSessionConfig,
    session: &ConsoleSession,
    alert: Option<&str>,
    status: StatusCode,
) -> Response {
    page_response(status, login_html(alert), config, session)
}

pub(crate) fn landing_path(user: &ConsoleUser) -> String {
    if user.admin {
        "/console/principals".to_owned()
    } else {
        "/console/integrations".to_owned()
    }
}

/// Configured SSO providers in Console's display order.
fn providers() -> Vec<&'static str> {
    super::areas::auth_pages::providers()
}

fn login_html(alert: Option<&str>) -> String {
    let providers = providers();
    let provider_buttons = providers
        .iter()
        .map(|provider| {
            format!(
                "        <a href=\"/auth/{provider}/start\" class=\"oauth-button oauth-button-outline\">Continue with {}</a>\n",
                capitalize(provider)
            )
        })
        .collect::<String>();
    let providers_block = if providers.is_empty() {
        String::new()
    } else {
        format!(
            "    <div class=\"oauth-stack\">\n{provider_buttons}    </div>\n\n{}",
            if crate::console::password_login_enabled() {
                "    <div class=\"oauth-divider\">or sign in with email</div>\n"
            } else {
                ""
            }
        )
    };
    let password_form = if crate::console::password_login_enabled() {
        r#"    <form class="oauth-login-form" action="/login" accept-charset="UTF-8" method="post">
      <div class="oauth-field">
        <label class="oauth-label" for="email">Email</label>
        <input class="oauth-input" placeholder="you@example.com" autocomplete="username" autofocus="autofocus" required="required" type="email" name="email" id="email">
      </div>
      <div class="oauth-field">
        <label class="oauth-label" for="password">Password</label>
        <input class="oauth-input" autocomplete="current-password" required="required" type="password" name="password" id="password">
      </div>

      <input type="submit" value="Sign in" class="oauth-button oauth-button-primary">
    </form>
"#
        .to_owned()
    } else {
        String::new()
    };
    let alert = alert.map_or(String::new(), |alert| {
        format!(
            "    <div class=\"oauth-alert\" role=\"alert\">{}</div>\n",
            escape_html(alert)
        )
    });

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <title>Sign In · Centaur Console</title>
    <meta name="color-scheme" content="dark light">
    <link rel="icon" href="/icon.svg" type="image/svg+xml">
    <link rel="stylesheet" href="{stylesheet}">
  </head>

  <body>
    <main class="oauth-main">
      <div class="oauth-column">
<div class="oauth-header">
  <img src="{logo}" alt="Centaur" class="oauth-logo" width="497" height="127">
  <h1 class="oauth-subtitle">Operator console sign in.</h1>
</div>

<section class="oauth-card">
{alert}
{providers_block}
{password_form}  </section>
      </div>
    </main>
  </body>
</html>
"##,
        stylesheet = escape_html(&assets::path("oauth.css").expect("bundled oauth.css")),
        logo =
            escape_html(&assets::path("centaur-lockup-white.svg").expect("bundled lockup asset")),
    )
}

fn pending_html(email: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <title>Awaiting Approval · Centaur Console</title>
    <meta name="color-scheme" content="dark light">
    <link rel="icon" href="/icon.svg" type="image/svg+xml">
    <link rel="stylesheet" href="{stylesheet}">
  </head>

  <body>
    <main class="oauth-main">
      <div class="oauth-column">
<div class="oauth-header">
  <img src="{logo}" alt="Centaur" class="oauth-logo" width="497" height="127">
  <p class="oauth-subtitle">Operator console.</p>
</div>

<section class="oauth-card oauth-center" aria-labelledby="pending-heading">
  <h1 id="pending-heading" class="oauth-result-heading">Awaiting approval</h1>
  <p class="oauth-result-message">
    You're signed in as <strong class="oauth-strong">{email}</strong>, but your account
    is pending approval by an administrator. You'll have access once it's approved.
  </p>

  <div class="oauth-result-retry">
    <form class="oauth-button-form" action="/logout" accept-charset="UTF-8" method="post"><button class="oauth-button oauth-button-outline" type="submit">Sign out</button></form>
  </div>
</section>
      </div>
    </main>
  </body>
</html>
"##,
        stylesheet = escape_html(&assets::path("oauth.css").expect("bundled oauth.css")),
        logo =
            escape_html(&assets::path("centaur-lockup-white.svg").expect("bundled lockup asset")),
        email = escape_html(email),
    )
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

pub(crate) fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str =
        "test-only-console-session-secret-0000000000000000000000000000000000000000000000000000";

    fn config() -> ConsoleSessionConfig {
        ConsoleSessionConfig::from_settings(Some(SECRET), Some("https://console.example.test"))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn login_page_posts_the_expected_fields_and_escapes_the_alert() {
        let html = login_html(Some("\"alert\""));
        assert!(html.contains("action=\"/login\""));
        assert!(html.contains("method=\"post\""));
        assert!(html.contains("name=\"email\""));
        assert!(html.contains("name=\"password\""));
        assert!(html.contains("&quot;alert&quot;"));
        assert!(html.contains("/assets/oauth-page/oauth.css?v="));
        assert!(html.contains("/assets/oauth-page/centaur-lockup-white.svg?v="));
    }

    #[test]
    fn login_error_codes_map_to_their_redirect_messages() {
        assert_eq!(
            login_error::message(login_error::ACCOUNT_DISABLED),
            Some("Your account is disabled.")
        );
        assert_eq!(
            login_error::message(login_error::SESSION_EXPIRED),
            Some("Please sign in again.")
        );
        assert_eq!(
            login_error::message(login_error::SSO_UNAVAILABLE),
            Some("That sign-in method is not available.")
        );
        assert_eq!(login_error::message("unknown"), None);
    }

    #[test]
    fn pending_page_escapes_the_email_and_posts_logout() {
        let html = pending_html("<user@example.test>");
        assert!(html.contains("&lt;user@example.test&gt;"));
        assert!(html.contains("action=\"/logout\""));
        assert!(html.contains("method=\"post\""));
    }

    #[test]
    fn form_verification_requires_an_exact_origin() {
        let config = config();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            "https://console.example.test".parse().unwrap(),
        );
        assert!(origin_allowed(&headers, &config));
        // An omitted Origin is not accepted.
        assert!(!origin_allowed(&HeaderMap::new(), &config));

        let mut null_origin = HeaderMap::new();
        null_origin.insert(axum::http::header::ORIGIN, "null".parse().unwrap());
        assert!(!origin_allowed(&null_origin, &config));

        for wrong in [
            "https://evil.example.test",
            "https://console.example.test/",
            "http://console.example.test",
        ] {
            let mut wrong_origin = HeaderMap::new();
            wrong_origin.insert(axum::http::header::ORIGIN, wrong.parse().unwrap());
            assert!(!origin_allowed(&wrong_origin, &config), "{wrong}");
        }
    }

    #[tokio::test]
    async fn server_rendered_pages_and_redirects_carry_the_narrowed_csp() {
        use axum::{
            body::Body,
            http::{Method, Request, header},
        };
        use tower::ServiceExt;

        assert_eq!(
            SERVER_PAGE_CSP,
            "default-src 'self'; script-src 'none'; style-src 'self'; img-src 'self' data:; \
             font-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; \
             object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
        );
        assert!(!SERVER_PAGE_CSP.contains("form-action"));
        let config = config();
        let cookie = config
            .cookie()
            .set_cookie_header(&ConsoleSession::anonymous())
            .unwrap();
        let app = crate::build_router_with_app_state(
            crate::AppState::unready(crate::ApiAuthConfig::testing("test-secret"))
                .with_console_session(config),
        );
        let assert_csp = |response: &Response| {
            assert_eq!(
                response.headers().get("content-security-policy").unwrap(),
                SERVER_PAGE_CSP
            );
            assert!(
                response
                    .headers()
                    .get("content-security-policy-report-only")
                    .is_none()
            );
        };
        for (method, uri, headers, status) in [
            (Method::GET, "/login", vec![], StatusCode::OK),
            (Method::GET, "/pending", vec![], StatusCode::FOUND),
            (
                Method::POST,
                "/logout",
                vec![(
                    header::COOKIE.to_string(),
                    cookie.to_str().unwrap().to_owned(),
                )],
                StatusCode::FOUND,
            ),
            (
                Method::POST,
                "/login",
                vec![
                    (
                        header::COOKIE.to_string(),
                        cookie.to_str().unwrap().to_owned(),
                    ),
                    (
                        header::CONTENT_TYPE.to_string(),
                        "application/x-www-form-urlencoded".to_owned(),
                    ),
                ],
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let mut request = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "console.example.test");
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{uri}");
            assert_csp(&response);
        }
    }

    #[tokio::test]
    async fn logout_with_an_anonymous_session_redirects_before_the_origin_check() {
        use axum::{
            body::Body,
            http::{Method, Request, header},
        };
        use tower::ServiceExt;

        let config = config();
        let cookie = config
            .cookie()
            .set_cookie_header(&ConsoleSession::anonymous())
            .unwrap();
        let app = crate::build_router_with_app_state(
            crate::AppState::unready(crate::ApiAuthConfig::testing("test-secret"))
                .with_console_session(config),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/logout")
                    .header(header::HOST, "console.example.test")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // The login gate runs first: no signed-in user redirects to the form
        // instead of rejecting the missing Origin.
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()[LOCATION], "/login");
    }

    #[test]
    fn password_hashing_uses_bcrypt_so_rails_digests_verify() {
        // Deterministic fake password only: this exercises the crate boundary,
        // not a published credential.
        let digest = bcrypt::hash("fake-test-password", bcrypt::DEFAULT_COST).unwrap();
        assert!(digest.starts_with("$2"));
        assert!(verify("fake-test-password", &digest).unwrap());
        assert!(!verify("wrong-password", &digest).unwrap());
    }

    #[test]
    fn html_escape_covers_attribute_and_text_characters() {
        assert_eq!(escape_html("<&>\"'"), "&lt;&amp;&gt;&quot;&#39;");
    }
}
