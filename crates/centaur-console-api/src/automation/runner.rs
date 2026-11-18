//! The scheduled-task runner's console endpoints (RFC 0006a): api-rs
//! claims an occurrence of a task and reports the workflow run it spawned.
//! Errors here are flat, `{"error": "<message>"}`.
//!
//! `claim` advances the schedule: the task's `next_run_at` moves to the
//! occurrence after the claimed one and `last_enqueued_at` records the
//! claimed one. It is idempotent per occurrence while `last_enqueued_at`
//! still holds it, so the runner serializes the claims of one task (a later
//! occurrence does not claim before an earlier one's answer was
//! checkpointed). Manual claims change nothing. `result` writes
//! only the run bookkeeping columns. Neither touches the user-editable
//! columns, and edits never touch these.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_console_db::{decode_oid, schedule};
use chrono::{DateTime, SubsecRound, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tasks::{self, Owner, Task};
use crate::{
    AppState,
    auth::InternalCaller,
    extract::Path,
    principals::provision::{self, ConsoleUser},
};

const DESTINATION_UNAVAILABLE: &str =
    "Slack delivery destination is no longer available to the task owner";

pub enum Failure {
    NotFound,
    Unprocessable(String),
    Internal,
}

impl From<sqlx::Error> for Failure {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "scheduled task runner endpoint failed");
        Self::Internal
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::NotFound => (StatusCode::NOT_FOUND, "scheduled task not found".to_owned()),
            Self::Unprocessable(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_owned(),
            ),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

fn task_id(id: &str) -> Result<i64, Failure> {
    decode_oid(tasks::OID_PREFIX, id).ok_or(Failure::NotFound)
}

fn parse<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, Failure> {
    serde_json::from_slice(bytes)
        .map_err(|error| Failure::Unprocessable(format!("invalid request body: {error}")))
}

#[derive(Deserialize)]
struct ClaimBody {
    scheduled_for: DateTime<Utc>,
    #[serde(default)]
    manual: bool,
}

#[derive(Deserialize)]
struct ResultBody {
    scheduled_for: DateTime<Utc>,
    #[serde(default)]
    manual: bool,
    run_id: Option<String>,
    error: Option<String>,
}

fn answer(
    outcome: &str,
    reason: Option<&str>,
    error: Option<&str>,
    api_input: Option<Value>,
    task: &Task,
) -> Response {
    Json(json!({
        "outcome": outcome,
        "reason": reason,
        "error": error,
        "api_input": api_input,
        "enabled": task.enabled,
        "next_run_at": task.next_run_at,
    }))
    .into_response()
}

/// `POST /internal/v1/scheduled_tasks/{oid}/claim`.
pub async fn claim(
    State(state): State<AppState>,
    _caller: InternalCaller,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<Response, Failure> {
    let id = task_id(&id)?;
    let body: ClaimBody = parse(&bytes)?;
    let scheduled_for = body.scheduled_for.trunc_subsecs(6);
    let manual = body.manual;

    let mut tx = state.db.begin().await?;
    let task: Task = sqlx::query_as(&format!(
        "SELECT {} FROM scheduled_tasks WHERE id = $1 FOR UPDATE",
        tasks::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Failure::NotFound)?;
    if !task.enabled {
        return Ok(answer("skipped", Some("task_disabled"), None, None, &task));
    }
    let now = state.now();
    let task = if manual {
        task
    } else if task.next_run_at == Some(scheduled_for) {
        // The occurrence after the claimed one, counted from whichever of
        // now and the claimed time is later: a console clock slightly behind
        // the runner's must not hand the same occurrence out again.
        let next = task
            .cron_expression
            .as_deref()
            .and_then(|cron| schedule::next_run(cron, &task.timezone, now.max(scheduled_for)));
        sqlx::query(
            "UPDATE scheduled_tasks SET last_enqueued_at = $2, next_run_at = $3, updated_at = $4 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(scheduled_for.naive_utc())
        .bind(next.map(|time| time.naive_utc()))
        .bind(now.naive_utc())
        .execute(&mut *tx)
        .await?;
        Task {
            last_enqueued_at: Some(scheduled_for),
            next_run_at: next,
            updated_at: now,
            ..task
        }
    } else if task.last_enqueued_at == Some(scheduled_for) {
        // A replay of a claim that already committed.
        task
    } else {
        return Ok(answer("skipped", Some("not_due"), None, None, &task));
    };
    tx.commit().await?;

    // The claim is durable; building the run's input only reads (and
    // provisions the author's principal). A failure is recorded on the task
    // and reported, and the schedule stays advanced.
    match api_input(&state, &task).await {
        Ok(input) => Ok(answer("claimed", None, None, Some(input), &task)),
        Err((reason, message)) => {
            sqlx::query(
                "UPDATE scheduled_tasks SET last_error = $2, updated_at = $3 WHERE id = $1",
            )
            .bind(id)
            .bind(&message)
            .bind(now.naive_utc())
            .execute(&state.db)
            .await?;
            tracing::warn!(scheduled_task = %task.oid(), reason, "scheduled_task_claim_failed");
            let task = Task {
                last_error: Some(message.clone()),
                ..task
            };
            Ok(answer("failed", Some(reason), Some(&message), None, &task))
        }
    }
}

/// The workflow input of a run: the prompt, the principal it runs as, and
/// where its output goes. `Err` is `(reason, message)`.
async fn api_input(state: &AppState, task: &Task) -> Result<Value, (&'static str, String)> {
    let failed = |error: sqlx::Error| {
        tracing::error!(%error, "scheduled_task_api_input_failed");
        (
            "api_input_failed",
            "could not build the run input".to_owned(),
        )
    };
    let mut conn = state.db.acquire().await.map_err(failed)?;
    let (owner, channel_foreign_id) = match (task.author_id, task.principal_id) {
        (_, Some(principal_id)) => {
            let (foreign_id, team_id, channel_id): (
                Option<String>,
                Option<String>,
                Option<String>,
            ) = sqlx::query_as(
                "SELECT foreign_id, slack_team_id, slack_channel_id FROM principals WHERE id = $1",
            )
            .bind(principal_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(failed)?;
            let owner = Owner::Channel {
                principal_id,
                channel_id,
                team_id,
            };
            (owner, foreign_id)
        }
        (Some(author_id), None) => (Owner::Author(author_id), None),
        (None, None) => return Err(("api_input_failed", "the task has no owner".to_owned())),
    };
    if !owner
        .destination_available(&mut conn, &task.delivery_channel)
        .await
        .map_err(failed)?
    {
        return Err((
            "delivery_destination_unavailable",
            DESTINATION_UNAVAILABLE.to_owned(),
        ));
    }
    let (foreign_id, slack_user_id) = match &owner {
        Owner::Author(author_id) => {
            let (email, name): (String, Option<String>) =
                sqlx::query_as("SELECT email, name FROM users WHERE id = $1")
                    .bind(*author_id)
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(failed)?;
            // Creates or refreshes the author's principal, then reconciles
            // its credentials and grants the personal SSH key.
            provision::provision(
                state,
                &ConsoleUser {
                    id: *author_id,
                    email: &email,
                    name: name.as_deref(),
                },
            )
            .await
            .map_err(|error| {
                tracing::error!(%error, "scheduled_task_principal_provisioning_failed");
                (
                    "principal_provisioning_failed",
                    "console user principal could not be provisioned".to_owned(),
                )
            })?;
            let foreign_id = Some(provision::console_user_foreign_id(&email));
            let identity = owner.identity(&mut conn).await.map_err(failed)?;
            (foreign_id, identity.map(|identity| identity.user_id))
        }
        // Anyone in a channel can edit its tasks, so their output is not
        // attributed to a person.
        Owner::Channel { .. } => (channel_foreign_id, None),
    };
    let mut input = json!({
        "prompt": task.prompt,
        "channel": task.delivery_channel,
        "scheduled_task_id": task.oid(),
        "scheduled_task_name": task.name,
    });
    if let Some(foreign_id) = foreign_id {
        input["principal"] = json!(foreign_id);
    }
    if let Some(slack_user_id) = slack_user_id {
        input["slack_user_id"] = json!(slack_user_id);
    }
    Ok(input)
}

/// `POST /internal/v1/scheduled_tasks/{oid}/result`.
pub async fn result(
    State(state): State<AppState>,
    _caller: InternalCaller,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<Response, Failure> {
    let id = task_id(&id)?;
    let body: ResultBody = parse(&bytes)?;
    let (run_id, error) = match (body.run_id, body.error) {
        (Some(run_id), None) => (Some(run_id), None),
        (None, Some(error)) => (None, Some(error)),
        _ => {
            return Err(Failure::Unprocessable(
                "exactly one of run_id or error is required".into(),
            ));
        }
    };
    tracing::info!(
        scheduled_for = %body.scheduled_for,
        manual = body.manual,
        "scheduled_task_result"
    );
    let now = state.now().naive_utc();
    let row: Option<(bool, Option<DateTime<Utc>>)> =
        match run_id {
            Some(run_id) => sqlx::query_as(
                "UPDATE scheduled_tasks SET last_run_id = $2, last_run_at = $3, last_error = NULL, \
                 updated_at = $3 WHERE id = $1 \
                 RETURNING enabled, next_run_at AT TIME ZONE 'UTC'",
            )
            .bind(id)
            .bind(run_id)
            .bind(now)
            .fetch_optional(&state.db)
            .await?,
            None => {
                sqlx::query_as(
                    "UPDATE scheduled_tasks SET last_error = $2, updated_at = $3 WHERE id = $1 \
                 RETURNING enabled, next_run_at AT TIME ZONE 'UTC'",
                )
                .bind(id)
                .bind(error)
                .bind(now)
                .fetch_optional(&state.db)
                .await?
            }
        };
    let (enabled, next_run_at) = row.ok_or(Failure::NotFound)?;
    Ok(Json(json!({"enabled": enabled, "next_run_at": next_run_at})).into_response())
}
