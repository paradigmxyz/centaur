use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

pub const TASKS_ENQUEUED: &str = "company_context_tasks_enqueued_total";

pub fn init_metrics() -> Result<PrometheusHandle, metrics_exporter_prometheus::BuildError> {
    let handle = PrometheusBuilder::new().install_recorder()?;
    metrics::describe_counter!(
        TASKS_ENQUEUED,
        "Company context tasks durably enqueued by task type."
    );
    metrics::describe_counter!(
        "company_context_scheduler_errors_total",
        "Company context scheduler enqueue failures."
    );
    metrics::describe_counter!(
        "company_context_slack_requests_total",
        "Slack Web API requests by method and outcome."
    );
    metrics::describe_histogram!(
        "company_context_slack_rate_limit_wait_seconds",
        "Time Slack requests wait for a shared rate-limit slot, by method."
    );
    Ok(handle)
}

pub fn task_enqueued(task: &'static str, created: bool) {
    metrics::counter!(TASKS_ENQUEUED, "task" => task, "created" => created.to_string())
        .increment(1);
}
