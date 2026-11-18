//! Server-rendered pages: askama templates extending
//! `templates/pages/layout.html`, rendered with the strict page CSP (no
//! script, no inline style).

use askama::Template;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};

/// CSP of the SPA shell and API responses.
pub const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
style-src-elem 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; \
worker-src 'self'; manifest-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// CSP of server-rendered pages: no script, no inline style, no
/// `form-action` (a sign-in redirect may end at a custom-scheme client).
pub const PAGE_CSP: &str = "default-src 'self'; script-src 'none'; style-src 'self'; \
img-src 'self' data:; font-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; \
object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// Renders `page` with `status` and the page CSP.
pub fn render<T: Template>(status: StatusCode, page: &T) -> Response {
    match page.render() {
        Ok(html) => (
            status,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
                (
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static(PAGE_CSP),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            html,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "page template failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// A 302 to `location` (a same-origin path, or a URL the caller validated).
pub fn redirect(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => (StatusCode::FOUND, [(header::LOCATION, value)]).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
