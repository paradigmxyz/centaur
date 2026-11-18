//! Bootstrap: the first operator.
//!
//! When `CENTAUR_CONSOLE_INITIAL_USER_EMAIL` is set and no user exists, one
//! transaction (under an advisory lock) creates that user active and admin
//! with `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD`, ensures the default `infra`
//! role, and creates a `bootstrap` API key: the supplied
//! `CENTAUR_CONSOLE_INITIAL_API_KEY` (which must be `iak_` + 64 lowercase hex),
//! or a generated one. A generated key is written to the log so the operator
//! can retrieve it; a supplied key never is.

use centaur_console_api::{env::Env, identity::provision::valid_email};
use centaur_console_auth::{bearer::hash_token, password, random};
use centaur_console_db::Clock;
use sqlx::PgPool;

/// Serializes concurrent bootstraps across replicas.
pub const ADVISORY_LOCK_KEY: i64 = 0x0017_00B0_07C0_FFEE;

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("{0}")]
    Invalid(String),
    #[error("centaur-console bootstrap failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// What bootstrap did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `INITIAL_USER_EMAIL` is unset.
    NotConfigured,
    /// A user already exists.
    AlreadyBootstrapped,
    Created {
        user_id: i64,
        api_key_id: i64,
    },
}

fn valid_token(token: &str) -> bool {
    token.strip_prefix("iak_").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn validate_user(email: &str, password: &str) -> Vec<&'static str> {
    let mut problems = Vec::new();
    if !valid_email(email) {
        problems.push("email is invalid");
    }
    if password.chars().count() < 12 {
        problems.push("password is too short (minimum is 12 characters)");
    }
    problems
}

/// Runs bootstrap with `env`'s settings at `clock`'s time.
pub async fn run(pool: &PgPool, env: &Env, clock: &Clock) -> Result<Outcome, BootstrapError> {
    let email = env
        .get("INITIAL_USER_EMAIL")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if email.is_empty() {
        return Ok(Outcome::NotConfigured);
    }
    let password = env
        .get("INITIAL_USER_PASSWORD")
        .unwrap_or_default()
        .to_owned();
    if password.is_empty() {
        return Err(BootstrapError::Invalid(format!(
            "{} is set but {} is missing",
            Env::key("INITIAL_USER_EMAIL"),
            Env::key("INITIAL_USER_PASSWORD")
        )));
    }
    let supplied_token = env.get("INITIAL_API_KEY").unwrap_or_default().to_owned();

    let users_table: bool = sqlx::query_scalar("SELECT to_regclass('public.users') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if !users_table {
        return Ok(Outcome::NotConfigured);
    }

    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADVISORY_LOCK_KEY)
        .execute(&mut *tx)
        .await?;
    let any_user: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(&mut *tx)
        .await?;
    if any_user {
        tx.rollback().await?;
        return Ok(Outcome::AlreadyBootstrapped);
    }

    let problems = validate_user(&email, &password);
    if !problems.is_empty() {
        return Err(BootstrapError::Invalid(format!(
            "centaur-console bootstrap failed: user {}",
            problems.join(", ")
        )));
    }
    let (token, generated) = if supplied_token.is_empty() {
        (format!("iak_{}", random::hex32()), true)
    } else {
        (supplied_token, false)
    };
    if !valid_token(&token) {
        return Err(BootstrapError::Invalid(
            "centaur-console bootstrap failed: the API key must be iak_ followed by 64 \
             lowercase hex characters"
                .into(),
        ));
    }

    let now = clock.now().naive_utc();
    let digest = password::hash(&password, password::COST).await;
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, admin, status, password_digest, created_at, updated_at) \
         VALUES ($1, true, 'active', $2, $3, $3) RETURNING id",
    )
    .bind(&email)
    .bind(digest)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    ensure_default_infra_role(&mut tx, user_id, now).await?;
    let api_key_id: i64 = sqlx::query_scalar(
        "INSERT INTO api_keys (name, token_hash, user_id, created_at, updated_at) \
         VALUES ('bootstrap', $1, $2, $3, $3) RETURNING id",
    )
    .bind(hash_token(&token))
    .bind(user_id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut line = format!(
        "centaur-console bootstrap: created user id={user_id} email={email} api_key_id={api_key_id}"
    );
    if generated {
        line.push_str(&format!(" api_key={token}"));
    }
    tracing::info!("{line}");
    Ok(Outcome::Created {
        user_id,
        api_key_id,
    })
}

