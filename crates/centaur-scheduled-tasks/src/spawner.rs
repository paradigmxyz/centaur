//! Spawning the `console_workflow` run for a claimed occurrence.

use async_trait::async_trait;
use centaur_workflows::{CreateWorkflowRunRequest, WorkflowRuntime, WorkflowRuntimeError};

/// A `console_workflow` run to create.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRequest {
    pub workflow_name: String,
    pub input: serde_json::Value,
    pub idempotency_key: Option<String>,
    pub max_attempts: Option<i32>,
}

impl RunRequest {
    /// What `POST /api/workflows/runs` deserializes from Rails' payload.
    pub fn to_workflow_request(&self) -> CreateWorkflowRunRequest {
        CreateWorkflowRunRequest {
            workflow_name: self.workflow_name.clone(),
            input: self.input.clone(),
            idempotency_key: self.idempotency_key.clone(),
            harness_type: None,
            max_attempts: self.max_attempts,
        }
    }
}

/// `ScheduledTask::WORKFLOW_NAME`.
pub const WORKFLOW_NAME: &str = "console_workflow";
/// `ScheduledTaskRunJob::MAX_ATTEMPTS`.
pub const RUN_MAX_ATTEMPTS: i32 = 3;
/// The caller class the runner starts workflows as: Rails' runner called
/// `POST /api/workflows/runs` with the console service JWT.
pub const STARTER_CALLER_CLASS: &str = "console";

/// A spawned (or already existing) workflow run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnedRun {
    pub run_id: String,
    pub created: bool,
}

/// A failed spawn. `message` is what Rails stored in `last_error`: the
/// `error` field of the response `POST /api/workflows/runs` would have given.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SpawnError {
    pub message: String,
    /// Rails retried only `CentaurApiClient::Error`; anything else failed the
    /// run job at once.
    pub retryable: bool,
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl SpawnError {
    /// A retryable error (a `CentaurApiClient::Error`).
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            source: None,
        }
    }

    pub fn terminal(message: impl Into<String>) -> Self {
        Self {
            retryable: false,
            ..Self::new(message)
        }
    }
}

/// Creates workflow runs. [`WorkflowRuntime`] is the production
/// implementation; tests spawn onto their own queue.
#[async_trait]
pub trait RunSpawner: Send + Sync + 'static {
    async fn spawn_run(&self, request: RunRequest) -> Result<SpawnedRun, SpawnError>;
}

#[async_trait]
impl RunSpawner for WorkflowRuntime {
    /// What `POST /api/workflows/runs` does for the console caller.
    async fn spawn_run(&self, request: RunRequest) -> Result<SpawnedRun, SpawnError> {
        let request = request.to_workflow_request();
        let starters = self.workflow_starters(&request.workflow_name);
        if starters.is_some_and(|starters| !starters.contains(STARTER_CALLER_CLASS)) {
            return Err(SpawnError::new(format!(
                "caller is not allowed to start workflow {:?}",
                request.workflow_name.trim()
            )));
        }
        match self.create_run(request).await {
            Ok(run) => Ok(SpawnedRun {
                run_id: run.run_id,
                created: run.created,
            }),
            Err(error) => Err(SpawnError {
                message: api_error_message(&error),
                retryable: true,
                source: Some(Box::new(error)),
            }),
        }
    }
}

/// The `error` api-rs's `ApiError` renders for a workflow error: the message
/// for 4xx-class errors, `internal server error` for 5xx-class ones.
pub fn api_error_message(error: &WorkflowRuntimeError) -> String {
    match error {
        WorkflowRuntimeError::BadRequest(_)
        | WorkflowRuntimeError::Disabled(_)
        | WorkflowRuntimeError::NotFound(_) => error.to_string(),
        _ => "internal server error".to_owned(),
    }
}

/// The run request Rails' `ScheduledTaskRunJob` sent.
pub fn run_request(input: serde_json::Value, idempotency_key: String) -> RunRequest {
    RunRequest {
        workflow_name: WORKFLOW_NAME.to_owned(),
        input,
        idempotency_key: Some(idempotency_key),
        max_attempts: Some(RUN_MAX_ATTEMPTS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_match_the_api_error_body() {
        assert_eq!(
            api_error_message(&WorkflowRuntimeError::Disabled(
                "workflow \"console_workflow\" is disabled".into()
            )),
            "workflow \"console_workflow\" is disabled"
        );
        assert_eq!(
            api_error_message(&WorkflowRuntimeError::Internal("secret detail".into())),
            "internal server error"
        );
        assert_eq!(
            api_error_message(&WorkflowRuntimeError::Upstream("slack".into())),
            "internal server error"
        );
    }
}
