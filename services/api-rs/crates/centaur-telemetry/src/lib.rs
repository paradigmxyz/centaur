//! Shared telemetry setup for the Rust Centaur control plane.

use std::{
    collections::HashSet,
    env, fmt as std_fmt,
    io::{Read, Write},
    net::TcpStream,
    sync::{LazyLock, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub use metrics_exporter_prometheus::PrometheusHandle;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use opentelemetry::{
    Context, KeyValue,
    trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
        TracerProvider as _,
    },
};
use opentelemetry_proto::tonic::{
    collector::trace::v1::ExportTraceServiceRequest,
    common::v1::{AnyValue, InstrumentationScope, KeyValue as ProtoKeyValue, any_value},
    resource::v1::Resource as ProtoResource,
    trace::v1::{ResourceSpans, ScopeSpans, Span as ProtoSpan, span},
};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use prost::Message as _;
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
pub const SESSION_FIRST_TOKEN_LATENCY_SECONDS: &str = "centaur_session_first_token_latency_seconds";
pub const SESSION_FAILURES_TOTAL: &str = "centaur_session_failures_total";
pub const SANDBOX_OPERATIONS_TOTAL: &str = "centaur_sandbox_operations_total";
pub const SANDBOX_STARTUP_DURATION_SECONDS: &str = "centaur_sandbox_startup_duration_seconds";
pub const SANDBOX_WARM_POOL_CLAIMS_TOTAL: &str = "centaur_sandbox_warm_pool_claims_total";
pub const ETL_ACTIVE_SCOPES: &str = "etl_active_scopes";
pub const ETL_FAILED_SCOPES: &str = "etl_failed_scopes";
pub const ETL_SCOPE_SYNC_FRESHNESS_SECONDS: &str = "etl_scope_sync_freshness_seconds";
pub const ETL_ITEMS_SEEN_TOTAL: &str = "etl_items_seen_total";
pub const ETL_ITEMS_ENQUEUED_TOTAL: &str = "etl_items_enqueued_total";
pub const ETL_ITEMS_UPSERTED_TOTAL: &str = "etl_items_upserted_total";
pub const ETL_ITEMS_DELETED_TOTAL: &str = "etl_items_deleted_total";
pub const ETL_ITEMS_FAILED_TOTAL: &str = "etl_items_failed_total";
pub const ETL_BACKFILL_JOBS: &str = "etl_backfill_jobs";
pub const ETL_BACKFILL_JOB_AGE_SECONDS: &str = "etl_backfill_job_age_seconds";
pub const COMPANY_CONTEXT_DOCUMENTS_CHANGED_TOTAL: &str = "company_context_documents_changed_total";
pub const COMPANY_CONTEXT_DOCUMENT_SIZE_CHARS: &str = "company_context_document_size_chars";
pub const COMPANY_CONTEXT_PROJECTION_LAG_SECONDS: &str = "company_context_projection_lag_seconds";
pub const WORKFLOW_QUEUE_TASKS: &str = "workflow_queue_tasks";
pub const WORKFLOW_QUEUE_TASKS_BY_WORKFLOW: &str = "workflow_queue_tasks_by_workflow";
pub const WORKFLOW_QUEUE_OLDEST_TASK_AGE_SECONDS: &str = "workflow_queue_oldest_task_age_seconds";
pub const WORKFLOW_QUEUE_OLDEST_TASK_AGE_BY_WORKFLOW_SECONDS: &str =
    "workflow_queue_oldest_task_age_by_workflow_seconds";
pub const SLACK_ARCHIVE_IMPORT_RUNS_TOTAL: &str = "slack_archive_import_runs_total";
pub const SLACK_ARCHIVE_IMPORT_DURATION_SECONDS: &str = "slack_archive_import_duration_seconds";
pub const SLACK_ARCHIVE_IMPORT_BYTES_TOTAL: &str = "slack_archive_import_bytes_total";
pub const SLACK_ARCHIVE_IMPORT_CHANNELS_TOTAL: &str = "slack_archive_import_channels_total";
pub const SLACK_ARCHIVE_IMPORT_USERS_TOTAL: &str = "slack_archive_import_users_total";
pub const SLACK_ARCHIVE_IMPORT_MESSAGES_TOTAL: &str = "slack_archive_import_messages_total";
pub const SLACK_ARCHIVE_IMPORT_MESSAGE_FILES_TOTAL: &str =
    "slack_archive_import_message_files_total";
pub const SLACK_ARCHIVE_IMPORT_ATTACHMENTS_TOTAL: &str = "slack_archive_import_attachments_total";
pub const SLACK_ARCHIVE_IMPORT_BATCH_DURATION_SECONDS: &str =
    "slack_archive_import_batch_duration_seconds";
pub const SLACK_ARCHIVE_IMPORT_BATCH_SIZE: &str = "slack_archive_import_batch_size";
pub const SLACK_ARCHIVE_IMPORT_FAILURES_TOTAL: &str = "slack_archive_import_failures_total";
pub const SLACK_ARCHIVE_IMPORT_SKIPPED_ITEMS_TOTAL: &str =
    "slack_archive_import_skipped_items_total";
pub const SLACK_ARCHIVE_IMPORT_BATCH_FAILURES_TOTAL: &str =
    "slack_archive_import_batch_failures_total";
pub const SLACK_ARCHIVE_IMPORT_LAST_FAILURE_TIMESTAMP_SECONDS: &str =
    "slack_archive_import_last_failure_timestamp_seconds";
pub const SLACK_RETENTION_RUNS_TOTAL: &str = "slack_retention_runs_total";
pub const SLACK_RETENTION_RUN_DURATION_SECONDS: &str = "slack_retention_run_duration_seconds";
pub const SLACK_RETENTION_MESSAGES_PROCESSED_TOTAL: &str =
    "slack_retention_messages_processed_total";
pub const SLACK_RETENTION_BACKFILL_JOBS_TOTAL: &str = "slack_retention_backfill_jobs_total";
pub const SLACK_RETENTION_FAILURES_TOTAL: &str = "slack_retention_failures_total";
pub const SLACK_RETENTION_API_REQUESTS_TOTAL: &str = "slack_retention_api_requests_total";
pub const SLACK_RETENTION_API_RATE_LIMITED_TOTAL: &str = "slack_retention_api_rate_limited_total";
pub const SLACK_RETENTION_BACKFILL_JOB_FAILURES_TOTAL: &str =
    "slack_retention_backfill_job_failures_total";
pub const SLACK_RETENTION_BACKFILL_TERMINAL_SKIPS_TOTAL: &str =
    "slack_retention_backfill_terminal_skips_total";
pub const SLACK_RETENTION_CHANNEL_FAILURES_TOTAL: &str = "slack_retention_channel_failures_total";
pub const SLACK_RETENTION_LAST_FAILURE_TIMESTAMP_SECONDS: &str =
    "slack_retention_last_failure_timestamp_seconds";
pub const SLACK_RETENTION_WATERMARK_LAG_SECONDS: &str = "slack_retention_watermark_lag_seconds";

const HTTP_REQUEST_DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const SESSION_EXECUTION_DURATION_BUCKETS: &[f64] = &[
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0,
];
const SESSION_FIRST_TOKEN_LATENCY_BUCKETS: &[f64] = &[
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0,
];
const SANDBOX_STARTUP_DURATION_BUCKETS: &[f64] =
    &[0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0];
const COMPANY_CONTEXT_DOCUMENT_SIZE_BUCKETS: &[f64] = &[
    100.0, 500.0, 1_000.0, 5_000.0, 10_000.0, 25_000.0, 50_000.0, 100_000.0, 250_000.0, 500_000.0,
];
const SLACK_ARCHIVE_IMPORT_DURATION_BUCKETS: &[f64] = &[
    1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1_200.0, 3_600.0,
];
const SLACK_ARCHIVE_IMPORT_BATCH_SIZE_BUCKETS: &[f64] =
    &[1.0, 10.0, 100.0, 500.0, 1_000.0, 5_000.0, 10_000.0];
const SLACK_RETENTION_DURATION_BUCKETS: &[f64] =
    &[1.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1_200.0];

static PROMETHEUS_HANDLE: LazyLock<Mutex<Option<PrometheusHandle>>> =
    LazyLock::new(|| Mutex::new(None));
static EXPORTED_THREAD_ROOT_SPANS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

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
        if let Some(provider) = self.tracer_provider.take()
            && let Err(error) = provider.shutdown()
        {
            tracing::warn!(%error, "failed to shut down OpenTelemetry tracer provider");
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
        .set_buckets_for_metric(
            Matcher::Full(SESSION_FIRST_TOKEN_LATENCY_SECONDS.to_owned()),
            SESSION_FIRST_TOKEN_LATENCY_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(SANDBOX_STARTUP_DURATION_SECONDS.to_owned()),
            SANDBOX_STARTUP_DURATION_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(COMPANY_CONTEXT_DOCUMENT_SIZE_CHARS.to_owned()),
            COMPANY_CONTEXT_DOCUMENT_SIZE_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(SLACK_ARCHIVE_IMPORT_DURATION_SECONDS.to_owned()),
            SLACK_ARCHIVE_IMPORT_DURATION_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(SLACK_ARCHIVE_IMPORT_BATCH_DURATION_SECONDS.to_owned()),
            SLACK_ARCHIVE_IMPORT_DURATION_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(SLACK_ARCHIVE_IMPORT_BATCH_SIZE.to_owned()),
            SLACK_ARCHIVE_IMPORT_BATCH_SIZE_BUCKETS,
        )?
        .set_buckets_for_metric(
            Matcher::Full(SLACK_RETENTION_RUN_DURATION_SECONDS.to_owned()),
            SLACK_RETENTION_DURATION_BUCKETS,
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

pub fn record_session_first_token_latency(harness: &str, duration: Duration) {
    metrics::histogram!(
        SESSION_FIRST_TOKEN_LATENCY_SECONDS,
        "harness" => normalize_label(harness),
    )
    .record(duration.as_secs_f64());
}

pub fn record_session_failure(harness: &str, failure_class: &str) {
    metrics::counter!(
        SESSION_FAILURES_TOTAL,
        "failure_class" => normalize_label(failure_class),
        "harness" => normalize_label(harness),
    )
    .increment(1);
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

pub fn record_sandbox_startup_duration(backend: &str, status: &'static str, duration: Duration) {
    metrics::histogram!(
        SANDBOX_STARTUP_DURATION_SECONDS,
        "backend" => normalize_label(backend),
        "status" => status,
    )
    .record(duration.as_secs_f64());
}

pub fn record_sandbox_warm_pool_claim(result: &'static str) {
    metrics::counter!(
        SANDBOX_WARM_POOL_CLAIMS_TOTAL,
        "result" => result,
    )
    .increment(1);
}

pub fn record_workflow_counter(name: &str, labels: &[(String, String)], value: u64) {
    metrics::counter!(name.to_owned(), workflow_metric_labels(labels)).increment(value);
}

pub fn set_workflow_gauge(name: &str, labels: &[(String, String)], value: f64) {
    if !value.is_finite() {
        return;
    }
    metrics::gauge!(name.to_owned(), workflow_metric_labels(labels)).set(value);
}

pub fn record_workflow_histogram(name: &str, labels: &[(String, String)], value: f64) {
    if !value.is_finite() {
        return;
    }
    metrics::histogram!(name.to_owned(), workflow_metric_labels(labels)).record(value);
}

pub fn set_workflow_queue_tasks(queue: &str, state: &str, value: f64) {
    metrics::gauge!(
        WORKFLOW_QUEUE_TASKS,
        "queue" => queue.to_owned(),
        "state" => state.to_owned(),
    )
    .set(value);
}

pub fn set_workflow_queue_tasks_by_workflow(
    queue: &str,
    state: &str,
    workflow_name: &str,
    value: f64,
) {
    metrics::gauge!(
        WORKFLOW_QUEUE_TASKS_BY_WORKFLOW,
        "queue" => queue.to_owned(),
        "state" => state.to_owned(),
        "workflow_name" => workflow_name.to_owned(),
    )
    .set(value);
}

pub fn set_workflow_queue_oldest_task_age_seconds(queue: &str, state: &str, value: f64) {
    if !value.is_finite() {
        return;
    }
    metrics::gauge!(
        WORKFLOW_QUEUE_OLDEST_TASK_AGE_SECONDS,
        "queue" => queue.to_owned(),
        "state" => state.to_owned(),
    )
    .set(value);
}

pub fn set_workflow_queue_oldest_task_age_by_workflow_seconds(
    queue: &str,
    state: &str,
    workflow_name: &str,
    value: f64,
) {
    if !value.is_finite() {
        return;
    }
    metrics::gauge!(
        WORKFLOW_QUEUE_OLDEST_TASK_AGE_BY_WORKFLOW_SECONDS,
        "queue" => queue.to_owned(),
        "state" => state.to_owned(),
        "workflow_name" => workflow_name.to_owned(),
    )
    .set(value);
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

/// W3C `traceparent` for a tracing span, when the OpenTelemetry layer is
/// installed and the span carries a valid trace context. The sampled flag is
/// always `01`: downstream harness exporters (codex OTLP) must keep emitting
/// usage/cost spans regardless of any upstream sampling decision.
pub fn traceparent_for_span(span: &tracing::Span) -> Option<String> {
    let context = span.context();
    let span_context = context.span().span_context().clone();
    if !span_context.is_valid() {
        return None;
    }
    Some(format!(
        "00-{}-{}-01",
        span_context.trace_id(),
        span_context.span_id()
    ))
}

/// Assign a remote parent trace to a not-yet-entered tracing span.
///
/// `trace_id` may be a UUID string or 32-character W3C trace id. The parent
/// span id is a deterministic thread-root span id; callers should export that
/// root span so trace viewers have a parentless node to render.
pub fn set_span_parent_trace(span: &tracing::Span, trace_id: &str, parent_span_id: &str) -> bool {
    let Some(parent_context) = remote_parent_context(trace_id, parent_span_id) else {
        return false;
    };
    span.set_parent(parent_context).is_ok()
}

pub async fn export_thread_trace_root_span(
    trace_id: &str,
    root_span_id: &str,
    thread_key: &str,
) -> bool {
    if TraceExporter::from_env() != TraceExporter::Otlp {
        return false;
    }

    let export_key = format!("{trace_id}:{root_span_id}");
    {
        let mut exported = EXPORTED_THREAD_ROOT_SPANS
            .lock()
            .expect("thread root span export lock poisoned");
        if !exported.insert(export_key.clone()) {
            return true;
        }
    }

    if let Err(error) =
        export_thread_trace_root_span_inner(trace_id, root_span_id, thread_key).await
    {
        EXPORTED_THREAD_ROOT_SPANS
            .lock()
            .expect("thread root span export lock poisoned")
            .remove(&export_key);
        tracing::warn!(
            %error,
            trace_id,
            root_span_id,
            thread_key,
            "failed to export thread trace root span"
        );
        return false;
    }

    true
}

async fn export_thread_trace_root_span_inner(
    trace_id: &str,
    root_span_id: &str,
    thread_key: &str,
) -> Result<(), String> {
    let trace_id = trace_id.to_owned();
    let root_span_id = root_span_id.to_owned();
    let thread_key = thread_key.to_owned();
    tokio::task::spawn_blocking(move || {
        export_thread_trace_root_span_blocking(&trace_id, &root_span_id, &thread_key)
    })
    .await
    .map_err(|error| format!("thread root span export task failed: {error}"))?
}

fn export_thread_trace_root_span_blocking(
    trace_id: &str,
    root_span_id: &str,
    thread_key: &str,
) -> Result<(), String> {
    let endpoint = otlp_traces_endpoint()
        .ok_or_else(|| "OTLP traces endpoint is not configured".to_owned())?;
    let request =
        thread_trace_root_export_request(trace_id, root_span_id, thread_key, SystemTime::now())?;
    let mut headers = otlp_export_headers();
    headers.push(("x-trace-id".to_owned(), trace_id.to_owned()));
    headers.push(("x-centaur-thread-key".to_owned(), thread_key.to_owned()));
    post_otlp_trace_payload(&endpoint, &headers, &request.encode_to_vec())
}

fn thread_trace_root_export_request(
    trace_id: &str,
    root_span_id: &str,
    thread_key: &str,
    start_time: SystemTime,
) -> Result<ExportTraceServiceRequest, String> {
    let config = TelemetryConfig::from_env();
    let trace_id = trace_id_bytes(trace_id)?;
    let span_id = span_id_bytes(root_span_id)?;
    let end_time = start_time
        .checked_add(Duration::from_nanos(1))
        .unwrap_or(start_time);
    let start_time_unix_nano = unix_time_nanos(start_time);
    let end_time_unix_nano = unix_time_nanos(end_time).max(start_time_unix_nano + 1);

    Ok(ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(ProtoResource {
                attributes: vec![
                    proto_kv_string("service.name", &config.service_name),
                    proto_kv_string(OTEL_SERVICE_NAMESPACE, SERVICE_NAMESPACE),
                    proto_kv_string(OTEL_DEPLOYMENT_ENVIRONMENT_NAME, &config.environment),
                    proto_kv_string("deployment.environment", &config.environment),
                ],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: "centaur.api-rs".to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    ..Default::default()
                }),
                spans: vec![ProtoSpan {
                    trace_id,
                    span_id,
                    parent_span_id: Vec::new(),
                    name: "centaur.api_rs.thread".to_owned(),
                    kind: span::SpanKind::Internal as i32,
                    start_time_unix_nano,
                    end_time_unix_nano,
                    attributes: vec![
                        proto_kv_string(FIELD_COMPONENT, "session_runtime"),
                        proto_kv_string(FIELD_EVENT, "thread_trace_root"),
                        proto_kv_string("centaur.thread_key", thread_key),
                        proto_kv_string(FIELD_THREAD_KEY, thread_key),
                    ],
                    flags: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    })
}

fn post_otlp_trace_payload(
    endpoint: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(), String> {
    let target = OtlpHttpTarget::parse(endpoint)?;
    let mut upstream = TcpStream::connect((target.host.as_str(), target.port))
        .map_err(|error| format!("failed to connect to OTLP endpoint: {error}"))?;
    upstream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| format!("failed to set OTLP read timeout: {error}"))?;
    upstream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| format!("failed to set OTLP write timeout: {error}"))?;
    write!(
        upstream,
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n",
        target.path,
        target.host_header,
        body.len()
    )
    .map_err(|error| format!("failed to write OTLP request headers: {error}"))?;
    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "authorization" | "x-trace-id" | "x-centaur-thread-key"
        ) {
            write!(upstream, "{name}: {value}\r\n")
                .map_err(|error| format!("failed to write OTLP header {name}: {error}"))?;
        }
    }
    upstream
        .write_all(b"\r\n")
        .and_then(|()| upstream.write_all(body))
        .and_then(|()| upstream.flush())
        .map_err(|error| format!("failed to write OTLP request body: {error}"))?;

    let mut response = Vec::new();
    upstream
        .read_to_end(&mut response)
        .map_err(|error| format!("failed to read OTLP response: {error}"))?;
    let status = http_status_code(&response).unwrap_or(0);
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!(
            "OTLP trace export failed with HTTP status {status}"
        ))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct OtlpHttpTarget {
    host: String,
    port: u16,
    host_header: String,
    path: String,
}

impl OtlpHttpTarget {
    fn parse(endpoint: &str) -> Result<Self, String> {
        let endpoint = endpoint.trim();
        let rest = endpoint.strip_prefix("http://").ok_or_else(|| {
            "only http OTLP endpoints are supported for root span export".to_owned()
        })?;
        let (host_port, path) = match rest.split_once('/') {
            Some((host_port, path)) => (host_port, format!("/{path}")),
            None => (rest, "/v1/traces".to_owned()),
        };
        if host_port.is_empty() {
            return Err("OTLP endpoint host is empty".to_owned());
        }
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .map_err(|error| format!("invalid OTLP endpoint port: {error}"))?;
                (host.to_owned(), port)
            }
            None => (host_port.to_owned(), 80),
        };
        if host.is_empty() {
            return Err("OTLP endpoint host is empty".to_owned());
        }
        Ok(Self {
            host,
            port,
            host_header: host_port.to_owned(),
            path,
        })
    }
}

fn http_status_code(response: &[u8]) -> Option<u16> {
    let line = String::from_utf8_lossy(response).lines().next()?.to_owned();
    let mut parts = line.split_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse().ok()
}

fn otlp_traces_endpoint() -> Option<String> {
    first_nonempty_env(&["OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"]).or_else(|| {
        first_nonempty_env(&["OTEL_EXPORTER_OTLP_ENDPOINT"]).map(|endpoint| {
            if endpoint.ends_with("/v1/traces") {
                endpoint
            } else {
                format!("{}/v1/traces", endpoint.trim_end_matches('/'))
            }
        })
    })
}

fn otlp_export_headers() -> Vec<(String, String)> {
    first_nonempty_env(&[
        "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
        "OTEL_EXPORTER_OTLP_HEADERS",
    ])
    .map(|raw| parse_otlp_headers(&raw))
    .unwrap_or_default()
}

fn parse_otlp_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|part| {
            let (name, value) = part.split_once('=')?;
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() {
                return None;
            }
            Some((name, percent_decode(value.trim())))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            out.push(high << 4 | low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn proto_kv_string(key: &str, value: &str) -> ProtoKeyValue {
    ProtoKeyValue {
        key: key.to_owned(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_owned())),
        }),
        ..Default::default()
    }
}

