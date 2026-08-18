use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{Json, extract::State};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::{error::ApiError, routes::AppState};

// Read-only operational snapshot for status surfaces (chat-bot "status"
// commands, dashboards): recent/in-flight executions, a 24h tally, active
// session sandboxes, warm-pool state, and a 7-calendar-day (UTC) run
// histogram. api-rs owns the session schema, so the SQL lives here rather
// than in every ingress service that wants a status view.
//
// The report is cached briefly in-process: the history queries scan
// session_executions (append-only, never pruned), and status commands are
// human-triggered but scriptable — the cache caps the database cost at one
// scan set per TTL regardless of how often callers ask.

const CACHE_TTL: Duration = Duration::from_secs(10);
const ERROR_SNIPPET_CHARS: i32 = 200;
const RECENT_LIMIT: i64 = 20;

static CACHE: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();

pub(crate) async fn status_report(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((stored_at, report)) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        && stored_at.elapsed() < CACHE_TTL
    {
        return Ok(Json(report));
    }

    let pool = state.pool()?;
    let report = build_report(&pool).await?;
    *cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((Instant::now(), report.clone()));
    Ok(Json(report))
}

#[derive(Serialize, sqlx::FromRow)]
struct ExecutionRow {
    /// Seconds since the execution was created.
    age_seconds: f64,
    duration_seconds: Option<f64>,
    error: Option<String>,
    status: String,
    thread_key: String,
    title: Option<String>,
    user_name: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct CountRow {
    count: i64,
    status: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct SandboxRow {
    idle_seconds: f64,
    sandbox_id: String,
    thread_key: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct DailyRow {
    /// UTC calendar date, `YYYY-MM-DD`.
    day: String,
    failed: i64,
    runs: i64,
}

async fn build_report(pool: &PgPool) -> Result<Value, ApiError> {
    let recent = sqlx::query_as::<_, ExecutionRow>(
        "SELECT e.thread_key, e.status, \
                left(e.error, $1) AS error, \
                extract(epoch FROM (now() - e.created_at))::float8 AS age_seconds, \
                extract(epoch FROM (e.completed_at - e.started_at))::float8 AS duration_seconds, \
                e.metadata ->> 'user_name' AS user_name, \
                coalesce(s.title, s.metadata ->> 'discord_conversation_name', \
                         s.metadata ->> 'linear_conversation_name', \
                         s.metadata ->> 'slack_conversation_name') AS title \
         FROM session_executions e \
         LEFT JOIN sessions s ON s.thread_key = e.thread_key \
         ORDER BY e.created_at DESC \
         LIMIT $2",
    )
    .bind(ERROR_SNIPPET_CHARS)
    .bind(RECENT_LIMIT)
    .fetch_all(pool)
    .await?;

    let in_flight = sqlx::query_as::<_, ExecutionRow>(
        "SELECT e.thread_key, e.status, \
                NULL::text AS error, \
                extract(epoch FROM (now() - e.created_at))::float8 AS age_seconds, \
                NULL::float8 AS duration_seconds, \
                e.metadata ->> 'user_name' AS user_name, \
                coalesce(s.title, s.metadata ->> 'discord_conversation_name', \
                         s.metadata ->> 'linear_conversation_name', \
                         s.metadata ->> 'slack_conversation_name') AS title \
         FROM session_executions e \
         LEFT JOIN sessions s ON s.thread_key = e.thread_key \
         WHERE e.status IN ('queued', 'running') \
         ORDER BY e.created_at ASC \
         LIMIT $1",
    )
    .bind(RECENT_LIMIT)
    .fetch_all(pool)
    .await?;

    let tally_24h = sqlx::query_as::<_, CountRow>(
        "SELECT status, count(*) AS count \
         FROM session_executions \
         WHERE created_at > now() - interval '24 hours' \
         GROUP BY status",
    )
    .fetch_all(pool)
    .await?;

    let active_sandboxes = sqlx::query_as::<_, SandboxRow>(
        "SELECT thread_key, sandbox_id, \
                extract(epoch FROM (now() - sandbox_last_active_at))::float8 AS idle_seconds \
         FROM sessions \
         WHERE sandbox_id IS NOT NULL \
           AND sandbox_last_active_at > now() - interval '2 hours' \
         ORDER BY sandbox_last_active_at DESC \
         LIMIT $1",
    )
    .bind(RECENT_LIMIT)
    .fetch_all(pool)
    .await?;

    // ready/evicting are the pool's current state. claimed/failed rows are
    // lifetime history (claiming flips status in place, rows are never
    // deleted), so an unfiltered count reads like a leak — window them to 24h
    // churn instead.
    let warm_pool = sqlx::query_as::<_, CountRow>(
        "SELECT status, count(*) AS count \
         FROM session_warm_sandboxes \
         WHERE status IN ('ready', 'evicting') \
            OR updated_at > now() - interval '24 hours' \
         GROUP BY status",
    )
    .fetch_all(pool)
    .await?;

    // Calendar-day (UTC) buckets over the last 7 days INCLUDING today, so the
    // per-day rows and any total computed from them describe the same window
    // (a rolling now()-7d fetch would include a partial 8th calendar day).
    let daily = sqlx::query_as::<_, DailyRow>(
        "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS day, \
                count(*) AS runs, \
                count(*) FILTER (WHERE status = 'failed') AS failed \
         FROM session_executions \
         WHERE created_at >= \
               (date_trunc('day', now() AT TIME ZONE 'UTC') - interval '6 days') \
                   AT TIME ZONE 'UTC' \
         GROUP BY 1 \
         ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;

    Ok(json!({
        "ok": true,
        "active_sandboxes": active_sandboxes,
        "daily": daily,
        "in_flight": in_flight,
        "recent_executions": recent,
        "tally_24h": tally_24h,
        "warm_pool": warm_pool,
    }))
}
