//! Scheduled tasks: the stored row, validation, and the targeted writes
//! shared by the console, sandbox, and bearer endpoints and by the runner's
//! claim/result endpoints (`runner.rs`).
//!
//! A task row has two kinds of columns. The user-editable ones (`name`,
//! `prompt`, `delivery_channel`, `cron_expression`, `enabled`) are written
//! by [`create`] and [`update`]. The runner-owned ones (`next_run_at`,
//! `last_enqueued_at`, `last_run_id`, `last_run_at`, `last_error`) are
//! written only by the claim and result endpoints; an edit rewrites
//! `next_run_at` solely when the schedule it changes (cron or enabled)
//! needs a new one, under the task's row lock.

use centaur_console_db::{oid, schedule};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, QueryBuilder};

use super::delivery::{self, SlackIdentity};
use crate::{ApiError, ApiResult, AppState, FieldErrors};

pub const OID_PREFIX: &str = "tsk";
pub const MAX_NAME: usize = 100;
pub const MAX_PROMPT: usize = 64 * 1024;

pub const COLUMNS: &str = "id, author_id, principal_id, name, prompt, delivery_channel, \
     cron_expression, timezone, enabled, \
     next_run_at AT TIME ZONE 'UTC' AS next_run_at, \
     last_enqueued_at AT TIME ZONE 'UTC' AS last_enqueued_at, last_run_id, \
     last_run_at AT TIME ZONE 'UTC' AS last_run_at, last_error, \
     created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at";

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Task {
    pub id: i64,
    pub author_id: Option<i64>,
    pub principal_id: Option<i64>,
    pub name: String,
    pub prompt: String,
    pub delivery_channel: String,
    pub cron_expression: Option<String>,
    pub timezone: String,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_enqueued_at: Option<DateTime<Utc>>,
    pub last_run_id: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    pub fn oid(&self) -> String {
        oid(OID_PREFIX, self.id)
    }
}

/// Who owns a task: a console user, or a Slack channel principal (whose
/// tasks always deliver to that channel).
#[derive(Clone, Debug)]
pub enum Owner {
    Author(i64),
    Channel {
        principal_id: i64,
        channel_id: Option<String>,
        team_id: Option<String>,
    },
}

impl Owner {
    fn column(&self) -> (&'static str, i64) {
        match self {
            Self::Author(id) => ("author_id", *id),
            Self::Channel { principal_id, .. } => ("principal_id", *principal_id),
        }
    }

    /// Whether `destination` is available to this owner.
    pub async fn destination_available(
        &self,
        conn: &mut PgConnection,
        destination: &str,
    ) -> Result<bool, sqlx::Error> {
        match self {
            Self::Channel { team_id, .. } => {
                delivery::active_channel_exists(conn, team_id.as_deref(), destination).await
            }
            Self::Author(user_id) => {
                let identity = delivery::slack_identity(conn, *user_id).await?;
                delivery::allowed(conn, identity.as_ref(), destination).await
            }
        }
    }

    pub async fn identity(
        &self,
        conn: &mut PgConnection,
    ) -> Result<Option<SlackIdentity>, sqlx::Error> {
        match self {
            Self::Author(user_id) => delivery::slack_identity(conn, *user_id).await,
            Self::Channel { .. } => Ok(None),
        }
    }
}

// --- normalization ----------------------------------------------------------

pub fn normalize_name(value: &str) -> String {
    value.trim().to_owned()
}

pub fn normalize_prompt(value: &str) -> String {
    value.replace("\r\n", "\n").trim().to_owned()
}

pub fn normalize_cron(value: Option<&str>) -> Option<String> {
    schedule::normalize_cron_expression(value)
}

// --- reads ------------------------------------------------------------------

pub async fn list_owned(state: &AppState, owner: &Owner) -> Result<Vec<Task>, sqlx::Error> {
    let (column, id) = owner.column();
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM scheduled_tasks WHERE {column} = $1 ORDER BY name, id"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await
}

fn decode(id: &str) -> ApiResult<i64> {
    centaur_console_db::decode_oid(OID_PREFIX, id).ok_or_else(ApiError::not_found)
}

