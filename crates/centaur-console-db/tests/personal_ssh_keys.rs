//! Granting a personal SSH key to the principals that verifiably are its
//! owner. Needs `CONSOLE_DB_TEST_DATABASE_URL`.

use centaur_console_db::{personal_ssh_keys, testing::TestDb};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_794_538_800, 0).unwrap()
}

async fn user(pool: &PgPool, email: &str, status: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO users (email, status, admin, created_at, updated_at) \
         VALUES ($1, $2, FALSE, now(), now()) RETURNING id",
    )
    .bind(email)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn key(pool: &PgPool, owner: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO ssh_key_secrets (name, public_key, owner_id, created_by_id, created_at, updated_at) \
         VALUES ('k', 'ssh-ed25519 AAAA', $1, $1, now(), now()) RETURNING id",
    )
    .bind(owner)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn principal(
    pool: &PgPool,
    kind: &str,
    user: Option<i64>,
    slack: Option<(&str, &str)>,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO principals (kind, console_user_id, slack_user_id, slack_team_id, created_by_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 1, now(), now()) RETURNING id",
    )
    .bind(kind)
    .bind(user)
    .bind(slack.map(|s| s.0))
    .bind(slack.map(|s| s.1))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn identity(pool: &PgPool, user: i64, subject: &str, team: &str) {
    sqlx::query(
        "INSERT INTO user_identities (user_id, provider, subject, team_id, created_at, updated_at) \
         VALUES ($1, 'slack', $2, $3, now(), now())",
    )
    .bind(user)
    .bind(subject)
    .bind(team)
    .execute(pool)
    .await
    .unwrap();
}

async fn grants(pool: &PgPool, principal: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM grants WHERE principal_id = $1 AND ssh_key_secret_id IS NOT NULL",
    )
    .bind(principal)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn version(pool: &PgPool, principal: i64) -> i64 {
    sqlx::query_scalar("SELECT sync_config_cache_version FROM principals WHERE id = $1")
        .bind(principal)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_key_goes_to_the_console_and_verified_slack_principals_once() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let pool = &db.pool;
    let owner = user(pool, "owner@example.test", "active").await;
    key(pool, owner).await;
    identity(pool, owner, "U0OWNER01", "T00000001").await;
    let console = principal(pool, "console_user", Some(owner), None).await;
    let dm = principal(pool, "slack_dm", None, Some(("U0OWNER01", "T00000001"))).await;
    let other_team = principal(pool, "slack_dm", None, Some(("U0OWNER01", "T00000002"))).await;
    let stranger = principal(
        pool,
        "console_user",
        Some(user(pool, "s@example.test", "active").await),
        None,
    )
    .await;

    assert_eq!(
        personal_ssh_keys::apply_for_user(pool, owner, now())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        personal_ssh_keys::apply_for_user(pool, owner, now())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        (grants(pool, console).await, grants(pool, dm).await),
        (1, 1)
    );
    assert_eq!(
        (grants(pool, other_team).await, grants(pool, stranger).await),
        (0, 0)
    );
    assert_eq!(
        (version(pool, console).await, version(pool, dm).await),
        (1, 1)
    );
    assert_eq!(version(pool, other_team).await, 0);
    let (priority, created_by): (i32, i64) =
        sqlx::query_as("SELECT priority, created_by_id FROM grants LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((priority, created_by), (100, owner));
}

#[tokio::test]
async fn an_ambiguous_or_malformed_slack_identity_grants_nothing() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let pool = &db.pool;
    let owner = user(pool, "owner@example.test", "active").await;
    key(pool, owner).await;
    identity(pool, owner, "U0OWNER01", "T00000001").await;
    identity(pool, owner, "U0OWNER02", "T00000001").await;
    let dm = principal(pool, "slack_dm", None, Some(("U0OWNER01", "T00000001"))).await;
    assert_eq!(
        personal_ssh_keys::apply_for_user(pool, owner, now())
            .await
            .unwrap(),
        0
    );
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, dm, now())
            .await
            .unwrap()
    );
    sqlx::query("DELETE FROM user_identities WHERE subject = 'U0OWNER02'")
        .execute(pool)
        .await
        .unwrap();
    assert!(
        personal_ssh_keys::apply_for_principal(pool, dm, now())
            .await
            .unwrap()
    );
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, dm, now())
            .await
            .unwrap()
    );
    assert_eq!(grants(pool, dm).await, 1);

    let other = user(pool, "other@example.test", "active").await;
    key(pool, other).await;
    identity(pool, other, "lowercase-id", "T00000001").await;
    let bad = principal(pool, "slack_dm", None, Some(("lowercase-id", "T00000001"))).await;
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, bad, now())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn only_active_owners_with_a_key_are_granted() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let pool = &db.pool;
    let pending = user(pool, "pending@example.test", "pending").await;
    key(pool, pending).await;
    let pending_principal = principal(pool, "console_user", Some(pending), None).await;
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, pending_principal, now())
            .await
            .unwrap()
    );
    let keyless = user(pool, "keyless@example.test", "active").await;
    let keyless_principal = principal(pool, "console_user", Some(keyless), None).await;
    assert_eq!(
        personal_ssh_keys::apply_for_user(pool, keyless, now())
            .await
            .unwrap(),
        0
    );
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, keyless_principal, now())
            .await
            .unwrap()
    );
    let unknown_kind = principal(pool, "unknown", Some(pending), None).await;
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, unknown_kind, now())
            .await
            .unwrap()
    );
    assert!(
        !personal_ssh_keys::apply_for_principal(pool, 424_242, now())
            .await
            .unwrap()
    );
    let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM grants")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(grants, 0);
}
