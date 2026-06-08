//! Shared telemetry setup for the Rust Centaur control plane.

use std::{
    env, fmt as std_fmt,
    sync::{LazyLock, Mutex},
    time::Duration,
};

pub use metrics_exporter_prometheus::PrometheusHandle;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use serde_json::Value;
use thiserror::Error;
use tracing::{Event, Subscriber};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::{
    EnvFilter, Layer as _,
    fmt::{
        self, FmtContext,
        format::{self as fmt_format, FormatEvent, FormatFields, Writer},
    },
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

pub const DEFAULT_SERVICE_NAME: &str = "centaur-api-rs";
pub const SERVICE_NAMESPACE: &str = "centaur";
pub const OTEL_SERVICE_NAMESPACE: &str = "service.namespace";
pub const OTEL_DEPLOYMENT_ENVIRONMENT_NAME: &str = "deployment.environment.name";

pub const FIELD_COMPONENT: &str = "component";
pub const FIELD_EVENT: &str = "event";
pub const FIELD_EXECUTION_ID: &str = "execution_id";
pub const FIELD_SANDBOX_ID: &str = "sandbox_id";
pub const FIELD_THREAD_KEY: &str = "thread_key";

pub const HTTP_REQUESTS_TOTAL: &str = "http_server_requests_total";
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "http_server_request_duration_seconds";
pub const HTTP_REQUESTS_IN_FLIGHT: &str = "http_server_requests_in_flight";
pub const SESSION_EXECUTIONS_TOTAL: &str = "centaur_session_executions_total";
pub const SESSION_EXECUTION_DURATION_SECONDS: &str = "centaur_session_execution_duration_seconds";
pub const SANDBOX_OPERATIONS_TOTAL: &str = "centaur_sandbox_operations_total";
pub const SANDBOX_WARM_POOL_CLAIMS_TOTAL: &str = "centaur_sandbox_warm_pool_claims_total";

const HTTP_REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const SESSION_EXECUTION_DURATION_BUCKETS: &[f64] = &[
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0,
];

static PROMETHEUS_HANDLE: LazyLock<Mutex<Option<PrometheusHandle>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetryConfig {
    pub service_name: String,
    pub environment: String,
    pub rust_log: String,
    pub traces_exporter: TraceExporter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceExporter {
    None,
    Otlp,
}

#[derive(Debug)]
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
}

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("failed to build Prometheus metrics exporter: {0}")]
    PrometheusExporter(#[from] metrics_exporter_prometheus::BuildError),
    #[error("failed to build OTLP trace exporter: {0}")]
    OtlpExporter(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("failed to install global tracing subscriber: {0}")]
    SetGlobalSubscriber(#[from] tracing_subscriber::util::TryInitError),
}

impl TelemetryConfig {
    pub fn from_env() -> Self {
        let service_name = env::var("OTEL_SERVICE_NAME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_owned());
        let environment = first_nonempty_env(&["CENTAUR_ENVIRONMENT", "DEPLOY_ENV", "ENVIRONMENT"])
            .unwrap_or_else(|| "local".to_owned());
        let rust_log = env::var("RUST_LOG")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "info".to_owned());
        let traces_exporter = TraceExporter::from_env();

        Self {
            service_name,
            environment,
            rust_log,
            traces_exporter,
        }
    }
}

impl TraceExporter {
    fn from_env() -> Self {
        let exporter = env::var("OTEL_TRACES_EXPORTER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let has_endpoint = first_nonempty_env(&[
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
        ])
        .is_some();

        Self::from_values(&exporter, has_endpoint)
    }

    fn from_values(exporter: &str, has_endpoint: bool) -> Self {
        if matches!(exporter, "none" | "false" | "0" | "off") {
            Self::None
        } else if exporter == "otlp" || has_endpoint {
            Self::Otlp
        } else {
            Self::None
        }
    }
}

impl TelemetryGuard {
    pub fn shutdown(mut self) {
        if let Some(provider) = self.tracer_provider.take() {
            if let Err(error) = provider.shutdown() {
                tracing::warn!(%error, "failed to shut down OpenTelemetry tracer provider");
            }
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take()
            && let Err(error) = provider.shutdown()
        {
            tracing::warn!(%error, "failed to shut down OpenTelemetry tracer provider");
        }
    }
}

pub fn prometheus_handle() -> Result<PrometheusHandle, TelemetryError> {
    let mut handle = PROMETHEUS_HANDLE
        .lock()
        .expect("prometheus handle lock poisoned");
    if let Some(handle) = handle.as_ref() {
        return Ok(handle.clone());
    }

    let new_handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(HTTP_REQUEST_DURATION_SECONDS.to_owned()),
            HTTP_REQUEST_DURATION_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(SESSION_EXECUTION_DURATION_SECONDS.to_owned()),
            SESSION_EXECUTION_DURATION_BUCKETS,
        )?
        .install_recorder()?;
    describe_metrics();
    *handle = Some(new_handle.clone());
    Ok(new_handle)
}

