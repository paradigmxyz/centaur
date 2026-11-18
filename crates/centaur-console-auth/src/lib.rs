//! The console's credential schemes (RFC 0006 Decision 4): a private-cookie session with its own CSRF secret,
//! HMAC-signed `state` tokens, bearer API keys, sandbox entitlement JWTs,
//! the api-rs service token, and JWT minting. Framework-neutral: plain
//! `http` types, the database, and `chrono` times.
//!
//! `centaur-console-api` wires these into axum and decides which namespace
//! takes which credential.

pub mod bearer;
pub mod csrf;
pub mod denial;
pub mod flow_cookie;
pub mod internal;
pub mod password;
pub mod random;
pub mod return_path;
pub mod sandbox;
pub mod session;
pub mod state_token;
pub mod tokens;

pub use denial::Denial;

/// A console user as the credential checks see them.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct AuthUser {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
    pub admin: bool,
}

impl AuthUser {
    pub const COLUMNS: &'static str = "id, email, name, status, admin";

    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    pub fn is_disabled(&self) -> bool {
        self.status == "disabled"
    }

    pub async fn find(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM users WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
    }
}

/// The signing secret is not configured (a deployment error, not a bad
/// credential).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("CENTAUR_JWT_SIGNING_SECRET is not configured")]
pub struct NotConfigured;

/// Why a credential check could not reach a verdict.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    NotConfigured(#[from] NotConfigured),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
