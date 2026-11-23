use sqlx::{PgPool, Row};

use crate::Result;

/// A Console user as the API's session layer needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleUser {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub admin: bool,
    pub status: ConsoleUserStatus,
}

/// Console's `users.status` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleUserStatus {
    Pending,
    Active,
    Disabled,
}

impl ConsoleUserStatus {
    /// Console has only these three statuses. Treating anything else as
    /// disabled keeps a future or corrupted value from granting access.
    fn from_db(value: &str) -> Self {
        match value {
            "active" => Self::Active,
            "pending" => Self::Pending,
            _ => Self::Disabled,
        }
    }
}

/// The email of the Console user api-rs's own writes (principal, role,
/// secret, grant, and proxy registration) are attributed to. `.invalid` is a
/// reserved domain, so no identity provider can verify it and link a sign-in
/// to this user; it has no password or API keys either.
pub const SYSTEM_USER_EMAIL: &str = "system@centaur.invalid";

/// The id of the system user ([`SYSTEM_USER_EMAIL`]), creating it on first use:
/// active, not an admin, and unable to sign in.
pub async fn system_user_id(pool: &PgPool) -> Result<i64> {
    // A concurrent first insert by another process is invisible to this
    // statement's snapshot, so the second attempt finds the committed row.
    for _ in 0..2 {
        let row: Option<(i64, bool)> = sqlx::query_as(
            "WITH created AS ( \
               INSERT INTO users (email, name, status, admin, created_at, updated_at) \
               VALUES ($1, 'Centaur', 'active', false, \
                       now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC') \
               ON CONFLICT (email) DO NOTHING RETURNING id, false AS has_password) \
             SELECT id, has_password FROM created \
             UNION ALL SELECT id, password_digest IS NOT NULL FROM users WHERE email = $1 \
             LIMIT 1",
        )
        .bind(SYSTEM_USER_EMAIL)
        .fetch_optional(pool)
        .await?;
        match row {
            // Something gave it a password, so someone could sign in as it.
            Some((_, true)) => {
                return Err(crate::Error::InvalidData(format!(
                    "the Console user {SYSTEM_USER_EMAIL} has a password; it is reserved for \
                     api-rs and must not be able to sign in"
                )));
            }
            Some((id, false)) => return Ok(id),
            None => {}
        }
    }
    Err(crate::Error::InvalidData(
        "the Console system user could not be created".to_owned(),
    ))
}

/// The Console user whose id is `id`, or `None` when it does not exist.
pub async fn find_console_user(pool: &PgPool, id: i64) -> Result<Option<ConsoleUser>> {
    let row = sqlx::query("SELECT id, email, name, admin, status FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(console_user).transpose()
}

/// The Console user with exactly `email`, with their bcrypt digest for the
/// password sign-in form. The digest stays out of [`ConsoleUser`] so ordinary
/// request contexts never carry it.
pub async fn find_console_password_user_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<(ConsoleUser, Option<String>)>> {
    let row = sqlx::query(
        "SELECT id, email, name, admin, status, password_digest FROM users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(pool)
    .await?;
    row.as_ref()
        .map(|row| Ok((console_user(row)?, row.try_get("password_digest")?)))
        .transpose()
}

/// The active API key with this `token_hash` and its owner. Callers that need
/// the authenticated key's identity (Console's self-revoke guard) read the id;
/// the bearer guard passes it on rather than re-hashing the header.
pub async fn find_console_api_key(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<(i64, ConsoleUser)>> {
    let row = sqlx::query(
        "SELECT k.id AS api_key_id, u.id, u.email, u.name, u.admin, u.status \
         FROM api_keys k JOIN users u ON u.id = k.user_id \
         WHERE k.token_hash = $1 AND k.deleted_at IS NULL AND u.status = 'active'",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let key_id = row.try_get("api_key_id")?;
    Ok(Some((key_id, console_user(&row)?)))
}

fn console_user(row: &sqlx::postgres::PgRow) -> Result<ConsoleUser> {
    let status: String = row.try_get("status")?;
    Ok(ConsoleUser {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        name: row.try_get("name")?,
        admin: row.try_get("admin")?,
        status: ConsoleUserStatus::from_db(&status),
    })
}