/// The owner's task, or a 404 (other owners' tasks are not found).
pub async fn find_owned(state: &AppState, owner: &Owner, oid: &str) -> ApiResult<Task> {
    let (column, id) = owner.column();
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM scheduled_tasks WHERE id = $1 AND {column} = $2"
    ))
    .bind(decode(oid)?)
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)
}

// --- validation -------------------------------------------------------------

struct Fields<'a> {
    name: &'a str,
    prompt: &'a str,
    delivery_channel: &'a str,
    cron: Option<&'a str>,
    timezone: &'a str,
}

async fn validate(
    conn: &mut PgConnection,
    owner: &Owner,
    fields: &Fields<'_>,
    destination_changed: bool,
) -> ApiResult<()> {
    let mut errors = FieldErrors::new();
    if fields.name.is_empty() {
        errors.add("name", "can't be blank");
    } else if fields.name.chars().count() > MAX_NAME {
        errors.add(
            "name",
            format!("is too long (maximum is {MAX_NAME} characters)"),
        );
    }
    if fields.prompt.is_empty() {
        errors.add("prompt", "can't be blank");
    } else if fields.prompt.chars().count() > MAX_PROMPT {
        errors.add(
            "prompt",
            format!("is too long (maximum is {MAX_PROMPT} characters)"),
        );
    }
    if fields.delivery_channel.is_empty() {
        errors.add("delivery_channel", "can't be blank");
    } else if !delivery::valid_destination(fields.delivery_channel) {
        errors.add("delivery_channel", "must be a Slack channel or user ID");
    } else if destination_changed
        && !owner
            .destination_available(conn, fields.delivery_channel)
            .await?
    {
        errors.add(
            "delivery_channel",
            match owner {
                Owner::Channel { .. } => "is not a channel the Slack bot is in",
                Owner::Author(_) => "is not available to the author",
            },
        );
    }
    if let Some(message) = schedule::schedule_error(fields.cron, fields.timezone) {
        errors.add("cron_expression", message);
    }
    errors.finish()
}

// --- writes -----------------------------------------------------------------

pub struct NewTask {
    pub name: String,
    pub prompt: String,
    pub delivery_channel: String,
    pub cron_expression: Option<String>,
    pub enabled: bool,
}

pub async fn create(state: &AppState, owner: &Owner, new: NewTask) -> ApiResult<Task> {
    let timezone = schedule::DEFAULT_TIMEZONE;
    let mut conn = state.db.acquire().await?;
    validate(
        &mut conn,
        owner,
        &Fields {
            name: &new.name,
            prompt: &new.prompt,
            delivery_channel: &new.delivery_channel,
            cron: new.cron_expression.as_deref(),
            timezone,
        },
        true,
    )
    .await?;
    let now = state.now();
    let next =
        schedule::next_run_if_enabled(new.enabled, new.cron_expression.as_deref(), timezone, now);
    let (column, owner_id) = owner.column();
    sqlx::query_as(&format!(
        "INSERT INTO scheduled_tasks ({column}, name, prompt, delivery_channel, cron_expression, \
           timezone, enabled, next_run_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) RETURNING {COLUMNS}"
    ))
    .bind(owner_id)
    .bind(&new.name)
    .bind(&new.prompt)
    .bind(&new.delivery_channel)
    .bind(&new.cron_expression)
    .bind(timezone)
    .bind(new.enabled)
    .bind(next.map(|time| time.naive_utc()))
    .bind(now.naive_utc())
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| ApiError::unique(error, "name", "has already been taken"))
}

/// The fields a request changes. `cron_expression: Some(None)` clears the
/// schedule.
#[derive(Default)]
pub struct Edit {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub delivery_channel: Option<String>,
    pub cron_expression: Option<Option<String>>,
    pub enabled: Option<bool>,
}

