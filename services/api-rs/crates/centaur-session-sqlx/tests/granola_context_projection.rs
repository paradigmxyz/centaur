use std::{
    env,
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{Connection, Executor, PgConnection, Row};

const GRANOLA_SYNC_SQL: &str = include_str!("../migrations/0040_granola_sync_tables.sql");
const GRANOLA_CONTEXT_PROJECTION_SQL: &str =
    include_str!("../migrations/0044_granola_context_projection.sql");

#[tokio::test]
async fn granola_notes_project_into_their_dedicated_rls_protected_context_table()
-> Result<(), Box<dyn Error>> {
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
    set_search_path(conn, schema).await?;
    create_roles(conn).await?;
    create_slack_identity_helpers(conn).await?;
    execute_migration(conn, &granola_sync_without_bm25()).await?;
    sqlx::raw_sql(
        r#"
        insert into granola_sync_notes (
            note_id, title, owner_email, access_emails, content_text, source_created_at
        ) values (
            'note_backfilled', 'Existing note', 'alice@example.com',
            array['alice@example.com'], 'Existing source data', '2026-07-13T09:00:00Z'
        );

        insert into granola_context_documents (
            document_id, note_id, title, body, content_hash
        ) values (
            'granola:note:note_backfilled', 'note_backfilled',
            'Stale title', 'Stale body', 'stale-hash'
        );
        "#,
    )
    .execute(&mut *conn)
    .await?;
    execute_migration(conn, GRANOLA_CONTEXT_PROJECTION_SQL).await?;
    grant_schema_usage(conn, schema).await?;

    let backfilled = sqlx::query(
        "select document_id, title, body from granola_context_documents \
         where note_id = 'note_backfilled'",
    )
    .fetch_one(&mut *conn)
    .await?;
    assert_eq!(
        backfilled.try_get::<String, _>("document_id")?,
        "granola:note:note_backfilled"
    );
    assert_eq!(backfilled.try_get::<String, _>("title")?, "Existing note");
    assert_eq!(
        backfilled.try_get::<String, _>("body")?,
        "Existing source data"
    );

    sqlx::raw_sql(
        r#"
        insert into slack_sync_users (team_id, user_id, raw_payload) values
            ('T_HOME', 'U_ALICE', '{"profile":{"email":"alice@example.com"}}'),
            ('T_HOME', 'U_BOB', '{"profile":{"email":"bob@example.com"}}');

        insert into granola_sync_notes (
            note_id, title, owner_id, owner_email, owner_name, attendees,
            access_emails, calendar_event, content_text, source_created_at,
            source_updated_at
        ) values
            (
                'note_alice', 'Launch review', 'owner_alice', 'alice@example.com', 'Alice',
                '[{"name":"Bob", "email":"bob@example.com"}]',
                array['alice@example.com', 'bob@example.com'],
                '{"title":"Launch review"}', 'Launch status and risks',
                '2026-07-13T10:00:00Z', '2026-07-13T11:00:00Z'
            ),
            (
                'note_bob', 'Budget review', 'owner_bob', 'bob@example.com', 'Bob',
                '[]', array['bob@example.com'], '{}', 'Budget details',
                '2026-07-13T12:00:00Z', '2026-07-13T13:00:00Z'
            );
        "#,
    )
    .execute(&mut *conn)
    .await?;

    let projection = sqlx::query(
        "select document_id, title, body, attendee_labels, access_emails, metadata \
         from granola_context_documents where note_id = 'note_alice'",
    )
    .fetch_one(&mut *conn)
    .await?;
    assert_eq!(
        projection.try_get::<String, _>("document_id")?,
        "granola:note:note_alice"
    );
    assert_eq!(projection.try_get::<String, _>("title")?, "Launch review");
    assert_eq!(
        projection.try_get::<String, _>("body")?,
        "Launch status and risks"
    );
    assert_eq!(
        projection.try_get::<Vec<String>, _>("attendee_labels")?,
        vec!["Bob"]
    );
    assert_eq!(
        projection.try_get::<Vec<String>, _>("access_emails")?,
        vec!["alice@example.com", "bob@example.com"]
    );
    assert_eq!(
        projection
            .try_get::<serde_json::Value, _>("metadata")?
            .get("source_type")
            .and_then(serde_json::Value::as_str),
        Some("granola_note")
    );

    sqlx::query(
        "update granola_sync_notes set title = 'Launch decision', \
         content_text = 'Approved the launch', access_emails = array['alice@example.com'] \
         where note_id = 'note_alice'",
    )
    .execute(&mut *conn)
    .await?;
    let updated = sqlx::query(
        "select title, body, access_emails from granola_context_documents where note_id = 'note_alice'",
    )
    .fetch_one(&mut *conn)
    .await?;
    assert_eq!(updated.try_get::<String, _>("title")?, "Launch decision");
    assert_eq!(updated.try_get::<String, _>("body")?, "Approved the launch");
    assert_eq!(
        updated.try_get::<Vec<String>, _>("access_emails")?,
        vec!["alice@example.com"]
    );

    assert_visible_documents(
        conn,
        schema,
        "U_ALICE",
        &["granola:note:note_alice", "granola:note:note_backfilled"],
    )
    .await?;
    assert_visible_documents(conn, schema, "U_BOB", &["granola:note:note_bob"]).await?;
    Ok(())
}

async fn assert_visible_documents(
    conn: &mut PgConnection,
    schema: &str,
    user_id: &str,
    expected: &[&str],
) -> Result<(), Box<dyn Error>> {
    let user_email = match user_id {
        "U_ALICE" => "alice@example.com",
        "U_BOB" => "bob@example.com",
        _ => unreachable!("test only defines Alice and Bob"),
    };
    sqlx::query("set role centaur_slack_reader")
        .execute(&mut *conn)
        .await?;
    sqlx::query("select set_config('centaur.slack_team_id', 'T_HOME', false)")
        .execute(&mut *conn)
        .await?;
    sqlx::query("select set_config('centaur.slack_user_id', $1, false)")
        .bind(user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("select set_config('centaur.user_email', $1, false)")
        .bind(user_email)
        .execute(&mut *conn)
        .await?;
    let rows =
        sqlx::query("select document_id from granola_context_documents order by document_id")
            .fetch_all(&mut *conn)
            .await?;
    let actual = rows
        .iter()
        .map(|row| row.try_get::<String, _>("document_id"))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|document_id| (*document_id).to_owned())
            .collect::<Vec<_>>()
    );
    conn.execute("reset role").await?;
    set_search_path(conn, schema).await?;
    Ok(())
}

