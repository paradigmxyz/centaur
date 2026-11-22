//! Host-based routing between the Console surface and api-rs.
//!
//! Both surfaces share one listener. A request whose `Host` matches
//! `CENTAUR_CONSOLE_PUBLIC_URL` is a browser request and may only reach the
//! Console router; every other host is in-cluster traffic and may only reach
//! api-rs's own routes plus the Console API namespaces under `/api/v1`.
//!
//! The ingress reaches api-rs directly, so routing reads `Host` only. api-rs
//! also reads `X-Forwarded-Host` when it builds absolute OAuth metadata URLs,
//! but it does not trust that header for request routing.

use axum::{
    extract::{Request, State},
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, HOST},
        uri::Authority,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use url::{Host, Url};

use crate::AppState;

/// api-rs's own route families other than the Console namespaces under
/// `/api/v1`. On the Console host these answer 404; on any other host they are
/// the ordinary api-rs routes.
const API_RS_PREFIXES: &[&str] = &[
    "/api/session",
    "/api/sandboxes",
    "/api/slack",
    "/api/workflows",
    "/api/scheduled-tasks",
    "/api/admin",
    "/api/webhooks",
];

/// The host-routing middleware. Mounted only when the Console layer is enabled;
/// without it the listener behaves exactly as it did before the port.
pub(crate) async fn route(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(config) = state.console_session() else {
        return next.run(request).await;
    };
    let path = request.uri().path();
    if is_console_host(config.public_origin(), request.headers()) {
        // Only the Console router answers on the public host. `/api/v1/...`
        // belongs to the Console's bearer, sandbox, and console namespaces.
        if is_api_rs_only(path) {
            return StatusCode::NOT_FOUND.into_response();
        }
    } else {
        // In-cluster callers get api-rs plus the Console API namespaces. The
        // browser surface (SPA, server pages, cookie session API) is not
        // reachable on an internal host.
        if !is_internal_path(path) {
            return StatusCode::NOT_FOUND.into_response();
        }
        if is_console_session_path(path) && !request.headers().contains_key(AUTHORIZATION) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    next.run(request).await
}

/// Whether `headers` address the Console's public host.
///
/// The comparison is case-insensitive and port-aware: a `Host` without a port
/// uses the scheme's default port, so a public URL without an explicit port
/// matches `Host` with or without that default. An explicitly configured
/// non-default port must be repeated in `Host`.
pub(crate) fn is_console_host(public_origin: &str, headers: &HeaderMap) -> bool {
    let Ok(url) = Url::parse(public_origin) else {
        return false;
    };
    let Some(public_host) = url.host() else {
        return false;
    };
    let Some(public_port) = url.port_or_known_default() else {
        return false;
    };
    // An absent `Host` port means the scheme's default, not the public URL's
    // explicitly configured (possibly non-default) port.
    let scheme_default = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let Some(request_host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(authority) = request_host.parse::<Authority>() else {
        return false;
    };
    let request_port = authority.port_u16().or(scheme_default);
    canonical_host(authority.host()) == canonical_public_host(&public_host)
        && request_port == Some(public_port)
}

fn canonical_host(host: &str) -> String {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn canonical_public_host(host: &Host<&str>) -> String {
    match host {
        Host::Domain(domain) => domain.trim_end_matches('.').to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => address.to_string(),
    }
}

/// Paths api-rs serves itself. `/api/v1` is deliberately absent: it is the
/// Console namespace the public host also answers.
fn is_api_rs_only(path: &str) -> bool {
    matches!(
        path,
        "/healthz" | "/readyz" | "/metrics" | "/mcp" | "/oauth/token"
    ) || path == "/.well-known/oauth-protected-resource"
        || path.starts_with("/.well-known/oauth-protected-resource/")
        || API_RS_PREFIXES
            .iter()
            .any(|prefix| under_or_self(path, prefix))
}

fn is_console_namespace(path: &str) -> bool {
    under_or_self(path, "/api/v1")
}

fn is_console_session_path(path: &str) -> bool {
    under_or_self(path, "/api/v1/console")
}

fn is_internal_path(path: &str) -> bool {
    is_console_namespace(path) || is_api_rs_only(path)
}

fn under_or_self(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, host.parse().unwrap());
        headers
    }

    #[test]
    fn console_host_matches_case_insensitively_and_by_port() {
        let origin = "https://Console.Example.Test:8443";
        assert!(is_console_host(
            origin,
            &headers("console.example.test:8443")
        ));
        assert!(is_console_host(
            origin,
            &headers("CONSOLE.EXAMPLE.TEST:8443")
        ));
        assert!(!is_console_host(origin, &headers("console.example.test")));
        assert!(!is_console_host(
            origin,
            &headers("console.example.test:443")
        ));
        assert!(!is_console_host(
            origin,
            &headers("other.example.test:8443")
        ));
        assert!(!is_console_host(origin, &HeaderMap::new()));

        // A URL without an explicit port uses the scheme's default, which a
        // request may spell out or omit.
        let default = "https://console.example.test";
        assert!(is_console_host(default, &headers("console.example.test")));
        assert!(is_console_host(
            default,
            &headers("console.example.test:443")
        ));
        assert!(!is_console_host(
            default,
            &headers("console.example.test:444")
        ));

        // The harness binds an explicit loopback port.
        let harness = "http://127.0.0.1:4282";
        assert!(is_console_host(harness, &headers("127.0.0.1:4282")));
        assert!(!is_console_host(harness, &headers("localhost:4282")));
    }

    #[test]
    fn console_host_handles_ipv6_and_ignores_forwarded_host() {
        let origin = "http://[::1]:4282";
        assert!(is_console_host(origin, &headers("[::1]:4282")));
        assert!(!is_console_host(origin, &headers("::1:4282")));

        let mut forwarded = HeaderMap::new();
        forwarded.insert(HOST, "api.internal.test:4282".parse().unwrap());
        forwarded.insert("x-forwarded-host", "console.example.test".parse().unwrap());
        assert!(!is_console_host("https://console.example.test", &forwarded));
    }

    #[test]
    fn the_public_host_answers_only_the_console_namespaces() {
        for path in [
            "/",
            "/login",
            "/console/principals",
            "/oauth/app/start",
            "/mcp/oauth/authorize",
            "/.well-known/oauth-authorization-server",
            "/assets/oauth-page/oauth.css",
            "/api/v1/api_keys",
            "/api/v1/console/me",
            "/api/v1/sandbox/probe",
        ] {
            assert!(!is_api_rs_only(path), "{path}");
        }
        for path in [
            "/healthz",
            "/readyz",
            "/metrics",
            "/mcp",
            "/oauth/token",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
            "/api/session/slack:1",
            "/api/sandboxes/drain",
            "/api/slack/files",
            "/api/workflows/runs",
            "/api/scheduled-tasks/task/runs",
            "/api/admin/slack/dm-sync/checkpoints",
            "/api/webhooks/hook",
        ] {
            assert!(is_api_rs_only(path), "{path}");
        }
    }

    #[test]
    fn an_internal_host_answers_api_rs_and_the_console_api_only() {
        for path in [
            "/healthz",
            "/readyz",
            "/metrics",
            "/mcp",
            "/oauth/token",
            "/api/session/slack:1",
            "/api/webhooks/hook",
            "/api/v1/api_keys",
            "/api/v1/console/me",
            "/api/v1/sandbox/probe",
        ] {
            assert!(is_internal_path(path), "{path}");
        }
        for path in [
            "/",
            "/login",
            "/logout",
            "/pending",
            "/console/principals",
            "/auth/github/start",
            "/oauth/app/start",
            "/oauth/app/callback",
            "/mcp/oauth/authorize",
            "/.well-known/oauth-authorization-server",
            "/assets/oauth-page/oauth.css",
            "/not-a-console-path",
        ] {
            assert!(!is_internal_path(path), "{path}");
        }
    }

    #[test]
    fn only_the_cookie_session_namespace_is_restricted_internally() {
        assert!(is_console_session_path("/api/v1/console/me"));
        assert!(is_console_session_path("/api/v1/console"));
        assert!(!is_console_session_path("/api/v1/console-api"));
        assert!(!is_console_session_path("/api/v1/api_keys"));
        assert!(!is_console_session_path("/api/v1/sandbox/probe"));
    }
}
