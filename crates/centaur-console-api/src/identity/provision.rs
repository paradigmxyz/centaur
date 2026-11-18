//! Linking a verified SSO identity to a console user, in one transaction.
//!
//! A returning identity matches by `(provider, subject)`. A new one adopts
//! the existing user with the same IdP-verified email; otherwise a new
//! active user is created (an admin when the verified email is a bootstrap
//! admin). A pending user is activated; a disabled one is returned as is
//! (the caller refuses it).

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use super::settings::SsoPolicy;
use crate::{ApiError, error::is_unique_violation};

/// What the provider verified about the person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SsoIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
    /// Slack workspace id (Slack only).
    pub team_id: Option<String>,
}

#[derive(Debug)]
pub enum ProvisionError {
    /// The email's domain is not allowed.
    DomainNotAllowed,
    /// The identity could not become a valid user (no usable email).
    Invalid(&'static str),
    Database(sqlx::Error),
    /// A concurrent first sign-in created the user (or the identity) for
    /// this person first. A fresh attempt links to it.
    Conflict,
}

impl From<sqlx::Error> for ProvisionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<ProvisionError> for ApiError {
    fn from(error: ProvisionError) -> Self {
        match error {
            ProvisionError::Database(error) => error.into(),
            other => ApiError::internal(format!("{other:?}")),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct Provisioned {
    pub id: i64,
    pub status: String,
    pub admin: bool,
    pub email: String,
    /// A Slack identity was created or changed: reconcile its credentials
    /// once the transaction is committed.
    #[sqlx(skip)]
    pub slack_changed: bool,
}

fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// A deliberately small check: `local@domain`, no whitespace, a dotted or
/// single-label domain made of letters, digits, and hyphens.
pub fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !local.chars().any(|c| c.is_whitespace() || c == '@')
        && !domain.is_empty()
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

pub async fn link_or_provision(
    pool: &PgPool,
    now: DateTime<Utc>,
    provider: &str,
    identity: &SsoIdentity,
    policy: &SsoPolicy,
) -> Result<Provisioned, ProvisionError> {
    if !policy.email_allowed(identity.email.as_deref()) {
        return Err(ProvisionError::DomainNotAllowed);
    }
    match link_once(pool, now, provider, identity, policy).await {
        // Two first sign-ins of one person race: the winner's rows are
        // committed by the time the insert conflicts, so one more attempt
        // links to them instead of failing the sign-in.
        Err(ProvisionError::Conflict) => link_once(pool, now, provider, identity, policy)
            .await
            .map_err(|error| match error {
                // Still conflicting: this is the unverified-email case, where
                // a second account with the same email is not allowed.
                ProvisionError::Conflict => ProvisionError::Invalid("email"),
                other => other,
            }),
        other => other,
    }
}

/// One attempt: the whole link or provision, in its own transaction.
async fn link_once(
    pool: &PgPool,
    now: DateTime<Utc>,
    provider: &str,
    identity: &SsoIdentity,
    policy: &SsoPolicy,
) -> Result<Provisioned, ProvisionError> {
    let mut tx = pool.begin().await?;
    let now = now.naive_utc();
    let email = identity
        .email
        .as_deref()
        .map(normalize_email)
        .filter(|email| !email.is_empty());
    let team_id = identity
        .team_id
        .as_deref()
        .map(str::trim)
        .filter(|team| !team.is_empty() && provider == "slack");

    let existing: Option<(i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT id, user_id, team_id FROM user_identities \
         WHERE provider = $1 AND subject = $2 FOR UPDATE",
    )
    .bind(provider)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;

    let slack_changed;
    let user_id = if let Some((identity_id, user_id, old_team)) = existing {
        sqlx::query(
            "UPDATE user_identities SET email = $2, email_verified = $3, \
             team_id = COALESCE($4, team_id), updated_at = $5 WHERE id = $1",
        )
        .bind(identity_id)
        .bind(&email)
        .bind(identity.email_verified)
        .bind(team_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        slack_changed =
            provider == "slack" && team_id.is_some_and(|new| old_team.as_deref() != Some(new));
        user_id
    } else {
        let user_id = match linkable_user(&mut tx, identity, email.as_deref()).await? {
            Some(id) => id,
            None => create_user(&mut tx, now, identity, email.as_deref(), policy).await?,
        };
        sqlx::query(
            "INSERT INTO user_identities \
             (user_id, provider, subject, email, email_verified, team_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
        )
        .bind(user_id)
        .bind(provider)
        .bind(&identity.subject)
        .bind(&email)
        .bind(identity.email_verified)
        .bind(team_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(concurrent_conflict)?;
        slack_changed = provider == "slack";
        user_id
    };

    // Fill a blank name from the IdP; activate a legacy pending user.
    if let Some(name) = identity
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        sqlx::query(
            "UPDATE users SET name = $2, updated_at = $3 \
             WHERE id = $1 AND (name IS NULL OR btrim(name) = '')",
        )
        .bind(user_id)
        .bind(name)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE users SET status = 'active', approved_at = $2, updated_at = $2 \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let mut user: Provisioned =
        sqlx::query_as("SELECT id, status, admin, email FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    user.slack_changed = slack_changed;
    Ok(user)
}

/// Only an IdP-verified email may adopt an existing account.
async fn linkable_user(
    conn: &mut PgConnection,
    identity: &SsoIdentity,
    email: Option<&str>,
) -> Result<Option<i64>, sqlx::Error> {
    match email {
        Some(email) if identity.email_verified => {
            sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
                .bind(email)
                .fetch_optional(conn)
                .await
        }
        _ => Ok(None),
    }
}

async fn create_user(
    conn: &mut PgConnection,
    now: chrono::NaiveDateTime,
    identity: &SsoIdentity,
    email: Option<&str>,
    policy: &SsoPolicy,
) -> Result<i64, ProvisionError> {
    let Some(email) = email.filter(|email| valid_email(email)) else {
        return Err(ProvisionError::Invalid("email"));
    };
    let admin = identity.email_verified && policy.bootstrap_admin(Some(email));
    sqlx::query_scalar(
        "INSERT INTO users (email, name, admin, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'active', $4, $4) RETURNING id",
    )
    .bind(email)
    .bind(
        identity
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty()),
    )
    .bind(admin)
    .bind(now)
    .fetch_one(conn)
    .await
    .map_err(concurrent_conflict)
}

/// A unique violation means another request created the row first, whether
/// it is this user's email or its identity. Anything else is a database
/// error. (A unique violation aborts the transaction, hence the retry.)
fn concurrent_conflict(error: sqlx::Error) -> ProvisionError {
    if is_unique_violation(&error) {
        ProvisionError::Conflict
    } else {
        error.into()
    }
}

#[cfg(test)]
mod tests {
    use super::valid_email;

    #[test]
    fn email_shape() {
        for ok in ["a@b.test", "first.last+tag@sub.example.org", "x@localhost"] {
            assert!(valid_email(ok), "{ok}");
        }
        for bad in [
            "",
            "a",
            "a@",
            "@b.test",
            "a b@c.test",
            "a@b..test",
            "a@-b.test",
            "a@b@c",
        ] {
            assert!(!valid_email(bad), "{bad}");
        }
    }
}
