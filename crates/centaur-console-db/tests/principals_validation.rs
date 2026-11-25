//! The principals area's validation against a Postgres database holding
//! Console's schema.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. Every test creates uniquely named rows and removes
//! them again.
//!
//! These tests pin what the write paths accept and reject: the field rules are
//! `garde` validators on the request, while `foreign_id` uniqueness, a stored
//! non-hash `labels`, and the changed-field conditions stay in the write path.

use centaur_console_db::{
    areas::principals::{
        Grantee, PermissionWrite, PrincipalError, PrincipalWrite, RoleWrite, create_principal,
        create_role, replace_slack_channel_permissions, upsert_principal, upsert_role,
    },
    model::{ConsoleModel, Principal as PrincipalModel, Role as RoleModel},
};
use sqlx::PgPool;

mod common;

use common::database;

struct Fixture {
    pool: PgPool,
    unique: String,
    user_id: i64,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let pool = database().await?;
        let unique = format!("{:016x}", rand::random::<u64>());
        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, name, admin, status, created_at, updated_at) \
             VALUES ($1, $2, true, 'active', now(), now()) RETURNING id",
        )
        .bind(format!("validation-{unique}@example.com"))
        .bind(format!("Validation {unique}"))
        .fetch_one(&pool)
        .await
        .unwrap();
        Some(Self {
            pool,
            unique,
            user_id,
        })
    }

    fn name(&self, suffix: &str) -> String {
        format!("validation-{}-{suffix}", self.unique)
    }

    async fn cleanup(&self) {
        sqlx::query(
            "DELETE FROM principal_roles WHERE principal_id IN \
             (SELECT id FROM principals WHERE created_by_id = $1) \
             OR role_id IN (SELECT id FROM roles WHERE created_by_id = $1)",
        )
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "DELETE FROM slack_channel_permissions WHERE principal_id IN \
                     (SELECT id FROM principals WHERE created_by_id = $1)",
        )
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM principals WHERE created_by_id = $1")
            .bind(self.user_id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM roles WHERE created_by_id = $1")
            .bind(self.user_id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(self.user_id)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}

fn keys(errors: PrincipalError) -> Vec<String> {
    let PrincipalError::Validation(errors) = errors else {
        panic!("expected a validation failure, got {errors:?}");
    };
    let mut keys: Vec<String> = errors
        .entries()
        .map(|(attribute, _)| attribute.to_owned())
        .collect();
    keys.sort();
    keys
}

/// A principal whose every field rule fails is rejected and writes nothing.
#[tokio::test]
async fn a_principal_write_rejects_every_failing_field() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let write = PrincipalWrite {
        foreign_id: Some("not url safe".to_owned()),
        kind: Some(None),
        sandbox_repo_cache: Some(Some("some".to_owned())),
        slack_user_id: Some(Some("bogus".to_owned())),
        slack_channel_id: Some(Some("bogus".to_owned())),
        slack_team_id: Some(Some("bogus".to_owned())),
        slack_email: Some(Some("bogus".to_owned())),
        ..PrincipalWrite::default()
    };
    let errors = create_principal(&fixture.pool, &write, fixture.user_id)
        .await
        .unwrap_err();
    assert_eq!(
        keys(errors),
        [
            "foreign_id",
            "kind",
            "sandbox_repo_cache",
            "slack_channel_id",
            "slack_email",
            "slack_team_id",
            "slack_user_id",
        ]
    );

    // Nothing was written.
    let created: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM principals WHERE created_by_id = $1")
            .bind(fixture.user_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(created, 0);

    // The reserved opaque-id prefix is rejected too.
    let write = PrincipalWrite {
        foreign_id: Some(format!("prn_{}", fixture.unique)),
        ..PrincipalWrite::default()
    };
    let errors = create_principal(&fixture.pool, &write, fixture.user_id)
        .await
        .unwrap_err();
    assert_eq!(keys(errors), ["foreign_id"]);

    // A duplicate foreign_id needs the database, so it is merged with the
    // request's own errors.
    let foreign_id = fixture.name("taken");
    create_principal(
        &fixture.pool,
        &PrincipalWrite {
            foreign_id: Some(foreign_id.clone()),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    let errors = create_principal(
        &fixture.pool,
        &PrincipalWrite {
            foreign_id: Some(foreign_id),
            kind: Some(Some("bogus".to_owned())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["foreign_id", "kind"]);

    fixture.cleanup().await;
}

/// A formatted field is re-validated only when the save changes it, so an
/// unrelated update of a row that already holds an unformatted value succeeds.
#[tokio::test]
async fn an_update_only_revalidates_the_fields_it_changes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let foreign_id = fixture.name("stored");
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO principals (foreign_id, kind, labels, sandbox_repo_cache, slack_user_id, \
         created_by_id, created_at, updated_at) \
         VALUES ($1, 'slack_channel', '{}', 'all', 'bogus', $2, now(), now()) RETURNING id",
    )
    .bind(&foreign_id)
    .bind(fixture.user_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let oid = PrincipalModel::oid(id);

    // Renaming does not touch `slack_user_id`.
    let (view, was_new) = upsert_principal(
        &fixture.pool,
        &oid,
        &PrincipalWrite {
            name: Some(Some("Renamed".to_owned())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert!(!was_new);
    assert_eq!(view.row.name.as_deref(), Some("Renamed"));

    // Re-sending the stored value is not a change either.
    upsert_principal(
        &fixture.pool,
        &oid,
        &PrincipalWrite {
            slack_user_id: Some(Some("bogus".to_owned())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();

    // A new value is.
    let errors = upsert_principal(
        &fixture.pool,
        &oid,
        &PrincipalWrite {
            slack_user_id: Some(Some("still-bogus".to_owned())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["slack_user_id"]);

    fixture.cleanup().await;
}

/// Linking a console user by Slack email runs only when the upsert body carries
/// `slack_email`: a `slack_dm` principal is linked when that explicit email
/// matches a Console user, an explicit non-matching email clears the link, and
/// an omitted (or blank) email leaves the stored link alone.
#[tokio::test]
async fn a_slack_dm_upsert_links_console_user_only_from_an_explicit_email() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let admin_email = format!("validation-{}@example.com", fixture.unique);
    let unmatched_email = format!("unmatched-{}@example.com", fixture.unique);
    let foreign_id = fixture.name("dm-link");

    let (view, was_new) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            kind: Some(Some("slack_dm".to_owned())),
            slack_email: Some(Some(admin_email.clone())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert!(was_new);
    assert_eq!(view.row.console_user_id, Some(fixture.user_id));

    // An omitted email never re-runs the lookup, so the link survives.
    let (view, _) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            name: Some(Some("Renamed".to_owned())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert_eq!(view.row.console_user_id, Some(fixture.user_id));

    // A supplied email with no matching Console user clears the previous link.
    let (view, _) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            slack_email: Some(Some(unmatched_email.clone())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert_eq!(view.row.console_user_id, None);

    // A supplied matching email links again.
    let (view, _) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            slack_email: Some(Some(admin_email.clone())),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert_eq!(view.row.console_user_id, Some(fixture.user_id));

    // An explicit null (or blank) email is blank, so it keeps the link.
    let (view, _) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            slack_email: Some(None),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert_eq!(view.row.console_user_id, Some(fixture.user_id));

    // A direct console_user_id still sets and clears the link when no email is
    // supplied; the email lookup never runs in that case.
    let (view, _) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            console_user_id: Some(None),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert_eq!(view.row.console_user_id, None);
    let (view, _) = upsert_principal(
        &fixture.pool,
        &foreign_id,
        &PrincipalWrite {
            console_user_id: Some(Some(fixture.user_id)),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert_eq!(view.row.console_user_id, Some(fixture.user_id));

    // Only a `slack_dm` principal has the Slack email bridge.
    let (view, was_new) = upsert_principal(
        &fixture.pool,
        &fixture.name("channel-link"),
        &PrincipalWrite {
            kind: Some(Some("slack_channel".to_owned())),
            slack_email: Some(Some(admin_email)),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    assert!(was_new);
    assert_eq!(view.row.console_user_id, None);

    fixture.cleanup().await;
}

#[tokio::test]
async fn a_role_write_rejects_a_duplicate_or_unsafe_foreign_id_and_stored_labels() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let errors = create_role(
        &fixture.pool,
        &RoleWrite {
            foreign_id: Some("not url safe".to_owned()),
            ..RoleWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["foreign_id"]);

    let foreign_id = fixture.name("role-taken");
    create_role(
        &fixture.pool,
        &RoleWrite {
            foreign_id: Some(foreign_id.clone()),
            ..RoleWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    let errors = create_role(
        &fixture.pool,
        &RoleWrite {
            foreign_id: Some(foreign_id),
            ..RoleWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["foreign_id"]);

    // A row whose stored labels are not a hash cannot be replaced from the API
    // (the wire type is an object), so only a stored value reaches this rule.
    let stored_id: i64 = sqlx::query_scalar(
        "INSERT INTO roles (foreign_id, name, labels, assign_by_default, created_by_id, \
         created_at, updated_at) VALUES ($1, 'Stored', '[]', false, $2, now(), now()) \
         RETURNING id",
    )
    .bind(fixture.name("role-labels"))
    .bind(fixture.user_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let errors = upsert_role(
        &fixture.pool,
        &RoleModel::oid(stored_id),
        &RoleWrite {
            name: Some(Some("Renamed".to_owned())),
            ..RoleWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["labels"]);

    fixture.cleanup().await;
}

#[tokio::test]
async fn permission_rows_are_validated_after_normalization() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let view = create_principal(
        &fixture.pool,
        &PrincipalWrite {
            foreign_id: Some(fixture.name("permissions")),
            ..PrincipalWrite::default()
        },
        fixture.user_id,
    )
    .await
    .unwrap();
    let owner = Grantee::Principal(view.row.id);

    // A blank channel id and no enabled flag fail both rules.
    let errors = replace_slack_channel_permissions(
        &fixture.pool,
        owner,
        &[PermissionWrite {
            channel_id: Some("  ".to_owned()),
            ..PermissionWrite::default()
        }],
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["base", "channel_id"]);

    // Only the first failing row is reported: the payload renders one record.
    let errors = replace_slack_channel_permissions(
        &fixture.pool,
        owner,
        &[
            PermissionWrite {
                channel_id: Some("C0DIFFVALID1".to_owned()),
                upload_enabled: Some(true),
                ..PermissionWrite::default()
            },
            PermissionWrite {
                channel_id: Some("nope".to_owned()),
                upload_enabled: Some(true),
                ..PermissionWrite::default()
            },
            PermissionWrite {
                channel_id: Some("nope-too".to_owned()),
                upload_enabled: Some(true),
                ..PermissionWrite::default()
            },
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(keys(errors), ["channel_id"]);

    // A lower-case id is upper-cased before the format rule, so it passes.
    let rows = replace_slack_channel_permissions(
        &fixture.pool,
        owner,
        &[PermissionWrite {
            channel_id: Some(" c0diffvalid1 ".to_owned()),
            download_enabled: Some(true),
            ..PermissionWrite::default()
        }],
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].channel_id, "C0DIFFVALID1");

    fixture.cleanup().await;
}

/// Concurrent first-time upserts of one foreign id (api-rs registers the same
/// Slack requester from parallel turns) all succeed and leave one row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_upserts_of_one_foreign_id_all_succeed() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect_with((*fixture.pool.connect_options()).clone())
        .await
        .unwrap();

    let principal = fixture.name("race-principal");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (pool, principal, user) = (pool.clone(), principal.clone(), fixture.user_id);
        tasks.spawn(async move {
            upsert_principal(
                &pool,
                &principal,
                &PrincipalWrite {
                    name: Some(Some("Race".to_owned())),
                    kind: Some(Some("workflow".to_owned())),
                    ..PrincipalWrite::default()
                },
                user,
            )
            .await
            .map(|(_, was_new)| was_new)
        });
    }
    let mut created = 0;
    while let Some(result) = tasks.join_next().await {
        created += usize::from(result.unwrap().expect("every upsert succeeds"));
    }
    assert_eq!(created, 1, "exactly one upsert creates the principal");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM principals WHERE foreign_id = $1")
        .bind(&principal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);

    let role = fixture.name("race-role");
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (pool, role, user) = (pool.clone(), role.clone(), fixture.user_id);
        tasks.spawn(async move {
            upsert_role(
                &pool,
                &role,
                &RoleWrite {
                    name: Some(Some("Race".to_owned())),
                    ..RoleWrite::default()
                },
                user,
            )
            .await
            .map(|(_, was_new)| was_new)
        });
    }
    let mut created = 0;
    while let Some(result) = tasks.join_next().await {
        created += usize::from(result.unwrap().expect("every upsert succeeds"));
    }
    assert_eq!(created, 1, "exactly one upsert creates the role");

    fixture.cleanup().await;
}
