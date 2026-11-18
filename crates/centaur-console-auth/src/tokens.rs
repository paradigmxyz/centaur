//! JWTs the console mints (HS256 with `CENTAUR_JWT_SIGNING_SECRET`) and the
//! claims other services verify: the api-rs principal and console-service
//! tokens, the sandbox entitlement token, the api-rs service token (the
//! mirror image, verified in [`super::internal`]), and MCP access tokens.
//!
//! Tokens are "windowed": the same subject gets the same `iat` for a window
//! (offset by a hash of the subject so rotations spread out), so repeated
//! mints within a window are identical and consumers can cache by claim.

use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::NotConfigured;

pub const API_AUDIENCE: &str = "centaur-api";
pub const CONSOLE_ISSUER: &str = "centaur-console";
pub const API_WINDOW_SECONDS: i64 = 15 * 60;
pub const API_TTL_SECONDS: i64 = 60 * 60;
pub const SANDBOX_WINDOW_SECONDS: i64 = 24 * 60 * 60;
pub const SANDBOX_TTL_SECONDS: i64 = 3 * 24 * 60 * 60;

/// Signs `claims` as an HS256 JWT.
pub fn sign<T: Serialize>(secret: &str, claims: &T) -> Result<String, NotConfigured> {
    if secret.trim().is_empty() {
        return Err(NotConfigured);
    }
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| NotConfigured)
}

/// Start of the rotation window containing `now` for `subject`.
pub fn window_start(subject: &str, now: i64, window: i64) -> i64 {
    let offset = i64::from(crc32fast::hash(subject.as_bytes())) % window;
    now - (now - offset).rem_euclid(window)
}

fn windowed(
    secret: &str,
    subject: &str,
    (issuer, audience): (&str, &str),
    (window, ttl): (i64, i64),
    extra: Value,
    now: DateTime<Utc>,
) -> Result<String, NotConfigured> {
    let iat = window_start(subject, now.timestamp(), window);
    let mut claims = Map::new();
    claims.insert("iss".into(), issuer.into());
    claims.insert("aud".into(), audience.into());
    claims.insert("iat".into(), iat.into());
    claims.insert("exp".into(), (iat + ttl).into());
    if let Value::Object(extra) = extra {
        claims.extend(extra);
    }
    sign(secret, &claims)
}

pub const CONSOLE_SERVICE_SUBJECT: &str = "centaur-console";

/// The token the console itself uses against api-rs admin routes.
pub fn console_service(secret: &str, now: DateTime<Utc>) -> Result<String, NotConfigured> {
    windowed(
        secret,
        CONSOLE_SERVICE_SUBJECT,
        (CONSOLE_ISSUER, API_AUDIENCE),
        (API_WINDOW_SECONDS, API_TTL_SECONDS),
        json!({"sub": CONSOLE_SERVICE_SUBJECT, "token_use": "console_service"}),
        now,
    )
}

/// Issuer and audience of sandbox entitlement tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxJwtConfig {
    pub audience: String,
    pub issuer: String,
}

impl SandboxJwtConfig {
    pub const DEFAULT_AUDIENCE: &'static str = "centaur-console-sandbox-entitlements";

    /// Blank settings take the defaults.
    pub fn new(audience: Option<&str>, issuer: Option<&str>) -> Self {
        let present = |value: Option<&str>, default: &str| {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(default)
                .to_owned()
        };
        Self {
            audience: present(audience, Self::DEFAULT_AUDIENCE),
            issuer: present(issuer, CONSOLE_ISSUER),
        }
    }
}

impl Default for SandboxJwtConfig {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// The entitlement token a sandbox presents to `/api/v1/sandbox/*`
/// (proxy-sync mints the same claims for running sandboxes).
pub fn sandbox_entitlement(
    secret: &str,
    config: &SandboxJwtConfig,
    proxy_oid: &str,
    proxy_name: &str,
    principal_oid: &str,
    now: DateTime<Utc>,
) -> Result<String, NotConfigured> {
    windowed(
        secret,
        proxy_oid,
        (&config.issuer, &config.audience),
        (SANDBOX_WINDOW_SECONDS, SANDBOX_TTL_SECONDS),
        json!({
            "sub": proxy_name,
            "sandbox_id": proxy_name,
            "proxy_id": proxy_oid,
            "principal_id": principal_oid,
        }),
        now,
    )
}

/// The service token api-rs presents to `/internal/*`.
pub fn internal_service(secret: &str, now: DateTime<Utc>) -> Result<String, NotConfigured> {
    windowed(
        secret,
        super::internal::SUBJECT,
        (super::internal::ISSUER, super::internal::AUDIENCE),
        (API_WINDOW_SECONDS, API_TTL_SECONDS),
        json!({"sub": super::internal::SUBJECT, "token_use": super::internal::TOKEN_USE}),
        now,
    )
}

/// An MCP OAuth access token: the caller supplies every claim.
pub fn mcp_access(secret: &str, claims: &impl Serialize) -> Result<String, NotConfigured> {
    sign(secret, claims)
}
