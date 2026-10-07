use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use absurd::{Error as AbsurdError, Hooks, TaskTerminalHook, TaskTerminalOutcome};
use futures_util::FutureExt;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use serde_json::Value;

pub const TASKS_ENQUEUED: &str = "company_context_tasks_enqueued_total";
pub const TASK_RUNS: &str = "company_context_task_runs_total";
pub const TASK_RUN_DURATION: &str = "company_context_task_run_duration_seconds";
pub const TASKS_IN_FLIGHT: &str = "company_context_tasks_in_flight";
pub const TASK_TERMINAL: &str = "company_context_task_terminal_total";
pub const WORKER_CONCURRENCY: &str = "company_context_worker_concurrency";
pub const SCHEDULER_ERRORS: &str = "company_context_scheduler_errors_total";
pub const UPSTREAM_REQUESTS: &str = "company_context_upstream_requests_total";
pub const UPSTREAM_REQUEST_DURATION: &str = "company_context_upstream_request_duration_seconds";
pub const UPSTREAM_RETRIES: &str = "company_context_upstream_retries_total";
pub const EMBEDDING_INPUTS: &str = "company_context_embedding_inputs_total";
pub const EMBEDDING_TOKENS: &str = "company_context_embedding_tokens_total";
pub const SLACK_RATE_LIMIT_WAIT: &str = "company_context_slack_rate_limit_wait_seconds";
pub const QUEUE_TASKS: &str = "company_context_queue_tasks";
pub const QUEUE_OLDEST_TASK_AGE: &str = "company_context_queue_oldest_task_age_seconds";
pub const ITEMS: &str = "company_context_items";
pub const CHECKPOINT_OLDEST_SUCCESS_AGE: &str =
    "company_context_checkpoint_oldest_success_age_seconds";
pub const CHECKPOINTS_WITH_ERRORS: &str = "company_context_checkpoints_with_errors";
pub const LIVE_CREDENTIALS: &str = "company_context_live_credentials";
pub const SAMPLE_ERRORS: &str = "company_context_metrics_sample_errors_total";

/// Every histogram measures seconds, from sub-second API calls to long scans.
const SECONDS_BUCKETS: &[f64] = &[
    0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
];

pub fn init_metrics() -> Result<PrometheusHandle, metrics_exporter_prometheus::BuildError> {
    let handle = PrometheusBuilder::new()
        .set_buckets(SECONDS_BUCKETS)?
        .install_recorder()?;
    let counters = [
        (TASKS_ENQUEUED, "Tasks durably enqueued by task type."),
        (
            TASK_RUNS,
            "Task run attempts by queue, task, and outcome (completed, rejected, superseded, skipped, denied, suspended, cancelled, or failed).",
        ),
        (
            TASK_TERMINAL,
            "Tasks reaching a terminal state by queue, task, and state; failed means retries were exhausted.",
        ),
        (
            SCHEDULER_ERRORS,
            "Scheduler failures by source and stage (load_credentials or enqueue).",
        ),
        (
            UPSTREAM_REQUESTS,
            "Upstream API requests by upstream, operation, and outcome.",
        ),
        (
            UPSTREAM_RETRIES,
            "Upstream requests retried in process, by upstream and reason.",
        ),
        (EMBEDDING_INPUTS, "Texts sent to the embeddings API."),
        (
            EMBEDDING_TOKENS,
            "Prompt tokens reported by the embeddings API, by model.",
        ),
        (
            SAMPLE_ERRORS,
            "Failures sampling queue and corpus gauges from the database.",
        ),
    ];
    for (name, description) in counters {
        metrics::describe_counter!(name, description);
    }
    let gauges = [
        (
            TASKS_IN_FLIGHT,
            "Task runs currently executing in this replica, by queue.",
        ),
        (
            WORKER_CONCURRENCY,
            "Configured worker concurrency, by queue.",
        ),
        (
            QUEUE_TASKS,
            "Unfinished tasks by queue, task, and state: ready to claim, running, or waiting for a retry, sleep, or event. Identical on every replica.",
        ),
        (
            QUEUE_OLDEST_TASK_AGE,
            "Oldest task in each state: time since it became claimable (ready), was claimed (running), or was enqueued (waiting).",
        ),
        (
            ITEMS,
            "Staged source items by source, processing stage, and status.",
        ),
        (
            CHECKPOINT_OLDEST_SUCCESS_AGE,
            "Time since the least recently synchronized live checkpoint last succeeded, by source.",
        ),
        (
            CHECKPOINTS_WITH_ERRORS,
            "Live checkpoints whose last sync recorded an error, by source.",
        ),
        (
            LIVE_CREDENTIALS,
            "Live broker credentials within rollout limits, by source.",
        ),
    ];
    for (name, description) in gauges {
        metrics::describe_gauge!(name, description);
    }
    let histograms = [
        (
            TASK_RUN_DURATION,
            "Task run duration by queue, task, and outcome.",
        ),
        (
            UPSTREAM_REQUEST_DURATION,
            "Upstream API request latency by upstream and operation.",
        ),
        (
            SLACK_RATE_LIMIT_WAIT,
            "Time Slack requests wait for a shared rate-limit slot, by method.",
        ),
    ];
    for (name, description) in histograms {
        metrics::describe_histogram!(name, description);
    }
    Ok(handle)
}

pub fn task_enqueued(task: &'static str, created: bool) {
    metrics::counter!(TASKS_ENQUEUED, "task" => task, "created" => created.to_string())
        .increment(1);
}

pub fn worker_concurrency(queue: &'static str, concurrency: usize) {
    metrics::gauge!(WORKER_CONCURRENCY, "queue" => queue).set(concurrency as f64);
}

