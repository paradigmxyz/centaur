//! Console schema migration and bootstrap, each against a scratch database.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to any database on a server where that
//! role may create databases. Each test creates and drops its own.

use centaur_console_db::{
    Bootstrap, InitialAdmin, SYSTEM_USER_EMAIL, SetupError, bootstrap_initial_admin, migrate,
    rails_versions, system_user_id,
};
use sqlx::{
    Connection, Executor, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

mod common;

const BASELINE: &str = include_str!("../migrations/0001_baseline.sql");

/// A database created for one test and dropped with it.
struct Scratch {
    server: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl Scratch {
    async fn new() -> Option<Self> {
        let pool = common::database().await?;
        let server = pool.connect_options().as_ref().clone();
        let name = format!("console_migrate_{:016x}", rand::random::<u64>());
        let mut admin = PgConnection::connect_with(&server).await.unwrap();
        admin
            .execute(format!("CREATE DATABASE {name} TEMPLATE template0").as_str())
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(server.clone().database(&name))
            .await
            .unwrap();
        Some(Self { server, name, pool })
    }

    /// The schema exactly as the retired Rails Console left it: the baseline,
    /// without sqlx's table.
    async fn load_rails_schema(&self) {
        self.pool.execute(BASELINE).await.unwrap();
    }

    async fn drop(self) {
        self.pool.close().await;
        let mut admin = PgConnection::connect_with(&self.server).await.unwrap();
        admin
            .execute(format!("DROP DATABASE {} WITH (FORCE)", self.name).as_str())
            .await
            .unwrap();
    }
}

async fn baseline_recorded(pool: &PgPool) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = 1)")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn migrates_an_empty_database() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    migrate(&db.pool).await.unwrap();
    assert!(baseline_recorded(&db.pool).await);
    // The retired Rails Console's bookkeeping says it is current, so a rollback
    // to a chart that runs `db:prepare` migrates nothing rather than loading the
    // schema dump.
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM schema_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(recorded, rails_versions().len() as i64);
    migrate(&db.pool).await.unwrap();
    db.drop().await;
}

#[tokio::test]
async fn adopts_a_schema_rails_migrated() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    db.load_rails_schema().await;
    sqlx::query(
        "INSERT INTO users (email, status, created_at, updated_at) \
         VALUES ('existing@example.com', 'active', now(), now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    migrate(&db.pool).await.unwrap();
    assert!(baseline_recorded(&db.pool).await);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(users, 1);
    db.drop().await;
}

#[tokio::test]
async fn refuses_a_schema_rails_left_behind() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    db.load_rails_schema().await;
    let last = rails_versions().into_iter().next_back().unwrap();
    sqlx::query("DELETE FROM schema_migrations WHERE version = $1")
        .bind(last)
        .execute(&db.pool)
        .await
        .unwrap();
    let error = migrate(&db.pool).await.unwrap_err();
    assert!(
        matches!(&error, SetupError::Schema(message) if message.contains(last)),
        "{error}"
    );
    db.drop().await;
}

#[tokio::test]
async fn refuses_rails_migrations_the_baseline_lacks() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    db.load_rails_schema().await;
    sqlx::query("INSERT INTO schema_migrations (version) VALUES ('29991231000000')")
        .execute(&db.pool)
        .await
        .unwrap();
    let error = migrate(&db.pool).await.unwrap_err();
    assert!(
        matches!(&error, SetupError::Schema(message) if message.contains("29991231000000")),
        "{error}"
    );
    db.drop().await;
}

#[tokio::test]
async fn refuses_tables_without_history() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    db.pool
        .execute("CREATE TABLE users (id bigint)")
        .await
        .unwrap();
    assert!(matches!(
        migrate(&db.pool).await.unwrap_err(),
        SetupError::Schema(_)
    ));
    db.drop().await;
}

#[tokio::test]
async fn concurrent_migrations_apply_once() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    db.load_rails_schema().await;
    let (first, second) = tokio::join!(migrate(&db.pool), migrate(&db.pool));
    first.unwrap();
    second.unwrap();
    assert!(baseline_recorded(&db.pool).await);
    db.drop().await;
}

fn admin() -> InitialAdmin {
    InitialAdmin {
        email: " Admin@Example.com ".to_owned(),
        password: "correct horse battery".to_owned(),
    }
}

#[tokio::test]
async fn bootstraps_the_initial_admin_once() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    migrate(&db.pool).await.unwrap();
    let Bootstrap::Created { user_id } = bootstrap_initial_admin(&db.pool, &admin()).await.unwrap()
    else {
        panic!("expected a new admin");
    };
    let (email, status, is_admin, digest): (String, String, bool, String) =
        sqlx::query_as("SELECT email, status, admin, password_digest FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(email, "admin@example.com");
    assert_eq!(status, "active");
    assert!(is_admin);
    assert!(digest.starts_with("$2a$"));
    assert!(bcrypt::verify("correct horse battery", &digest).unwrap());
    // Operators create API keys from the Console; the bootstrap makes none.
    let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(keys, 0);
    let (assign_by_default, labels): (bool, serde_json::Value) =
        sqlx::query_as("SELECT assign_by_default, labels FROM roles WHERE foreign_id = 'infra'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(assign_by_default);
    assert_eq!(labels, serde_json::json!({"managed-by": "centaur"}));

    assert_eq!(
        bootstrap_initial_admin(&db.pool, &admin()).await.unwrap(),
        Bootstrap::Existing
    );
    db.drop().await;
}

#[tokio::test]
async fn bootstrap_rejects_unusable_settings() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    migrate(&db.pool).await.unwrap();
    let mut short = admin();
    short.password = "short".to_owned();
    let mut bad_email = admin();
    bad_email.email = "not an email".to_owned();
    let mut no_password = admin();
    no_password.password.clear();
    for settings in [short, bad_email, no_password] {
        assert!(matches!(
            bootstrap_initial_admin(&db.pool, &settings).await,
            Err(SetupError::Bootstrap(_))
        ));
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
    db.drop().await;
}

#[tokio::test]
async fn the_system_user_is_created_once_and_does_not_block_bootstrap() {
    let Some(db) = Scratch::new().await else {
        return;
    };
    migrate(&db.pool).await.unwrap();
    let (first, second) = tokio::join!(system_user_id(&db.pool), system_user_id(&db.pool));
    let id = first.unwrap();
    assert_eq!(second.unwrap(), id);
    assert_eq!(system_user_id(&db.pool).await.unwrap(), id);
    let (email, status, is_admin, digest): (String, String, bool, Option<String>) =
        sqlx::query_as("SELECT email, status, admin, password_digest FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(email, SYSTEM_USER_EMAIL);
    assert_eq!(status, "active");
    assert!(!is_admin);
    assert_eq!(digest, None);

    // The system user is not an operator: the initial admin is still created,
    // and cannot take the reserved email.
    let mut reserved = admin();
    reserved.email = SYSTEM_USER_EMAIL.to_uppercase();
    assert!(matches!(
        bootstrap_initial_admin(&db.pool, &reserved).await,
        Err(SetupError::Bootstrap(_))
    ));
    assert!(matches!(
        bootstrap_initial_admin(&db.pool, &admin()).await.unwrap(),
        Bootstrap::Created { .. }
    ));

    // A system user that somehow got a password is refused, not written as.
    sqlx::query("UPDATE users SET password_digest = 'x' WHERE id = $1")
        .bind(id)
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(system_user_id(&db.pool).await.is_err());
    db.drop().await;
}
