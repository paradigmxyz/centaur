//! The Console's static files: the SPA shell, its hashed assets, and the root
//! public files.
//!
//! Nothing here reads the session, the database, or request state; it is a
//! static mount behind the Console host router. `services/console/web` builds
//! the shell into `services/console/public/spa/index.html` and content-hashed
//! assets into `services/console/public/spa/assets`; the api-rs image copies
//! both and points `CENTAUR_CONSOLE_SPA_SHELL`, `CENTAUR_CONSOLE_SPA_ASSETS`,
//! and `CENTAUR_CONSOLE_PUBLIC_ROOT` at them. A checkout defaults to the
//! repository paths.
//!
//! The shell is the same file for `/` and every `/console/...` path; the SPA
//! routes on the client. A signed-out visitor is sent to
//! `/login?return_to=<page>` by the SPA itself when its first API call returns
//! 401, so the shell needs no session.

use std::{
    env,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY},
    },
    middleware::map_response,
    response::Response,
};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
};

use crate::{AppState, console::assets::PUBLIC_ASSET_CACHE_CONTROL};

/// The app-wide policy for the SPA and the pages that redirect into it.
pub(crate) const APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     style-src-elem 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; \
     connect-src 'self'; worker-src 'self'; manifest-src 'self'; object-src 'none'; \
     base-uri 'none'; frame-ancestors 'none'";

/// Content-hashed Vite output can be cached forever. The shell is no-cache, so
/// a deploy picks up the new asset names on the next page load.
const SPA_ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Root public files the shell and pages fetch. `/icon.svg` is embedded and
/// served by `pages`.
const PUBLIC_FILES: &[&str] = &["icon-dark.svg", "icon-light.svg", "robots.txt"];

fn console_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("services/console")
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

pub(crate) fn spa_shell_path() -> PathBuf {
    env_path("CENTAUR_CONSOLE_SPA_SHELL")
        .unwrap_or_else(|| console_root().join("public/spa/index.html"))
}

fn spa_assets_root() -> PathBuf {
    env_path("CENTAUR_CONSOLE_SPA_ASSETS")
        .unwrap_or_else(|| console_root().join("public/spa/assets"))
}

fn public_root() -> PathBuf {
    env_path("CENTAUR_CONSOLE_PUBLIC_ROOT").unwrap_or_else(|| console_root().join("public"))
}

struct Paths {
    shell: PathBuf,
    assets: PathBuf,
    public: PathBuf,
}

pub(crate) fn router() -> Router<AppState> {
    router_at(Paths {
        shell: spa_shell_path(),
        assets: spa_assets_root(),
        public: public_root(),
    })
}

fn router_at(paths: Paths) -> Router<AppState> {
    let shell_path = paths.shell;
    if !shell_path.is_file() {
        tracing::warn!(
            path = %shell_path.display(),
            "Console SPA shell not found; build services/console/web or set CENTAUR_CONSOLE_SPA_SHELL"
        );
    }
    let shell = Router::new()
        .route_service("/", ServeFile::new(&shell_path))
        .route_service("/console", ServeFile::new(&shell_path))
        .route_service("/console/{*path}", ServeFile::new(&shell_path))
        .layer(map_response(shell_headers));

    let assets = Router::new()
        .nest_service("/spa/assets", ServeDir::new(paths.assets))
        .layer(CompressionLayer::new())
        .layer(map_response(asset_headers));

    let root = paths.public;
    let mut public = Router::new();
    for name in PUBLIC_FILES {
        public = public.route_service(&format!("/{name}"), ServeFile::new(root.join(name)));
    }
    let public = public.layer(map_response(public_headers));

    shell.merge(assets).merge(public)
}

fn served(response: &Response) -> bool {
    response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED
}

async fn shell_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(APP_CSP));
    response
}

async fn asset_headers(mut response: Response) -> Response {
    if served(&response) {
        response.headers_mut().insert(
            CACHE_CONTROL,
            HeaderValue::from_static(SPA_ASSET_CACHE_CONTROL),
        );
    }
    response
}

async fn public_headers(mut response: Response) -> Response {
    if served(&response) {
        response.headers_mut().insert(
            CACHE_CONTROL,
            HeaderValue::from_static(PUBLIC_ASSET_CACHE_CONTROL),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, header::CONTENT_TYPE},
    };
    use tower::ServiceExt;

    async fn get_path(router: &Router, path: &str) -> Response {
        router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn header(response: &Response, name: axum::http::HeaderName) -> Option<&str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    #[tokio::test]
    async fn static_files_carry_their_cache_and_csp_policies() {
        let dir = tempfile::tempdir().unwrap();
        // The build layout: the shell sits beside its assets directory.
        let spa = dir.path().join("spa");
        std::fs::create_dir(&spa).unwrap();
        let shell = spa.join("index.html");
        std::fs::write(&shell, "<!doctype html><title>shell</title>").unwrap();
        let assets = spa.join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join("index-abc123.js"), "console.log(1)").unwrap();
        let public = dir.path().join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::write(public.join("robots.txt"), "User-agent: *").unwrap();
        let app = router_at(Paths {
            shell,
            assets,
            public,
        })
        .with_state(AppState::unready(crate::ApiAuthConfig::testing("s")));

        for path in ["/", "/console", "/console/principals/prn_1"] {
            let response = get_path(&app, path).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(header(&response, CONTENT_TYPE), Some("text/html"), "{path}");
            assert_eq!(header(&response, CACHE_CONTROL), Some("no-cache"));
            assert_eq!(header(&response, CONTENT_SECURITY_POLICY), Some(APP_CSP));
        }

        let response = get_path(&app, "/spa/assets/index-abc123.js").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header(&response, CACHE_CONTROL),
            Some(SPA_ASSET_CACHE_CONTROL)
        );
        for path in ["/spa/assets/missing.js"] {
            let response = get_path(&app, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(header(&response, CACHE_CONTROL), None, "{path}");
        }

        let response = get_path(&app, "/robots.txt").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            header(&response, CACHE_CONTROL),
            Some(PUBLIC_ASSET_CACHE_CONTROL)
        );
        // The shell is reachable only through the shell routes, with the CSP.
        for path in [
            "/spa/index.html",
            "/spa/assets/../index.html",
            "/index.html",
        ] {
            let response = get_path(&app, path).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }

        // Only `/` and `/console/...` are SPA navigations.
        let response = get_path(&app, "/elsewhere").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
