use std::time::Duration;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

const HTTP_REQUESTS_TOTAL: &str = "http_server_requests_total";
const HTTP_REQUEST_DURATION_SECONDS: &str = "http_server_request_duration_seconds";
const HTTP_REQUESTS_IN_FLIGHT: &str = "http_server_requests_in_flight";
const SYNC_CACHE_LOOKUPS_TOTAL: &str = "proxy_sync_cache_lookups_total";
const HTTP_REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub(crate) fn init_metrics() -> Result<PrometheusHandle, metrics_exporter_prometheus::BuildError> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(HTTP_REQUEST_DURATION_SECONDS.to_owned()),
            HTTP_REQUEST_DURATION_BUCKETS,
        )?
        .install_recorder()?;

    metrics::describe_counter!(
        HTTP_REQUESTS_TOTAL,
        "Total HTTP requests served by proxy-sync."
    );
    metrics::describe_histogram!(
        HTTP_REQUEST_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "HTTP request latency in seconds for proxy-sync."
    );
    metrics::describe_gauge!(
        HTTP_REQUESTS_IN_FLIGHT,
        "Number of in-flight HTTP requests in proxy-sync."
    );
    metrics::describe_counter!(
        SYNC_CACHE_LOOKUPS_TOTAL,
        "Proxy sync configuration cache lookups by result."
    );

    Ok(handle)
}

pub(crate) fn record_http_request_started() {
    metrics::gauge!(HTTP_REQUESTS_IN_FLIGHT).increment(1.0);
}

pub(crate) fn record_http_request_finished(
    method: &str,
    route: &str,
    status: u16,
    duration: Duration,
) {
    metrics::gauge!(HTTP_REQUESTS_IN_FLIGHT).decrement(1.0);
    metrics::counter!(
        HTTP_REQUESTS_TOTAL,
        "method" => method.to_owned(),
        "route" => route.to_owned(),
        "status" => status.to_string(),
    )
    .increment(1);
    metrics::histogram!(
        HTTP_REQUEST_DURATION_SECONDS,
        "method" => method.to_owned(),
        "route" => route.to_owned(),
        "status_class" => http_status_class(status),
    )
    .record(duration.as_secs_f64());
}

pub(crate) fn record_cache_lookup(result: &'static str) {
    metrics::counter!(SYNC_CACHE_LOOKUPS_TOTAL, "result" => result).increment(1);
}

fn http_status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        init_metrics, record_cache_lookup, record_http_request_finished,
        record_http_request_started,
    };

    #[test]
    fn renders_http_and_cache_metrics() {
        let handle = init_metrics().expect("metrics recorder should initialize");

        record_http_request_started();
        record_http_request_finished("GET", "/healthz", 200, Duration::from_millis(10));
        record_cache_lookup("hit");

        let output = handle.render();
        assert!(output.contains("http_server_requests_total"));
        assert!(output.contains("method=\"GET\""));
        assert!(output.contains("route=\"/healthz\""));
        assert!(output.contains("status=\"200\""));
        assert!(output.contains("http_server_request_duration_seconds_count"));
        assert!(output.contains("status_class=\"2xx\""));
        assert!(output.contains("http_server_requests_in_flight 0"));
        assert!(output.contains("proxy_sync_cache_lookups_total{result=\"hit\"} 1"));
    }
}