fn test_database_url() -> Option<String> {
    env::var("SESSION_SQLX_TEST_DATABASE_URL")
        .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
        .map_err(|_| {
            eprintln!(
                "skipping Granola context projection tests: set SESSION_SQLX_TEST_DATABASE_URL to a Postgres URL"
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
        let name = format!("granola_context_{}_{}", std::process::id(), nanos);
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
        end
        $$;
        "#,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn create_slack_identity_helpers(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(
        r#"
        -- The Granola RLS helper intentionally pins its security-definer search
        -- path to public, matching production. CI's disposable Postgres starts
        -- without its source table and identity functions, so provide their
        -- minimal public definitions. Local development already has the real
        -- objects and is left untouched by the conditional setup below.
        create table if not exists public.slack_sync_users (
            team_id text not null,
            user_id text not null,
            raw_payload jsonb not null default '{}'::jsonb,
            primary key (team_id, user_id)
        );

        create table slack_sync_users (
            team_id text not null,
            user_id text not null,
            raw_payload jsonb not null default '{}'::jsonb,
            primary key (team_id, user_id)
        );

        do $$
        begin
            if to_regprocedure('public.centaur_current_slack_team_id()') is null then
                execute $function$
                    create function public.centaur_current_slack_team_id()
                    returns text language sql stable as $body$
                        select nullif(current_setting('centaur.slack_team_id', true), '')
                    $body$
                $function$;
            end if;
            if to_regprocedure('public.centaur_current_slack_user_id()') is null then
                execute $function$
                    create function public.centaur_current_slack_user_id()
                    returns text language sql stable as $body$
                        select nullif(current_setting('centaur.slack_user_id', true), '')
                    $body$
                $function$;
            end if;
        end
        $$;

        create function centaur_current_slack_team_id()
        returns text language sql stable as $$
            select nullif(current_setting('centaur.slack_team_id', true), '')
        $$;

        create function centaur_current_slack_user_id()
        returns text language sql stable as $$
            select nullif(current_setting('centaur.slack_user_id', true), '')
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
            r#"grant usage on schema "{}" to centaur_slack_reader"#,
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

fn granola_sync_without_bm25() -> String {
    let sql = GRANOLA_SYNC_SQL.replace(
        "create extension if not exists pg_search;",
        "-- search extension unavailable in this test database",
    );
    let (before_bm25, rest) = sql
        .split_once("drop index if exists idx_granola_context_documents_bm25;")
        .expect("Granola migration should contain BM25 index block");
    let (_, after_bm25) = rest
        .split_once("create table if not exists granola_sync_checkpoints")
        .expect("Granola migration should create sync checkpoints after the BM25 index");

    format!("{before_bm25}create table if not exists granola_sync_checkpoints{after_bm25}")
}