/// The `infra` role: created (name "Infra", assigned by default) or flagged
/// `assign_by_default`.
async fn ensure_default_infra_role(
    conn: &mut sqlx::PgConnection,
    created_by_id: i64,
    now: chrono::NaiveDateTime,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE roles SET assign_by_default = true, updated_at = $1 \
         WHERE foreign_id = 'infra' AND NOT assign_by_default",
    )
    .bind(now)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() == 0 {
        sqlx::query(
            "INSERT INTO roles (foreign_id, name, labels, assign_by_default, created_by_id, \
                                created_at, updated_at) \
             SELECT 'infra', 'Infra', '{\"managed-by\":\"centaur\"}'::jsonb, true, $1, $2, $2 \
             WHERE NOT EXISTS (SELECT 1 FROM roles WHERE foreign_id = 'infra')",
        )
        .bind(created_by_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use centaur_console_db::testing::TestDb;
    use sqlx::Row;

    use super::*;

    const VALID_TOKEN: &str =
        "iak_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn env(email: Option<&str>, password: Option<&str>, api_key: Option<&str>) -> Env {
        let mut pairs = Vec::new();
        if let Some(email) = email {
            pairs.push(("CENTAUR_CONSOLE_INITIAL_USER_EMAIL", email));
        }
        if let Some(password) = password {
            pairs.push(("CENTAUR_CONSOLE_INITIAL_USER_PASSWORD", password));
        }
        if let Some(api_key) = api_key {
            pairs.push(("CENTAUR_CONSOLE_INITIAL_API_KEY", api_key));
        }
        Env::from_pairs(pairs)
    }

    fn clock() -> Clock {
        Clock::fixed(chrono::DateTime::from_timestamp(1_794_538_800, 0).unwrap())
    }

    async fn users(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_no_op_without_an_email_or_when_a_user_exists() {
        let Some(db) = TestDb::migrated().await else {
            return;
        };
        assert_eq!(
            run(&db.pool, &env(None, None, None), &clock())
                .await
                .unwrap(),
            Outcome::NotConfigured
        );
        assert_eq!(users(&db.pool).await, 0);

        sqlx::query(
            "INSERT INTO users (email, admin, status, created_at, updated_at) VALUES ('preexisting@example.com', false, 'pending', now(), now())",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let outcome = run(
            &db.pool,
            &env(
                Some("boot@example.com"),
                Some("password123456"),
                Some(VALID_TOKEN),
            ),
            &clock(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::AlreadyBootstrapped);
        assert_eq!(users(&db.pool).await, 1);
        let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(keys, 0);
        db.drop_database().await;
    }

    #[tokio::test]
    async fn creates_the_user_the_infra_role_and_the_supplied_key() {
        let Some(db) = TestDb::migrated().await else {
            return;
        };
        run(
            &db.pool,
            &env(
                Some(" Boot@Example.com "),
                Some("password123456"),
                Some(VALID_TOKEN),
            ),
            &clock(),
        )
        .await
        .unwrap();
        let row = sqlx::query(
            "SELECT id, admin, status, password_digest FROM users WHERE email = 'boot@example.com'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let user_id: i64 = row.get("id");
        assert!(row.get::<bool, _>("admin"));
        assert_eq!(row.get::<String, _>("status"), "active");
        let digest: String = row.get("password_digest");
        assert!(password::verify("password123456", Some(&digest)).await);
        assert!(digest.starts_with("$2a$12$"));
        let (key_user, key_name): (i64, String) =
            sqlx::query_as("SELECT user_id, name FROM api_keys WHERE token_hash = $1")
                .bind(hash_token(VALID_TOKEN))
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!((key_user, key_name.as_str()), (user_id, "bootstrap"));
        let (created_by, assign_by_default, name, labels): (i64, bool, String, serde_json::Value) = sqlx::query_as(
            "SELECT created_by_id, assign_by_default, name, labels FROM roles WHERE foreign_id = 'infra'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            (created_by, assign_by_default, name.as_str()),
            (user_id, true, "Infra")
        );
        assert_eq!(labels, serde_json::json!({"managed-by": "centaur"}));
        db.drop_database().await;
    }

    #[tokio::test]
    async fn generates_an_api_key_when_none_is_supplied() {
        let Some(db) = TestDb::migrated().await else {
            return;
        };
        run(
            &db.pool,
            &env(Some("boot@example.com"), Some("password123456"), None),
            &clock(),
        )
        .await
        .unwrap();
        let hashes: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM api_keys")
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].len(), 64);
        db.drop_database().await;
    }

    #[tokio::test]
    async fn refuses_a_missing_password_and_malformed_keys() {
        let Some(db) = TestDb::migrated().await else {
            return;
        };
        let error = run(
            &db.pool,
            &env(Some("boot@example.com"), None, None),
            &clock(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "CENTAUR_CONSOLE_INITIAL_USER_EMAIL is set but CENTAUR_CONSOLE_INITIAL_USER_PASSWORD is missing"
        );
        let bad_tokens = [
            format!("wrong_{}", "a".repeat(64)),
            "iak_abc".to_owned(),
            format!("iak_{}", "g".repeat(64)),
            format!("iak_{}", "A".repeat(64)),
        ];
        for bad in bad_tokens {
            let error = run(
                &db.pool,
                &env(Some("boot@example.com"), Some("password123456"), Some(&bad)),
                &clock(),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, BootstrapError::Invalid(_)), "{bad}");
            assert!(
                error
                    .to_string()
                    .starts_with("centaur-console bootstrap failed: the API key must be"),
                "{error}"
            );
            assert_eq!(users(&db.pool).await, 0, "rolled back");
        }
        let error = run(
            &db.pool,
            &env(Some("not-an-email"), Some("short"), None),
            &clock(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "centaur-console bootstrap failed: user email is invalid, password is too short (minimum is 12 characters)"
        );
        db.drop_database().await;
    }
}
