//! Static files and the SPA shell.
//!
//! - `public/` (the built web app's `assets/`, icons) is served by
//!   `tower-http`'s `ServeDir`, which handles content types, conditional
//!   requests, and ranges;
//! - [`Assets`] adds `?v=<digest>` to the server-rendered pages' own CSS and
//!   images so a changed file is refetched;
//! - [`fallback`] answers everything no route matched: the SPA shell for a
//!   browser navigation outside the API and protocol prefixes, else a JSON
//!   404.

use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use axum::{
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
};
use http::{HeaderValue, Method, StatusCode, header};
use sha2::{Digest, Sha256};

use super::{error::ApiError, state::AppState};

/// Digest-stamped URLs for `public/assets/oauth-page/*`.
#[derive(Debug)]
pub struct Assets {
    directory: PathBuf,
    digests: Mutex<HashMap<String, String>>,
}

impl Assets {
    pub const DIRECTORY: &'static str = "assets/oauth-page";

    pub fn new(public_dir: &std::path::Path) -> Self {
        Self {
            directory: public_dir.join(Self::DIRECTORY),
            digests: Mutex::new(HashMap::new()),
        }
    }

    /// `/assets/oauth-page/<name>?v=<12 hex of the file's SHA-256>`; without
    /// the query when the file is missing.
    pub fn path(&self, name: &str) -> String {
        let mut digests = self.digests.lock().expect("asset digests");
        let digest = digests.entry(name.to_owned()).or_insert_with(|| {
            std::fs::read(self.directory.join(name))
                .map(|bytes| hex::encode(Sha256::digest(bytes))[..12].to_owned())
                .unwrap_or_default()
        });
        match digest.as_str() {
            "" => format!("/{}/{name}", Self::DIRECTORY),
            digest => format!("/{}/{name}?v={digest}", Self::DIRECTORY),
        }
    }
}

/// Paths that belong to the API or a protocol and never get the SPA shell.
const NOT_SPA: &[&str] = &[
    "/api",
    "/internal",
    "/mcp",
    "/oauth",
    "/auth",
    "/login",
    "/logout",
    "/pending",
    "/.well-known",
    "/up",
    "/assets",
];

pub fn spa_path(path: &str) -> bool {
    NOT_SPA.iter().all(|prefix| {
        !(path == *prefix
            || path.starts_with(&format!("{prefix}/"))
            || path.starts_with(&format!("{prefix}.")))
    })
}

const NOT_BUILT: &str = "The console web app has not been built.\n\
Run `pnpm -C services/console/web build`, or use the Vite dev server\n\
(`pnpm -C services/console/web dev`) during development.\n";

/// The router's fallback.
pub async fn fallback(State(state): State<AppState>, request: axum::extract::Request) -> Response {
    let navigation = matches!(*request.method(), Method::GET | Method::HEAD)
        && request
            .headers()
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"))
        && spa_path(request.uri().path());
    if !navigation {
        return ApiError::not_found().into_response();
    }
    match tokio::fs::read(&state.config.spa_index).await {
        Ok(html) => {
            let mut response = Response::new(Body::from(html));
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            // The shell references content-hashed assets; never cache it.
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, NOT_BUILT).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::spa_path;

    #[test]
    fn spa_paths_exclude_api_and_protocol_namespaces() {
        for path in ["/", "/console", "/console/principals/prn_1", "/authors"] {
            assert!(spa_path(path), "{path}");
        }
        for path in [
            "/api",
            "/api/v1/me",
            "/internal/v1/x",
            "/mcp",
            "/mcp.html",
            "/oauth/x/start",
            "/auth/google/start",
            "/login",
            "/pending",
            "/.well-known/oauth-authorization-server",
            "/up",
            "/assets/app.js",
        ] {
            assert!(!spa_path(path), "{path}");
        }
    }
}
