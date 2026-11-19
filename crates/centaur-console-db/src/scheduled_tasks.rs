//! Runs Console's scheduled tasks.
//!
//! Console owns the `scheduled_tasks` rows: users create and edit them, and
//! Console computes `next_run_at` whenever a schedule changes. This module is
//! the scheduler. It claims due tasks, starts their `console_workflow` runs,
//! and writes the scheduling and outcome columns back, as Console's Solid
//! Queue jobs did:
//!
//! - `next_run_at` and `last_enqueued_at` advance when an occurrence is
//!   claimed;
//! - `last_run_id` and `last_run_at` record the started run;
//! - `last_error` records why an occurrence did not start, and clears when one
//!   does.
//!
//! Connections to Console's database are read-only by default, so every write
//! here runs in an explicitly read-write transaction. A due task is claimed
//! with `FOR UPDATE SKIP LOCKED`, and its run is started before the claim
//! commits under an idempotency key naming the task and occurrence, so
//! concurrent schedulers neither skip nor duplicate a run.

use std::future::Future;

use chrono::{DateTime, SubsecRound, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;

use crate::{Result, cron::Schedule, oid::oid};

/// Console's opaque-id prefix for scheduled tasks.
pub const OID_PREFIX: &str = "tsk";
/// The workflow that runs a task's prompt and delivers the response.
pub const WORKFLOW_NAME: &str = "console_workflow";
/// Attempts for each workflow run.
pub const MAX_ATTEMPTS: i32 = 3;

/// Due tasks claimed per pass.
const BATCH_SIZE: i64 = 100;

/// How late an occurrence may start after transient failures. Past it, the
/// occurrence is skipped so a long outage does not fire stale runs.
const RETRY_WINDOW: chrono::Duration = chrono::Duration::minutes(15);

const DESTINATION_UNAVAILABLE: &str =
    "Slack delivery destination is no longer available to the task owner";

const TASK_COLUMNS: &str = "id, name, prompt, author_id, principal_id, delivery_channel, \
     cron_expression, timezone, enabled, next_run_at AT TIME ZONE 'UTC' AS next_run_at";

/// A workflow run to start for one occurrence of a task.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRequest {
    pub task_id: i64,
    pub workflow_name: &'static str,
    pub input: Value,
    pub idempotency_key: String,
    pub max_attempts: i32,
}

/// Why a workflow run did not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartError {
    pub message: String,
    /// Whether a later attempt may succeed, as with an unavailable database.
    pub retryable: bool,
}

/// What a scheduler pass did with one due task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Started {
        task_id: i64,
        run_id: String,
    },
    /// The occurrence did not start and will not be retried.
    Failed {
        task_id: i64,
        error: String,
    },
    /// The occurrence did not start and stays due for the next pass.
    Deferred {
        task_id: i64,
        error: String,
    },
}

/// The result of running a task on demand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManualRun {
    Started { run_id: String },
    NotFound,
    Disabled,
    Failed(StartError),
}

#[derive(Clone, Debug)]
struct Task {
    id: i64,
    name: String,
    prompt: String,
    author_id: Option<i64>,
    principal_id: Option<i64>,
    delivery_channel: String,
    cron_expression: Option<String>,
    timezone: String,
    enabled: bool,
    next_run_at: Option<DateTime<Utc>>,
}