pub fn render_metrics() -> Result<String, TelemetryError> {
    Ok(prometheus_handle()?.render())
}

pub fn record_http_request_started() {
    metrics::gauge!(HTTP_REQUESTS_IN_FLIGHT).increment(1.0);
}

pub fn record_http_request_finished(method: &str, route: &str, status: u16, duration: Duration) {
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

pub fn record_session_execution_started(harness: &str) {
    metrics::counter!(
        SESSION_EXECUTIONS_TOTAL,
        "harness" => normalize_label(harness),
        "status" => "started",
    )
    .increment(1);
}

pub fn record_session_execution_finished(
    harness: &str,
    status: &'static str,
    duration: Option<Duration>,
) {
    metrics::counter!(
        SESSION_EXECUTIONS_TOTAL,
        "harness" => normalize_label(harness),
        "status" => status,
    )
    .increment(1);
    if let Some(duration) = duration {
        metrics::histogram!(
            SESSION_EXECUTION_DURATION_SECONDS,
            "harness" => normalize_label(harness),
            "status" => status,
        )
        .record(duration.as_secs_f64());
    }
}

pub fn record_sandbox_operation(backend: &str, operation: &'static str, status: &'static str) {
    metrics::counter!(
        SANDBOX_OPERATIONS_TOTAL,
        "backend" => normalize_label(backend),
        "operation" => operation,
        "status" => status,
    )
    .increment(1);
}

pub fn record_sandbox_warm_pool_claim(result: &'static str) {
    metrics::counter!(
        SANDBOX_WARM_POOL_CLAIMS_TOTAL,
        "result" => result,
    )
    .increment(1);
}

pub fn http_status_class(status: u16) -> &'static str {
    match status / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "unknown",
    }
}

pub fn init_telemetry(config: TelemetryConfig) -> Result<TelemetryGuard, TelemetryError> {
    let _metrics = prometheus_handle()?;
    let filter = EnvFilter::try_new(&config.rust_log).unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = fmt::layer()
        .json()
        .event_format(TraceContextJsonFormatter::new(config.service_name.clone()));

    match config.traces_exporter {
        TraceExporter::None => {
            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .try_init()?;
            Ok(TelemetryGuard {
                tracer_provider: None,
            })
        }
        TraceExporter::Otlp => {
            let tracer_provider = build_otlp_tracer_provider(&config)?;
            let tracer = tracer_provider.tracer(config.service_name.clone());
            let otel_layer = tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(
                    EnvFilter::try_new(&config.rust_log).unwrap_or_else(|_| EnvFilter::new("info")),
                );

            tracing_subscriber::registry()
                .with(filter)
                .with(fmt_layer)
                .with(otel_layer)
                .try_init()?;

            Ok(TelemetryGuard {
                tracer_provider: Some(tracer_provider),
            })
        }
    }
}

fn describe_metrics() {
    metrics::describe_counter!(
        HTTP_REQUESTS_TOTAL,
        "Total HTTP requests served by the Rust API."
    );
    metrics::describe_histogram!(
        HTTP_REQUEST_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "HTTP request latency in seconds for the Rust API."
    );
    metrics::describe_gauge!(
        HTTP_REQUESTS_IN_FLIGHT,
        "Number of in-flight HTTP requests in the Rust API."
    );
    metrics::describe_counter!(
        SESSION_EXECUTIONS_TOTAL,
        "Session execution lifecycle events by harness and status."
    );
    metrics::describe_histogram!(
        SESSION_EXECUTION_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Session execution runtime in seconds by harness and terminal status."
    );
    metrics::describe_counter!(
        SANDBOX_OPERATIONS_TOTAL,
        "Sandbox manager operation attempts by backend, operation, and status."
    );
    metrics::describe_counter!(
        SANDBOX_WARM_POOL_CLAIMS_TOTAL,
        "Session warm-pool claim attempts by result."
    );
}