pub fn scheduler_error(source: &'static str, stage: &'static str) {
    metrics::counter!(SCHEDULER_ERRORS, "source" => source, "stage" => stage).increment(1);
}

/// Absurd client hooks that record every task run.
pub fn task_hooks() -> Hooks {
    Hooks {
        wrap_task_execution: Some(Arc::new(|ctx, execute| {
            async move {
                let run = TaskRun::start(ctx.queue_name(), ctx.task_name());
                let result = execute().await;
                run.finish(&result);
                result
            }
            .boxed()
        })),
        ..Hooks::default()
    }
}

pub fn task_terminal_hook() -> TaskTerminalHook {
    Arc::new(task_terminal)
}

/// Records one task run when dropped, so a run abandoned by cancellation or a
/// panic is still counted and leaves the in-flight gauge.
struct TaskRun {
    queue: String,
    task: String,
    started: Instant,
    outcome: Option<&'static str>,
}

impl TaskRun {
    fn start(queue: &str, task: &str) -> Self {
        metrics::gauge!(TASKS_IN_FLIGHT, "queue" => queue.to_owned()).increment(1);
        Self {
            queue: queue.to_owned(),
            task: task.to_owned(),
            started: Instant::now(),
            outcome: None,
        }
    }

    fn finish(mut self, result: &Result<Value, AbsurdError>) {
        self.outcome = Some(run_outcome(result));
    }
}

impl Drop for TaskRun {
    fn drop(&mut self) {
        let outcome = self.outcome.unwrap_or(if std::thread::panicking() {
            "failed"
        } else {
            "cancelled"
        });
        let labels = [
            ("queue", self.queue.clone()),
            ("task", self.task.clone()),
            ("outcome", outcome.to_owned()),
        ];
        metrics::gauge!(TASKS_IN_FLIGHT, "queue" => self.queue.clone()).decrement(1);
        metrics::counter!(TASK_RUNS, &labels).increment(1);
        metrics::histogram!(TASK_RUN_DURATION, &labels).record(self.started.elapsed());
    }
}

/// Task summaries report a `status`; a successful run is labeled with it.
fn run_outcome(result: &Result<Value, AbsurdError>) -> &'static str {
    match result {
        Ok(summary) => match summary.get("status").and_then(Value::as_str) {
            Some("rejected") => "rejected",
            Some("superseded") => "superseded",
            Some("skipped") => "skipped",
            Some("denied") => "denied",
            _ => "completed",
        },
        Err(AbsurdError::Suspend) => "suspended",
        Err(AbsurdError::Cancelled) => "cancelled",
        Err(_) => "failed",
    }
}

fn task_terminal(outcome: TaskTerminalOutcome) {
    let state = serde_json::to_value(&outcome.state)
        .ok()
        .and_then(|state| state.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    metrics::counter!(
        TASK_TERMINAL,
        "queue" => outcome.queue_name,
        "task" => outcome.task_name,
        "state" => state
    )
    .increment(1);
}

pub fn upstream_request(
    upstream: &'static str,
    operation: &str,
    outcome: &'static str,
    elapsed: Duration,
) {
    metrics::counter!(
        UPSTREAM_REQUESTS,
        "upstream" => upstream,
        "operation" => operation.to_owned(),
        "outcome" => outcome
    )
    .increment(1);
    metrics::histogram!(
        UPSTREAM_REQUEST_DURATION,
        "upstream" => upstream,
        "operation" => operation.to_owned()
    )
    .record(elapsed);
}

/// Records an HTTP request to an upstream by its transport result and status.
pub fn upstream_response(
    upstream: &'static str,
    operation: &str,
    started: Instant,
    response: &reqwest::Result<reqwest::Response>,
) {
    let outcome = match response {
        Err(_) => "transport_error",
        Ok(response) => http_outcome(response.status()),
    };
    upstream_request(upstream, operation, outcome, started.elapsed());
}

pub fn http_outcome(status: reqwest::StatusCode) -> &'static str {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        "rate_limited"
    } else if status.is_server_error() {
        "server_error"
    } else if status.is_client_error() {
        "client_error"
    } else {
        "ok"
    }
}

pub fn upstream_retry(upstream: &'static str, reason: &'static str) {
    metrics::counter!(UPSTREAM_RETRIES, "upstream" => upstream, "reason" => reason).increment(1);
}

pub fn embedding_usage(model: &str, inputs: usize, prompt_tokens: Option<u64>) {
    metrics::counter!(EMBEDDING_INPUTS).increment(inputs as u64);
    if let Some(tokens) = prompt_tokens {
        metrics::counter!(EMBEDDING_TOKENS, "model" => model.to_owned()).increment(tokens);
    }
}

pub fn sample_error() {
    metrics::counter!(SAMPLE_ERRORS).increment(1);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn run_outcome_reflects_summary_status_and_control_flow() {
        assert_eq!(
            run_outcome(&Ok(json!({"status": "completed"}))),
            "completed"
        );
        assert_eq!(run_outcome(&Ok(json!({"status": "rejected"}))), "rejected");
        assert_eq!(
            run_outcome(&Ok(json!({"status": "superseded"}))),
            "superseded"
        );
        assert_eq!(run_outcome(&Ok(json!({"status": "skipped"}))), "skipped");
        assert_eq!(run_outcome(&Ok(json!({"status": "denied"}))), "denied");
        assert_eq!(run_outcome(&Ok(json!(null))), "completed");
        assert_eq!(run_outcome(&Err(AbsurdError::Suspend)), "suspended");
        assert_eq!(run_outcome(&Err(AbsurdError::Cancelled)), "cancelled");
        assert_eq!(run_outcome(&Err(AbsurdError::FailedRun)), "failed");
    }
}
