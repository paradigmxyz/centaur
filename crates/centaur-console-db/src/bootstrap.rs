//! The initial Console admin: on a database with no users other than the system
//! user api-rs writes as, create the configured operator as an active admin and
//! the default `infra` role. It does nothing once any operator exists, so it is
//! safe to run on every start.
//!
//! It creates no API key: the operator signs in and creates one on the Console's
//! API Keys page.

use sqlx::PgPool;

use crate::SetupError as Error;

type Result<T> = std::result::Result<T, Error>;

/// The advisory lock key, so another replica bootstrapping the same database at
/// the same time waits for this one and finds the user.
#[allow(clippy::unusual_byte_groupings)]
const ADVISORY_LOCK_KEY: i64 = 0x1700_B007_C0FFEE & 0x7FFF_FFFF_FFFF_FFFF;

/// Minimum password length: 12 characters.
const MIN_PASSWORD_LENGTH: usize = 12;

/// The operator to create on an empty database.
#[derive(Clone)]
pub struct InitialAdmin {
    pub email: String,
    pub password: String,
}

impl std::fmt::Debug for InitialAdmin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InitialAdmin")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// What [`bootstrap_initial_admin`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Bootstrap {
    /// A user already exists; nothing changed.
    Existing,
    Created {
        user_id: i64,
    },
}

/// Create `admin` when the database has no operators.
pub async fn bootstrap_initial_admin(pool: &PgPool, admin: &InitialAdmin) -> Result<Bootstrap> {
    if admin.password.is_empty() {
        return Err(Error::Bootstrap(
            "the initial Console user's email is set but its password is missing".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    // The system user api-rs writes as is not an operator.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE email <> $1)")
        .bind(crate::SYSTEM_USER_EMAIL)
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Ok(Bootstrap::Existing);
    }

    // Validated only when creating: an existing deployment whose configured
    // values no longer pass keeps starting.
    let email = admin.email.trim().to_lowercase();
    if email == crate::SYSTEM_USER_EMAIL {
        return Err(Error::Bootstrap(format!(
            "{} is reserved for the system user; choose another initial Console user email",
            crate::SYSTEM_USER_EMAIL
        )));
    }
    if !valid_email(&email) {
        return Err(Error::Bootstrap(
            "the initial Console user's email is invalid".to_owned(),
        ));
    }
    if admin.password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(Error::Bootstrap(format!(
            "the initial Console user's password must be at least {MIN_PASSWORD_LENGTH} characters"
        )));
    }
    // `$2a$` digests at bcrypt's default cost, like the existing stored digests.
    let digest = bcrypt::hash_with_result(&admin.password, bcrypt::DEFAULT_COST)
        .map_err(|error| Error::Bootstrap(format!("hashing the initial password: {error}")))?
        .format_for_version(bcrypt::Version::TwoA);

    // The initial operator predates any approver, so it is created active and
    // admin: it is the account that approves everyone else.
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password_digest, status, admin, created_at, updated_at) \
         VALUES ($1, $2, 'active', true, now(), now()) RETURNING id",
    )
    .bind(&email)
    .bind(&digest)
    .fetch_one(&mut *tx)
    .await?;
    ensure_default_infra_role(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(Bootstrap::Created { user_id })
}

/// The `infra` role, assigned by default.
async fn ensure_default_infra_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    created_by: i64,
) -> Result<()> {
    let updated = sqlx::query(
        "UPDATE roles SET assign_by_default = true, updated_at = now() \
         WHERE foreign_id = 'infra' AND NOT assign_by_default",
    )
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() > 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO roles (foreign_id, name, labels, assign_by_default, created_by_id, created_at, updated_at) \
         SELECT 'infra', 'Infra', '{\"managed-by\": \"centaur\"}', true, $1, now(), now() \
         WHERE NOT EXISTS (SELECT 1 FROM roles WHERE foreign_id = 'infra')",
    )
    .bind(created_by)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A simplified email shape: one `@`, a non-empty local part, and a domain of
/// non-empty dot-separated labels.
fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c));
    let domain_ok = !domain.is_empty()
        && domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    local_ok && domain_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_validation_matches_the_common_cases() {
        assert!(valid_email("admin@example.com"));
        assert!(valid_email("first.last+tag@sub.example.com"));
        assert!(!valid_email("admin"));
        assert!(!valid_email("@example.com"));
        assert!(!valid_email("admin@"));
        assert!(!valid_email("admin@example..com"));
        assert!(!valid_email("a b@example.com"));
    }
}
