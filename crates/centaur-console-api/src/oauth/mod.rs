//! OAuth: the MCP authorization server, broker consent flows, OAuth apps and
//! their providers, and broker credentials.
//!
//! - `mcp`: `/.well-known/*`, `/mcp/oauth/{register,authorize,token}` (RFC
//!   7591/6749/7636 for public PKCE clients);
//! - `flows`: `/oauth/{slug}/{start,callback}`, the consent flow that mints a
//!   broker credential from an OAuth app;
//! - `apps`: `/oauth_apps`, `/oauth_providers`, presets;
//! - `credentials`: `/broker_credentials`;
//! - `integrations`: what the Integrations page shows one user;
//! - `providers`, `exchange`: the per-IdP strategies and the upstream calls;
//! - `enrich`, `wrapping`: Slack profile enrichment and the grantable static
//!   secret that wraps a minted credential.

use axum::{
    Router,
    extract::RawQuery,
    middleware::map_response,
    response::{IntoResponse, Response},
    routing::{MethodRouter, get, post},
};
use http::StatusCode;
use serde_json::{Map, Value, json};

pub(crate) use super::auth::request_base;
use super::{ApiError, AppState, Routes};

pub mod apps;
pub mod credentials;
pub mod enrich;
pub mod exchange;
pub mod flows;
pub mod integrations;
pub mod mcp;
pub mod providers;
pub mod wrapping;

pub fn routes() -> Routes {
    Routes {
        bearer: Router::new()
            .route(
                "/broker_credentials",
                get(credentials::bearer_index).post(credentials::bearer_create),
            )
            .route(
                "/broker_credentials/lookup/default/{foreign_id}",
                get(credentials::bearer_lookup),
            )
            .route(
                "/broker_credentials/lookup/{foreign_id}",
                get(credentials::bearer_lookup),
            )
            .route(
                "/broker_credentials/{id}",
                get(credentials::bearer_show)
                    .patch(credentials::bearer_update)
                    .put(credentials::bearer_update)
                    .delete(credentials::destroy),
            )
            .route(
                "/oauth_apps",
                get(apps::bearer_index).post(apps::bearer_create),
            )
            .route("/oauth_apps/lookup/{slug}", get(apps::bearer_lookup))
            .route(
                "/oauth_apps/{id}",
                get(apps::bearer_show)
                    .patch(apps::bearer_update)
                    .put(apps::bearer_update)
                    .delete(apps::destroy),
            ),
        console: Router::new()
            .route(
                "/broker_credentials",
                get(credentials::console_index).post(credentials::console_create),
            )
            .route(
                "/broker_credentials/{id}",
                get(credentials::console_show)
                    .patch(credentials::console_update)
                    .put(credentials::console_update)
                    .delete(credentials::destroy),
            )
            .route(
                "/oauth_apps",
                get(apps::console_index).post(apps::console_create),
            )
            .route("/oauth_apps/presets/{provider}", post(apps::console_preset))
            .route(
                "/oauth_apps/{id}/credentials",
                get(apps::console_credentials),
            )
            .route(
                "/oauth_apps/{id}/provider_details",
                get(apps::console_provider_details),
            )
            .route(
                "/oauth_apps/{id}",
                get(apps::console_show)
                    .patch(apps::console_update)
                    .put(apps::console_update)
                    .delete(apps::destroy),
            )
            .route("/oauth_providers", get(apps::providers_index)),
        console_active: Router::new().route("/integrations", get(integrations::index)),
        public: Router::new()
            .route(
                "/.well-known/oauth-authorization-server",
                uncacheable(get(mcp::metadata)),
            )
            .route(
                "/.well-known/openid-configuration",
                uncacheable(get(mcp::metadata)),
            )
            .route("/mcp/oauth/register", uncacheable(post(mcp::register)))
            .route(
                "/mcp/oauth/authorize",
                uncacheable(get(mcp::authorize).post(mcp::approve)),
            )
            .route("/mcp/oauth/token", post(mcp::token))
            .route("/oauth/{slug}/start", get(flows::start))
            .route("/oauth/{slug}/callback", get(flows::callback)),
        ..Routes::default()
    }
}

