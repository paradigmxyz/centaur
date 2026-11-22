//! The built SPA and the public files its shell and manifest reference.
//!
//! `services/console/web` builds content-hashed assets into
//! `services/console/public/spa/assets` and the shell into
//! `services/console/spa-shell/index.html`; the api-rs image copies both there.
//! The shell is served by `auth_pages` with the app CSP, while this module owns
//! the `/spa/assets/*` files and the root public files (`manifest.json` and the
//! service worker are separate Console routes).
//!
//! Paths can be overridden with `CENTAUR_CONSOLE_SPA_SHELL`,
//! `CENTAUR_CONSOLE_SPA_ASSETS`, and `CENTAUR_CONSOLE_PUBLIC_ROOT`. The image
//! sets them to its baked copies; a checkout defaults to the repository paths.

use std::{
    env,
    path::{Component, Path, PathBuf},
};

use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, Path as AxumPath},
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::compression::CompressionLayer;

use crate::{
    AppState,
    console::{assets::PUBLIC_ASSET_CACHE_CONTROL, http::ConsoleError, http::json_not_found},
};

/// Content-hashed Vite output can be cached forever. The shell is served
/// no-cache by `auth_pages`, so a deploy picks up a new asset manifest at once.
pub(crate) const SPA_ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// The repository's Console directory, used when no environment override is
/// set. In the release image the Dockerfile points the variables at `/app`.
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
        .unwrap_or_else(|| console_root().join("spa-shell/index.html"))
}

fn spa_assets_root() -> PathBuf {
    env_path("CENTAUR_CONSOLE_SPA_ASSETS")
        .unwrap_or_else(|| console_root().join("public/spa/assets"))
}

fn public_root() -> PathBuf {
    env_path("CENTAUR_CONSOLE_PUBLIC_ROOT").unwrap_or_else(|| console_root().join("public"))
}

struct PublicFile {
    route: &'static str,
    name: &'static str,
    content_type: &'static str,
}

/// Files the shell, manifest, and service worker fetch from the site root.
/// `/icon.svg` is already served (embedded) by `pages`; the manifest and service
/// worker are separate no-cache Console routes. Rails serves these from
/// `public/`, so they keep its long-lived public cache.
const PUBLIC_FILES: &[PublicFile] = &[
    PublicFile {
        route: "/icon-dark.svg",
        name: "icon-dark.svg",
        content_type: "image/svg+xml",
    },
    PublicFile {
        route: "/icon-light.svg",
        name: "icon-light.svg",
        content_type: "image/svg+xml",
    },
    PublicFile {
        route: "/pwa-icon.svg",
        name: "pwa-icon.svg",
        content_type: "image/svg+xml",
    },
    PublicFile {
        route: "/pwa-icon-192.png",
        name: "pwa-icon-192.png",
        content_type: "image/png",
    },
    PublicFile {
        route: "/pwa-icon-512.png",
        name: "pwa-icon-512.png",
        content_type: "image/png",
    },
    PublicFile {
        route: "/offline.html",
        name: "offline.html",
        content_type: "text/html; charset=utf-8",
    },
    PublicFile {
        route: "/robots.txt",
        name: "robots.txt",
        content_type: "text/plain; charset=utf-8",
    },
];

/// `/spa/assets/*` (with gzip/brotli) plus the root public files.
pub(crate) fn router() -> Router<AppState> {
    let spa_assets = Router::new()
        .route("/spa/assets/{*path}", get(spa_asset))
        .layer(CompressionLayer::new());
    let mut router = Router::new().merge(spa_assets);
    for file in PUBLIC_FILES {
        router = router.route(file.route, get(public_file));
    }
    router
}

async fn spa_asset(AxumPath(path): AxumPath<String>) -> Response {
    let Some(relative) = safe_asset_path(&path) else {
        return json_not_found();
    };
    let content_type = asset_content_type(&path);
    match tokio::fs::read(spa_assets_root().join(relative)).await {
        Ok(bytes) => file_response(bytes, content_type, SPA_ASSET_CACHE_CONTROL),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json_not_found(),
        Err(error) => ConsoleError::internal(error).into_response(),
    }
}

async fn public_file(OriginalUri(uri): OriginalUri) -> Response {
    let Some(file) = PUBLIC_FILES.iter().find(|file| file.route == uri.path()) else {
        return json_not_found();
    };
    match tokio::fs::read(public_root().join(file.name)).await {
        Ok(bytes) => file_response(bytes, file.content_type, PUBLIC_ASSET_CACHE_CONTROL),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json_not_found(),
        Err(error) => ConsoleError::internal(error).into_response(),
    }
}

/// Reject absolute paths, `.`/`..`, and source maps before joining the asset
/// root. Vite emits flat hashed names, but nested paths are harmless when each
/// component stays below the root.
fn safe_asset_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.ends_with(".map") {
        return None;
    }
    let mut relative = PathBuf::new();
    let mut has_component = false;
    for component in Path::new(path).components() {
        let Component::Normal(name) = component else {
            return None;
        };
        relative.push(name);
        has_component = true;
    }
    has_component.then_some(relative)
}

fn asset_content_type(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    match extension.to_ascii_lowercase().as_str() {
        "avif" => "image/avif",
        "css" => "text/css; charset=utf-8",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "otf" => "font/otf",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn file_response(
    bytes: Vec<u8>,
    content_type: &'static str,
    cache_control: &'static str,
) -> Response {
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_paths_reject_traversal_absolute_paths_and_source_maps() {
        assert_eq!(
            safe_asset_path("index-abc123.js"),
            Some(PathBuf::from("index-abc123.js"))
        );
        assert_eq!(
            safe_asset_path("nested/index-abc123.js"),
            Some(PathBuf::from("nested/index-abc123.js"))
        );
        for path in [
            "",
            "../secret",
            "nested/../../secret",
            "/etc/passwd",
            "./index.js",
            "index.js.map",
        ] {
            assert_eq!(safe_asset_path(path), None, "{path:?}");
        }
    }

    #[test]
    fn asset_content_types_cover_the_vite_output() {
        assert_eq!(
            asset_content_type("index.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(asset_content_type("index.css"), "text/css; charset=utf-8");
        assert_eq!(asset_content_type("logo.svg"), "image/svg+xml");
        assert_eq!(asset_content_type("font.woff2"), "font/woff2");
        assert_eq!(asset_content_type("binary.bin"), "application/octet-stream");
    }

    #[test]
    fn static_responses_carry_their_cache_policy() {
        let response = file_response(
            Vec::new(),
            "text/css; charset=utf-8",
            SPA_ASSET_CACHE_CONTROL,
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/css; charset=utf-8"
        );

        let response = file_response(Vec::new(), "image/svg+xml", PUBLIC_ASSET_CACHE_CONTROL);
        assert_eq!(
            response.headers().get(CACHE_CONTROL).unwrap(),
            PUBLIC_ASSET_CACHE_CONTROL
        );
    }
}
