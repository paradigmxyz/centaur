use std::{
    env,
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{Connection, Executor, PgConnection, Row};

const SLACK_CONTEXT_RLS_SQL: &str = include_str!("../migrations/0016_slack_context_rls.sql");
const SLACK_ATTACHMENTS_RLS_SQL: &str =
    include_str!("../migrations/0017_slack_sync_message_attachments.sql");
const SLACK_CONTEXT_ADMIN_CHANNELS_SQL: &str =
    include_str!("../migrations/0018_slack_context_rls_admin_channels.sql");
const CENTAUR_READONLY_ROLE_ONLY_SQL: &str =
    include_str!("../migrations/0020_centaur_readonly_role_only.sql");
const ETL_CONTEXT_RLS_SQL: &str = include_str!("../migrations/0021_etl_context_rls.sql");
const DROP_SLACK_CONTEXT_ADMIN_CHANNELS_SQL: &str =
    include_str!("../migrations/0022_drop_slack_context_rls_admin_channels.sql");
const CENTAUR_READONLY_RLS_POLICIES_SQL: &str =
    include_str!("../migrations/0023_centaur_readonly_rls_policies.sql");
const SLACK_PRIVATE_CHANNELS_SQL: &str =
    include_str!("../migrations/0038_slack_private_channels.sql");

const RLS_TABLES: &[&str] = &[
    "slack_sync_channels",
    "slack_sync_users",
    "slack_sync_messages",
    "slack_sync_message_attachments",
    "company_context_documents",
    "google_drive_sync_runs",
    "google_drive_sync_files",
    "google_drive_sync_checkpoints",
    "google_calendar_sync_runs",
    "google_calendar_sync_calendars",
    "google_calendar_sync_events",
    "google_calendar_sync_checkpoints",
    "linear_sync_runs",
    "linear_sync_projects",
    "linear_sync_issues",
    "linear_sync_comments",
    "linear_sync_checkpoints",
];

#[derive(Debug, PartialEq, Eq)]
struct VisibleRows {
    slack_channels: Vec<String>,
    slack_users: Vec<String>,
    slack_messages: Vec<String>,
    slack_attachments: Vec<String>,
    context_docs: Vec<String>,
    google_drive_runs: i64,
    google_drive_files: i64,
    google_drive_checkpoints: i64,
    google_calendar_runs: i64,
    google_calendar_calendars: i64,
    google_calendar_events: i64,
    google_calendar_checkpoints: i64,
    linear_runs: i64,
    linear_projects: i64,
    linear_issues: i64,
    linear_comments: i64,
    linear_checkpoints: i64,
}

#[tokio::test]
async fn etl_context_rls_enforces_channel_visibility() -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        return Ok(());
    };
    let mut conn = PgConnection::connect(&database_url).await?;
    let schema = TestSchema::create(&mut conn).await?;

    let result = run_rls_assertions(&mut conn, &schema.name).await;
    schema.drop(&mut conn).await?;
    result
}