// --- shared helpers -----------------------------------------------------------

/// Marks every response of `routes` uncacheable: MCP metadata, registration,
/// and consent answer per-client or per-user content (RFC 6749 §5.1).
fn uncacheable(routes: MethodRouter<AppState>) -> MethodRouter<AppState> {
    routes.layer(map_response(|response: Response| async move {
        mcp::no_store(response)
    }))
}

/// `base` with `path` (an absolute path) as its path.
pub(crate) fn join(base: &str, path: &str) -> Result<String, ApiError> {
    url::Url::parse(base)
        .and_then(|base| base.join(path))
        .map(String::from)
        .map_err(|_| ApiError::internal("the public URL is not a valid base URL"))
}

/// The redirect URI an app's consent flow registers with its IdP.
pub(crate) fn callback_url(base: &str, slug: &str) -> Result<String, ApiError> {
    join(base, &format!("/oauth/{slug}/callback"))
}

/// Where a user starts an app's consent flow.
pub(crate) fn start_url(base: &str, slug: &str) -> Result<String, ApiError> {
    join(base, &format!("/oauth/{slug}/start"))
}

/// An upstream URL. Debug builds with `CENTAUR_CONSOLE_OAUTH_TEST_BASE_URL`
/// send every IdP and provider call to that stub as `{base}/{host}{path}`,
/// so tests can answer for any host; release builds never rewrite.
pub(crate) fn upstream_url(state: &AppState, url: &str) -> String {
    let base = cfg!(debug_assertions)
        .then(|| {
            state
                .config
                .env
                .var_present("CENTAUR_CONSOLE_OAUTH_TEST_BASE_URL")
        })
        .flatten();
    match (base, url::Url::parse(url)) {
        (Some(base), Ok(parsed)) => {
            let mut rewritten = format!(
                "{}/{}{}",
                base.trim_end_matches('/'),
                parsed.host_str().unwrap_or_default(),
                parsed.path()
            );
            if let Some(query) = parsed.query() {
                rewritten.push('?');
                rewritten.push_str(query);
            }
            rewritten
        }
        _ => url.to_owned(),
    }
}

/// `{"error":{"message":...}}` with `status`, for statuses `ApiError` has no
/// variant for.
pub(crate) fn error_response(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({"error": {"message": message}}))).into_response()
}

/// The `labels[key]=value` list filter (JSONB containment; scalar values).
pub(crate) fn label_filter(raw_query: &RawQuery) -> Map<String, Value> {
    let mut labels = Map::new();
    for (key, value) in url::form_urlencoded::parse(raw_query.0.as_deref().unwrap_or("").as_bytes())
    {
        if let Some(label) = key
            .strip_prefix("labels[")
            .and_then(|k| k.strip_suffix(']'))
        {
            labels.insert(label.to_owned(), Value::String(value.into_owned()));
        }
    }
    labels
}

/// A field that may be absent, `null`, or a value: `Some(None)` is `null`.
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

/// A URL-safe identifier: `A-Za-z0-9-._~`.
pub(crate) fn url_safe(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
}

pub(crate) const URL_SAFE_MESSAGE: &str =
    "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)";

pub(crate) fn blank(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_an_absolute_path_onto_the_public_url() {
        assert_eq!(
            callback_url("https://c.example.test:8443/base?q", "google").unwrap(),
            "https://c.example.test:8443/oauth/google/callback"
        );
        assert!(start_url("not a url", "x").is_err());
    }

    #[test]
    fn parses_label_filters() {
        let raw = RawQuery(Some("labels[team]=a%20b&limit=5&labels[x]=1".into()));
        let labels = label_filter(&raw);
        assert_eq!(labels["team"], "a b");
        assert_eq!(labels["x"], "1");
        assert_eq!(labels.len(), 2);
    }
}
