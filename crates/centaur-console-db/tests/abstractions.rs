//! Tests for the shared Console abstractions against a Postgres database with
//! Console's schema.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. Each test inserts uniquely named rows, so tests can
//! share one database and run in parallel.

use centaur_console_db::areas::secrets::SecretKind;
use centaur_console_db::invalidate::{self, Scope};
use centaur_console_db::model::{self, ApiKey, ConsoleModel, StaticSecret};
use centaur_console_db::write;
use sqlx::PgPool;
use time::PrimitiveDateTime;

mod common;

use common::database;

struct Fixture {
    pool: PgPool,
    admin: i64,
    unique: String,
}

#[derive(sqlx::FromRow)]
struct ApiKeyTestRow {
    id: i64,
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    created_at: PrimitiveDateTime,
    updated_at: PrimitiveDateTime,
}

#[derive(sqlx::FromRow)]
struct SecretRow {
    id: i64,
    name: Option<String>,
    foreign_id: Option<String>,
    created_at: PrimitiveDateTime,
    updated_at: PrimitiveDateTime,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let pool = database().await?;
        let unique = format!("{:032x}", rand::random::<u128>());
        let admin = sqlx::query_scalar(
            "INSERT INTO users (email, admin, status, created_at, updated_at) \
             VALUES ($1, true, 'active', now(), now()) RETURNING id",
        )
        .bind(format!("admin-{unique}@example.com"))
        .fetch_one(&pool)
        .await
        .unwrap();
        Some(Self {
            pool,
            admin,
            unique,
        })
    }

    fn name(&self, label: &str) -> String {
        format!("{label}-{}", self.unique)
    }

    async fn principal(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, name, kind, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Example', 'slack_dm', '{}', $2, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn role(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO roles (foreign_id, name, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Example', '{}', $2, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn assign(&self, principal_id: i64, role_id: i64) {
        sqlx::query(
            "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
             VALUES ($1, $2, now(), now())",
        )
        .bind(principal_id)
        .bind(role_id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn static_secret(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO static_secrets (name, foreign_id, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, '{}', $3, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.name(&format!("{label}-foreign")))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn grant(&self, principal_id: Option<i64>, role_id: Option<i64>, secret_id: i64) {
        sqlx::query(
            "INSERT INTO grants (principal_id, role_id, static_secret_id, priority, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, 0, $4, now(), now())",
        )
        .bind(principal_id)
        .bind(role_id)
        .bind(secret_id)
        .bind(self.admin)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn broker_credential(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO broker_credentials (foreign_id, name, labels, token_endpoint, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Example', '{}', 'https://example.test/token', $2, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn token_broker_source(&self, secret_id: i64, credential_id: i64) {
        sqlx::query(
            "INSERT INTO secret_sources \
                (static_secret_id, source_type, broker_credential_id, config, created_at, updated_at) \
             VALUES ($1, 'token_broker', $2, '{}', now(), now())",
        )
        .bind(secret_id)
        .bind(credential_id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn version(&self, principal_id: i64) -> i64 {
        sqlx::query_scalar("SELECT sync_config_cache_version FROM principals WHERE id = $1")
            .bind(principal_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn invalidate_bumps_the_versions_rails_would() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let direct = fixture.principal("direct").await;
    let in_role = fixture.principal("in-role").await;
    let role = fixture.role("example").await;
    fixture.assign(in_role, role).await;
    let secret = fixture.static_secret("secret").await;
    fixture.grant(Some(direct), None, secret).await;
    fixture.grant(None, Some(role), secret).await;
    let credential = fixture.broker_credential("credential").await;
    fixture.token_broker_source(secret, credential).await;

    let mut conn = fixture.pool.acquire().await.unwrap();

    // One principal, one increment.
    invalidate::invalidate(&mut conn, Scope::Principal(direct))
        .await
        .unwrap();
    assert_eq!(fixture.version(direct).await, 1);
    assert_eq!(fixture.version(in_role).await, 0);

    // A set is deduplicated: each principal moves once.
    invalidate::invalidate(&mut conn, Scope::Principals(&[direct, direct, in_role]))
        .await
        .unwrap();
    assert_eq!(fixture.version(direct).await, 2);
    assert_eq!(fixture.version(in_role).await, 1);

    // A grant reaches its direct principal or every holder of its role.
    invalidate::invalidate(
        &mut conn,
        Scope::Grantee {
            principal_id: Some(direct),
            role_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(fixture.version(direct).await, 3);
    invalidate::invalidate(
        &mut conn,
        Scope::Grantee {
            principal_id: None,
            role_id: Some(role),
        },
    )
    .await
    .unwrap();
    assert_eq!(fixture.version(in_role).await, 2);

    // A role change reaches every principal that holds it.
    invalidate::invalidate(&mut conn, Scope::Role(role))
        .await
        .unwrap();
    assert_eq!(fixture.version(in_role).await, 3);

    // A secret write bumps each grantee once and reaches both the direct
    // grantee and the role's holders.
    invalidate::invalidate(
        &mut conn,
        Scope::Secret {
            kind: SecretKind::Static,
            id: secret,
        },
    )
    .await
    .unwrap();
    assert_eq!(fixture.version(direct).await, 4);
    assert_eq!(fixture.version(in_role).await, 4);

    // A delete bumps the same grantees while the grants still exist.
    invalidate::invalidate(
        &mut conn,
        Scope::Secret {
            kind: SecretKind::Static,
            id: secret,
        },
    )
    .await
    .unwrap();
    assert_eq!(fixture.version(direct).await, 5);
    assert_eq!(fixture.version(in_role).await, 5);

    // A broker credential reaches every principal granted a secret its
    // `token_broker` source delivers.
    invalidate::invalidate(&mut conn, Scope::BrokerCredential(credential))
        .await
        .unwrap();
    assert_eq!(fixture.version(direct).await, 6);
    assert_eq!(fixture.version(in_role).await, 6);
}

#[tokio::test]
async fn model_lookups_and_delete_follow_the_descriptor() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let secret = fixture.static_secret("lookup").await;
    let foreign_id: String =
        sqlx::query_scalar("SELECT foreign_id FROM static_secrets WHERE id = $1")
            .bind(secret)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    let by_oid =
        model::find_by_oid::<StaticSecret, SecretRow, _>(&fixture.pool, &StaticSecret::oid(secret))
            .await
            .unwrap()
            .expect("the row exists");
    assert_eq!(by_oid.name, Some(fixture.name("lookup")));
    assert_eq!(by_oid.foreign_id.as_deref(), Some(foreign_id.as_str()));
    assert!(
        model::find_by_oid::<StaticSecret, SecretRow, _>(&fixture.pool, "ssr_not-an-oid")
            .await
            .unwrap()
            .is_none()
    );
    let by_foreign =
        model::find_by_foreign_id::<StaticSecret, SecretRow, _>(&fixture.pool, &foreign_id)
            .await
            .unwrap()
            .expect("foreign_id lookup");
    assert_eq!(by_foreign.id, secret);
    let by_ident = model::find_by_ident::<StaticSecret, SecretRow, _>(&fixture.pool, &foreign_id)
        .await
        .unwrap()
        .expect("ident lookup falls back to foreign_id");
    assert_eq!(by_ident.id, secret);
    let by_ident_oid = model::find_by_ident::<StaticSecret, SecretRow, _>(
        &fixture.pool,
        &StaticSecret::oid(secret),
    )
    .await
    .unwrap()
    .expect("ident lookup accepts an oid");
    assert_eq!(by_ident_oid.id, secret);

    assert_eq!(
        StaticSecret::not_found().to_string(),
        "StaticSecret not found"
    );

    assert_eq!(
        model::delete::<StaticSecret, _>(&fixture.pool, secret)
            .await
            .unwrap(),
        1
    );
    assert!(
        model::find_by_oid::<StaticSecret, SecretRow, _>(&fixture.pool, &StaticSecret::oid(secret))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn api_keys_are_soft_deleted_without_touching_updated_at() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let token_hash = format!("{:064x}", rand::random::<u128>());
    let sql = write::insert_returning_sql(
        ApiKey::TABLE,
        &["user_id", "name", "token_hash"],
        "id, name, created_at, updated_at",
    );
    let row: ApiKeyTestRow = sqlx::query_as(&sql)
        .bind(fixture.admin)
        .bind(fixture.name("key"))
        .bind(&token_hash)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(row.created_at, row.updated_at);
    // A soft delete writes only `deleted_at`, so mark updated_at first and
    // require it to survive.
    sqlx::query("UPDATE api_keys SET updated_at = created_at - interval '1 day' WHERE id = $1")
        .bind(row.id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let before: PrimitiveDateTime =
        sqlx::query_scalar("SELECT updated_at FROM api_keys WHERE id = $1")
            .bind(row.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    assert_eq!(
        model::delete::<ApiKey, _>(&fixture.pool, row.id)
            .await
            .unwrap(),
        1
    );
    assert!(
        model::find_by_oid::<ApiKey, ApiKeyTestRow, _>(&fixture.pool, &ApiKey::oid(row.id))
            .await
            .unwrap()
            .is_none(),
        "the soft-deleted row is out of the model scope"
    );
    let (deleted_at, updated_at): (Option<PrimitiveDateTime>, PrimitiveDateTime) =
        sqlx::query_as("SELECT deleted_at, updated_at FROM api_keys WHERE id = $1")
            .bind(row.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(deleted_at.is_some());
    assert_eq!(updated_at, before);
}

#[tokio::test]
async fn write_helpers_bump_updated_at_only_on_a_change() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let sql = write::insert_returning_sql(
        StaticSecret::TABLE,
        &["name", "foreign_id", "created_by_id"],
        "id, name, foreign_id, created_at, updated_at",
    );
    let row: SecretRow = sqlx::query_as(&sql)
        .bind(fixture.name("written"))
        .bind(fixture.name("written-foreign"))
        .bind(fixture.admin)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(row.created_at, row.updated_at);
    sqlx::query(
        "UPDATE static_secrets SET updated_at = created_at - interval '1 day' WHERE id = $1",
    )
    .bind(row.id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let old_updated_at: PrimitiveDateTime =
        sqlx::query_scalar("SELECT updated_at FROM static_secrets WHERE id = $1")
            .bind(row.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    // Writing the same name affects no row and leaves updated_at alone.
    let sql = write::update_changed_sql(StaticSecret::TABLE, &["name"]);
    let affected = sqlx::query(&sql)
        .bind(&row.name)
        .bind(row.id)
        .execute(&fixture.pool)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(affected, 0);
    let unchanged: PrimitiveDateTime =
        sqlx::query_scalar("SELECT updated_at FROM static_secrets WHERE id = $1")
            .bind(row.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(unchanged, old_updated_at);

    // A real change bumps updated_at.
    let affected = sqlx::query(&sql)
        .bind(fixture.name("renamed"))
        .bind(row.id)
        .execute(&fixture.pool)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(affected, 1);
    let (name, changed): (Option<String>, PrimitiveDateTime) =
        sqlx::query_as("SELECT name, updated_at FROM static_secrets WHERE id = $1")
            .bind(row.id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(name, Some(fixture.name("renamed")));
    assert!(changed > old_updated_at);
}