async fn run_rls_assertions(conn: &mut PgConnection, schema: &str) -> Result<(), Box<dyn Error>> {
    set_search_path(conn, schema).await?;
    create_minimal_etl_tables(conn).await?;
    execute_migration(conn, SLACK_CONTEXT_RLS_SQL).await?;
    execute_migration(conn, SLACK_ATTACHMENTS_RLS_SQL).await?;
    execute_migration(conn, SLACK_CONTEXT_ADMIN_CHANNELS_SQL).await?;
    create_minimal_non_slack_etl_tables(conn).await?;
    execute_migration(conn, CENTAUR_READONLY_ROLE_ONLY_SQL).await?;
    execute_migration(conn, ETL_CONTEXT_RLS_SQL).await?;
    execute_migration(conn, DROP_SLACK_CONTEXT_ADMIN_CHANNELS_SQL).await?;
    execute_migration(conn, CENTAUR_READONLY_RLS_POLICIES_SQL).await?;
    execute_migration(conn, SLACK_PRIVATE_CHANNELS_SQL).await?;
    grant_schema_usage(conn, schema).await?;

    assert_rls_enabled(conn).await?;
    assert_expected_policies(conn).await?;
    assert_legacy_admin_state_is_removed(conn).await?;

    insert_fixture_rows(conn).await?;

    let c_alpha = visible_rows(conn, schema, "centaur_slack_reader", Some("C_ALPHA")).await?;
    assert_eq!(
        c_alpha,
        VisibleRows {
            slack_channels: vec!["C_ALPHA".to_owned()],
            slack_users: vec![],
            slack_messages: vec!["C_ALPHA:1000.000001".to_owned()],
            slack_attachments: vec!["C_ALPHA:1000.000001:F_ALPHA".to_owned()],
            context_docs: vec!["doc_slack_alpha".to_owned()],
            google_drive_runs: 0,
            google_drive_files: 0,
            google_drive_checkpoints: 0,
            google_calendar_runs: 0,
            google_calendar_calendars: 0,
            google_calendar_events: 0,
            google_calendar_checkpoints: 0,
            linear_runs: 0,
            linear_projects: 0,
            linear_issues: 0,
            linear_comments: 0,
            linear_checkpoints: 0,
        }
    );

    let c_beta = visible_rows(conn, schema, "centaur_slack_reader", Some("C_BETA")).await?;
    assert_eq!(
        c_beta,
        VisibleRows {
            slack_channels: vec!["C_BETA".to_owned()],
            slack_users: vec![],
            slack_messages: vec!["C_BETA:1000.000002".to_owned()],
            slack_attachments: vec!["C_BETA:1000.000002:F_BETA".to_owned()],
            context_docs: vec!["doc_slack_beta".to_owned()],
            google_drive_runs: 0,
            google_drive_files: 0,
            google_drive_checkpoints: 0,
            google_calendar_runs: 0,
            google_calendar_calendars: 0,
            google_calendar_events: 0,
            google_calendar_checkpoints: 0,
            linear_runs: 0,
            linear_projects: 0,
            linear_issues: 0,
            linear_comments: 0,
            linear_checkpoints: 0,
        }
    );

    let dm_or_missing_channel =
        visible_rows(conn, schema, "centaur_slack_reader", Some("")).await?;
    assert_eq!(dm_or_missing_channel, empty_visible_rows());

    let unset_channel = visible_rows(conn, schema, "centaur_slack_reader", None).await?;
    assert_eq!(unset_channel, empty_visible_rows());

    let formerly_admin_channel =
        visible_rows(conn, schema, "centaur_slack_reader", Some("C_ADMIN")).await?;
    assert_eq!(
        formerly_admin_channel,
        VisibleRows {
            slack_channels: vec!["C_ADMIN".to_owned()],
            slack_users: vec![],
            slack_messages: vec![],
            slack_attachments: vec![],
            context_docs: vec![],
            google_drive_runs: 0,
            google_drive_files: 0,
            google_drive_checkpoints: 0,
            google_calendar_runs: 0,
            google_calendar_calendars: 0,
            google_calendar_events: 0,
            google_calendar_checkpoints: 0,
            linear_runs: 0,
            linear_projects: 0,
            linear_issues: 0,
            linear_comments: 0,
            linear_checkpoints: 0,
        }
    );

    let readonly_role = visible_rows(conn, schema, "centaur_readonly", None).await?;
    assert_eq!(readonly_role, public_visible_rows());

    let readonly_private_channel =
        visible_rows(conn, schema, "centaur_readonly", Some("G_PRIVATE")).await?;
    assert_eq!(readonly_private_channel, public_and_private_visible_rows());

    Ok(())
}

fn test_database_url() -> Option<String> {
    env::var("SESSION_SQLX_TEST_DATABASE_URL")
        .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
        .map_err(|_| {
            eprintln!(
                "skipping ETL RLS tests: set SESSION_SQLX_TEST_DATABASE_URL to a Postgres URL"
            );
        })
        .ok()
}

struct TestSchema {
    name: String,
}