fn trace_id_bytes(trace_id: &str) -> Result<Vec<u8>, String> {
    let trace_hex =
        normalize_trace_id_hex(trace_id).ok_or_else(|| "invalid thread trace id".to_owned())?;
    let bytes = hex_to_bytes(&trace_hex)?;
    if bytes.len() != 16 || bytes.iter().all(|byte| *byte == 0) {
        return Err("invalid zero thread trace id".to_owned());
    }
    Ok(bytes)
}

fn span_id_bytes(span_id: &str) -> Result<Vec<u8>, String> {
    let span_id = span_id.trim();
    if span_id.len() != 16 || !span_id.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("invalid thread root span id".to_owned());
    }
    let bytes = hex_to_bytes(span_id)?;
    if bytes.len() != 8 || bytes.iter().all(|byte| *byte == 0) {
        return Err("invalid zero thread root span id".to_owned());
    }
    Ok(bytes)
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("hex value must have even length".to_owned());
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(hex.len() / 2);
    for index in (0..bytes.len()).step_by(2) {
        let high = hex_value(bytes[index]).ok_or_else(|| "invalid hex digit".to_owned())?;
        let low = hex_value(bytes[index + 1]).ok_or_else(|| "invalid hex digit".to_owned())?;
        out.push(high << 4 | low);
    }
    Ok(out)
}