/// Starts every task due at `now`, oldest first.
pub async fn run_due<S, F>(pool: &PgPool, now: DateTime<Utc>, start: S) -> Result<Vec<Outcome>>
where
    S: Fn(RunRequest) -> F,
    F: Future<Output = std::result::Result<String, StartError>>,
{
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM scheduled_tasks \
         WHERE enabled AND next_run_at <= ($1::timestamptz AT TIME ZONE 'UTC') \
         ORDER BY next_run_at, id LIMIT $2",
    )
    .bind(offset(now))
    .bind(BATCH_SIZE)
    .fetch_all(pool)
    .await?;
    let mut outcomes = Vec::new();
    for id in ids {
        if let Some(outcome) = run_due_task(pool, id, now, &start).await? {
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

async fn run_due_task<S, F>(
    pool: &PgPool,
    id: i64,
    now: DateTime<Utc>,
    start: &S,
) -> Result<Option<Outcome>>
where
    S: Fn(RunRequest) -> F,
    F: Future<Output = std::result::Result<String, StartError>>,
{
    let mut tx = begin_write(pool).await?;
    // Another scheduler holding the row is already running this occurrence.
    let Some(task) = lock_task(&mut tx, id, "FOR UPDATE SKIP LOCKED").await? else {
        return Ok(None);
    };
    let Some(scheduled_for) = task.next_run_at.filter(|due| task.enabled && *due <= now) else {
        return Ok(None);
    };
    let (next_run_at, schedule_error) = match next_occurrence(&task, now) {
        Ok(next) => (next, None),
        Err(error) => (None, Some(error)),
    };

    let started = match run_input(&mut tx, &task).await? {
        Ok(input) => start(run_request(&task, input, scheduled_for)).await,
        Err(message) => Err(StartError {
            message,
            retryable: false,
        }),
    };
    let outcome = match started {
        Ok(run_id) => {
            advance(&mut tx, id, scheduled_for, next_run_at).await?;
            record_run(&mut tx, id, &run_id, now, schedule_error.as_deref()).await?;
            Outcome::Started {
                task_id: id,
                run_id,
            }
        }
        Err(error) if error.retryable && now - scheduled_for < RETRY_WINDOW => {
            record_error(&mut tx, id, &error.message).await?;
            Outcome::Deferred {
                task_id: id,
                error: error.message,
            }
        }
        Err(error) => {
            advance(&mut tx, id, scheduled_for, next_run_at).await?;
            let message = schedule_error.unwrap_or(error.message);
            record_error(&mut tx, id, &message).await?;
            Outcome::Failed {
                task_id: id,
                error: message,
            }
        }
    };
    tx.commit().await?;
    Ok(Some(outcome))
}

/// Runs an enabled task now, outside its schedule. `scheduled_for` names the
/// occurrence in the run's idempotency key; it defaults to `now`.
pub async fn run_now<S, F>(
    pool: &PgPool,
    id: i64,
    scheduled_for: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    start: S,
) -> Result<ManualRun>
where
    S: FnOnce(RunRequest) -> F,
    F: Future<Output = std::result::Result<String, StartError>>,
{
    let mut tx = begin_write(pool).await?;
    let Some(task) = lock_task(&mut tx, id, "FOR UPDATE").await? else {
        return Ok(ManualRun::NotFound);
    };
    if !task.enabled {
        return Ok(ManualRun::Disabled);
    }
    let started = match run_input(&mut tx, &task).await? {
        Ok(input) => start(run_request(&task, input, scheduled_for.unwrap_or(now))).await,
        Err(message) => Err(StartError {
            message,
            retryable: false,
        }),
    };
    let result = match started {
        Ok(run_id) => {
            record_run(&mut tx, id, &run_id, now, None).await?;
            ManualRun::Started { run_id }
        }
        Err(error) => {
            record_error(&mut tx, id, &error.message).await?;
            ManualRun::Failed(error)
        }
    };
    tx.commit().await?;
    Ok(result)
}

fn run_request(task: &Task, input: Value, scheduled_for: DateTime<Utc>) -> RunRequest {
    RunRequest {
        task_id: task.id,
        workflow_name: WORKFLOW_NAME,
        input,
        idempotency_key: idempotency_key(task.id, scheduled_for),
        max_attempts: MAX_ATTEMPTS,
    }
}

/// The key Console's run job used, so an occurrence it already started is not
/// started again.
fn idempotency_key(task_id: i64, scheduled_for: DateTime<Utc>) -> String {
    format!(
        "scheduled-task:{task_id}:{}",
        scheduled_for.trunc_subsecs(0).format("%Y-%m-%dT%H:%M:%SZ")
    )
}

/// The next occurrence after `now`, or `None` for a task without a schedule.
fn next_occurrence(
    task: &Task,
    now: DateTime<Utc>,
) -> std::result::Result<Option<DateTime<Utc>>, String> {
    let Some(expression) = task
        .cron_expression
        .as_deref()
        .filter(|cron| !cron.trim().is_empty())
    else {
        return Ok(None);
    };
    let schedule = Schedule::parse(expression, &task.timezone)
        .map_err(|error| format!("schedule is invalid: {error}"))?;
    schedule
        .next_after(now)
        .map(Some)
        .ok_or_else(|| format!("schedule {expression:?} has no next run"))
}

/// The `console_workflow` input, or why the task cannot run: its owner can no
/// longer deliver to its destination, or has no principal to run as.
async fn run_input(
    connection: &mut PgConnection,
    task: &Task,
) -> Result<std::result::Result<Value, String>> {
    let mut input = Map::new();
    input.insert("prompt".to_owned(), json!(task.prompt));
    if let Some(principal_id) = task.principal_id {
        let row = sqlx::query("SELECT foreign_id, slack_team_id FROM principals WHERE id = $1")
            .bind(principal_id)
            .fetch_optional(&mut *connection)
            .await?;
        let Some(row) = row else {
            return Ok(Err(
                "the task's Slack channel principal no longer exists".to_owned()
            ));
        };
        let Some(foreign_id) = row.try_get::<Option<String>, _>("foreign_id")? else {
            return Ok(Err(
                "the task's Slack channel principal has no foreign ID".to_owned()
            ));
        };
        let team_id = present(row.try_get("slack_team_id")?);
        if !bot_channel_available(connection, &task.delivery_channel, team_id.as_deref(), None)
            .await?
        {
            return Ok(Err(DESTINATION_UNAVAILABLE.to_owned()));
        }
        input.insert("principal".to_owned(), json!(foreign_id));
    } else if let Some(author_id) = task.author_id {
        let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(author_id)
            .fetch_optional(&mut *connection)
            .await?;
        let Some(email) = email else {
            return Ok(Err("the task's author no longer exists".to_owned()));
        };
        let foreign_id = console_user_foreign_id(&email);
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM principals WHERE foreign_id = $1)")
                .bind(&foreign_id)
                .fetch_one(&mut *connection)
                .await?;
        if !exists {
            return Ok(Err(
                "the task's author has no Console principal; save the task to create one"
                    .to_owned(),
            ));
        }
        let identity = slack_identity(connection, author_id).await?;
        if !author_may_deliver(connection, &task.delivery_channel, identity.as_ref()).await? {
            return Ok(Err(DESTINATION_UNAVAILABLE.to_owned()));
        }
        input.insert("principal".to_owned(), json!(foreign_id));
        if let Some((slack_user_id, _)) = identity {
            input.insert("slack_user_id".to_owned(), json!(slack_user_id));
        }
    } else {
        return Ok(Err("the task has no owner".to_owned()));
    }
    input.insert("channel".to_owned(), json!(task.delivery_channel));
    input.insert(
        "scheduled_task_id".to_owned(),
        json!(oid(OID_PREFIX, task.id)),
    );
    input.insert("scheduled_task_name".to_owned(), json!(task.name));
    Ok(Ok(Value::Object(input)))
}

/// `ConsoleUserPrincipalProvisioner#foreign_id`: the principal Console
/// provisions for a user, named from their email.
pub fn console_user_foreign_id(email: &str) -> String {
    // Ruby's `String#strip` removes ASCII whitespace and NUL.
    let normalized = email
        .to_lowercase()
        .trim_matches(|c| matches!(c, '\0' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | ' '))
        .to_owned();
    let mut safe = String::new();
    for character in normalized.chars() {
        let character = if character.is_ascii_alphanumeric() || "-._~".contains(character) {
            character
        } else {
            '-'
        };
        if !(character == '-' && safe.ends_with('-')) {
            safe.push(character);
        }
    }
    safe.truncate(48);
    let digest = hex::encode(Sha256::digest(normalized.as_bytes()));
    format!("console-user-{safe}-{}", &digest[..12])
}

/// `SlackDeliveryPolicy#slack_identity`: the user's one Slack user and team,
/// gathered from their sign-in identities, Console principals, and Slack OAuth
/// credentials. `None` when there is none or more than one.
async fn slack_identity(
    connection: &mut PgConnection,
    user_id: i64,
) -> Result<Option<(String, String)>> {
    let rows = sqlx::query(
        "SELECT subject AS slack_user_id, team_id AS slack_team_id, NULL AS app_team_id \
         FROM user_identities WHERE user_id = $1 AND provider = 'slack' \
         UNION ALL \
         SELECT slack_user_id, slack_team_id, NULL \
         FROM principals WHERE kind = 'console_user' AND console_user_id = $1 \
         UNION ALL \
         SELECT c.provider_subject, c.labels->>'slack_team_id', a.labels->>'slack_team_id' \
         FROM broker_credentials c JOIN oauth_apps a ON a.id = c.oauth_app_id \
         WHERE c.created_by_id = $1 AND a.provider = 'slack'",
    )
    .bind(user_id)
    .fetch_all(&mut *connection)
    .await?;
    let mut identities: Vec<(String, String)> = Vec::new();
    for row in rows {
        let user: Option<String> = row.try_get("slack_user_id")?;
        let team = present(row.try_get("slack_team_id")?).or(present(row.try_get("app_team_id")?));
        let (Some(user), Some(team)) = (user, team) else {
            continue;
        };
        if slack_user_id_valid(&user)
            && slack_team_id_valid(&team)
            && !identities.contains(&(user.clone(), team.clone()))
        {
            identities.push((user, team));
        }
    }
    Ok(match identities.len() {
        1 => identities.pop(),
        _ => None,
    })
}

/// `SlackDeliveryPolicy#allowed?`: the author's own DM, a public channel the
/// bot is in, or a private channel both the author and the bot are in.
async fn author_may_deliver(
    connection: &mut PgConnection,
    destination: &str,
    identity: Option<&(String, String)>,
) -> Result<bool> {
    let destination = destination.trim().to_uppercase();
    if identity.is_some_and(|(user, _)| *user == destination) {
        return Ok(true);
    }
    bot_channel_available(
        connection,
        &destination,
        identity.map(|(_, team)| team.as_str()),
        Some(identity.map(|(user, _)| user.as_str())),
    )
    .await
}

/// Whether `channel` is an active channel the bot is in, within `team_id` when
/// given. `private_member` restricts private channels: `None` allows any,
/// `Some(None)` none, and `Some(Some(user))` those the user is also in.
async fn bot_channel_available(
    connection: &mut PgConnection,
    channel: &str,
    team_id: Option<&str>,
    private_member: Option<Option<&str>>,
) -> Result<bool> {
    let (restrict_private, member) = match private_member {
        None => (false, None),
        Some(member) => (true, member),
    };
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM slack_bot_channels \
         WHERE active AND NOT archived AND channel_id = $1 \
           AND ($2::text IS NULL OR team_id = $2) \
           AND (NOT $3 OR NOT private \
                OR ($4::text IS NOT NULL AND member_user_ids @> ARRAY[bot_user_id::text, $4::text])))",
    )
    .bind(channel)
    .bind(team_id)
    .bind(restrict_private)
    .bind(member)
    .fetch_one(&mut *connection)
    .await?)
}

