//! Console-user principal provisioning against a Postgres database holding
//! Console's schema.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. Every test creates uniquely named rows and removes
//! them again: the default role a test creates is `assign_by_default`, which is
//! global state, so leaving it behind would change how every other test's
//! principals are provisioned.
//!
//! The MCP consent flow and the scheduled-task author hook both provision a
//! console-user principal; these tests pin the row that provisioner writes
//! (labels, `created_by`, the `user-mcp` and default role assignments, and the
//! sync-config cache version the write produces) so neither path can drift.

use centaur_console_db::{
    ConsoleUser, ConsoleUserStatus, areas::auth_pages::provision_user_principal,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

mod common;

use common::database;

/// The default role is global state and cleanup removes every stale
/// `provisioning-test-default-` row, so these tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    /// Read/write, as the Console provisioner runs in api-rs.
    pool: PgPool,
    unique: String,
    user_id: i64,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let serial = SERIAL.lock().await;
        let pool = database().await?;
        let unique = format!("{:016x}", rand::random::<u64>());
        // A crashed earlier run can leave its default role behind; only this
        // test uses the `provisioning-test-default-` prefix.
        sqlx::query(
            "DELETE FROM principal_roles WHERE role_id IN \
             (SELECT id FROM roles WHERE foreign_id LIKE 'provisioning-test-default-%')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM roles WHERE foreign_id LIKE 'provisioning-test-default-%'")
            .execute(&pool)
            .await
            .unwrap();
        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, name, admin, status, created_at, updated_at) \
             VALUES ($1, $2, true, 'active', now(), now()) RETURNING id",
        )
        .bind(format!("provisioner-{unique}@example.com"))
        .bind(format!("Provisioner {unique}"))
        .fetch_one(&pool)
        .await
        .unwrap();
        Some(Self {
            pool,
            unique,
            user_id,
            _serial: serial,
        })
    }

    fn user(&self, name: Option<&str>) -> ConsoleUser {
        ConsoleUser {
            id: self.user_id,
            email: format!("provisioner-{}@example.com", self.unique),
            name: name.map(str::to_owned),
            admin: true,
            status: ConsoleUserStatus::Active,
        }
    }

    async fn principal(&self, id: i64) -> (Value, String, Option<i64>, Option<i64>, i64) {
        let row = sqlx::query(
            "SELECT labels, kind, console_user_id, created_by_id, sync_config_cache_version \
             FROM principals WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (
            row.try_get("labels").unwrap(),
            row.try_get("kind").unwrap(),
            row.try_get("console_user_id").unwrap(),
            row.try_get("created_by_id").unwrap(),
            row.try_get("sync_config_cache_version").unwrap(),
        )
    }

    async fn role_ids(&self, principal_id: i64) -> Vec<String> {
        let mut rows: Vec<String> = sqlx::query_scalar(
            "SELECT r.foreign_id FROM principal_roles pr JOIN roles r ON r.id = pr.role_id \
             WHERE pr.principal_id = $1 ORDER BY r.foreign_id",
        )
        .bind(principal_id)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        rows.sort();
        rows
    }

    async fn default_role(&self) -> String {
        let foreign_id = format!("provisioning-test-default-{}", self.unique);
        sqlx::query(
            "INSERT INTO roles (foreign_id, name, labels, assign_by_default, created_by_id, \
                               created_at, updated_at) \
             VALUES ($1, 'Default Role', '{}', true, $2, now(), now())",
        )
        .bind(&foreign_id)
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .unwrap();
        foreign_id
    }

    /// Removes the users, principals, grants, and roles this test created.
    async fn cleanup(&self, principal_ids: &[i64]) {
        for &principal_id in principal_ids {
            sqlx::query("DELETE FROM principal_roles WHERE principal_id = $1")
                .bind(principal_id)
                .execute(&self.pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM grants WHERE principal_id = $1")
                .bind(principal_id)
                .execute(&self.pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM principals WHERE id = $1")
                .bind(principal_id)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "DELETE FROM principal_roles WHERE role_id IN \
                     (SELECT id FROM roles WHERE foreign_id LIKE 'provisioning-test-default-%')",
        )
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM roles WHERE foreign_id LIKE 'provisioning-test-default-%'")
            .execute(&self.pool)
            .await
            .unwrap();
        // The `user-mcp` role keeps `created_by_id` pointing at this user (the
        // first provisioner to run in the database), so the user stays.
    }
}

#[tokio::test]
async fn provisions_the_rails_row_and_assignments() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let default_role = fixture.default_role().await;

    let principal = provision_user_principal(&fixture.pool, &fixture.user(None))
        .await
        .unwrap();
    assert!(
        principal
            .foreign_id
            .starts_with("console-user-provisioner-")
    );

    let (labels, kind, console_user_id, created_by_id, version) =
        fixture.principal(principal.id).await;
    assert_eq!(kind, "console_user");
    assert_eq!(console_user_id, Some(fixture.user_id));
    assert_eq!(created_by_id, Some(fixture.user_id));
    assert_eq!(
        labels,
        json!({ "managed-by": "centaur", "centaur.sandbox_repo_cache": "all" }),
        "the sandbox repo-cache label plus managed-by"
    );
    // The principal insert itself leaves the version at 0; the `user-mcp`
    // PrincipalRole insert bumps it once, and the default-role insert does not.
    assert_eq!(version, 1);
    let roles = fixture.role_ids(principal.id).await;
    assert!(
        roles.contains(&default_role),
        "after_create :assign_default_roles, got {roles:?}"
    );
    assert!(
        roles.contains(&"user-mcp".to_owned()),
        "assign_user_mcp_role, got {roles:?}"
    );
    fixture.cleanup(&[principal.id]).await;
}

#[tokio::test]
async fn reprovisioning_is_idempotent_and_updates_like_rails() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let first = provision_user_principal(&fixture.pool, &fixture.user(Some("First")))
        .await
        .unwrap();
    let (_, _, _, _, version) = fixture.principal(first.id).await;
    assert_eq!(version, 1);
    let roles = fixture.role_ids(first.id).await;
    assert!(roles.contains(&"user-mcp".to_owned()));

    // An unchanged re-run writes nothing and so produces no extra invalidation.
    let again = provision_user_principal(&fixture.pool, &fixture.user(Some("First")))
        .await
        .unwrap();
    assert_eq!(again.id, first.id);
    let (_, _, _, _, version) = fixture.principal(first.id).await;
    assert_eq!(version, 1);

    // A changed name is a sync-config field, so it bumps the version once.
    let renamed = provision_user_principal(&fixture.pool, &fixture.user(Some("Second")))
        .await
        .unwrap();
    assert_eq!(renamed.id, first.id);
    let (labels, _, _, _, version) = fixture.principal(first.id).await;
    assert_eq!(version, 2);
    assert_eq!(
        labels,
        json!({ "managed-by": "centaur", "centaur.sandbox_repo_cache": "all" })
    );
    // Role assignments survive the update unchanged; the `user-mcp` assignment
    // only runs for a newly created principal.
    assert_eq!(fixture.role_ids(first.id).await, roles);
    fixture.cleanup(&[first.id]).await;
}

#[tokio::test]
async fn a_blank_name_falls_back_to_the_email() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = provision_user_principal(&fixture.pool, &fixture.user(Some("   ")))
        .await
        .unwrap();
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM principals WHERE id = $1")
        .bind(principal.id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        name,
        Some(format!("provisioner-{}@example.com", fixture.unique))
    );
    fixture.cleanup(&[principal.id]).await;
}