impl TestSchema {
    async fn create(conn: &mut PgConnection) -> Result<Self, Box<dyn Error>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let name = format!("etl_rls_{}_{}", std::process::id(), nanos);
        conn.execute(format!(r#"create schema "{}""#, name).as_str())
            .await?;
        Ok(Self { name })
    }

    async fn drop(self, conn: &mut PgConnection) -> Result<(), Box<dyn Error>> {
        conn.execute(format!(r#"drop schema if exists "{}" cascade"#, self.name).as_str())
            .await?;
        Ok(())
    }
}

async fn set_search_path(conn: &mut PgConnection, schema: &str) -> Result<(), sqlx::Error> {
    conn.execute(format!(r#"set search_path to "{}", public"#, schema).as_str())
        .await?;
    Ok(())
}

async fn grant_schema_usage(conn: &mut PgConnection, schema: &str) -> Result<(), sqlx::Error> {
    conn.execute(
        format!(
            r#"grant usage on schema "{}" to centaur_slack_reader, centaur_readonly"#,
            schema
        )
        .as_str(),
    )
    .await?;
    conn.execute(
        format!(
            r#"grant select on all tables in schema "{}" to centaur_readonly"#,
            schema
        )
        .as_str(),
    )
    .await?;
    Ok(())
}

async fn execute_migration(conn: &mut PgConnection, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sql).execute(&mut *conn).await?;
    Ok(())
}

async fn create_minimal_etl_tables(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        create table slack_sync_channels (
            channel_id text primary key,
            channel_name text not null default ''
        );

        create table slack_sync_users (
            user_id text primary key,
            user_name text not null default ''
        );

        create table slack_sync_runs (
            run_id text primary key
        );

        create table slack_sync_messages (
            channel_id text not null references slack_sync_channels(channel_id) on delete cascade,
            message_ts text not null,
            user_id text not null default '',
            text text not null default '',
            primary key (channel_id, message_ts)
        );

        create table company_context_documents (
            document_id text primary key,
            source text not null,
            source_type text not null,
            metadata jsonb not null default '{}'::jsonb
        );
        "#,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn create_minimal_non_slack_etl_tables(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        create table google_drive_sync_runs (run_id text primary key);
        create table google_drive_sync_files (file_id text primary key);
        create table google_drive_sync_checkpoints (scope_id text primary key);

        create table google_calendar_sync_runs (run_id text primary key);
        create table google_calendar_sync_calendars (calendar_id text primary key);
        create table google_calendar_sync_events (
            calendar_id text not null,
            event_id text not null,
            primary key (calendar_id, event_id)
        );
        create table google_calendar_sync_checkpoints (calendar_id text primary key);

        create table linear_sync_runs (run_id text primary key);
        create table linear_sync_projects (project_id text primary key);
        create table linear_sync_issues (issue_id text primary key);
        create table linear_sync_comments (comment_id text primary key);
        create table linear_sync_checkpoints (scope_id text primary key);
        "#,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn assert_rls_enabled(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    for table in RLS_TABLES {
        let enabled: bool = sqlx::query_scalar(
            r#"
            select relrowsecurity
            from pg_class
            where oid = to_regclass($1)::oid
            "#,
        )
        .bind(*table)
        .fetch_one(&mut *conn)
        .await?;
        assert!(enabled, "expected row level security on {table}");
    }
    Ok(())
}

async fn assert_expected_policies(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    let policies: Vec<(String, String)> = sqlx::query(
        r#"
        select tablename, policyname
        from pg_policies
        where schemaname = current_schema()
        order by tablename, policyname
        "#,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| (row.get("tablename"), row.get("policyname")))
    .collect();

    for expected in expected_policies() {
        assert!(
            policies.contains(&expected),
            "missing RLS policy {} on {}",
            expected.1,
            expected.0
        );
    }

    for table in RLS_TABLES {
        let expected = (*table, format!("centaur_readonly_{table}_select"));
        assert!(
            policies
                .iter()
                .any(|(table, policy)| table == expected.0 && policy == &expected.1),
            "missing centaur_readonly RLS policy on {table}"
        );
    }
    Ok(())
}

fn expected_policies() -> Vec<(String, String)> {
    [
        (
            "slack_sync_channels",
            "centaur_slack_channels_reader_select",
        ),
        ("slack_sync_users", "centaur_slack_users_reader_select"),
        (
            "slack_sync_messages",
            "centaur_slack_messages_reader_select",
        ),
        (
            "slack_sync_message_attachments",
            "centaur_slack_message_attachments_reader_select",
        ),
        (
            "company_context_documents",
            "centaur_context_docs_reader_select",
        ),
        (
            "company_context_documents",
            "centaur_readonly_company_context_documents_select",
        ),
        (
            "google_drive_sync_runs",
            "centaur_google_drive_runs_reader_select",
        ),
        (
            "google_drive_sync_runs",
            "centaur_readonly_google_drive_sync_runs_select",
        ),
        (
            "google_drive_sync_files",
            "centaur_google_drive_files_reader_select",
        ),
        (
            "google_drive_sync_files",
            "centaur_readonly_google_drive_sync_files_select",
        ),
        (
            "google_drive_sync_checkpoints",
            "centaur_google_drive_checkpoints_reader_select",
        ),
        (
            "google_drive_sync_checkpoints",
            "centaur_readonly_google_drive_sync_checkpoints_select",
        ),
        (
            "google_calendar_sync_runs",
            "centaur_google_calendar_runs_reader_select",
        ),
        (
            "google_calendar_sync_runs",
            "centaur_readonly_google_calendar_sync_runs_select",
        ),
        (
            "google_calendar_sync_calendars",
            "centaur_google_calendar_calendars_reader_select",
        ),
        (
            "google_calendar_sync_calendars",
            "centaur_readonly_google_calendar_sync_calendars_select",
        ),
        (
            "google_calendar_sync_events",
            "centaur_google_calendar_events_reader_select",
        ),
        (
            "google_calendar_sync_events",
            "centaur_readonly_google_calendar_sync_events_select",
        ),
        (
            "google_calendar_sync_checkpoints",
            "centaur_google_calendar_checkpoints_reader_select",
        ),
        (
            "google_calendar_sync_checkpoints",
            "centaur_readonly_google_calendar_sync_checkpoints_select",
        ),
        ("linear_sync_runs", "centaur_linear_runs_reader_select"),
        (
            "linear_sync_runs",
            "centaur_readonly_linear_sync_runs_select",
        ),
        (
            "linear_sync_projects",
            "centaur_linear_projects_reader_select",
        ),
        (
            "linear_sync_projects",
            "centaur_readonly_linear_sync_projects_select",
        ),
        ("linear_sync_issues", "centaur_linear_issues_reader_select"),
        (
            "linear_sync_issues",
            "centaur_readonly_linear_sync_issues_select",
        ),
        (
            "linear_sync_comments",
            "centaur_linear_comments_reader_select",
        ),
        (
            "linear_sync_comments",
            "centaur_readonly_linear_sync_comments_select",
        ),
        (
            "linear_sync_checkpoints",
            "centaur_linear_checkpoints_reader_select",
        ),
        (
            "linear_sync_checkpoints",
            "centaur_readonly_linear_sync_checkpoints_select",
        ),
        (
            "slack_sync_channels",
            "centaur_readonly_slack_sync_channels_select",
        ),
        (
            "slack_sync_users",
            "centaur_readonly_slack_sync_users_select",
        ),
        (
            "slack_sync_messages",
            "centaur_readonly_slack_sync_messages_select",
        ),
        (
            "slack_sync_message_attachments",
            "centaur_readonly_slack_sync_message_attachments_select",
        ),
    ]
    .into_iter()
    .map(|(table, policy)| (table.to_owned(), policy.to_owned()))
    .collect()
}

async fn assert_legacy_admin_state_is_removed(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    let table_name: Option<String> =
        sqlx::query_scalar("select to_regclass('slack_context_rls_admin_channels')::text")
            .fetch_one(&mut *conn)
            .await?;
    assert_eq!(
        table_name, None,
        "admin channels must be managed by iron-control"
    );

    let function_count: i64 = sqlx::query_scalar(
        "select count(*) from pg_proc where proname = 'centaur_etl_admin_channel'",
    )
    .fetch_one(&mut *conn)
    .await?;
    assert_eq!(
        function_count, 0,
        "admin-channel lookup function must be removed"
    );

    let admin_role_count: i64 =
        sqlx::query_scalar("select count(*) from pg_roles where rolname = 'centaur_slack_admin'")
            .fetch_one(&mut *conn)
            .await?;
    assert_eq!(
        admin_role_count, 0,
        "legacy slack admin DB role must be removed"
    );
    Ok(())
}

async fn insert_fixture_rows(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        insert into slack_sync_channels (channel_id, channel_name, is_private) values
            ('C_ALPHA', 'alpha', false),
            ('C_BETA', 'beta', false),
            ('C_ADMIN', 'admin', false),
            ('G_PRIVATE', 'private', true);

        insert into slack_sync_users (user_id, user_name) values
            ('U_ALPHA', 'alpha user'),
            ('U_BETA', 'beta user'),
            ('U_PRIVATE', 'private user');

        insert into slack_sync_messages (channel_id, message_ts, user_id, text) values
            ('C_ALPHA', '1000.000001', 'U_ALPHA', 'alpha channel message'),
            ('C_BETA', '1000.000002', 'U_BETA', 'beta channel message'),
            ('G_PRIVATE', '1000.000003', 'U_PRIVATE', 'private channel message');

        insert into slack_sync_message_attachments
            (channel_id, message_ts, slack_file_id, name)
        values
            ('C_ALPHA', '1000.000001', 'F_ALPHA', 'alpha.pdf'),
            ('C_BETA', '1000.000002', 'F_BETA', 'beta.pdf'),
            ('G_PRIVATE', '1000.000003', 'F_PRIVATE', 'private.pdf');

        insert into company_context_documents (document_id, source, source_type, metadata) values
            ('doc_slack_alpha', 'slack', 'slack_thread', '{"channel_id": "C_ALPHA"}'),
            ('doc_slack_beta', 'slack', 'slack_thread', '{"channel_id": "C_BETA"}'),
            ('doc_slack_private', 'slack', 'slack_thread', '{"channel_id": "G_PRIVATE"}'),
            ('doc_slack_unknown_channel', 'slack', 'slack_thread', '{}'),
            ('doc_gdrive', 'google_drive', 'google_doc', '{}'),
            ('doc_gcal', 'google_calendar', 'calendar_event', '{}'),
            ('doc_linear', 'linear', 'linear_issue', '{}');

        insert into google_drive_sync_runs (run_id) values ('gdrive_run');
        insert into google_drive_sync_files (file_id) values ('gdrive_file');
        insert into google_drive_sync_checkpoints (scope_id) values ('gdrive_scope');

        insert into google_calendar_sync_runs (run_id) values ('gcal_run');
        insert into google_calendar_sync_calendars (calendar_id) values ('gcal_calendar');
        insert into google_calendar_sync_events (calendar_id, event_id)
            values ('gcal_calendar', 'gcal_event');
        insert into google_calendar_sync_checkpoints (calendar_id) values ('gcal_calendar');

        insert into linear_sync_runs (run_id) values ('linear_run');
        insert into linear_sync_projects (project_id) values ('linear_project');
        insert into linear_sync_issues (issue_id) values ('linear_issue');
        insert into linear_sync_comments (comment_id) values ('linear_comment');
        insert into linear_sync_checkpoints (scope_id) values ('linear_scope');
        "#,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn visible_rows(
    conn: &mut PgConnection,
    schema: &str,
    role: &str,
    slack_channel_id: Option<&str>,
) -> Result<VisibleRows, sqlx::Error> {
    let mut tx = conn.begin().await?;
    tx.execute(format!(r#"set local search_path to "{}", public"#, schema).as_str())
        .await?;
    tx.execute(format!("set role {role}").as_str()).await?;
    if let Some(channel_id) = slack_channel_id {
        sqlx::query("select set_config('centaur.slack_channel_id', $1, true)")
            .bind(channel_id)
            .execute(&mut *tx)
            .await?;
    }

    let rows = VisibleRows {
        slack_channels: text_array(
            &mut tx,
            "select coalesce(array_agg(channel_id order by channel_id), '{}') from slack_sync_channels",
        )
        .await?,
        slack_users: text_array(
            &mut tx,
            "select coalesce(array_agg(user_id order by user_id), '{}') from slack_sync_users",
        )
        .await?,
        slack_messages: text_array(
            &mut tx,
            "select coalesce(array_agg(channel_id || ':' || message_ts order by channel_id, message_ts), '{}') from slack_sync_messages",
        )
        .await?,
        slack_attachments: text_array(
            &mut tx,
            "select coalesce(array_agg(channel_id || ':' || message_ts || ':' || slack_file_id order by channel_id, message_ts, slack_file_id), '{}') from slack_sync_message_attachments",
        )
        .await?,
        context_docs: text_array(
            &mut tx,
            "select coalesce(array_agg(document_id order by document_id), '{}') from company_context_documents",
        )
        .await?,
        google_drive_runs: count(&mut tx, "google_drive_sync_runs").await?,
        google_drive_files: count(&mut tx, "google_drive_sync_files").await?,
        google_drive_checkpoints: count(&mut tx, "google_drive_sync_checkpoints").await?,
        google_calendar_runs: count(&mut tx, "google_calendar_sync_runs").await?,
        google_calendar_calendars: count(&mut tx, "google_calendar_sync_calendars").await?,
        google_calendar_events: count(&mut tx, "google_calendar_sync_events").await?,
        google_calendar_checkpoints: count(&mut tx, "google_calendar_sync_checkpoints").await?,
        linear_runs: count(&mut tx, "linear_sync_runs").await?,
        linear_projects: count(&mut tx, "linear_sync_projects").await?,
        linear_issues: count(&mut tx, "linear_sync_issues").await?,
        linear_comments: count(&mut tx, "linear_sync_comments").await?,
        linear_checkpoints: count(&mut tx, "linear_sync_checkpoints").await?,
    };

    tx.execute("reset role").await?;
    tx.rollback().await?;
    Ok(rows)
}

async fn text_array(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    query: &str,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(query).fetch_one(&mut **tx).await
}

async fn count(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(format!("select count(*) from {table}").as_str())
        .fetch_one(&mut **tx)
        .await
}

fn empty_visible_rows() -> VisibleRows {
    VisibleRows {
        slack_channels: vec![],
        slack_users: vec![],
        slack_messages: vec![],
        slack_attachments: vec![],
        context_docs: vec![],
        google_drive_runs: 0,
        google_drive_files: 0,
        google_drive_checkpoints: 0,
        google_calendar_runs: 0,
        google_calendar_calendars: 0,
        google_calendar_events: 0,
        google_calendar_checkpoints: 0,
        linear_runs: 0,
        linear_projects: 0,
        linear_issues: 0,
        linear_comments: 0,
        linear_checkpoints: 0,
    }
}

fn public_visible_rows() -> VisibleRows {
    VisibleRows {
        slack_channels: vec![
            "C_ADMIN".to_owned(),
            "C_ALPHA".to_owned(),
            "C_BETA".to_owned(),
        ],
        slack_users: vec![
            "U_ALPHA".to_owned(),
            "U_BETA".to_owned(),
            "U_PRIVATE".to_owned(),
        ],
        slack_messages: vec![
            "C_ALPHA:1000.000001".to_owned(),
            "C_BETA:1000.000002".to_owned(),
        ],
        slack_attachments: vec![
            "C_ALPHA:1000.000001:F_ALPHA".to_owned(),
            "C_BETA:1000.000002:F_BETA".to_owned(),
        ],
        context_docs: vec![
            "doc_gcal".to_owned(),
            "doc_gdrive".to_owned(),
            "doc_linear".to_owned(),
            "doc_slack_alpha".to_owned(),
            "doc_slack_beta".to_owned(),
        ],
        google_drive_runs: 1,
        google_drive_files: 1,
        google_drive_checkpoints: 1,
        google_calendar_runs: 1,
        google_calendar_calendars: 1,
        google_calendar_events: 1,
        google_calendar_checkpoints: 1,
        linear_runs: 1,
        linear_projects: 1,
        linear_issues: 1,
        linear_comments: 1,
        linear_checkpoints: 1,
    }
}

fn public_and_private_visible_rows() -> VisibleRows {
    let mut rows = public_visible_rows();
    rows.slack_channels.push("G_PRIVATE".to_owned());
    rows.slack_messages.push("G_PRIVATE:1000.000003".to_owned());
    rows.slack_attachments
        .push("G_PRIVATE:1000.000003:F_PRIVATE".to_owned());
    rows.context_docs.push("doc_slack_private".to_owned());
    rows
}