/// `Principal::SLACK_USER_ID_FORMAT`.
fn slack_user_id_valid(value: &str) -> bool {
    value == "USLACK" || slack_id_valid(value, &['U', 'W'])
}

/// `Principal::SLACK_TEAM_ID_FORMAT`.
fn slack_team_id_valid(value: &str) -> bool {
    slack_id_valid(value, &['T', 'E'])
}

fn slack_id_valid(value: &str, prefixes: &[char]) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| prefixes.contains(&first))
        && value.len() >= 9
        && characters.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Rails' `presence`: `None` for a missing or blank string.
fn present(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

async fn begin_write(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ WRITE")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

async fn lock_task(connection: &mut PgConnection, id: i64, lock: &str) -> Result<Option<Task>> {
    let row = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM scheduled_tasks WHERE id = $1 {lock}"
    ))
    .bind(id)
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(Task {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        prompt: row.try_get("prompt")?,
        author_id: row.try_get("author_id")?,
        principal_id: row.try_get("principal_id")?,
        delivery_channel: row.try_get("delivery_channel")?,
        cron_expression: row.try_get("cron_expression")?,
        timezone: row.try_get("timezone")?,
        enabled: row.try_get("enabled")?,
        next_run_at: row
            .try_get::<Option<OffsetDateTime>, _>("next_run_at")?
            .map(utc),
    }))
}

