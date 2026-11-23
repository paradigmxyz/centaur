//! Console's scheduled tasks, run by api-rs.
//!
//! Console stores the tasks; [`centaur_console_db::scheduled_tasks`] claims due
//! occurrences and records their outcome in Console's database. This module
//! drives it: a background loop starts due runs as `console_workflow` runs,
//! in-process. "Run now" is the Console's own route
//! (`console::areas::automation`).

use std::time::Duration;

use centaur_console_db::scheduled_tasks::{self, Outcome, RunRequest, StartError};
use centaur_workflows::{CreateWorkflowRunRequest, WorkflowRuntime, WorkflowRuntimeError};
use chrono::Utc;
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

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