/// Applies `edit` to the owner's task. The row is locked, so the values the
/// edit is validated and compared against are current; only the columns the
/// edit changes are written, plus `next_run_at` when the schedule changed.
pub async fn update(state: &AppState, owner: &Owner, oid: &str, edit: Edit) -> ApiResult<Task> {
    let id = decode(oid)?;
    let (column, owner_id) = owner.column();
    let mut tx = state.db.begin().await?;
    let current: Task = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM scheduled_tasks WHERE id = $1 AND {column} = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;

    let name = edit.name.unwrap_or_else(|| current.name.clone());
    let prompt = edit.prompt.unwrap_or_else(|| current.prompt.clone());
    let delivery_channel = edit
        .delivery_channel
        .unwrap_or_else(|| current.delivery_channel.clone());
    let cron = edit
        .cron_expression
        .unwrap_or_else(|| current.cron_expression.clone());
    let enabled = edit.enabled.unwrap_or(current.enabled);
    validate(
        &mut tx,
        owner,
        &Fields {
            name: &name,
            prompt: &prompt,
            delivery_channel: &delivery_channel,
            cron: cron.as_deref(),
            timezone: &current.timezone,
        },
        delivery_channel != current.delivery_channel,
    )
    .await?;

    let now = state.now();
    let schedule_changed = enabled != current.enabled || cron != current.cron_expression;
    let mut query = QueryBuilder::new("UPDATE scheduled_tasks SET ");
    let mut set = query.separated(", ");
    let mut changed = false;
    if name != current.name {
        set.push("name = ").push_bind_unseparated(name);
        changed = true;
    }
    if prompt != current.prompt {
        set.push("prompt = ").push_bind_unseparated(prompt);
        changed = true;
    }
    if delivery_channel != current.delivery_channel {
        set.push("delivery_channel = ")
            .push_bind_unseparated(delivery_channel);
        changed = true;
    }
    if schedule_changed {
        let next = schedule::next_run_if_enabled(enabled, cron.as_deref(), &current.timezone, now);
        set.push("cron_expression = ").push_bind_unseparated(cron);
        set.push("enabled = ").push_bind_unseparated(enabled);
        set.push("next_run_at = ")
            .push_bind_unseparated(next.map(|time| time.naive_utc()));
        changed = true;
    }
    if !changed {
        tx.rollback().await?;
        return Ok(current);
    }
    set.push("updated_at = ")
        .push_bind_unseparated(now.naive_utc());
    query
        .push(" WHERE id = ")
        .push_bind(id)
        .push(format!(" RETURNING {COLUMNS}"));
    let updated = query
        .build_query_as::<Task>()
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| ApiError::unique(error, "name", "has already been taken"))?;
    tx.commit().await?;
    Ok(updated)
}

pub async fn delete(state: &AppState, owner: &Owner, oid: &str) -> ApiResult<Task> {
    let (column, owner_id) = owner.column();
    sqlx::query_as(&format!(
        "DELETE FROM scheduled_tasks WHERE id = $1 AND {column} = $2 RETURNING {COLUMNS}"
    ))
    .bind(decode(oid)?)
    .bind(owner_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)
}

// --- api-rs hints -----------------------------------------------------------

/// Tells api-rs to reconcile the task's tick. A hint (api-rs also
/// reconciles on an interval): failures are logged, never returned.
pub async fn notify_reconcile(state: &AppState, oid: &str) {
    if let Err(error) = super::centaur_api::CentaurApiClient::new(state)
        .reconcile_scheduled_task(oid)
        .await
    {
        tracing::warn!(scheduled_task = %oid, %error, "scheduled_task_reconcile_notify_failed");
    }
}

/// Asks api-rs for a manual run; whether it was queued.
pub async fn queue_manual_run(state: &AppState, oid: &str) -> bool {
    match super::centaur_api::CentaurApiClient::new(state)
        .run_scheduled_task(oid)
        .await
    {
        Ok(queued) => queued,
        Err(error) => {
            tracing::warn!(scheduled_task = %oid, %error, "scheduled_task_run_queue_failed");
            false
        }
    }
}

pub fn schedule_label(task: &Task) -> String {
    schedule::label(task.cron_expression.as_deref())
}
