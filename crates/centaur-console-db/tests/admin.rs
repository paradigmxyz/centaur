//! DB-backed tests for the admin area's converted queries and writes.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. Each test inserts uniquely named rows, so tests can
//! share one database and run in parallel.

use centaur_console_db::areas::admin::{
    PROXY_LIST_FROM, PROXY_LIST_ORDER, PROXY_SELECT, ProxyChange, ProxyRow, SystemSettingValues,
    admin_proxy, admin_user, admin_users, api_client, approve_user, attach_api_client_principal,
    create_api_client, create_proxy, delete_api_client, delete_proxy, demote_user,
    detach_api_client_principal, disable_user, existing_role_ids, find_api_client_link,
    find_api_client_principal, make_default_api_client_principal, promote_user,
    replace_default_roles, roles, rotate_api_client_secret, set_api_client_disabled,
    system_setting, update_api_client, update_proxy, write_system_settings,
};
use sqlx::PgPool;
use sqlx::types::Json;

mod common;

use common::database;

struct Fixture {
    pool: PgPool,
    admin: i64,
    unique: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let pool = database().await?;
        let unique = format!("{:032x}", rand::random::<u128>());
        let admin: i64 = sqlx::query_scalar(
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

    async fn user(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (email, status, created_at, updated_at) \
             VALUES ($1, 'pending', now(), now()) RETURNING id",
        )
        .bind(format!("{}-{label}@example.com", self.unique))
        .fetch_one(&self.pool)
        .await
        .unwrap()
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
}

#[tokio::test]
async fn users_read_and_every_action_follow_the_rails_writes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let user = fixture.user("pending").await;

    let listed = admin_users(&fixture.pool, Some("pending")).await.unwrap();
    assert!(listed.iter().any(|row| row.id == user));
    assert!(
        admin_users(&fixture.pool, Some("disabled"))
            .await
            .unwrap()
            .iter()
            .all(|row| row.status == "disabled")
    );

    let row = admin_users(&fixture.pool, Some("active"))
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == fixture.admin)
        .expect("the admin is active");
    assert!(row.admin);
    assert_eq!(row.approved_by, None);

    approve_user(&fixture.pool, user, fixture.admin)
        .await
        .unwrap();
    let approved = admin_user(&fixture.pool, user)
        .await
        .unwrap()
        .expect("the user exists");
    assert_eq!(approved.status, "active");
    assert!(approved.approved_at.is_some());
    assert_eq!(
        approved.approved_by,
        Some(centaur_console_db::areas::admin::ApproverRow {
            id: fixture.admin,
            email: format!("admin-{}@example.com", fixture.unique),
        })
    );

    disable_user(&fixture.pool, user).await.unwrap();
    assert_eq!(
        admin_user(&fixture.pool, user)
            .await
            .unwrap()
            .unwrap()
            .status,
        "disabled"
    );
    promote_user(&fixture.pool, user).await.unwrap();
    let promoted = admin_user(&fixture.pool, user).await.unwrap().unwrap();
    assert!(promoted.admin);
    assert_eq!(promoted.status, "active");
    demote_user(&fixture.pool, user).await.unwrap();
    assert!(
        !admin_user(&fixture.pool, user)
            .await
            .unwrap()
            .unwrap()
            .admin
    );
}