fn unix_time_nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn remote_parent_context(trace_id: &str, parent_span_id: &str) -> Option<Context> {
    let trace_id = normalize_trace_id_hex(trace_id)?;
    let trace_id = TraceId::from_hex(&trace_id).ok()?;
    let parent_span_id = SpanId::from_hex(parent_span_id).ok()?;
    if trace_id == TraceId::INVALID || parent_span_id == SpanId::INVALID {
        return None;
    }
    let span_context = SpanContext::new(
        trace_id,
        parent_span_id,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    Some(Context::new().with_remote_span_context(span_context))
}

fn normalize_trace_id_hex(trace_id: &str) -> Option<String> {
    let hex: String = trace_id.chars().filter(|ch| *ch != '-').collect();
    if hex.len() == 32 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(hex)
    } else {
        None
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
    metrics::describe_histogram!(
        SESSION_FIRST_TOKEN_LATENCY_SECONDS,
        metrics::Unit::Seconds,
        "Latency from session execution start to first answer token by harness."
    );
    metrics::describe_counter!(
        SESSION_FAILURES_TOTAL,
        "Session execution failures by harness and low-cardinality failure class."
    );
    metrics::describe_counter!(
        SANDBOX_OPERATIONS_TOTAL,
        "Sandbox manager operation attempts by backend, operation, and status."
    );
    metrics::describe_histogram!(
        SANDBOX_STARTUP_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Sandbox create-to-ready latency in seconds by backend and status."
    );
    metrics::describe_counter!(
        SANDBOX_WARM_POOL_CLAIMS_TOTAL,
        "Session warm-pool claim attempts by result."
    );
    metrics::describe_gauge!(ETL_ACTIVE_SCOPES, "Current active ETL scopes by source.");
    metrics::describe_gauge!(ETL_FAILED_SCOPES, "Current failed ETL scopes by source.");
    metrics::describe_gauge!(
        ETL_SCOPE_SYNC_FRESHNESS_SECONDS,
        metrics::Unit::Seconds,
        "Oldest successful ETL scope sync age in seconds by source."
    );
    metrics::describe_counter!(
        ETL_ITEMS_SEEN_TOTAL,
        "Source items fetched or observed by ETL workflows."
    );
    metrics::describe_counter!(
        ETL_ITEMS_ENQUEUED_TOTAL,
        "Source items enqueued by ETL workflows."
    );
    metrics::describe_counter!(
        ETL_ITEMS_UPSERTED_TOTAL,
        "Source items upserted by ETL workflows."
    );
    metrics::describe_counter!(
        ETL_ITEMS_DELETED_TOTAL,
        "Source items deleted by ETL workflows."
    );
    metrics::describe_counter!(
        ETL_ITEMS_FAILED_TOTAL,
        "Source items that failed processing in ETL workflows."
    );
    metrics::describe_gauge!(
        ETL_BACKFILL_JOBS,
        "Current ETL backfill jobs by source, job type, and status."
    );
    metrics::describe_gauge!(
        ETL_BACKFILL_JOB_AGE_SECONDS,
        metrics::Unit::Seconds,
        "Oldest ETL backfill job age in seconds by source, job type, and status."
    );
    metrics::describe_counter!(
        COMPANY_CONTEXT_DOCUMENTS_CHANGED_TOTAL,
        "Company context document changes observed by ETL workflows."
    );
    metrics::describe_histogram!(
        COMPANY_CONTEXT_DOCUMENT_SIZE_CHARS,
        "Company context document sizes in characters."
    );
    metrics::describe_gauge!(
        COMPANY_CONTEXT_PROJECTION_LAG_SECONDS,
        metrics::Unit::Seconds,
        "Company context projection lag in seconds."
    );
    metrics::describe_gauge!(
        WORKFLOW_QUEUE_TASKS,
        "Current non-terminal workflow task count by queue and state."
    );
    metrics::describe_gauge!(
        WORKFLOW_QUEUE_TASKS_BY_WORKFLOW,
        "Current non-terminal workflow task count by queue, state, and workflow name."
    );
    metrics::describe_gauge!(
        WORKFLOW_QUEUE_OLDEST_TASK_AGE_SECONDS,
        metrics::Unit::Seconds,
        "Oldest non-terminal workflow task age in seconds by queue and state."
    );
    metrics::describe_gauge!(
        WORKFLOW_QUEUE_OLDEST_TASK_AGE_BY_WORKFLOW_SECONDS,
        metrics::Unit::Seconds,
        "Oldest non-terminal workflow task age in seconds by queue, state, and workflow name."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_RUNS_TOTAL,
        "Slack archive import lifecycle events by status and reason."
    );
    metrics::describe_histogram!(
        SLACK_ARCHIVE_IMPORT_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Slack archive import run duration in seconds by status."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_BYTES_TOTAL,
        "Slack archive import zip bytes processed."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_CHANNELS_TOTAL,
        "Slack archive import channel rows by result."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_USERS_TOTAL,
        "Slack archive import user rows by result."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_MESSAGES_TOTAL,
        "Slack archive import message rows by result."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_MESSAGE_FILES_TOTAL,
        "Slack archive import message files by result."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_ATTACHMENTS_TOTAL,
        "Slack archive import attachment rows by result."
    );
    metrics::describe_histogram!(
        SLACK_ARCHIVE_IMPORT_BATCH_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Slack archive import batch write duration in seconds by entity."
    );
    metrics::describe_histogram!(
        SLACK_ARCHIVE_IMPORT_BATCH_SIZE,
        "Slack archive import batch size by entity."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_FAILURES_TOTAL,
        "Slack archive import failures by stage and reason."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_SKIPPED_ITEMS_TOTAL,
        "Slack archive import skipped items by type and reason."
    );
    metrics::describe_counter!(
        SLACK_ARCHIVE_IMPORT_BATCH_FAILURES_TOTAL,
        "Slack archive import batch failures by entity and reason."
    );
    metrics::describe_gauge!(
        SLACK_ARCHIVE_IMPORT_LAST_FAILURE_TIMESTAMP_SECONDS,
        metrics::Unit::Seconds,
        "Unix timestamp of the most recent Slack archive import failure."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_RUNS_TOTAL,
        "Slack retention workflow lifecycle events by workflow, status, mode, and reason."
    );
    metrics::describe_histogram!(
        SLACK_RETENTION_RUN_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Slack retention workflow run duration in seconds."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_MESSAGES_PROCESSED_TOTAL,
        "Slack retention messages processed by workflow, mode, and result."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_BACKFILL_JOBS_TOTAL,
        "Slack retention backfill job transitions by job type, result, and reason."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_FAILURES_TOTAL,
        "Slack retention failures by workflow, operation, and reason."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_API_REQUESTS_TOTAL,
        "Slack API requests made by retention workflows by operation, result, and reason."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_API_RATE_LIMITED_TOTAL,
        "Slack API rate-limit events observed by retention workflows by operation."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_BACKFILL_JOB_FAILURES_TOTAL,
        "Slack retention backfill job failures by job type and reason."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_BACKFILL_TERMINAL_SKIPS_TOTAL,
        "Slack retention backfill jobs terminally skipped by job type and reason."
    );
    metrics::describe_counter!(
        SLACK_RETENTION_CHANNEL_FAILURES_TOTAL,
        "Slack retention channel failures by workflow and reason."
    );
    metrics::describe_gauge!(
        SLACK_RETENTION_LAST_FAILURE_TIMESTAMP_SECONDS,
        metrics::Unit::Seconds,
        "Unix timestamp of the most recent Slack retention failure by workflow."
    );
    metrics::describe_gauge!(
        SLACK_RETENTION_WATERMARK_LAG_SECONDS,
        metrics::Unit::Seconds,
        "Slack retention watermark lag in seconds by mode."
    );
}

fn workflow_metric_labels(labels: &[(String, String)]) -> Vec<metrics::Label> {
    labels
        .iter()
        .map(|(key, value)| metrics::Label::new(key.clone(), value.clone()))
        .collect()
}

fn build_otlp_tracer_provider(
    config: &TelemetryConfig,
) -> Result<SdkTracerProvider, TelemetryError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()?;

    Ok(SdkTracerProvider::builder()
        .with_resource(otlp_resource(config))
        .with_batch_exporter(exporter)
        .build())
}

