//! The static files behind the server-rendered sign-in and OAuth pages.
//!
//! Console serves `services/console/public/assets/oauth-page`, with a content
//! digest in each URL. The binary embeds the files and serves them at the same
//! paths so rendered forms keep loading.

use axum::{
    Router,
    body::Body,
    extract::Path,
    http::{StatusCode, header},
    response::Response,
    routing::get,
};
use sha2::{Digest, Sha256};

use crate::{AppState, console::http::json_not_found};

struct Asset {
    name: &'static str,
    bytes: &'static [u8],
    content_type: &'static str,
}

const ASSETS: &[Asset] = &[
    Asset {
        name: "oauth.css",
        bytes: include_bytes!("../../../../services/console/public/assets/oauth-page/oauth.css"),
        content_type: "text/css; charset=utf-8",
    },
    Asset {
        name: "geist-latin-wght-normal.woff2",
        bytes: include_bytes!(
            "../../../../services/console/public/assets/oauth-page/geist-latin-wght-normal.woff2"
        ),
        content_type: "font/woff2",
    },
    Asset {
        name: "centaur-lockup-white.svg",
        bytes: include_bytes!(
            "../../../../services/console/public/assets/oauth-page/centaur-lockup-white.svg"
        ),
        content_type: "image/svg+xml",
    },
    Asset {
        name: "GEIST-LICENSE.txt",
        bytes: include_bytes!(
            "../../../../services/console/public/assets/oauth-page/GEIST-LICENSE.txt"
        ),
        content_type: "text/plain; charset=utf-8",
    },
];

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/assets/oauth-page/{name}", get(serve_asset))
}

/// The digest-qualified URL for a file, or `None` when the name is not one of
/// the assets Console publishes.
pub(crate) fn path(name: &str) -> Option<String> {
    let asset = ASSETS.iter().find(|asset| asset.name == name)?;
    Some(format!(
        "/assets/oauth-page/{}?v={}",
        asset.name,
        digest(asset.bytes)
    ))
}

/// Public asset cache policy: `public, max-age=31556952` (one year).
pub(crate) const PUBLIC_ASSET_CACHE_CONTROL: &str = "public, max-age=31556952";

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))[..12].to_owned()
}

async fn serve_asset(Path(name): Path<String>) -> Response {
    let Some(asset) = ASSETS.iter().find(|asset| asset.name == name) else {
        return json_not_found();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset.content_type)
        // One year in seconds, no `immutable`. Keep both the lifetime and the
        // directive list equal.
        .header(header::CACHE_CONTROL, PUBLIC_ASSET_CACHE_CONTROL)
        .body(Body::from(asset.bytes))
        .expect("valid static asset response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_asset_paths_carry_a_twelve_hex_digest() {
        assert_eq!(
            path("oauth.css").unwrap(),
            format!("/assets/oauth-page/oauth.css?v={}", digest(ASSETS[0].bytes))
        );
        assert_eq!(digest(ASSETS[0].bytes).len(), 12);
        assert!(
            digest(ASSETS[0].bytes)
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
        assert_eq!(path("missing.css"), None);
    }
}
