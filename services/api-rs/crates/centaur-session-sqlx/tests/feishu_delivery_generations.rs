use std::{
    env,
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{Connection, Executor, PgConnection, Row};

const MIGRATION_SQL: &str = include_str!("../migrations/0058_feishu_delivery_generations.sql");

#[tokio::test]
async fn migration_backfills_the_latest_delivery_operation() -> Result<(), Box<dyn Error>> {
    let Some(database_url) = test_database_url() else {
        return Ok(());
    };
    let mut conn = PgConnection::connect(&database_url).await?;
    let schema = TestSchema::create(&mut conn).await?;

    let result = run_assertions(&mut conn, &schema.name).await;
    schema.drop(&mut conn).await?;
    result
}

async fn run_assertions(conn: &mut PgConnection, schema: &str) -> Result<(), Box<dyn Error>> {
    conn.execute(format!(r#"set search_path to "{schema}", public"#).as_str())
        .await?;
    sqlx::raw_sql(
        r#"
        create table sessions (
            thread_key text primary key
        );
        create table session_executions (
            execution_id text primary key,
            thread_key text not null references sessions(thread_key),
            created_at timestamptz not null
        );
        create table session_workspaces (
            workspace_id text primary key,
            thread_key text not null unique references sessions(thread_key)
        );
        create table development_selection_flows (
            selection_flow_id text primary key,
            workspace_id text not null references session_workspaces(workspace_id),
            execution_id text references session_executions(execution_id),
            state text not null,
            updated_at timestamptz not null
        );
        create table feishu_deliveries (
            delivery_id text primary key,
            thread_key text not null unique references sessions(thread_key),
            desired_version integer not null default 0,
            state text not null default 'pending',
            failure_code text,
            updated_at timestamptz not null default now()
        );

        insert into sessions (thread_key) values
            ('development:terminal-selection'),
            ('development:newer-execution');
        insert into session_executions (execution_id, thread_key, created_at) values
            ('exec-before-selection', 'development:terminal-selection', '2026-08-14 10:00:00Z'),
            ('exec-after-selection', 'development:newer-execution', '2026-08-14 12:00:00Z');
        insert into session_workspaces (workspace_id, thread_key) values
            ('workspace-terminal-selection', 'development:terminal-selection'),
            ('workspace-newer-execution', 'development:newer-execution');
        insert into development_selection_flows
            (selection_flow_id, workspace_id, execution_id, state, updated_at)
        values
            ('selection-terminal', 'workspace-terminal-selection', null, 'cancelled',
             '2026-08-14 11:00:00Z'),
            ('selection-old', 'workspace-newer-execution', null, 'confirmed',
             '2026-08-14 11:00:00Z');
        insert into feishu_deliveries (delivery_id, thread_key) values
            ('delivery-terminal-selection', 'development:terminal-selection'),
            ('delivery-newer-execution', 'development:newer-execution');
        "#,
    )
    .execute(&mut *conn)
    .await?;

    sqlx::raw_sql(MIGRATION_SQL).execute(&mut *conn).await?;

    let terminal = sqlx::query(
        "select selection_flow_id, execution_id from feishu_deliveries \
         where thread_key = 'development:terminal-selection'",
    )
    .fetch_one(&mut *conn)
    .await?;
    assert_eq!(
        terminal.try_get::<Option<String>, _>("selection_flow_id")?,
        Some("selection-terminal".to_owned())
    );
    assert_eq!(terminal.try_get::<Option<String>, _>("execution_id")?, None);

    let execution = sqlx::query(
        "select selection_flow_id, execution_id from feishu_deliveries \
         where thread_key = 'development:newer-execution'",
    )
    .fetch_one(&mut *conn)
    .await?;
    assert_eq!(
        execution.try_get::<Option<String>, _>("selection_flow_id")?,
        None
    );
    assert_eq!(
        execution.try_get::<Option<String>, _>("execution_id")?,
        Some("exec-after-selection".to_owned())
    );
    Ok(())
}

fn test_database_url() -> Option<String> {
    env::var("SESSION_SQLX_TEST_DATABASE_URL")
        .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
        .map_err(|_| {
            eprintln!(
                "skipping Feishu delivery migration test: set SESSION_SQLX_TEST_DATABASE_URL to a Postgres URL"
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
        let name = format!("feishu_delivery_{}_{}", std::process::id(), nanos);
        conn.execute(format!(r#"create schema "{name}""#).as_str())
            .await?;
        Ok(Self { name })
    }

    async fn drop(self, conn: &mut PgConnection) -> Result<(), Box<dyn Error>> {
        conn.execute(format!(r#"drop schema if exists "{}" cascade"#, self.name).as_str())
            .await?;
        Ok(())
    }
}
