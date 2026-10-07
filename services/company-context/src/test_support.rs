use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, Executor, PgConnection, PgPool};
use tokio::sync::Mutex;

use crate::database;

/// Migrations create cluster-global roles with a check-then-create, so
/// concurrently migrating test databases would race on `pg_authid`.
static MIGRATIONS: Mutex<()> = Mutex::const_new(());

/// A freshly migrated database that is dropped at the end of a test.
pub struct TestDatabase {
    pub pool: PgPool,
    admin: PgConnection,
    name: String,
}

impl TestDatabase {
    pub async fn create(database_url: &str, label: &str) -> Self {
        let mut admin = PgConnection::connect(database_url).await.unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("company_context_{label}_{}_{nanos}", std::process::id());
        admin
            .execute(format!(r#"create database "{name}""#).as_str())
            .await
            .unwrap();
        let mut url = url::Url::parse(database_url).unwrap();
        url.set_path(&name);
        let pool = {
            let _guard = MIGRATIONS.lock().await;
            database::connect_and_migrate(url.as_str()).await.unwrap()
        };
        Self { pool, admin, name }
    }

    pub async fn drop(mut self) {
        self.pool.close().await;
        self.admin
            .execute(format!(r#"drop database if exists "{}""#, self.name).as_str())
            .await
            .unwrap();
    }
}
