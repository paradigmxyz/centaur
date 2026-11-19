//! Console's scheduled tasks, run by api-rs.
//!
//! Console stores the tasks; [`centaur_console_db::scheduled_tasks`] claims due
//! occurrences and records their outcome in Console's database. This module
//! drives it: a background loop starts due runs, and Console's "run now"
//! requests start one on demand. Either way the run is a `console_workflow`
//! started in-process.

use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use centaur_console_db::{
    decode_oid,
    scheduled_tasks::{self, ManualRun, Outcome, RunRequest, StartError},
};
use centaur_workflows::{CreateWorkflowRunRequest, WorkflowRuntime, WorkflowRuntimeError};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::{ApiError, AppState, auth::AuthenticatedCaller, routes::caller_may_start_workflow};

/// How often due tasks are checked. Schedules have minute precision.
const TICK: Duration = Duration::from_secs(20);

/// Starts due scheduled tasks until the process exits.
pub fn spawn_scheduled_task_scheduler(console_db: PgPool, workflows: WorkflowRuntime) {
    tokio::spawn(async move {
        let mut ticker = interval(TICK);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let start = |request| start_run(&workflows, request);
            match scheduled_tasks::run_due(&console_db, Utc::now(), start).await {
                Ok(outcomes) => outcomes.iter().for_each(log_outcome),
                Err(error) => warn!(%error, "failed to run due scheduled tasks"),
            }
        }
    });
    info!(
        tick_secs = TICK.as_secs(),
        "started scheduled task scheduler"
    );
}

fn log_outcome(outcome: &Outcome) {
    match outcome {
        Outcome::Started { task_id, run_id } => {
            info!(task_id, run_id = %run_id, "started scheduled task run");
        }
        Outcome::Failed { task_id, error } => {
            warn!(task_id, %error, "scheduled task run did not start");
        }
        Outcome::Deferred { task_id, error } => {
            warn!(task_id, %error, "scheduled task run did not start; retrying");
        }
    }
}

async fn start_run(workflows: &WorkflowRuntime, request: RunRequest) -> Result<String, StartError> {
    workflows
        .create_run(CreateWorkflowRunRequest {
            workflow_name: request.workflow_name.to_owned(),
            input: request.input,
            idempotency_key: Some(request.idempotency_key),
            harness_type: None,
            max_attempts: Some(request.max_attempts),
        })
        .await
        .map(|run| run.run_id)
        .map_err(|error| StartError {
            retryable: !matches!(
                error,
                WorkflowRuntimeError::BadRequest(_)
                    | WorkflowRuntimeError::Disabled(_)
                    | WorkflowRuntimeError::NotFound(_)
            ),
            message: error.to_string(),
        })
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RunScheduledTaskRequest {
    /// The occurrence the run stands for, when it is not "now". Console's
    /// retired run job passes the occurrence it was enqueued for, so a run
    /// it already started is not started again.
    #[serde(default)]
    scheduled_for: Option<DateTime<Utc>>,
}

/// `POST /api/scheduled-tasks/{task_id}/runs`: runs an enabled task now.
pub(crate) async fn run_scheduled_task(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedCaller>,
    Path(task_id): Path<String>,
    Json(request): Json<RunScheduledTaskRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let workflows = state.workflows()?;
    // The run executes as the task's owner, so only callers allowed to start
    // the workflow directly may start it here.
    let starters = workflows.workflow_starters(scheduled_tasks::WORKFLOW_NAME);
    if !caller_may_start_workflow(starters.as_ref(), caller.class()) {
        return Err(ApiError::Forbidden(
            "caller is not allowed to run scheduled tasks".to_owned(),
        ));
    }
    let console_db = state.console_db()?;
    let id = decode_oid(scheduled_tasks::OID_PREFIX, &task_id)
        .ok_or_else(|| ApiError::NotFound("scheduled task not found".to_owned()))?;
    let start = |request| async { start_run(&workflows, request).await };
    let run = scheduled_tasks::run_now(&console_db, id, request.scheduled_for, Utc::now(), start)
        .await
        .map_err(|error| ApiError::Internal(format!("failed to run scheduled task: {error}")))?;
    match run {
        ManualRun::Started { run_id } => Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "ok": true, "run_id": run_id })),
        )),
        ManualRun::NotFound => Err(ApiError::NotFound("scheduled task not found".to_owned())),
        ManualRun::Disabled => Err(ApiError::Unprocessable(
            "scheduled task is disabled".to_owned(),
        )),
        ManualRun::Failed(error) if error.retryable => {
            Err(ApiError::ServiceUnavailable(error.message))
        }
        ManualRun::Failed(error) => Err(ApiError::Unprocessable(error.message)),
    }
}
