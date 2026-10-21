use absurd::{SpawnOptions, TaskContext};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::{error, info, warn};

use crate::{
    calendar::{CalendarListEntry, EventPage, RenderedEvent},
    config::{
        CALENDAR_CREDENTIALS_RECONCILE_TASK, CALENDAR_DISCOVER_TASK, CALENDAR_SYNC_TASK,
        GOOGLE_CALENDAR_DOCUMENT_ID_PREFIX,
    },
    credentials::{GoogleCredential, GoogleProduct},
    errors::is_rejected,
    tasks::{TaskState, bounded_error, run_task},
};

#[derive(Debug, Deserialize, Serialize)]
pub struct CalendarReconcileParams {
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CalendarDiscoverParams {
    pub credential_id: i64,
    pub bucket: u64,
}

/// One credential's view of one calendar.
#[derive(Debug, Deserialize, Serialize)]
pub struct CalendarSyncParams {
    pub credential_id: i64,
    pub calendar_id: String,
    pub bucket: u64,
}

#[derive(Debug, Serialize)]
pub struct CalendarSummary {
    status: &'static str,
    items: usize,
}

impl CalendarSummary {
    fn new(status: &'static str, items: usize) -> Self {
        Self { status, items }
    }
}

pub fn register(state: &TaskState) -> Result<()> {
    let reconcile_state = state.clone();
    state.absurd.register_task(
        CALENDAR_CREDENTIALS_RECONCILE_TASK,
        move |params: CalendarReconcileParams, ctx| {
            let state = reconcile_state.clone();
            async move { run_task(&ctx, reconcile_credentials(&state, params, &ctx)).await }
        },
    )?;

    let discover_state = state.clone();
    state.absurd.register_task(
        CALENDAR_DISCOVER_TASK,
        move |params: CalendarDiscoverParams, ctx| {
            let state = discover_state.clone();
            async move { run_task(&ctx, discover(&state, params, &ctx)).await }
        },
    )?;

    let sync_state = state.clone();
    state.absurd.register_task(
        CALENDAR_SYNC_TASK,
        move |params: CalendarSyncParams, ctx| {
            let state = sync_state.clone();
            async move { run_task(&ctx, sync_calendar(&state, params, &ctx)).await }
        },
    )?;

    Ok(())
}

/// Removes the calendars, with their events, of dead or deleted credentials.
async fn reconcile_credentials(
    state: &TaskState,
    params: CalendarReconcileParams,
    ctx: &TaskContext,
) -> Result<CalendarSummary> {
    let retained_ids = state
        .credentials
        .retained_google_credential_ids(GoogleProduct::Calendar)
        .await?;
    let removed = remove_unretained(&state.pool, &retained_ids).await?;
    info!(
        event = "company_context_calendar_credentials_reconciled",
        task_id = ctx.task_id(),
        bucket = params.bucket,
        calendars_removed = removed
    );
    Ok(CalendarSummary::new("completed", removed as usize))
}

async fn remove_unretained(pool: &PgPool, retained_ids: &[i64]) -> Result<u64> {
    Ok(sqlx::query(
        r#"
        DELETE FROM company_context_system.google_calendars
        WHERE NOT (broker_credential_id = ANY($1::bigint[]))
        "#,
    )
    .bind(retained_ids)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Records the calendars a credential can read and syncs each of them.
async fn discover(
    state: &TaskState,
    params: CalendarDiscoverParams,
    ctx: &TaskContext,
) -> Result<CalendarSummary> {
    let credential = state
        .credentials
        .google_credential(params.credential_id, GoogleProduct::Calendar)
        .await?;
    let result = async {
        let calendars = state.calendar.list_calendars(credential.id).await?;
        record_discovery(&state.pool, &credential, &calendars).await?;
        for calendar in &calendars {
            state
                .absurd
                .spawn(
                    CALENDAR_SYNC_TASK,
                    CalendarSyncParams {
                        credential_id: credential.id,
                        calendar_id: calendar.id.clone(),
                        bucket: params.bucket,
                    },
                    SpawnOptions {
                        idempotency_key: Some(format!(
                            "calendar.calendar.sync:{}:{}:{}",
                            credential.id, calendar.id, params.bucket
                        )),
                        ..SpawnOptions::default()
                    },
                )
                .await?;
        }
        Ok(calendars.len())
    }
    .await;

    match result {
        Ok(calendars) => {
            info!(
                event = "company_context_calendar_discovery_completed",
                task_id = ctx.task_id(),
                credential_id = credential.id,
                calendars
            );
            Ok(CalendarSummary::new("completed", calendars))
        }
        Err(error) if is_rejected(&error) => {
            warn!(
                event = "company_context_calendar_discovery_rejected",
                task_id = ctx.task_id(),
                credential_id = credential.id,
                error = %error
            );
            Ok(CalendarSummary::new("rejected", 0))
        }
        Err(error) => Err(error),
    }
}

/// Stores the credential's calendars and removes, with their events, those it
/// no longer lists.
async fn record_discovery(
    pool: &PgPool,
    credential: &GoogleCredential,
    calendars: &[CalendarListEntry],
) -> Result<()> {
    if credential.provider_subject.is_empty() {
        bail!(
            "Google broker credential {} has no provider subject",
            credential.id
        );
    }
    let mut calendars: Vec<&CalendarListEntry> = calendars.iter().collect();
    calendars.sort_by(|left, right| left.id.cmp(&right.id));
    calendars.dedup_by(|left, right| left.id == right.id);
    let ids: Vec<&str> = calendars.iter().map(|item| item.id.as_str()).collect();
    let summaries: Vec<&str> = calendars.iter().map(|item| item.summary.as_str()).collect();

    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_system.google_calendars
            (broker_credential_id, calendar_id, provider_subject, summary)
        SELECT $1, calendar_id, $2, summary
        FROM unnest($3::text[], $4::text[]) AS listed(calendar_id, summary)
        ORDER BY calendar_id
        ON CONFLICT (broker_credential_id, calendar_id) DO UPDATE
        SET provider_subject = EXCLUDED.provider_subject,
            summary = EXCLUDED.summary,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(credential.id)
    .bind(&credential.provider_subject)
    .bind(&ids)
    .bind(&summaries)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM company_context_system.google_calendars
        WHERE broker_credential_id = $1
          AND NOT (calendar_id = ANY($2::text[]))
        "#,
    )
    .bind(credential.id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Where a calendar's sync stands. A page is applied only while the cursor it
/// was listed from is still current, so concurrent syncs cannot interleave.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct Cursor {
    sync_token: String,
    page_token: String,
    full_sync_started_at: Option<DateTime<Utc>>,
    history_start: Option<DateTime<Utc>>,
}

/// Syncs the events of one credential's calendar. Events that ended before the
/// history cutoff are left out, unless recurring.
async fn sync_calendar(
    state: &TaskState,
    params: CalendarSyncParams,
    ctx: &TaskContext,
) -> Result<CalendarSummary> {
    let (credential_id, calendar_id) = (params.credential_id, params.calendar_id.as_str());
    let cutoff = Utc::now() - chrono::Duration::days(state.config.calendar_history_days as i64);
    let result = async {
        let mut events = 0;
        loop {
            let Some(cursor) = load_cursor(&state.pool, credential_id, calendar_id).await? else {
                return Ok(("removed", events));
            };
            // The cutoff only moves before the last full sync's when the
            // history days are raised; a full sync then backfills the older
            // events.
            if cursor.history_start.is_none_or(|start| cutoff < start) {
                if !restart_full_sync(&state.pool, credential_id, calendar_id, &cursor, cutoff)
                    .await?
                {
                    return Ok(("superseded", events));
                }
                continue;
            }
            let from_token = !(cursor.sync_token.is_empty() && cursor.page_token.is_empty());
            let page = match state
                .calendar
                .list_events(
                    credential_id,
                    calendar_id,
                    nonempty(&cursor.page_token),
                    nonempty(&cursor.sync_token),
                )
                .await
            {
                Ok(page) => page,
                // Stored tokens Google rejects are replaced by a full sync; a
                // rejected full sync is recorded as rejected.
                Err(error) if from_token && is_rejected(&error) => {
                    warn!(
                        event = "company_context_calendar_token_rejected",
                        task_id = ctx.task_id(),
                        credential_id,
                        calendar_id,
                        error = %error
                    );
                    None
                }
                Err(error) => return Err(error),
            };
            let Some(page) = page else {
                if !from_token {
                    bail!("Google Calendar refused a full sync of {calendar_id}");
                }
                warn!(
                    event = "company_context_calendar_full_sync_required",
                    task_id = ctx.task_id(),
                    credential_id,
                    calendar_id
                );
                if !restart_full_sync(&state.pool, credential_id, calendar_id, &cursor, cutoff)
                    .await?
                {
                    return Ok(("superseded", events));
                }
                continue;
            };
            events += page.items.len();
            let complete = page.next_page_token.is_none();
            if !apply_page(
                &state.pool,
                credential_id,
                calendar_id,
                &cursor,
                &page,
                cutoff,
            )
            .await?
            {
                return Ok(("superseded", events));
            }
            if complete {
                return Ok(("completed", events));
            }
        }
    }
    .await;

    match result {
        Ok((status, events)) => {
            info!(
                event = "company_context_calendar_sync_finished",
                task_id = ctx.task_id(),
                credential_id,
                calendar_id,
                status,
                events
            );
            Ok(CalendarSummary::new(status, events))
        }
        Err(error) => {
            if let Err(db_error) = sqlx::query(
                r#"
                UPDATE company_context_system.google_calendars
                SET last_error = $3,
                    updated_at = NOW()
                WHERE broker_credential_id = $1
                  AND calendar_id = $2
                "#,
            )
            .bind(credential_id)
            .bind(calendar_id)
            .bind(bounded_error(&error))
            .execute(&state.pool)
            .await
            {
                error!(event = "company_context_failure_record_failed", calendar_id, error = %db_error);
            }
            if is_rejected(&error) {
                warn!(
                    event = "company_context_calendar_sync_rejected",
                    task_id = ctx.task_id(),
                    credential_id,
                    calendar_id,
                    error = %error
                );
                return Ok(CalendarSummary::new("rejected", 0));
            }
            Err(error)
        }
    }
}

const CURSOR_COLUMNS: &str = "sync_token, page_token, full_sync_started_at, history_start";

async fn load_cursor(
    pool: &PgPool,
    credential_id: i64,
    calendar_id: &str,
) -> Result<Option<Cursor>> {
    Ok(sqlx::query_as(&format!(
        r#"
        SELECT {CURSOR_COLUMNS}
        FROM company_context_system.google_calendars
        WHERE broker_credential_id = $1
          AND calendar_id = $2
        "#
    ))
    .bind(credential_id)
    .bind(calendar_id)
    .fetch_optional(pool)
    .await?)
}

async fn lock_cursor(
    tx: &mut Transaction<'_, Postgres>,
    credential_id: i64,
    calendar_id: &str,
) -> Result<Option<Cursor>> {
    Ok(sqlx::query_as(&format!(
        r#"
        SELECT {CURSOR_COLUMNS}
        FROM company_context_system.google_calendars
        WHERE broker_credential_id = $1
          AND calendar_id = $2
        FOR UPDATE
        "#
    ))
    .bind(credential_id)
    .bind(calendar_id)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Starts a full sync keeping events that ended after `cutoff`, if the cursor
/// is still current.
async fn restart_full_sync(
    pool: &PgPool,
    credential_id: i64,
    calendar_id: &str,
    cursor: &Cursor,
    cutoff: DateTime<Utc>,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    if lock_cursor(&mut tx, credential_id, calendar_id)
        .await?
        .as_ref()
        != Some(cursor)
    {
        return Ok(false);
    }
    sqlx::query(
        r#"
        UPDATE company_context_system.google_calendars
        SET sync_token = '',
            page_token = '',
            full_sync_started_at = NOW(),
            history_start = $3,
            updated_at = NOW()
        WHERE broker_credential_id = $1
          AND calendar_id = $2
        "#,
    )
    .bind(credential_id)
    .bind(calendar_id)
    .bind(cutoff)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Publishes a page of events, removing cancelled ones and those that ended
/// before `cutoff`, and advances the cursor it was listed from. Returns
/// `false`, without storing anything, if the cursor moved.
async fn apply_page(
    pool: &PgPool,
    credential_id: i64,
    calendar_id: &str,
    cursor: &Cursor,
    page: &EventPage,
    cutoff: DateTime<Utc>,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    if lock_cursor(&mut tx, credential_id, calendar_id)
        .await?
        .as_ref()
        != Some(cursor)
    {
        return Ok(false);
    }
    for event in page.items.iter().filter(|event| !event.id.is_empty()) {
        let Some(rendered) = event
            .render()
            .filter(|rendered| rendered.recurring || !ended_before(rendered, cutoff))
        else {
            sqlx::query(
                r#"
                DELETE FROM company_context_data.google_calendar_documents
                WHERE broker_credential_id = $1
                  AND calendar_id = $2
                  AND event_id = $3
                "#,
            )
            .bind(credential_id)
            .bind(calendar_id)
            .bind(&event.id)
            .execute(&mut *tx)
            .await?;
            continue;
        };
        sqlx::query(
            r#"
            INSERT INTO company_context_data.google_calendar_documents AS documents (
                document_id, broker_credential_id, calendar_id, event_id, provider_subject,
                title, body, url, recurring, starts_at, ends_at, organizer_email, content_hash
            )
            SELECT $1, $2, $3, $4, calendars.provider_subject,
                   $5, $6, $7, $8, $9, $10, $11, $12
            FROM company_context_system.google_calendars calendars
            WHERE calendars.broker_credential_id = $2
              AND calendars.calendar_id = $3
            ON CONFLICT (document_id) DO UPDATE
            SET provider_subject = EXCLUDED.provider_subject,
                title = EXCLUDED.title,
                body = EXCLUDED.body,
                url = EXCLUDED.url,
                recurring = EXCLUDED.recurring,
                starts_at = EXCLUDED.starts_at,
                ends_at = EXCLUDED.ends_at,
                organizer_email = EXCLUDED.organizer_email,
                content_hash = EXCLUDED.content_hash,
                updated_at = CASE WHEN documents.content_hash = EXCLUDED.content_hash
                    THEN documents.updated_at ELSE NOW() END,
                last_seen_at = NOW()
            "#,
        )
        .bind(format!(
            "{GOOGLE_CALENDAR_DOCUMENT_ID_PREFIX}{credential_id}:{calendar_id}:{}",
            event.id
        ))
        .bind(credential_id)
        .bind(calendar_id)
        .bind(&event.id)
        .bind(&rendered.title)
        .bind(&rendered.content_text)
        .bind(&rendered.url)
        .bind(rendered.recurring)
        .bind(rendered.starts_at)
        .bind(rendered.ends_at)
        .bind(&rendered.organizer_email)
        .bind(&rendered.content_hash)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(page_token) = &page.next_page_token {
        sqlx::query(
            r#"
            UPDATE company_context_system.google_calendars
            SET page_token = $3,
                last_error = '',
                updated_at = NOW()
            WHERE broker_credential_id = $1
              AND calendar_id = $2
            "#,
        )
        .bind(credential_id)
        .bind(calendar_id)
        .bind(page_token)
        .execute(&mut *tx)
        .await?;
    } else {
        let sync_token = page
            .next_sync_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .context("Google Calendar's last event page omitted nextSyncToken")?;
        // A full sync lists every event, so events it did not list are gone,
        // and events that have since aged out of the history are removed too.
        sqlx::query(
            r#"
            DELETE FROM company_context_data.google_calendar_documents
            WHERE broker_credential_id = $1
              AND calendar_id = $2
              AND (
                  last_seen_at < $3
                  OR (NOT recurring AND COALESCE(ends_at, starts_at) < $4)
              )
            "#,
        )
        .bind(credential_id)
        .bind(calendar_id)
        .bind(cursor.full_sync_started_at)
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            UPDATE company_context_system.google_calendars
            SET sync_token = $3,
                page_token = '',
                full_sync_started_at = NULL,
                last_success_at = NOW(),
                last_error = '',
                updated_at = NOW()
            WHERE broker_credential_id = $1
              AND calendar_id = $2
            "#,
        )
        .bind(credential_id)
        .bind(calendar_id)
        .bind(sync_token)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

fn ended_before(event: &RenderedEvent, cutoff: DateTime<Utc>) -> bool {
    event
        .ends_at
        .or(event.starts_at)
        .is_some_and(|end| end < cutoff)
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::env;

    use serde_json::json;

    use super::*;
    use crate::test_support::TestDatabase;

    fn credential(id: i64) -> GoogleCredential {
        GoogleCredential {
            id,
            access_token: "token".to_owned(),
            provider_email: "ada@example.com".to_owned(),
            provider_subject: format!("G-{id}"),
            revision: String::new(),
        }
    }

    fn listed(id: &str) -> CalendarListEntry {
        CalendarListEntry {
            id: id.to_owned(),
            summary: "Team".to_owned(),
        }
    }

    fn page(
        events: serde_json::Value,
        next_page: Option<&str>,
        next_sync: Option<&str>,
    ) -> EventPage {
        serde_json::from_value(json!({
            "items": events,
            "nextPageToken": next_page,
            "nextSyncToken": next_sync,
        }))
        .unwrap()
    }

    fn event(id: &str, summary: &str) -> serde_json::Value {
        json!({ "id": id, "summary": summary, "start": { "date": "2024-03-01" } })
    }

    fn weekly() -> serde_json::Value {
        json!({
            "id": "WEEKLY",
            "summary": "Standup",
            "start": { "date": "2023-06-01" },
            "end": { "date": "2023-06-02" },
            "recurrence": ["RRULE:FREQ=WEEKLY"]
        })
    }

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    /// Published documents as (document ID, subject, title).
    async fn documents(pool: &PgPool) -> Vec<(String, String, String)> {
        sqlx::query_as(
            r#"
            SELECT document_id, provider_subject, title
            FROM company_context_data.google_calendar_documents
            ORDER BY document_id
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    fn document(credential_id: i64, event_id: &str, title: &str) -> (String, String, String) {
        (
            format!("google-calendar:{credential_id}:CAL:{event_id}"),
            format!("G-{credential_id}"),
            title.to_owned(),
        )
    }

    /// Applies a page from the current cursor of credential 1's calendar,
    /// keeping events that ended after `cutoff`.
    async fn apply(pool: &PgPool, page: &EventPage, cutoff: &str) {
        let cursor = load_cursor(pool, 1, "CAL").await.unwrap().unwrap();
        assert!(
            apply_page(pool, 1, "CAL", &cursor, page, at(cutoff))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn each_credential_publishes_its_own_calendars() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "calendar").await;
        let pool = &database.pool;
        let cutoff = "2024-01-01T00:00:00Z";

        // Two credentials list the same calendar; each publishes its own copy
        // for its own Google identity.
        record_discovery(pool, &credential(1), &[listed("CAL")])
            .await
            .unwrap();
        record_discovery(pool, &credential(2), &[listed("CAL")])
            .await
            .unwrap();
        let other = load_cursor(pool, 2, "CAL").await.unwrap().unwrap();
        assert!(
            restart_full_sync(pool, 2, "CAL", &other, at(cutoff))
                .await
                .unwrap()
        );
        let other = load_cursor(pool, 2, "CAL").await.unwrap().unwrap();
        let shared = page(json!([event("E1", "Plan")]), None, Some("t1"));
        assert!(
            apply_page(pool, 2, "CAL", &other, &shared, at(cutoff))
                .await
                .unwrap()
        );

        // A full sync starts from a fresh cursor, and a stale cursor can no
        // longer advance or publish.
        let stale = load_cursor(pool, 1, "CAL").await.unwrap().unwrap();
        assert!(
            restart_full_sync(pool, 1, "CAL", &stale, at(cutoff))
                .await
                .unwrap()
        );
        assert!(
            !restart_full_sync(pool, 1, "CAL", &stale, at(cutoff))
                .await
                .unwrap()
        );
        let first = page(
            json!([event("E1", "Plan"), event("E2", "Retro")]),
            Some("p2"),
            None,
        );
        assert!(
            !apply_page(pool, 1, "CAL", &stale, &first, at(cutoff))
                .await
                .unwrap()
        );
        assert_eq!(documents(pool).await, [document(2, "E1", "Plan")]);
        apply(pool, &first, cutoff).await;
        apply(pool, &page(json!([]), None, Some("s1")), cutoff).await;
        let cursor = load_cursor(pool, 1, "CAL").await.unwrap().unwrap();
        assert_eq!(
            (cursor.sync_token.as_str(), cursor.full_sync_started_at),
            ("s1", None)
        );
        assert_eq!(cursor.history_start, Some(at(cutoff)));
        assert_eq!(
            documents(pool).await,
            [
                document(1, "E1", "Plan"),
                document(1, "E2", "Retro"),
                document(2, "E1", "Plan")
            ]
        );

        // Incremental changes update changed events and remove cancelled
        // ones. Events that ended before the cutoff are left out unless they
        // recur.
        let changes = page(
            json!([
                event("E1", "Plan v2"),
                { "id": "E2", "status": "cancelled" },
                event("E3", "Offsite"),
                { "id": "OLD", "start": { "date": "2023-06-01" }, "end": { "date": "2023-06-02" } },
                weekly(),
            ]),
            None,
            Some("s2"),
        );
        apply(pool, &changes, cutoff).await;
        assert_eq!(
            documents(pool).await,
            [
                document(1, "E1", "Plan v2"),
                document(1, "E3", "Offsite"),
                document(1, "WEEKLY", "Standup"),
                document(2, "E1", "Plan")
            ]
        );

        // A full sync removes the events it did not list.
        let cursor = load_cursor(pool, 1, "CAL").await.unwrap().unwrap();
        assert!(
            restart_full_sync(pool, 1, "CAL", &cursor, at(cutoff))
                .await
                .unwrap()
        );
        let full = page(json!([event("E1", "Plan v2"), weekly()]), None, Some("s3"));
        apply(pool, &full, cutoff).await;

        // Events that age out of the history are removed by the next sync.
        apply(
            pool,
            &page(json!([]), None, Some("s4")),
            "2024-06-01T00:00:00Z",
        )
        .await;
        assert_eq!(
            documents(pool).await,
            [document(1, "WEEKLY", "Standup"), document(2, "E1", "Plan")]
        );

        // A calendar the credential no longer lists goes with its events, as
        // do the calendars of credentials that are not retained.
        record_discovery(pool, &credential(1), &[]).await.unwrap();
        assert_eq!(documents(pool).await, [document(2, "E1", "Plan")]);
        assert_eq!(remove_unretained(pool, &[2]).await.unwrap(), 0);
        assert_eq!(remove_unretained(pool, &[]).await.unwrap(), 1);
        assert!(documents(pool).await.is_empty());

        database.drop().await;
    }
}
