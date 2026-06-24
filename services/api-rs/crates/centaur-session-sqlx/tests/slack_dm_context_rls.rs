use std::{
    env,
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{Connection, Executor, PgConnection, Row};

const SLACK_DM_SYNC_SQL: &str = include_str!("../migrations/0027_slack_dm_sync_tables.sql");

const RLS_TABLES: &[&str] = &[
    "slack_dm_sync_conversations",
    "slack_dm_sync_conversation_members",
    "slack_dm_sync_messages",
    "slack_dm_sync_message_attachments",
    "slack_dm_sync_checkpoints",
    "slack_dm_sync_runs",
    "slack_dm_sync_backfill_jobs",
];

#[derive(Debug, PartialEq, Eq)]
struct VisibleDmRows {
    conversations: Vec<String>,
    members: Vec<String>,
    messages: Vec<String>,
    attachments: Vec<String>,
    checkpoints: Vec<String>,
    runs: i64,
    backfill_jobs: i64,
}

#[tokio::test]
async fn slack_dm_rls_requires_current_membership_and_user_setting() -> Result<(), Box<dyn Error>> {
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
    create_roles(conn).await?;
    execute_migration(conn, SLACK_DM_SYNC_SQL).await?;
    grant_schema_usage(conn, schema).await?;

    assert_rls_enabled(conn).await?;
    assert_expected_policies(conn).await?;
    insert_fixture_rows(conn).await?;

    let user_a = visible_rows(
        conn,
        schema,
        "centaur_slack_reader",
        Some("T_HOME"),
        Some("U_A"),
    )
    .await?;
    assert_eq!(
        user_a,
        VisibleDmRows {
            conversations: vec!["T_HOME:D_A".to_owned(), "T_HOME:G_MPIM".to_owned()],
            members: vec!["T_HOME:D_A:U_A".to_owned(), "T_HOME:G_MPIM:U_A".to_owned()],
            messages: vec![
                "T_HOME:D_A:1000.000001".to_owned(),
                "T_HOME:G_MPIM:1000.000003".to_owned(),
            ],
            attachments: vec!["T_HOME:D_A:1000.000001:F_A".to_owned()],
            checkpoints: vec!["bcr_a:T_HOME:D_A".to_owned()],
            runs: 0,
            backfill_jobs: 0,
        }
    );

    let user_b = visible_rows(
        conn,
        schema,
        "centaur_slack_reader",
        Some("T_HOME"),
        Some("U_B"),
    )
    .await?;
    assert_eq!(
        user_b,
        VisibleDmRows {
            conversations: vec!["T_HOME:D_B".to_owned(), "T_HOME:G_MPIM".to_owned()],
            members: vec!["T_HOME:D_B:U_B".to_owned(), "T_HOME:G_MPIM:U_B".to_owned()],
            messages: vec![
                "T_HOME:D_B:1000.000002".to_owned(),
                "T_HOME:G_MPIM:1000.000003".to_owned(),
            ],
            attachments: vec![],
            checkpoints: vec![],
            runs: 0,
            backfill_jobs: 0,
        }
    );

    let former_member = visible_rows(
        conn,
        schema,
        "centaur_slack_reader",
        Some("T_HOME"),
        Some("U_C"),
    )
    .await?;
    assert_eq!(former_member, empty_visible_dm_rows());

    let missing_user =
        visible_rows(conn, schema, "centaur_slack_reader", Some("T_HOME"), None).await?;
    assert_eq!(missing_user, empty_visible_dm_rows());

    let missing_team =
        visible_rows(conn, schema, "centaur_slack_reader", None, Some("U_A")).await?;
    assert_eq!(missing_team, empty_visible_dm_rows());

    let wrong_team = visible_rows(
        conn,
        schema,
        "centaur_slack_reader",
        Some("T_OTHER"),
        Some("U_A"),
    )
    .await?;
    assert_eq!(
        wrong_team,
        VisibleDmRows {
            conversations: vec!["T_OTHER:D_A".to_owned()],
            members: vec!["T_OTHER:D_A:U_A".to_owned()],
            messages: vec!["T_OTHER:D_A:1000.000004".to_owned()],
            attachments: vec![],
            checkpoints: vec![],
            runs: 0,
            backfill_jobs: 0,
        }
    );

    let readonly = visible_rows(
        conn,
        schema,
        "centaur_readonly",
        Some("T_HOME"),
        Some("U_A"),
    )
    .await?;
    assert_eq!(readonly, empty_visible_dm_rows());

    Ok(())
}

fn test_database_url() -> Option<String> {
    env::var("SESSION_SQLX_TEST_DATABASE_URL")
        .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
        .map_err(|_| {
            eprintln!(
                "skipping Slack DM RLS tests: set SESSION_SQLX_TEST_DATABASE_URL to a Postgres URL"
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
        let name = format!("slack_dm_rls_{}_{}", std::process::id(), nanos);
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

async fn create_roles(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        do $$
        begin
            if not exists (select 1 from pg_roles where rolname = 'centaur_slack_reader') then
                create role centaur_slack_reader nologin;
            end if;
            if not exists (select 1 from pg_roles where rolname = 'centaur_readonly') then
                create role centaur_readonly nologin;
            end if;
        end
        $$;
        "#,
    )
    .execute(&mut *conn)
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
    Ok(())
}

async fn execute_migration(conn: &mut PgConnection, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(sql).execute(&mut *conn).await?;
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

    for expected in [
        (
            "slack_dm_sync_conversations",
            "centaur_slack_dm_conversations_reader_select",
        ),
        (
            "slack_dm_sync_conversation_members",
            "centaur_slack_dm_members_reader_select",
        ),
        (
            "slack_dm_sync_messages",
            "centaur_slack_dm_messages_reader_select",
        ),
        (
            "slack_dm_sync_message_attachments",
            "centaur_slack_dm_attachments_reader_select",
        ),
        (
            "slack_dm_sync_checkpoints",
            "centaur_slack_dm_checkpoints_reader_select",
        ),
        ("slack_dm_sync_runs", "centaur_slack_dm_runs_reader_select"),
        (
            "slack_dm_sync_backfill_jobs",
            "centaur_slack_dm_backfill_jobs_reader_select",
        ),
        (
            "slack_dm_sync_conversations",
            "centaur_readonly_slack_dm_sync_conversations_select",
        ),
        (
            "slack_dm_sync_conversation_members",
            "centaur_readonly_slack_dm_sync_conversation_members_select",
        ),
        (
            "slack_dm_sync_messages",
            "centaur_readonly_slack_dm_sync_messages_select",
        ),
        (
            "slack_dm_sync_message_attachments",
            "centaur_readonly_slack_dm_sync_message_attachments_select",
        ),
        (
            "slack_dm_sync_checkpoints",
            "centaur_readonly_slack_dm_sync_checkpoints_select",
        ),
        (
            "slack_dm_sync_runs",
            "centaur_readonly_slack_dm_sync_runs_select",
        ),
        (
            "slack_dm_sync_backfill_jobs",
            "centaur_readonly_slack_dm_sync_backfill_jobs_select",
        ),
    ] {
        assert!(
            policies.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "missing RLS policy {} on {}",
            expected.1,
            expected.0
        );
    }
    Ok(())
}

async fn insert_fixture_rows(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        insert into slack_dm_sync_conversations
            (home_team_id, conversation_id, conversation_type)
        values
            ('T_HOME', 'D_A', 'im'),
            ('T_HOME', 'D_B', 'im'),
            ('T_HOME', 'G_MPIM', 'mpim'),
            ('T_OTHER', 'D_A', 'im');

        insert into slack_dm_sync_conversation_members
            (home_team_id, conversation_id, user_id, is_current_member)
        values
            ('T_HOME', 'D_A', 'U_A', true),
            ('T_HOME', 'D_A', 'U_OTHER', true),
            ('T_HOME', 'D_B', 'U_B', true),
            ('T_HOME', 'D_B', 'U_OTHER', true),
            ('T_HOME', 'G_MPIM', 'U_A', true),
            ('T_HOME', 'G_MPIM', 'U_B', true),
            ('T_HOME', 'G_MPIM', 'U_C', false),
            ('T_OTHER', 'D_A', 'U_A', true);

        insert into slack_dm_sync_runs (run_id, status, broker_credential_id)
        values ('run_a', 'completed', 'bcr_a');

        insert into slack_dm_sync_messages
            (home_team_id, conversation_id, message_ts, text, source_run_id)
        values
            ('T_HOME', 'D_A', '1000.000001', 'user a dm', 'run_a'),
            ('T_HOME', 'D_B', '1000.000002', 'user b dm', 'run_a'),
            ('T_HOME', 'G_MPIM', '1000.000003', 'shared mpim', 'run_a'),
            ('T_OTHER', 'D_A', '1000.000004', 'other workspace dm', 'run_a');

        insert into slack_dm_sync_message_attachments
            (home_team_id, conversation_id, message_ts, slack_file_id, name, source_run_id)
        values
            ('T_HOME', 'D_A', '1000.000001', 'F_A', 'a.pdf', 'run_a');

        insert into slack_dm_sync_checkpoints
            (broker_credential_id, home_team_id, conversation_id, watermark_ts)
        values
            ('bcr_a', 'T_HOME', 'D_A', '1000.000001');

        insert into slack_dm_sync_backfill_jobs
            (job_key, job_type, broker_credential_id, home_team_id, conversation_id)
        values
            ('bcr_a:T_HOME:D_A', 'conversation_bootstrap', 'bcr_a', 'T_HOME', 'D_A');
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
    slack_team_id: Option<&str>,
    slack_user_id: Option<&str>,
) -> Result<VisibleDmRows, sqlx::Error> {
    let mut tx = conn.begin().await?;
    tx.execute(format!(r#"set local search_path to "{}", public"#, schema).as_str())
        .await?;
    tx.execute(format!("set role {role}").as_str()).await?;
    if let Some(team_id) = slack_team_id {
        sqlx::query("select set_config('centaur.slack_team_id', $1, true)")
            .bind(team_id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(user_id) = slack_user_id {
        sqlx::query("select set_config('centaur.slack_user_id', $1, true)")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    }

    let rows = VisibleDmRows {
        conversations: text_array(
            &mut tx,
            "select coalesce(array_agg(home_team_id || ':' || conversation_id order by home_team_id, conversation_id), '{}') from slack_dm_sync_conversations",
        )
        .await?,
        members: text_array(
            &mut tx,
            "select coalesce(array_agg(home_team_id || ':' || conversation_id || ':' || user_id order by home_team_id, conversation_id, user_id), '{}') from slack_dm_sync_conversation_members",
        )
        .await?,
        messages: text_array(
            &mut tx,
            "select coalesce(array_agg(home_team_id || ':' || conversation_id || ':' || message_ts order by home_team_id, conversation_id, message_ts), '{}') from slack_dm_sync_messages",
        )
        .await?,
        attachments: text_array(
            &mut tx,
            "select coalesce(array_agg(home_team_id || ':' || conversation_id || ':' || message_ts || ':' || slack_file_id order by home_team_id, conversation_id, message_ts, slack_file_id), '{}') from slack_dm_sync_message_attachments",
        )
        .await?,
        checkpoints: text_array(
            &mut tx,
            "select coalesce(array_agg(broker_credential_id || ':' || home_team_id || ':' || conversation_id order by broker_credential_id, home_team_id, conversation_id), '{}') from slack_dm_sync_checkpoints",
        )
        .await?,
        runs: count(&mut tx, "slack_dm_sync_runs").await?,
        backfill_jobs: count(&mut tx, "slack_dm_sync_backfill_jobs").await?,
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

fn empty_visible_dm_rows() -> VisibleDmRows {
    VisibleDmRows {
        conversations: vec![],
        members: vec![],
        messages: vec![],
        attachments: vec![],
        checkpoints: vec![],
        runs: 0,
        backfill_jobs: 0,
    }
}
