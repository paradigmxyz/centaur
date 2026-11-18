//! Workflow runs, read from the api-rs database's read-only view
//! `centaur_readonly_workflow_runs`.

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};

const TABLE: &str = "centaur_readonly_workflow_runs";

const RECENCY_SQL: &str = "coalesce(completed_at, failed_at, cancelled_at, started_at, \
     first_started_at, available_at, created_at)";
const WORKFLOW_KEY_SQL: &str = "coalesce(nullif(workflow_name, ''), nullif(task_name, ''))";
const DISPLAY_STATUS_SQL: &str = "CASE WHEN cancelled_at IS NOT NULL THEN 'cancelled' \
     WHEN failed_at IS NOT NULL THEN 'failed' WHEN completed_at IS NOT NULL THEN 'completed' \
     WHEN claimed OR state = 'running' THEN 'running' ELSE coalesce(nullif(state, ''), 'unknown') END";

const COLUMNS: &str = "queue_name::text AS queue_name, run_id::text AS run_id, \
     task_id::text AS task_id, task_name::text AS task_name, \
     workflow_name::text AS workflow_name, harness_type::text AS harness_type, \
     state::text AS state, attempts::bigint AS attempts, max_attempts::bigint AS max_attempts, \
     created_at::timestamptz AS created_at, first_started_at::timestamptz AS first_started_at, \
     started_at::timestamptz AS started_at, completed_at::timestamptz AS completed_at, \
     failed_at::timestamptz AS failed_at, available_at::timestamptz AS available_at, \
     claimed::boolean AS claimed, cancelled_at::timestamptz AS cancelled_at";

#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct WorkflowRun {
    pub queue_name: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub task_name: Option<String>,
    pub workflow_name: Option<String>,
    pub harness_type: Option<String>,
    pub state: Option<String>,
    pub attempts: Option<i64>,
    pub max_attempts: Option<i64>,
    pub created_at: Option<DateTime<Utc>>,
    pub first_started_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    pub claimed: Option<bool>,
    pub cancelled_at: Option<DateTime<Utc>>,
    /// Only from [`latest_per_queue`].
    #[sqlx(default)]
    pub queue_run_count: Option<i64>,
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

impl WorkflowRun {
    pub fn workflow_key(&self) -> Option<String> {
        present(&self.workflow_name)
            .or_else(|| present(&self.task_name))
            .map(str::to_owned)
    }

    pub fn label(&self) -> String {
        self.workflow_key()
            .unwrap_or_else(|| "unknown workflow".to_owned())
    }

    pub fn queue_label(&self) -> String {
        queue_label(self.queue_name.as_deref().unwrap_or_default())
    }

    pub fn display_status(&self) -> String {
        if self.cancelled_at.is_some() {
            "cancelled".into()
        } else if self.failed_at.is_some() {
            "failed".into()
        } else if self.completed_at.is_some() {
            "completed".into()
        } else if self.claimed == Some(true) || self.state.as_deref() == Some("running") {
            "running".into()
        } else {
            present(&self.state).unwrap_or("unknown").to_owned()
        }
    }

    pub fn started_or_created_at(&self) -> Option<DateTime<Utc>> {
        self.started_at
            .or(self.first_started_at)
            .or(self.created_at)
    }

    pub fn terminal_at(&self) -> Option<DateTime<Utc>> {
        self.completed_at.or(self.failed_at).or(self.cancelled_at)
    }

    pub fn recency_at(&self) -> Option<DateTime<Utc>> {
        self.terminal_at().or(self
            .started_at
            .or(self.first_started_at)
            .or(self.available_at)
            .or(self.created_at))
    }
}

/// `centaur_workflows_high_priority` is "high priority", the bare prefix is
/// "default".
pub fn queue_label(queue_name: &str) -> String {
    let suffix = queue_name
        .strip_prefix("centaur_workflows")
        .unwrap_or(queue_name);
    let suffix = suffix.strip_prefix('_').unwrap_or(suffix);
    if suffix.trim().is_empty() {
        "default".into()
    } else {
        suffix.replace('_', " ")
    }
}