#[tokio::test]
async fn api_clients_round_trip_through_the_shared_helpers() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture.principal("attached").await;
    let digest = format!("{:064x}", rand::random::<u128>());
    let id = create_api_client(
        &fixture.pool,
        &fixture.name("client"),
        &["sessions:read".to_owned()],
        fixture.admin,
        &digest,
        Some(principal),
    )
    .await
    .unwrap();

    let client = api_client(&fixture.pool, id).await.unwrap().unwrap();
    assert_eq!(client.client.name, fixture.name("client"));
    assert_eq!(client.client.scopes, ["sessions:read"]);
    assert_eq!(client.links.len(), 1);
    assert!(client.links[0].default);

    update_api_client(
        &fixture.pool,
        id,
        &fixture.name("renamed"),
        &["sessions:read".to_owned(), "sessions:write".to_owned()],
    )
    .await
    .unwrap();
    let renamed = api_client(&fixture.pool, id).await.unwrap().unwrap();
    assert_eq!(renamed.client.name, fixture.name("renamed"));
    assert_eq!(renamed.client.scopes.len(), 2);

    let rotated = format!("{:064x}", rand::random::<u128>());
    rotate_api_client_secret(&fixture.pool, id, &rotated)
        .await
        .unwrap();
    let stored: String = sqlx::query_scalar("SELECT secret_digest FROM api_clients WHERE id = $1")
        .bind(id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(stored, rotated);

    set_api_client_disabled(&fixture.pool, id, true)
        .await
        .unwrap();
    assert!(
        api_client(&fixture.pool, id)
            .await
            .unwrap()
            .unwrap()
            .client
            .disabled()
    );
    set_api_client_disabled(&fixture.pool, id, false)
        .await
        .unwrap();
    assert!(
        !api_client(&fixture.pool, id)
            .await
            .unwrap()
            .unwrap()
            .client
            .disabled()
    );

    let other = fixture.principal("other").await;
    let attached = attach_api_client_principal(&fixture.pool, id, other)
        .await
        .unwrap();
    assert!(!attached.default);
    let made_default = make_default_api_client_principal(&fixture.pool, id, attached.id)
        .await
        .unwrap()
        .expect("the link exists");
    assert!(made_default.default);
    assert_eq!(
        find_api_client_link(&fixture.pool, id, attached.id)
            .await
            .unwrap(),
        Some(made_default.clone())
    );
    detach_api_client_principal(&fixture.pool, id, attached.id)
        .await
        .unwrap();
    assert!(
        find_api_client_link(&fixture.pool, id, attached.id)
            .await
            .unwrap()
            .is_none()
    );

    assert_eq!(
        find_api_client_principal(&fixture.pool, &centaur_console_db::oid("prn", principal))
            .await
            .unwrap()
            .unwrap()
            .id,
        principal
    );
    assert_eq!(
        find_api_client_principal(&fixture.pool, &fixture.name("attached"))
            .await
            .unwrap()
            .unwrap()
            .id,
        principal
    );
    assert_eq!(
        find_api_client_principal(&fixture.pool, "").await.unwrap(),
        None
    );
    assert_eq!(
        find_api_client_principal(&fixture.pool, &fixture.name("missing"))
            .await
            .unwrap(),
        None
    );

    delete_api_client(&fixture.pool, id).await.unwrap();
    assert!(api_client(&fixture.pool, id).await.unwrap().is_none());
}

