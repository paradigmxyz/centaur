use std::time::Instant;

use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};

const REFRESHES_TOTAL: &str = "token_broker_refreshes_total";
const REFRESH_DURATION_SECONDS: &str = "token_broker_refresh_duration_seconds";
const DUE_CREDENTIALS: &str = "token_broker_due_credentials";
const DEAD_CREDENTIALS: &str = "token_broker_dead_credentials";
const REFRESH_DURATION_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];

pub(crate) fn init_metrics() -> Result<PrometheusHandle, BuildError> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(REFRESH_DURATION_SECONDS.to_owned()),
            REFRESH_DURATION_BUCKETS,
        )?
        .install_recorder()?;
    metrics::describe_counter!(
        REFRESHES_TOTAL,
        "Broker credential refresh attempts by grant and outcome (success, retry, dead, error)."
    );
    metrics::describe_histogram!(
        REFRESH_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Broker credential refresh attempt latency, including the token endpoint request."
    );
    metrics::describe_gauge!(
        DUE_CREDENTIALS,
        "Broker credentials due for refresh at the start of the last tick."
    );
    metrics::describe_gauge!(
        DEAD_CREDENTIALS,
        "Broker credentials that need re-authorization."
    );
    Ok(handle)
}

pub(crate) fn record_refresh(grant: &str, outcome: &'static str, started: Instant) {
    metrics::counter!(REFRESHES_TOTAL, "grant" => grant.to_owned(), "outcome" => outcome)
        .increment(1);
    metrics::histogram!(REFRESH_DURATION_SECONDS, "outcome" => outcome)
        .record(started.elapsed().as_secs_f64());
}

pub(crate) fn record_due_credentials(count: usize) {
    metrics::gauge!(DUE_CREDENTIALS).set(count as f64);
}

pub(crate) fn record_dead_credentials(count: i64) {
    metrics::gauge!(DEAD_CREDENTIALS).set(count as f64);
}
