use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
    time::Instant,
};

use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};

const REFRESHES_TOTAL: &str = "token_broker_refreshes_total";
const REFRESH_DURATION_SECONDS: &str = "token_broker_refresh_duration_seconds";
const TOKEN_REQUESTS_TOTAL: &str = "token_broker_token_requests_total";
const DUE_CREDENTIALS: &str = "token_broker_due_credentials";
const DEAD_CREDENTIALS: &str = "token_broker_dead_credentials";
const BACKING_OFF_CREDENTIALS: &str = "token_broker_backing_off_credentials";
const LAST_POLL_TIMESTAMP_SECONDS: &str = "token_broker_last_poll_timestamp_seconds";
const REFRESH_DURATION_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];

/// Providers the credential gauges have reported, so a provider whose
/// credentials are all deleted is reset to zero rather than left stale.
static REPORTED_PROVIDERS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

pub(crate) fn init_metrics() -> Result<PrometheusHandle, BuildError> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(REFRESH_DURATION_SECONDS.to_owned()),
            REFRESH_DURATION_BUCKETS,
        )?
        .install_recorder()?;
    describe();
    Ok(handle)
}

fn describe() {
    metrics::describe_counter!(
        REFRESHES_TOTAL,
        "Broker credential refresh attempts by provider, grant, and outcome (success, retry, dead, error)."
    );
    metrics::describe_histogram!(
        REFRESH_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Broker credential refresh attempt latency, including the token endpoint requests."
    );
    metrics::describe_counter!(
        TOKEN_REQUESTS_TOTAL,
        "Token endpoint requests by provider and result (success, rejected, rate_limited, \
         server_error, redirect, client_error, malformed, timeout, connection_failed, \
         network_error)."
    );
    metrics::describe_gauge!(
        DUE_CREDENTIALS,
        "Broker credentials due for refresh at the start of the last tick."
    );
    metrics::describe_gauge!(
        DEAD_CREDENTIALS,
        "Broker credentials that need re-authorization, by provider."
    );
    metrics::describe_gauge!(
        LAST_POLL_TIMESTAMP_SECONDS,
        metrics::Unit::Seconds,
        "Unix time the last poll finished a pass over the due credentials, or the service started."
    );
    metrics::describe_gauge!(
        BACKING_OFF_CREDENTIALS,
        "Live broker credentials whose last refresh failed and is being retried, by provider."
    );
}

pub(crate) fn record_refresh(provider: &str, grant: &str, outcome: &'static str, started: Instant) {
    metrics::counter!(
        REFRESHES_TOTAL,
        "provider" => provider.to_owned(),
        "grant" => grant.to_owned(),
        "outcome" => outcome,
    )
    .increment(1);
    metrics::histogram!(
        REFRESH_DURATION_SECONDS,
        "provider" => provider.to_owned(),
        "outcome" => outcome,
    )
    .record(started.elapsed().as_secs_f64());
}

pub(crate) fn record_token_request(provider: &str, result: &'static str) {
    metrics::counter!(
        TOKEN_REQUESTS_TOTAL,
        "provider" => provider.to_owned(),
        "result" => result,
    )
    .increment(1);
}

pub(crate) fn record_tick_completed() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    metrics::gauge!(LAST_POLL_TIMESTAMP_SECONDS).set(now.as_secs_f64());
}

pub(crate) fn record_due_credentials(count: usize) {
    metrics::gauge!(DUE_CREDENTIALS).set(count as f64);
}

/// Sets the dead and backing-off gauges from `(dead, backing_off)` counts by
/// provider.
pub(crate) fn record_credential_counts(counts: &BTreeMap<String, (i64, i64)>) {
    let mut reported = REPORTED_PROVIDERS.lock().unwrap();
    for provider in reported
        .iter()
        .filter(|provider| !counts.contains_key(*provider))
    {
        metrics::gauge!(DEAD_CREDENTIALS, "provider" => provider.clone()).set(0.0);
        metrics::gauge!(BACKING_OFF_CREDENTIALS, "provider" => provider.clone()).set(0.0);
    }
    for (provider, (dead, backing_off)) in counts {
        metrics::gauge!(DEAD_CREDENTIALS, "provider" => provider.clone()).set(*dead as f64);
        metrics::gauge!(BACKING_OFF_CREDENTIALS, "provider" => provider.clone())
            .set(*backing_off as f64);
    }
    *reported = counts.keys().cloned().collect();
}