fn otlp_resource(config: &TelemetryConfig) -> Resource {
    Resource::builder()
        .with_service_name(config.service_name.clone())
        .with_attribute(KeyValue::new(OTEL_SERVICE_NAMESPACE, SERVICE_NAMESPACE))
        .with_attribute(KeyValue::new(
            OTEL_DEPLOYMENT_ENVIRONMENT_NAME,
            config.environment.clone(),
        ))
        .build()
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
    fn thread_trace_root_export_request_uses_parentless_thread_root() {
        let request = thread_trace_root_export_request(
            "01234567-89ab-cdef-0123-456789abcdef",
            "1111111111111111",
            "slack:T:C:1782217699.671539",
            SystemTime::UNIX_EPOCH,
        )
        .expect("thread root span");
        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(
            span.trace_id,
            vec![
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef,
            ]
        );
        assert_eq!(
            span.span_id,
            vec![0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11]
        );
        assert!(span.parent_span_id.is_empty());
        assert_eq!(span.name, "centaur.api_rs.thread");
        assert_eq!(span.start_time_unix_nano, 0);
        assert!(span.end_time_unix_nano > span.start_time_unix_nano);
        assert!(
            span.attributes
                .iter()
                .any(|attribute| attribute.key == "centaur.thread_key")
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
        record_session_first_token_latency("codex", Duration::from_millis(750));
        record_session_failure("codex", "timeout");
        record_sandbox_operation("local", "create", "success");
        record_sandbox_startup_duration("local", "success", Duration::from_secs(4));
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
        assert!(
            metrics
                .contains(r#"centaur_session_first_token_latency_seconds_count{harness="codex"}"#)
        );
        assert!(metrics.contains(
            r#"centaur_session_failures_total{failure_class="timeout",harness="codex"}"#
        ));
        assert!(metrics.contains(
            r#"centaur_sandbox_operations_total{backend="local",operation="create",status="success"}"#
        ));
        assert!(metrics.contains(
            r#"centaur_sandbox_startup_duration_seconds_count{backend="local",status="success"}"#
        ));
        assert!(metrics.contains(r#"centaur_sandbox_warm_pool_claims_total{result="hit"}"#));
    }

    #[test]
    fn prometheus_metrics_render_workflow_metrics() {
        prometheus_handle().unwrap();
        record_workflow_counter(
            ETL_ITEMS_SEEN_TOTAL,
            &[
                ("environment".to_owned(), "production".to_owned()),
                ("item_type".to_owned(), "thread_refresh_reply".to_owned()),
                ("namespace".to_owned(), "centaur-system".to_owned()),
                ("source".to_owned(), "slack".to_owned()),
                ("source_type".to_owned(), "channel".to_owned()),
            ],
            7,
        );
        record_workflow_counter(
            COMPANY_CONTEXT_DOCUMENTS_CHANGED_TOTAL,
            &[
                ("action".to_owned(), "noop".to_owned()),
                ("environment".to_owned(), "production".to_owned()),
                ("namespace".to_owned(), "centaur-system".to_owned()),
                ("source".to_owned(), "slack".to_owned()),
                ("source_type".to_owned(), "slack_thread".to_owned()),
            ],
            0,
        );
        set_workflow_gauge(
            ETL_BACKFILL_JOBS,
            &[
                ("environment".to_owned(), "production".to_owned()),
                ("job_type".to_owned(), "thread_refresh".to_owned()),
                ("namespace".to_owned(), "centaur-system".to_owned()),
                ("source".to_owned(), "slack".to_owned()),
                ("status".to_owned(), "pending".to_owned()),
            ],
            3.0,
        );
        set_workflow_gauge(
            ETL_ACTIVE_SCOPES,
            &[
                ("environment".to_owned(), "production".to_owned()),
                ("namespace".to_owned(), "centaur-system".to_owned()),
                ("source".to_owned(), "slack".to_owned()),
            ],
            11.0,
        );
        set_workflow_queue_tasks("centaur_workflows_slack_live", "pending", 1.0);
        set_workflow_queue_oldest_task_age_seconds("centaur_workflows_slack_live", "pending", 42.0);
        set_workflow_queue_tasks_by_workflow(
            "centaur_workflows_slack_live",
            "pending",
            "slack_sync",
            1.0,
        );
        set_workflow_queue_oldest_task_age_by_workflow_seconds(
            "centaur_workflows_slack_live",
            "pending",
            "slack_sync",
            42.0,
        );

        let metrics = render_metrics().unwrap();

        assert!(metrics.contains("etl_items_seen_total{"));
        assert!(metrics.contains("etl_active_scopes{"));
        assert!(metrics.contains("company_context_documents_changed_total{"));
        assert!(metrics.contains("etl_backfill_jobs{"));
        assert!(metrics.contains("workflow_queue_tasks{"));
        assert!(metrics.contains("workflow_queue_tasks_by_workflow{"));
        assert!(metrics.contains("workflow_queue_oldest_task_age_seconds{"));
        assert!(metrics.contains("workflow_queue_oldest_task_age_by_workflow_seconds{"));
        assert!(metrics.contains(r#"environment="production""#));
        assert!(metrics.contains(r#"namespace="centaur-system""#));
        assert!(metrics.contains(r#"source="slack""#));
        assert!(metrics.contains(r#"queue="centaur_workflows_slack_live""#));
        assert!(metrics.contains(r#"workflow_name="slack_sync""#));
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
