//! Application state: shared by every handler, cheap to clone.

use std::{ops::Deref, path::PathBuf, sync::Arc};

use centaur_console_auth::{
    flow_cookie::FlowCookies, session::SessionConfig, state_token::StateSigner,
    tokens::SandboxJwtConfig,
};
use centaur_console_db::{Clock, Encryptor, redact::Redacted};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::assets::Assets;
use crate::{env::Env, hosts::HostAllowlist};

/// Process configuration the HTTP layer reads. `Debug` hides secrets.
#[derive(Clone)]
pub struct Config {
    /// `CENTAUR_CONSOLE_PUBLIC_URL`, when set.
    pub public_url: Option<String>,
    /// `CENTAUR_JWT_SIGNING_SECRET` (empty when unset).
    pub jwt_signing_secret: String,
    pub sandbox_jwt: SandboxJwtConfig,
    /// The built web app's static files (`assets/`, icons).
    pub public_dir: PathBuf,
    /// `index.html` of the SPA.
    pub spa_index: PathBuf,
    /// The environment snapshot, for area settings (SSO, Slack, api-rs...).
    pub env: Env,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("public_url", &self.public_url)
            .field(
                "jwt_signing_secret",
                &(!self.jwt_signing_secret.is_empty()).then_some(Redacted),
            )
            .field("sandbox_jwt", &self.sandbox_jwt)
            .field("public_dir", &self.public_dir)
            .field("spa_index", &self.spa_index)
            .field("env", &self.env)
            .finish()
    }
}

impl Config {
    pub fn https_public_url(&self) -> bool {
        self.public_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://"))
    }
}

pub struct Inner {
    pub db: PgPool,
    /// Read-only connection to the api-rs database, when configured.
    pub api_db: Option<PgPool>,
    pub config: Config,
    /// ActiveRecord-compatible column encryption.
    pub encryptor: Encryptor,
    pub clock: Clock,
    /// Outbound HTTP (see [`super::http`]).
    pub http: reqwest::Client,
    pub sessions: SessionConfig,
    pub flow_cookies: FlowCookies,
    pub signer: StateSigner,
    pub assets: Assets,
    /// The hosts the console answers to; only these are trusted as
    /// `X-Forwarded-Host` values.
    pub hosts: HostAllowlist,
}

/// Cheaply cloneable handle to [`Inner`].
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(inner: Inner) -> Self {
        Self(Arc::new(inner))
    }

    /// The current time (the injected clock in tests).
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// `PUBLIC_URL`, else the request's own base URL.
    pub fn public_base_url(&self, request_base: &str) -> String {
        self.config
            .public_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or(request_base)
            .trim_end_matches('/')
            .to_owned()
    }
}

impl Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}
