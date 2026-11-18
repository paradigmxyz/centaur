//! The web app manifest, the service worker, and the `web+centaur://`
//! launch target mapping.

use axum::{body::Body, response::Response};
use http::{HeaderValue, header};

pub const MANIFEST: &[u8] = include_bytes!("../../assets/pwa/manifest.json");
pub const SERVICE_WORKER: &[u8] = include_bytes!("../../assets/pwa/service-worker.js");

fn serve(content_type: &'static str, bytes: &'static [u8]) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

pub async fn manifest() -> Response {
    serve("application/manifest+json", MANIFEST)
}

pub async fn service_worker() -> Response {
    serve("text/javascript", SERVICE_WORKER)
}

pub const SCHEME_PREFIX: &str = "web+centaur://";

/// The in-app path a `web+centaur://console/...` target opens. Only plain
/// paths inside the console SPA survive; anything else is the root.
pub fn launch_path(target: &str) -> String {
    let Some(rest) = target.strip_prefix(SCHEME_PREFIX) else {
        return "/".into();
    };
    let mut path = String::new();
    for segment in rest.split('/').filter(|segment| !segment.is_empty()) {
        path.push('/');
        path.push_str(segment);
    }
    let console = path == "/console"
        || path.strip_prefix("/console/").is_some_and(|rest| {
            rest.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'-'))
        });
    if console { path } else { "/".into() }
}

#[cfg(test)]
mod tests {
    use super::launch_path;

    #[test]
    fn maps_targets_onto_console_paths() {
        assert_eq!(
            launch_path("web+centaur://console/workflows"),
            "/console/workflows"
        );
        assert_eq!(launch_path("web+centaur://console"), "/console");
        assert_eq!(
            launch_path("web+centaur:///console//integrations/"),
            "/console/integrations"
        );
        for target in [
            "console/workflows",
            "",
            "web+centaur://",
            "web+centaur://   ",
            "web+centaur://../etc/passwd",
            "web+centaur://console/workflows?x=1",
            "web+centaur:////evil.example",
            "web+centaur://evil.example/console",
            "web+centaur://api/v1/me",
            "web+centaur://console/..%2f..%2fapi",
            "web+centaur://console\\evil",
            "web+centaur://console/a\n",
            "web+centaur://console/a/../b",
        ] {
            assert_eq!(launch_path(target), "/", "{target:?}");
        }
    }
}