/// Whether the view exists.
pub async fn available(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(TABLE)
        .fetch_one(pool)
        .await
}

pub async fn workflow_count<'e>(db: impl PgExecutor<'e>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM (SELECT DISTINCT {WORKFLOW_KEY_SQL} FROM {TABLE}) workflow_keys"
    ))
    .fetch_one(db)
    .await
}

/// The newest run of each workflow, newest first.
pub async fn latest_per_workflow<'e>(
    db: impl PgExecutor<'e>,
    limit: i64,
    offset: i64,
) -> Result<Vec<WorkflowRun>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM (SELECT DISTINCT ON ({WORKFLOW_KEY_SQL}) * FROM {TABLE} \
         ORDER BY {WORKFLOW_KEY_SQL}, {RECENCY_SQL} DESC, task_id DESC) latest_runs \
         ORDER BY {RECENCY_SQL} DESC, task_id DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

/// The newest run per (workflow, queue) with its run count, newest first
/// within each workflow.
pub async fn latest_per_queue<'e>(
    db: impl PgExecutor<'e>,
    workflow_keys: &[String],
) -> Result<Vec<WorkflowRun>, sqlx::Error> {
    if workflow_keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut runs: Vec<WorkflowRun> = sqlx::query_as(&format!(
        "SELECT {COLUMNS}, queue_run_count FROM (\
           SELECT DISTINCT ON ({WORKFLOW_KEY_SQL}, queue_name) *, \
             COUNT(*) OVER (PARTITION BY {WORKFLOW_KEY_SQL}, queue_name) AS queue_run_count \
           FROM {TABLE} WHERE {WORKFLOW_KEY_SQL} = ANY($1) \
           ORDER BY {WORKFLOW_KEY_SQL}, queue_name, {RECENCY_SQL} DESC, task_id DESC) runs"
    ))
    .bind(workflow_keys)
    .fetch_all(db)
    .await?;
    runs.sort_by_key(|run| std::cmp::Reverse(run.recency_at()));
    Ok(runs)
}

/// The rows of one workflow: `$1` its name, `$2` a display status, `$3` a
/// queue (each filter optional).
fn scope() -> String {
    format!(
        "(workflow_name = $1 OR ((workflow_name IS NULL OR workflow_name = '') AND task_name = $1)) \
         AND ($2::text IS NULL OR {DISPLAY_STATUS_SQL} = $2) AND ($3::text IS NULL OR queue_name = $3)"
    )
}

pub async fn for_workflow<'e>(
    db: impl PgExecutor<'e>,
    workflow: &str,
    limit: i64,
    offset: i64,
    status: Option<&str>,
    queue: Option<&str>,
) -> Result<Vec<WorkflowRun>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM {TABLE} WHERE {} ORDER BY {RECENCY_SQL} DESC, task_id DESC \
         LIMIT $4 OFFSET $5",
        scope()
    ))
    .bind(workflow)
    .bind(status)
    .bind(queue)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

pub async fn run_count<'e>(
    db: impl PgExecutor<'e>,
    workflow: &str,
    status: Option<&str>,
    queue: Option<&str>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {TABLE} WHERE {}", scope()))
        .bind(workflow)
        .bind(status)
        .bind(queue)
        .fetch_one(db)
        .await
}

/// `(status, count)` of one workflow's runs.
pub async fn status_counts<'e>(
    db: impl PgExecutor<'e>,
    workflow: &str,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {DISPLAY_STATUS_SQL} AS status, COUNT(*) FROM {TABLE} WHERE {} GROUP BY 1",
        scope()
    ))
    .bind(workflow)
    .bind(None::<String>)
    .bind(None::<String>)
    .fetch_all(db)
    .await
}

pub async fn queue_names<'e>(
    db: impl PgExecutor<'e>,
    workflow: &str,
) -> Result<Vec<Option<String>>, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT DISTINCT queue_name::text FROM {TABLE} WHERE {} ORDER BY 1",
        scope()
    ))
    .bind(workflow)
    .bind(None::<String>)
    .bind(None::<String>)
    .fetch_all(db)
    .await
}