#[tokio::test]
async fn proxies_round_trip_and_the_list_filter_selects_its_rows() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture.principal("proxy").await;
    let digest = format!("{:064x}", rand::random::<u128>());
    let reserved = create_proxy(
        &fixture.pool,
        &fixture.name("assigned"),
        &serde_json::json!({"env": "prod"}),
        &digest,
        Some(principal),
        None,
    )
    .await
    .unwrap();
    assert_eq!(reserved.principal_id, Some(principal));
    assert!(reserved.principal_assigned_at.is_some());
    assert!(reserved.requester_principal_assigned_at.is_none());

    let unassigned = create_proxy(
        &fixture.pool,
        &fixture.name("unassigned"),
        &serde_json::json!({}),
        &format!("{:064x}", rand::random::<u128>()),
        None,
        Some(principal),
    )
    .await
    .unwrap();
    assert!(unassigned.principal_assigned_at.is_none());
    assert!(unassigned.requester_principal_assigned_at.is_some());

    let labels: Option<Json<serde_json::Value>> = Some(Json(serde_json::json!({"env": "prod"})));
    let rows: Vec<ProxyRow> = sqlx::query_as(&format!(
        "{PROXY_SELECT} {PROXY_LIST_FROM} {PROXY_LIST_ORDER} LIMIT $3 OFFSET $4"
    ))
    .bind(Some(principal))
    .bind(labels)
    .bind(50_i64)
    .bind(0_i64)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(rows, std::slice::from_ref(&reserved));

    let updated = update_proxy(
        &fixture.pool,
        reserved.id,
        &ProxyChange {
            name: Some(fixture.name("renamed")),
            labels: None,
            principal_id: Some(None),
            requester_principal_id: None,
        },
    )
    .await
    .unwrap()
    .expect("the proxy exists");
    assert_eq!(updated.name, fixture.name("renamed"));
    assert_eq!(updated.principal_id, None);
    assert!(updated.principal_assigned_at.is_none());
    assert_eq!(updated.labels, serde_json::json!({"env": "prod"}));

    assert_eq!(
        admin_proxy(&fixture.pool, reserved.id).await.unwrap(),
        Some(updated)
    );
    delete_proxy(&fixture.pool, reserved.id).await.unwrap();
    assert!(
        admin_proxy(&fixture.pool, reserved.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn system_settings_write_only_when_a_value_changes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    // Role flags are deployment-wide: snapshot them so the shared test
    // database is left as it was found.
    let role_flags: Vec<(i64, bool)> =
        sqlx::query_as("SELECT id, assign_by_default FROM roles ORDER BY id")
            .fetch_all(&fixture.pool)
            .await
            .unwrap();
    let setting = system_setting(&fixture.pool).await.unwrap();
    write_system_settings(&fixture.pool, setting.id, &setting.values, None)
        .await
        .unwrap();
    let unchanged = system_setting(&fixture.pool).await.unwrap();
    assert_eq!(unchanged.updated_at, setting.updated_at);

    let role = fixture.role("default").await;
    let found = existing_role_ids(&fixture.pool, &[role, i64::MAX - 1])
        .await
        .unwrap();
    assert_eq!(found, [role]);

    let mut values: SystemSettingValues = setting.values.clone();
    values.default_sandbox_sessions_read_enabled = !values.default_sandbox_sessions_read_enabled;
    write_system_settings(&fixture.pool, setting.id, &values, Some(&[role]))
        .await
        .unwrap();
    let stored = system_setting(&fixture.pool).await.unwrap();
    assert_eq!(stored.values, values);
    assert!(stored.updated_at > setting.updated_at);
    let role_row = roles(&fixture.pool)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == role)
        .expect("the role is listed");
    assert!(role_row.assign_by_default);

    replace_default_roles(&fixture.pool, &[]).await.unwrap();
    let reset = roles(&fixture.pool)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == role)
        .unwrap();
    assert!(!reset.assign_by_default);

    // Restore the previous settings and role flags so a shared test database
    // keeps its values.
    write_system_settings(&fixture.pool, setting.id, &setting.values, None)
        .await
        .unwrap();
    for (id, assign_by_default) in role_flags {
        sqlx::query("UPDATE roles SET assign_by_default = $2 WHERE id = $1")
            .bind(id)
            .bind(assign_by_default)
            .execute(&fixture.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_system_user_is_hidden_from_operators() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let system = centaur_console_db::system_user_id(&fixture.pool)
        .await
        .unwrap();

    let listed = admin_users(&fixture.pool, None).await.unwrap();
    assert!(listed.iter().all(|user| user.id != system));
    // Every user action loads its target through `admin_user`, so none can
    // reach the system user.
    assert!(admin_user(&fixture.pool, system).await.unwrap().is_none());
    let candidates = centaur_console_db::areas::automation::skill_editor_candidates(
        &fixture.pool,
        fixture.admin,
        Some("centaur.invalid"),
    )
    .await
    .unwrap();
    assert!(candidates.is_empty());
    assert!(
        centaur_console_db::areas::automation::resolve_editor_user(
            &fixture.pool,
            centaur_console_db::SYSTEM_USER_EMAIL,
            false
        )
        .await
        .unwrap()
        .is_none()
    );
}