async fn advance(
    connection: &mut PgConnection,
    id: i64,
    scheduled_for: DateTime<Utc>,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query(
        "UPDATE scheduled_tasks SET \
           last_enqueued_at = ($2::timestamptz AT TIME ZONE 'UTC'), \
           next_run_at = ($3::timestamptz AT TIME ZONE 'UTC'), \
           updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE id = $1",
    )
    .bind(id)
    .bind(offset(scheduled_for))
    .bind(next_run_at.map(offset))
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn record_run(
    connection: &mut PgConnection,
    id: i64,
    run_id: &str,
    now: DateTime<Utc>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE scheduled_tasks SET \
           last_run_id = $2, \
           last_run_at = ($3::timestamptz AT TIME ZONE 'UTC'), \
           last_error = $4, \
           updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE id = $1",
    )
    .bind(id)
    .bind(run_id)
    .bind(offset(now))
    .bind(error)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

async fn record_error(connection: &mut PgConnection, id: i64, error: &str) -> Result<()> {
    sqlx::query("UPDATE scheduled_tasks SET last_error = $2 WHERE id = $1")
        .bind(id)
        .bind(error)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

fn offset(time: DateTime<Utc>) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(
        time.timestamp_nanos_opt().unwrap_or_default(),
    ))
    .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn utc(time: OffsetDateTime) -> DateTime<Utc> {
    DateTime::from_timestamp(time.unix_timestamp(), time.nanosecond()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_user_foreign_ids_match_console() {
        // ConsoleUserPrincipalProvisioner.new(User.new(email: ...)).send(:foreign_id)
        assert_eq!(
            console_user_foreign_id("Admin@Example.com"),
            "console-user-admin-example.com-258d8dc916db"
        );
        assert_eq!(
            console_user_foreign_id(" first+last@@example.com\n"),
            "console-user-first-last-example.com-03f672425c21"
        );
        assert_eq!(
            console_user_foreign_id(&format!("{}@example.com", "a".repeat(60))),
            format!("console-user-{}-e386173d7fd5", "a".repeat(48))
        );
        assert_eq!(
            console_user_foreign_id("Zoë@Example.com"),
            "console-user-zo-example.com-5418899f7aab"
        );
    }

    #[test]
    fn idempotency_keys_match_console() {
        let scheduled_for = DateTime::parse_from_rfc3339("2026-08-19T05:00:00.123-07:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            idempotency_key(42, scheduled_for),
            "scheduled-task:42:2026-08-19T12:00:00Z"
        );
    }

    #[test]
    fn slack_id_formats_match_console() {
        assert!(slack_user_id_valid("U0123456789"));
        assert!(slack_user_id_valid("W01234567"));
        assert!(slack_user_id_valid("USLACK"));
        assert!(!slack_user_id_valid("U0123"));
        assert!(!slack_user_id_valid("u0123456789"));
        assert!(slack_team_id_valid("T0123456789"));
        assert!(slack_team_id_valid("E0123456789"));
        assert!(!slack_team_id_valid("C0123456789"));
    }
}
