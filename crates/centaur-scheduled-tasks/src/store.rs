//! Read-only queries against the console's `scheduled_tasks` table and the
//! runner's Absurd tick table.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};

/// The scheduling columns of a `scheduled_tasks` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub id: i64,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_enqueued_at: Option<DateTime<Utc>>,
}

/// Rails `datetime` columns are `timestamp without time zone` holding UTC.
fn utc(value: Option<NaiveDateTime>) -> Option<DateTime<Utc>> {
    value.map(|value| value.and_utc())
}

pub async fn find_task(console_db: &PgPool, id: i64) -> Result<Option<TaskRow>, sqlx::Error> {
    let row = sqlx::query(
        "select id, enabled, next_run_at, last_enqueued_at from scheduled_tasks where id = $1",
    )
    .bind(id)
    .fetch_optional(console_db)
    .await?;
    row.map(|row| {
        Ok(TaskRow {
            id: row.try_get("id")?,
            enabled: row.try_get("enabled")?,
            next_run_at: utc(row.try_get("next_run_at")?),
            last_enqueued_at: utc(row.try_get("last_enqueued_at")?),
        })
    })
    .transpose()
}

/// Every task the scheduler would fire: Rails' `due` scope without the time
/// bound. A due task does not need a cron.
pub async fn scheduled_tasks(
    console_db: &PgPool,
) -> Result<Vec<(i64, DateTime<Utc>)>, sqlx::Error> {
    let rows = sqlx::query(
        "select id, next_run_at from scheduled_tasks \
         where enabled and next_run_at is not null order by next_run_at, id",
    )
    .fetch_all(console_db)
    .await?;
    rows.into_iter()
        .map(|row| {
            let at: NaiveDateTime = row.try_get("next_run_at")?;
            Ok((row.try_get("id")?, at.and_utc()))
        })
        .collect()
}

/// Which of `ids` still exist.
pub async fn existing_task_ids(console_db: &PgPool, ids: &[i64]) -> Result<Vec<i64>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar("select id from scheduled_tasks where id = any($1)")
        .bind(ids)
        .fetch_all(console_db)
        .await
}

/// A tick task row.
#[derive(Clone, Debug, PartialEq)]
pub struct TickRow {
    pub task_id: String,
    pub state: String,
    pub idempotency_key: Option<String>,
    pub params: Value,
}

impl TickRow {
    pub fn scheduled_task_id(&self) -> Option<i64> {
        self.params.get("scheduled_task_id").and_then(Value::as_i64)
    }

    pub fn manual(&self) -> bool {
        self.params
            .get("manual")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// The runner's queue tables. Queue names are validated by
/// [`crate::RunnerConfig`] to be plain identifiers.
pub(crate) fn task_table(queue: &str) -> String {
    format!("absurd.\"t_{queue}\"")
}

fn tick_from_row(row: sqlx::postgres::PgRow) -> Result<TickRow, sqlx::Error> {
    Ok(TickRow {
        task_id: row.try_get("task_id")?,
        state: row.try_get("state")?,
        idempotency_key: row.try_get("idempotency_key")?,
        params: row.try_get("params")?,
    })
}

/// Ticks with any of `keys`, in any state.
pub async fn ticks_by_key(
    pool: &PgPool,
    queue: &str,
    keys: &[String],
) -> Result<Vec<TickRow>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(&format!(
        "select task_id::text as task_id, state, idempotency_key, params \
         from {} where idempotency_key = any($1)",
        task_table(queue)
    ))
    .bind(keys)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(tick_from_row).collect()
}

/// Ticks that are not completed or cancelled (pending, running, sleeping,
/// failed), optionally for one scheduled task.
pub async fn open_ticks(
    pool: &PgPool,
    queue: &str,
    task_name: &str,
    scheduled_task_id: Option<i64>,
) -> Result<Vec<TickRow>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "select task_id::text as task_id, state, idempotency_key, params \
         from {} where task_name = $1 and state not in ('completed', 'cancelled') \
         and ($2::bigint is null or (params->>'scheduled_task_id')::bigint = $2)",
        task_table(queue)
    ))
    .bind(task_name)
    .bind(scheduled_task_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(tick_from_row).collect()
}

/// Whether an earlier scheduled tick of the task (`scheduled_for < before`,
/// other than `own`) is still open and has not checkpointed its claim.
///
/// A claim advances the task's `next_run_at` and overwrites
/// `last_enqueued_at`, which is all the console remembers of an occurrence it
/// claimed: if the answer to that claim was lost, only a retry of the same
/// tick can still recognize it. So a later occurrence does not claim before
/// every earlier tick has durably checkpointed its claim.
///
/// A failed tick is not pending: it exhausted its attempts without
/// checkpointing, and the console's compare-and-set decides the later claim.
pub async fn earlier_claim_pending(
    pool: &PgPool,
    queue: &str,
    task_name: &str,
    scheduled_task_id: i64,
    before: DateTime<Utc>,
    own: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "select exists (select 1 from {tasks} t \
         where t.task_name = $1 and t.state not in ('completed', 'cancelled', 'failed') \
           and (t.params->>'scheduled_task_id')::bigint = $2 \
           and not coalesce((t.params->>'manual')::boolean, false) \
           and (t.params->>'scheduled_for')::timestamptz < $3 \
           and t.task_id::text <> $4 \
           and not exists (select 1 from absurd.\"c_{queue}\" c \
                where c.task_id = t.task_id and c.checkpoint_name = 'claim' \
                  and c.status = 'committed'))",
        tasks = task_table(queue)
    ))
    .bind(task_name)
    .bind(scheduled_task_id)
    .bind(before)
    .bind(own)
    .fetch_one(pool)
    .await
}
