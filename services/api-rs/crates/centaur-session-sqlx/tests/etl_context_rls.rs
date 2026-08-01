use std::{
    env,
    error::Error,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{Connection, Executor, PgConnection, Row, postgres::PgConnectOptions};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

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

#[derive(Debug, PartialEq, Eq)]
struct CompanyContextSearchRows {
    company_context_docs: Vec<String>,
    google_docs: Vec<String>,
    granola_docs: Vec<String>,
    slack_private_docs: Vec<String>,
    slack_private_conversation_docs: Vec<String>,
}

#[tokio::test]
async fn etl_context_rls_enforces_channel_visibility() -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        return Ok(());
    };
    let mut admin_conn = PgConnection::connect(&database_url).await?;
    let database = TestDatabase::create(&mut admin_conn, &database_url).await?;
    let mut conn = match PgConnection::connect_with(&database.options).await {
        Ok(conn) => conn,
        Err(err) => {
            database.drop(&mut admin_conn).await?;
            return Err(err.into());
        }
    };

    let result = run_rls_assertions(&mut conn).await;
    let close_result = conn.close().await;
    let drop_result = database.drop(&mut admin_conn).await;

    result?;
    close_result?;
    drop_result?;
    Ok(())
}

async fn run_rls_assertions(conn: &mut PgConnection) -> Result<(), Box<dyn Error>> {
    MIGRATOR.run(&mut *conn).await?;

    assert_rls_enabled(conn).await?;
    assert_expected_policies(conn).await?;
    assert_legacy_admin_state_is_removed(conn).await?;

    insert_fixture_rows(conn).await?;

    let c_alpha = visible_rows(conn, "centaur_slack_reader", Some("C_ALPHA")).await?;
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

    let c_beta = visible_rows(conn, "centaur_slack_reader", Some("C_BETA")).await?;
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

    let dm_or_missing_channel = visible_rows(conn, "centaur_slack_reader", Some("")).await?;
    assert_eq!(dm_or_missing_channel, empty_visible_rows());

    let unset_channel = visible_rows(conn, "centaur_slack_reader", None).await?;
    assert_eq!(unset_channel, empty_visible_rows());

    let formerly_admin_channel =
        visible_rows(conn, "centaur_slack_reader", Some("C_ADMIN")).await?;
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

    let readonly_role = visible_rows(conn, "centaur_readonly", None).await?;
    assert_eq!(readonly_role, public_visible_rows());

    let readonly_private_channel =
        visible_rows(conn, "centaur_readonly", Some("G_PRIVATE")).await?;
    assert_eq!(readonly_private_channel, public_and_private_visible_rows());

    let company_context_public = company_context_docs(conn, None, r#"[]"#, true).await?;
    assert_eq!(
        company_context_public,
        vec!["doc_slack_alpha".to_owned(), "doc_slack_beta".to_owned(),]
    );

    let company_context_private_history =
        company_context_docs(conn, None, r#"["G_PRIVATE"]"#, true).await?;
    assert_eq!(
        company_context_private_history,
        vec![
            "doc_slack_alpha".to_owned(),
            "doc_slack_beta".to_owned(),
            "doc_slack_private".to_owned(),
        ]
    );

    let company_context_history_no_public =
        company_context_docs(conn, None, r#"["C_ALPHA"]"#, false).await?;
    assert_eq!(
        company_context_history_no_public,
        vec!["doc_slack_alpha".to_owned()]
    );

    let company_context_current_channel =
        company_context_docs(conn, Some("C_ALPHA"), r#"[]"#, false).await?;
    assert_eq!(
        company_context_current_channel,
        vec!["doc_slack_alpha".to_owned()]
    );

    let search_rows = company_context_search_rows(conn).await?;
    assert_eq!(
        search_rows,
        CompanyContextSearchRows {
            company_context_docs: vec![
                "doc_slack_alpha".to_owned(),
                "doc_slack_beta".to_owned(),
                "doc_slack_private".to_owned(),
            ],
            google_docs: vec!["gdocs_doc".to_owned()],
            granola_docs: vec!["granola:note:granola_note".to_owned()],
            slack_private_docs: vec!["slack_dm:T_HOME:D_VISIBLE:2000.000001".to_owned()],
            slack_private_conversation_docs: vec![
                "slack_dm_conversation:T_HOME:D_VISIBLE".to_owned()
            ],
        }
    );

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

struct TestDatabase {
    name: String,
    options: PgConnectOptions,
}

impl TestDatabase {
    async fn create(conn: &mut PgConnection, database_url: &str) -> Result<Self, Box<dyn Error>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let name = format!("centaur_etl_rls_{}_{}", std::process::id(), nanos);
        conn.execute(format!(r#"create database "{}""#, name).as_str())
            .await?;
        let options = PgConnectOptions::from_str(database_url)?.database(&name);
        Ok(Self { name, options })
    }

    async fn drop(&self, conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        conn.execute(format!(r#"drop database if exists "{}""#, self.name).as_str())
            .await?;
        Ok(())
    }
}

async fn assert_rls_enabled(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    let tables_without_rls: Vec<String> = sqlx::query_scalar(
        r#"
        select distinct policies.tablename
        from pg_policies policies
        join pg_class tables
          on tables.oid = to_regclass(quote_ident(policies.schemaname) || '.' || quote_ident(policies.tablename))
        where policies.schemaname = current_schema()
          and not tables.relrowsecurity
        order by policies.tablename
        "#,
    )
    .fetch_all(&mut *conn)
    .await?;

    assert!(
        tables_without_rls.is_empty(),
        "expected row level security on tables with policies: {tables_without_rls:?}"
    );
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
            "company_context_documents",
            "centaur_cc_reader_documents_select",
        ),
        ("slack_sync_channels", "centaur_cc_reader_channels_select"),
        (
            "granola_context_documents",
            "centaur_cc_reader_granola_documents_select",
        ),
        (
            "slack_private_context_documents",
            "centaur_cc_reader_private_docs_select",
        ),
        (
            "slack_private_conversation_context_documents",
            "centaur_cc_reader_private_conversation_docs_select",
        ),
        (
            "google_docs_sync_file_observations",
            "centaur_cc_reader_gdocs_observations_select",
        ),
        (
            "google_docs_context_documents",
            "centaur_cc_reader_gdocs_documents_select",
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

        insert into company_context_documents
            (document_id, source, source_type, source_document_id, metadata)
        values
            ('doc_slack_alpha', 'slack', 'slack_thread', 'C_ALPHA:1000.000001', '{"channel_id": "C_ALPHA"}'),
            ('doc_slack_beta', 'slack', 'slack_thread', 'C_BETA:1000.000002', '{"channel_id": "C_BETA"}'),
            ('doc_slack_private', 'slack', 'slack_thread', 'G_PRIVATE:1000.000003', '{"channel_id": "G_PRIVATE"}'),
            ('doc_slack_unknown_channel', 'slack', 'slack_thread', 'unknown', '{}'),
            ('doc_gdrive', 'google_drive', 'google_doc', 'gdrive_file', '{}'),
            ('doc_gcal', 'google_calendar', 'calendar_event', 'gcal_event', '{}'),
            ('doc_linear', 'linear', 'linear_issue', 'linear_issue', '{}');

        insert into google_drive_sync_runs (run_id, status) values ('gdrive_run', 'succeeded');
        insert into google_drive_sync_files (file_id) values ('gdrive_file');
        insert into google_drive_sync_checkpoints (scope_id) values ('gdrive_scope');

        insert into google_calendar_sync_runs (run_id, status) values ('gcal_run', 'succeeded');
        insert into google_calendar_sync_calendars (calendar_id) values ('gcal_calendar');
        insert into google_calendar_sync_events (calendar_id, event_id)
            values ('gcal_calendar', 'gcal_event');
        insert into google_calendar_sync_checkpoints (calendar_id) values ('gcal_calendar');

        insert into linear_sync_runs (run_id, status) values ('linear_run', 'succeeded');
        insert into linear_sync_projects (project_id) values ('linear_project');
        insert into linear_sync_issues (issue_id) values ('linear_issue');
        insert into linear_sync_comments (comment_id) values ('linear_comment');
        insert into linear_sync_checkpoints (scope_id) values ('linear_scope');

        insert into google_docs_sync_runs
            (run_id, status, broker_credential_id, provider_subject)
        values
            ('gdocs_run', 'succeeded', 'gdocs_credential', 'google_subject');
        insert into google_docs_sync_files (file_id, source_run_id)
        values ('gdocs_file', 'gdocs_run');
        insert into google_docs_sync_file_observations
            (broker_credential_id, observed_file_id, file_id, provider_subject, active)
        values
            ('gdocs_credential', 'gdocs_observed_file', 'gdocs_file', 'google_subject', true);
        insert into google_docs_context_documents
            (document_id, file_id, chunk_id, title, body)
        values
            ('gdocs_doc', 'gdocs_file', 'chunk_1', 'Google Doc', 'Google Doc body');

        insert into granola_sync_notes
            (note_id, title, access_emails)
        values
            ('granola_note', 'Granola note', array['viewer@example.com']);

        insert into slack_private_sync_conversations
            (home_team_id, conversation_id, conversation_type)
        values
            ('T_HOME', 'D_VISIBLE', 'im'),
            ('T_HOME', 'D_HIDDEN', 'im');
        insert into slack_private_sync_conversation_members
            (home_team_id, conversation_id, user_id, is_current_member)
        values
            ('T_HOME', 'D_VISIBLE', 'U_VIEWER', true),
            ('T_HOME', 'D_HIDDEN', 'U_OTHER', true);
        insert into slack_private_sync_messages
            (home_team_id, conversation_id, message_ts, user_id, text)
        values
            ('T_HOME', 'D_VISIBLE', '2000.000001', 'U_VIEWER', 'visible dm'),
            ('T_HOME', 'D_HIDDEN', '2000.000002', 'U_OTHER', 'hidden dm');
        "#,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn visible_rows(
    conn: &mut PgConnection,
    role: &str,
    slack_channel_id: Option<&str>,
) -> Result<VisibleRows, sqlx::Error> {
    let mut tx = conn.begin().await?;
    tx.execute("set local search_path to public").await?;
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

async fn company_context_docs(
    conn: &mut PgConnection,
    slack_channel_id: Option<&str>,
    slack_history_channel_ids: &str,
    include_public_slack: bool,
) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = conn.begin().await?;
    tx.execute("set local search_path to public").await?;
    tx.execute("set role centaur_company_context_reader")
        .await?;
    if let Some(channel_id) = slack_channel_id {
        sqlx::query("select set_config('centaur.slack_channel_id', $1, true)")
            .bind(channel_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("select set_config('centaur.slack_history_channel_ids', $1, true)")
        .bind(slack_history_channel_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.slack_include_public', $1, true)")
        .bind(if include_public_slack {
            "true"
        } else {
            "false"
        })
        .execute(&mut *tx)
        .await?;

    let rows = text_array(
        &mut tx,
        "select coalesce(array_agg(document_id order by document_id), '{}') from company_context_documents",
    )
    .await?;

    tx.execute("reset role").await?;
    tx.rollback().await?;
    Ok(rows)
}

async fn company_context_search_rows(
    conn: &mut PgConnection,
) -> Result<CompanyContextSearchRows, sqlx::Error> {
    let mut tx = conn.begin().await?;
    tx.execute("set local search_path to public").await?;
    tx.execute("set role centaur_company_context_reader")
        .await?;
    sqlx::query("select set_config('centaur.slack_channel_id', 'C_ALPHA', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.slack_history_channel_ids', $1, true)")
        .bind(r#"["G_PRIVATE"]"#)
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.slack_include_public', 'true', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.slack_team_id', 'T_HOME', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.slack_user_id', 'U_VIEWER', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.user_email', 'viewer@example.com', true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("select set_config('centaur.google_subject', 'google_subject', true)")
        .execute(&mut *tx)
        .await?;

    let rows = CompanyContextSearchRows {
        company_context_docs: text_array(
            &mut tx,
            "select coalesce(array_agg(document_id order by document_id), '{}') from company_context_documents",
        )
        .await?,
        google_docs: text_array(
            &mut tx,
            "select coalesce(array_agg(document_id order by document_id), '{}') from google_docs_context_documents",
        )
        .await?,
        granola_docs: text_array(
            &mut tx,
            "select coalesce(array_agg(document_id order by document_id), '{}') from granola_context_documents",
        )
        .await?,
        slack_private_docs: text_array(
            &mut tx,
            "select coalesce(array_agg(document_id order by document_id), '{}') from slack_private_context_documents",
        )
        .await?,
        slack_private_conversation_docs: text_array(
            &mut tx,
            "select coalesce(array_agg(document_id order by document_id), '{}') from slack_private_conversation_context_documents",
        )
        .await?,
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