fn build_otlp_tracer_provider(
    config: &TelemetryConfig,
) -> Result<SdkTracerProvider, TelemetryError> {
    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(opentelemetry::KeyValue::new(
            OTEL_SERVICE_NAMESPACE,
            SERVICE_NAMESPACE,
        ))
        .with_attribute(opentelemetry::KeyValue::new(
            OTEL_DEPLOYMENT_ENVIRONMENT_NAME,
            config.environment.clone(),
        ))
        .build();

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()?;

    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

#[derive(Debug, Clone)]
struct TraceContextJsonFormatter {
    inner: fmt_format::Format<fmt_format::Json>,
    service_name: String,
}

impl TraceContextJsonFormatter {
    fn new(service_name: String) -> Self {
        Self {
            inner: fmt_format::format().json().with_target(true),
            service_name,
        }
    }
}

impl<S, N> FormatEvent<S, N> for TraceContextJsonFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std_fmt::Result {
        let mut formatted = String::new();
        self.inner
            .format_event(ctx, Writer::new(&mut formatted), event)?;

        let enriched = inject_log_context(
            &formatted,
            &self.service_name,
            current_trace_context().as_ref(),
        )
        .unwrap_or(formatted);
        writer.write_str(&enriched)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct TraceLogContext {
    trace_id: String,
    span_id: String,
}

fn current_trace_context() -> Option<TraceLogContext> {
    let context = tracing::Span::current().context();
    let span_context = context.span().span_context().clone();
    if !span_context.is_valid() {
        return None;
    }

    Some(TraceLogContext {
        trace_id: span_context.trace_id().to_string(),
        span_id: span_context.span_id().to_string(),
    })
}

fn inject_log_context(
    log_line: &str,
    service_name: &str,
    trace_context: Option<&TraceLogContext>,
) -> Option<String> {
    let trimmed = log_line.trim_end_matches('\n');
    let had_newline = log_line.ends_with('\n');
    let mut value = serde_json::from_str::<Value>(trimmed).ok()?;
    let object = value.as_object_mut()?;

    object.insert("service".to_owned(), Value::String(service_name.to_owned()));
    if let Some(trace_context) = trace_context {
        object.insert(
            "trace_id".to_owned(),
            Value::String(trace_context.trace_id.clone()),
        );
        object.insert(
            "span_id".to_owned(),
            Value::String(trace_context.span_id.clone()),
        );
    }

    let mut enriched = serde_json::to_string(&value).ok()?;
    if had_newline {
        enriched.push('\n');
    }
    Some(enriched)
}

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn normalize_label(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "unknown".to_owned()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_exporter_defaults_to_none_without_endpoint() {
        assert_eq!(TraceExporter::from_values("", false), TraceExporter::None);
    }

    #[test]
    fn trace_exporter_uses_otlp_when_endpoint_is_present() {
        assert_eq!(TraceExporter::from_values("", true), TraceExporter::Otlp);
    }

    #[test]
    fn trace_exporter_can_be_forced_off() {
        assert_eq!(
            TraceExporter::from_values("none", true),
            TraceExporter::None
        );
    }

    #[test]
    fn prometheus_metrics_render_route_template_labels() {
        prometheus_handle().unwrap();
        record_http_request_started();
        record_http_request_finished(
            "POST",
            "/api/session/{thread_key}/execute_test",
            201,
            Duration::from_millis(42),
        );

        let metrics = render_metrics().unwrap();

        assert!(metrics.contains(
            r#"http_server_requests_total{method="POST",route="/api/session/{thread_key}/execute_test",status="201"}"#
        ));
        assert!(metrics.contains(
            r#"http_server_request_duration_seconds_count{method="POST",route="/api/session/{thread_key}/execute_test",status_class="2xx"}"#
        ));
        assert!(metrics.contains("http_server_requests_in_flight 0"));
    }

    #[test]
    fn prometheus_metrics_render_domain_metrics() {
        prometheus_handle().unwrap();
        record_session_execution_started("codex");
        record_session_execution_finished("codex", "completed", Some(Duration::from_secs(2)));
        record_sandbox_operation("local", "create", "success");
        record_sandbox_warm_pool_claim("hit");

        let metrics = render_metrics().unwrap();

        assert!(
            metrics
                .contains(r#"centaur_session_executions_total{harness="codex",status="started"}"#)
        );
        assert!(
            metrics.contains(
                r#"centaur_session_executions_total{harness="codex",status="completed"}"#
            )
        );
        assert!(metrics.contains(
            r#"centaur_session_execution_duration_seconds_count{harness="codex",status="completed"}"#
        ));
        assert!(metrics.contains(
            r#"centaur_sandbox_operations_total{backend="local",operation="create",status="success"}"#
        ));
        assert!(metrics.contains(r#"centaur_sandbox_warm_pool_claims_total{result="hit"}"#));
    }

    #[test]
    fn json_logs_are_enriched_with_service_and_trace_context() {
        let trace_context = TraceLogContext {
            trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
            span_id: "0123456789abcdef".to_owned(),
        };

        let enriched = inject_log_context(
            r#"{"timestamp":"2026-06-05T00:00:00Z","level":"INFO","fields":{"message":"ok"}}"#,
            "centaur-api-rs-test",
            Some(&trace_context),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&enriched).unwrap();

        assert_eq!(value["service"], "centaur-api-rs-test");
        assert_eq!(value["trace_id"], "0123456789abcdef0123456789abcdef");
        assert_eq!(value["span_id"], "0123456789abcdef");
        assert_eq!(value["fields"]["message"], "ok");
    }

    #[test]
    fn json_log_enrichment_preserves_newline() {
        let enriched = inject_log_context(
            "{\"level\":\"INFO\",\"fields\":{}}\n",
            "centaur-api-rs-test",
            None,
        )
        .unwrap();

        assert!(enriched.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<Value>(&enriched).unwrap()["service"],
            "centaur-api-rs-test"
        );
    }
}
