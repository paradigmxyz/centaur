//! OpenTelemetry spans for cells: one span per cell, a child span per tool
//! call, and a grandchild span per upstream request the call made.
//!
//! A cell is traced only when it has a parent trace context: in a session
//! sandbox, the turn trace file harness-server writes for the running turn
//! (`~/.centaur/turn-trace.json`); in `serve`, the `traceparent` header api-rs
//! sends. Without one, or without an OTLP endpoint, nothing is exported.
//!
//! Spans are built from the host's call records after the cell ends, so no
//! span state crosses into a QuickJS realm. They carry names, timings and
//! statuses only: never cell code, arguments, results, logs or query strings.

use std::{
    env,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant, SystemTime},
};

use opentelemetry::{
    Context, KeyValue,
    trace::{
        Span as _, SpanBuilder, SpanContext, SpanId, SpanKind, Status, TraceContextExt as _,
        TraceFlags, TraceId, TraceState, TracerProvider as _,
    },
};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{
    Resource,
    trace::{SdkTracer, SdkTracerProvider},
};
use serde_json::{Value as Json, json};

/// Overrides where the turn trace file is read. harness-server honors the
/// same variable when it writes the file.
const TURN_TRACE_FILE_ENV: &str = "CENTAUR_TURN_TRACE_FILE";
/// Relative to `$HOME` when `CENTAUR_TURN_TRACE_FILE` is unset.
const DEFAULT_TURN_TRACE_FILE: &str = ".centaur/turn-trace.json";
/// The CLI waits for its spans to export before it exits, so an unreachable
/// collector must not hold up the agent for the SDK's default 10 s.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(3);
/// Error text on a span.
const MAX_ERROR_CHARS: usize = 1024;
const LAMINAR_METADATA_PREFIX: &str = "lmnr.association.properties.metadata.";

/// The remote parent of a cell's span, with the session it belongs to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TraceParent {
    traceparent: String,
    thread_key: Option<String>,
    execution_id: Option<String>,
}

impl TraceParent {
    /// A W3C `traceparent` header value; `None` when it is malformed.
    pub fn parse(traceparent: &str) -> Option<Self> {
        let traceparent = traceparent.trim();
        span_context(traceparent)?;
        Some(Self {
            traceparent: traceparent.to_owned(),
            ..Self::default()
        })
    }

    /// The parent for a CLI cell: the running turn's trace file, else a
    /// `TRACEPARENT` environment variable.
    pub fn for_cli() -> Option<Self> {
        turn_trace_file()
            .and_then(|path| Self::from_file(&path))
            .or_else(|| Self::parse(&env::var("TRACEPARENT").ok()?))
    }

    /// The turn trace file: `{ "traceparent", "thread_key", "execution_id" }`.
    /// It is informational, like the sandbox context file: a wrong value only
    /// misplaces spans.
    fn from_file(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let file: Json = serde_json::from_slice(&bytes).ok()?;
        let text = |key: &str| {
            file.get(key)
                .and_then(Json::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            thread_key: text("thread_key"),
            execution_id: text("execution_id"),
            ..Self::parse(&text("traceparent")?)?
        })
    }

    fn context(&self) -> Option<Context> {
        Some(Context::new().with_remote_span_context(span_context(&self.traceparent)?))
    }

    /// The session association every span carries, as on harness spans.
    fn attributes(&self) -> Vec<KeyValue> {
        let mut attributes = Vec::new();
        if let Some(thread_key) = &self.thread_key {
            attributes.push(KeyValue::new("centaur.thread_key", thread_key.clone()));
            attributes.push(KeyValue::new(
                "lmnr.association.properties.session_id",
                thread_key.clone(),
            ));
        }
        if let Some(execution_id) = &self.execution_id {
            attributes.push(KeyValue::new("centaur.execution_id", execution_id.clone()));
            attributes.push(KeyValue::new(
                format!("{LAMINAR_METADATA_PREFIX}execution_id"),
                execution_id.clone(),
            ));
        }
        attributes
    }
}

fn turn_trace_file() -> Option<PathBuf> {
    if let Some(path) = env::var_os(TURN_TRACE_FILE_ENV).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(DEFAULT_TURN_TRACE_FILE))
}

/// `00-<trace id>-<span id>-<flags>`, with non-zero ids.
fn span_context(traceparent: &str) -> Option<SpanContext> {
    let mut fields = traceparent.split('-');
    let version = fields.next()?;
    let trace_id = fields.next()?;
    let span_id = fields.next()?;
    let flags = fields.next()?;
    if fields.next().is_some()
        || version != "00"
        || trace_id.len() != 32
        || span_id.len() != 16
        || flags.len() != 2
    {
        return None;
    }
    let context = SpanContext::new(
        TraceId::from_hex(trace_id).ok()?,
        SpanId::from_hex(span_id).ok()?,
        TraceFlags::new(u8::from_str_radix(flags, 16).ok()?),
        true,
        TraceState::default(),
    );
    context.is_valid().then_some(context)
}

/// What one finished cell did, as the host recorded it.
pub struct CellTrace {
    pub started: Instant,
    pub ended: Instant,
    pub error: Option<String>,
    /// The tool whose failure the cell did not catch.
    pub error_tool: Option<String>,
    pub calls: Vec<CallTrace>,
}

pub struct CallTrace {
    /// `connector.function`.
    pub tool: String,
    pub started: Instant,
    pub ended: Instant,
    pub error: Option<String>,
    pub requests: Vec<RequestTrace>,
    pub files: usize,
}

pub struct RequestTrace {
    pub method: String,
    /// Already redacted: no query string.
    pub url: String,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub started: Instant,
    pub ended: Instant,
}

/// Export one cell's spans under `parent`.
pub fn record_cell(tracer: &SdkTracer, parent: &TraceParent, cell: &CellTrace) {
    let Some(parent_context) = parent.context() else {
        return;
    };
    // Instants become wall-clock times against one anchor, so every span in
    // the cell keeps its exact offsets.
    let anchor = (Instant::now(), SystemTime::now());
    let wall = |at: Instant| match at.checked_duration_since(anchor.0) {
        Some(ahead) => anchor.1 + ahead,
        None => anchor.1 - anchor.0.duration_since(at),
    };
    let common = parent.attributes();

    let failed_calls = cell
        .calls
        .iter()
        .filter(|call| call.error.is_some())
        .count();
    let mut attributes = common.clone();
    attributes.extend([
        KeyValue::new("lmnr.span.type", "DEFAULT"),
        KeyValue::new("centaur.codemode.calls", cell.calls.len() as i64),
        KeyValue::new("centaur.codemode.failed_calls", failed_calls as i64),
        KeyValue::new("centaur.codemode.ok", cell.error.is_none()),
    ]);
    if let Some(tool) = &cell.error_tool {
        attributes.push(KeyValue::new("centaur.codemode.error_tool", tool.clone()));
    }
    let mut cell_span = SpanBuilder::from_name("codemode.cell")
        .with_kind(SpanKind::Internal)
        .with_start_time(wall(cell.started))
        .with_attributes(attributes)
        .start_with_context(tracer, &parent_context);
    let cell_context = Context::new().with_remote_span_context(cell_span.span_context().clone());

    for call in &cell.calls {
        let (connector, function) = call.tool.split_once('.').unwrap_or((&call.tool, ""));
        let mut attributes = common.clone();
        attributes.extend([
            KeyValue::new("lmnr.span.type", "TOOL"),
            KeyValue::new("tool.kind", "codemode"),
            KeyValue::new("tool.name", call.tool.clone()),
            KeyValue::new("tool.method", function.to_owned()),
            KeyValue::new("centaur.codemode.connector", connector.to_owned()),
            KeyValue::new("centaur.codemode.function", function.to_owned()),
            KeyValue::new("centaur.codemode.requests", call.requests.len() as i64),
            KeyValue::new(
                "lmnr.span.input",
                json!({ "connector": connector, "function": function }).to_string(),
            ),
        ]);
        if call.files > 0 {
            attributes.push(KeyValue::new("centaur.codemode.files", call.files as i64));
        }
        let mut call_span = SpanBuilder::from_name(format!("codemode.tool.{}", call.tool))
            .with_kind(SpanKind::Internal)
            .with_start_time(wall(call.started))
            .with_attributes(attributes)
            .start_with_context(tracer, &cell_context);
        let call_context =
            Context::new().with_remote_span_context(call_span.span_context().clone());

        for request in &call.requests {
            record_request(tracer, &call_context, &common, request, &wall);
        }

        let status = if call.error.is_some() {
            "failed"
        } else {
            "completed"
        };
        call_span.set_attribute(KeyValue::new("tool.status", status));
        call_span.set_attribute(KeyValue::new(
            "lmnr.span.output",
            json!({ "status": status }).to_string(),
        ));
        call_span.set_status(match &call.error {
            Some(error) => Status::error(bounded(error)),
            None => Status::Ok,
        });
        call_span.end_with_timestamp(wall(call.ended));
    }

    cell_span.set_status(match &cell.error {
        Some(error) => Status::error(bounded(error)),
        None => Status::Ok,
    });
    cell_span.end_with_timestamp(wall(cell.ended));
}

fn record_request(
    tracer: &SdkTracer,
    parent: &Context,
    common: &[KeyValue],
    request: &RequestTrace,
    wall: &impl Fn(Instant) -> SystemTime,
) {
    let host = reqwest::Url::parse(&request.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    let mut attributes = common.to_vec();
    attributes.extend([
        KeyValue::new("lmnr.span.type", "DEFAULT"),
        KeyValue::new("http.request.method", request.method.clone()),
        KeyValue::new("url.full", request.url.clone()),
    ]);
    if let Some(host) = &host {
        attributes.push(KeyValue::new("server.address", host.clone()));
    }
    if let Some(status) = request.status {
        attributes.push(KeyValue::new(
            "http.response.status_code",
            i64::from(status),
        ));
    }
    let name = match &host {
        Some(host) => format!("{} {host}", request.method),
        None => request.method.clone(),
    };
    let mut span = SpanBuilder::from_name(name)
        .with_kind(SpanKind::Client)
        .with_start_time(wall(request.started))
        .with_attributes(attributes)
        .start_with_context(tracer, parent);
    // As in OpenTelemetry's HTTP client conventions: a transport error or a
    // 4xx/5xx status is an error; anything else leaves the status unset.
    match (&request.error, request.status) {
        (Some(error), _) => {
            span.set_attribute(KeyValue::new("error.type", "transport"));
            span.set_status(Status::error(bounded(error)));
        }
        (None, Some(status)) if status >= 400 => {
            span.set_attribute(KeyValue::new("error.type", status.to_string()));
            span.set_status(Status::error(format!("HTTP {status}")));
        }
        _ => {}
    }
    span.end_with_timestamp(wall(request.ended));
}

fn bounded(text: &str) -> String {
    match text.char_indices().nth(MAX_ERROR_CHARS) {
        Some((end, _)) => format!("{}…[truncated]", &text[..end]),
        None => text.to_owned(),
    }
}

/// The process's OTLP tracer, built on first use; `None` when export is off
/// or no endpoint is configured.
pub fn tracer() -> Option<SdkTracer> {
    provider().map(|provider| provider.tracer("centaur.codemode"))
}

/// Export every span the batch processor holds. The CLI calls this before it
/// exits.
pub fn flush() {
    if let Some(Some(provider)) = PROVIDER.get() {
        let provider = provider.clone();
        // The blocking HTTP client must not run on a Tokio worker.
        let _ = std::thread::spawn(move || provider.force_flush()).join();
    }
}

static PROVIDER: OnceLock<Option<SdkTracerProvider>> = OnceLock::new();

fn provider() -> Option<&'static SdkTracerProvider> {
    PROVIDER
        .get_or_init(|| {
            if export_disabled() || endpoint().is_none() {
                return None;
            }
            // Building the exporter creates a blocking HTTP client, which
            // panics on a Tokio worker; build it on its own thread.
            match std::thread::spawn(build_provider).join() {
                Ok(Ok(provider)) => Some(provider),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "OTLP trace exporter failed to start");
                    None
                }
                Err(_) => None,
            }
        })
        .as_ref()
}

fn build_provider() -> Result<SdkTracerProvider, String> {
    let mut builder = opentelemetry_otlp::SpanExporter::builder().with_http();
    if env::var_os("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT").is_none()
        && env::var_os("OTEL_EXPORTER_OTLP_TIMEOUT").is_none()
    {
        builder = builder.with_timeout(EXPORT_TIMEOUT);
    }
    let exporter = builder.build().map_err(|error| error.to_string())?;
    let mut resource = Resource::builder().with_service_name("centaur-codemode");
    if let Some(environment) = environment() {
        resource = resource.with_attribute(KeyValue::new("deployment.environment", environment));
    }
    Ok(SdkTracerProvider::builder()
        .with_resource(resource.build())
        .with_batch_exporter(exporter)
        .build())
}

fn export_disabled() -> bool {
    matches!(
        env::var("OTEL_TRACES_EXPORTER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "none" | "false" | "0" | "off"
    ) || env::var("OTEL_SDK_DISABLED").is_ok_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

fn endpoint() -> Option<String> {
    [
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ]
    .into_iter()
    .find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// As harness-server reports it, so codemode and harness spans group together.
fn environment() -> Option<String> {
    let from_resource = env::var("OTEL_RESOURCE_ATTRIBUTES").ok().and_then(|raw| {
        raw.split(',').find_map(|item| {
            let (key, value) = item.split_once('=')?;
            matches!(
                key.trim(),
                "deployment.environment" | "deployment.environment.name"
            )
            .then(|| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        })
    });
    from_resource.or_else(|| {
        ["DEPLOY_ENV", "ENVIRONMENT"].into_iter().find_map(|name| {
            env::var(name)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};

    const PARENT: &str = "00-0123456789abcdef0123456789abcdef-1111111111111111-01";

    pub(crate) fn test_tracer() -> (InMemorySpanExporter, SdkTracerProvider, SdkTracer) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("test");
        (exporter, provider, tracer)
    }

    fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a opentelemetry::Value> {
        span.attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == key)
            .map(|attribute| &attribute.value)
    }

    #[test]
    fn parses_only_valid_traceparents() {
        assert!(TraceParent::parse(PARENT).is_some());
        assert!(TraceParent::parse(&format!(" {PARENT}\n")).is_some());
        for bad in [
            "",
            "garbage",
            "00-00000000000000000000000000000000-1111111111111111-01",
            "00-0123456789abcdef0123456789abcdef-0000000000000000-01",
            "01-0123456789abcdef0123456789abcdef-1111111111111111-01",
            "00-0123456789abcdef-1111111111111111-01",
            "00-0123456789abcdef0123456789abcdef-1111111111111111-01-extra",
        ] {
            assert!(TraceParent::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn reads_the_turn_trace_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turn-trace.json");
        std::fs::write(
            &path,
            json!({
                "version": 1,
                "traceparent": PARENT,
                "thread_key": "slack:C1:1.2",
                "execution_id": "exe_1",
            })
            .to_string(),
        )
        .unwrap();
        let parent = TraceParent::from_file(&path).unwrap();
        assert_eq!(parent.traceparent, PARENT);
        assert_eq!(parent.thread_key.as_deref(), Some("slack:C1:1.2"));
        assert_eq!(parent.execution_id.as_deref(), Some("exe_1"));

        std::fs::write(&path, r#"{"traceparent":"nope"}"#).unwrap();
        assert!(TraceParent::from_file(&path).is_none());
        assert!(TraceParent::from_file(&dir.path().join("missing.json")).is_none());
    }

    #[test]
    fn a_cell_is_a_tree_of_cell_call_and_request_spans() {
        let (exporter, _provider, tracer) = test_tracer();
        let parent = TraceParent {
            traceparent: PARENT.to_owned(),
            thread_key: Some("slack:C1:1.2".to_owned()),
            execution_id: Some("exe_1".to_owned()),
        };
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let cell = CellTrace {
            started: at(0),
            ended: at(100),
            error: Some("boom".to_owned()),
            error_tool: Some("linear.issue".to_owned()),
            calls: vec![
                CallTrace {
                    tool: "github.search".to_owned(),
                    started: at(5),
                    ended: at(50),
                    error: None,
                    requests: vec![RequestTrace {
                        method: "GET".to_owned(),
                        url: "https://api.github.com/search/issues".to_owned(),
                        status: Some(200),
                        error: None,
                        started: at(10),
                        ended: at(40),
                    }],
                    files: 0,
                },
                CallTrace {
                    tool: "linear.issue".to_owned(),
                    started: at(60),
                    ended: at(90),
                    error: Some("HTTP 404".to_owned()),
                    requests: vec![RequestTrace {
                        method: "POST".to_owned(),
                        url: "https://api.linear.app/graphql".to_owned(),
                        status: Some(404),
                        error: None,
                        started: at(61),
                        ended: at(89),
                    }],
                    files: 2,
                },
            ],
        };

        record_cell(&tracer, &parent, &cell);

        let spans = exporter.get_finished_spans().unwrap();
        let find = |name: &str| {
            spans
                .iter()
                .find(|span| span.name == name)
                .unwrap_or_else(|| panic!("no span {name}"))
        };
        let cell_span = find("codemode.cell");
        let search = find("codemode.tool.github.search");
        let issue = find("codemode.tool.linear.issue");
        let get = find("GET api.github.com");
        let post = find("POST api.linear.app");
        assert_eq!(spans.len(), 5);

        let trace_id = TraceId::from_hex("0123456789abcdef0123456789abcdef").unwrap();
        assert!(
            spans
                .iter()
                .all(|span| span.span_context.trace_id() == trace_id)
        );
        assert_eq!(
            cell_span.parent_span_id,
            SpanId::from_hex("1111111111111111").unwrap()
        );
        assert_eq!(search.parent_span_id, cell_span.span_context.span_id());
        assert_eq!(issue.parent_span_id, cell_span.span_context.span_id());
        assert_eq!(get.parent_span_id, search.span_context.span_id());
        assert_eq!(post.parent_span_id, issue.span_context.span_id());

        assert_eq!(cell_span.status, Status::error("boom"));
        assert_eq!(search.status, Status::Ok);
        assert_eq!(issue.status, Status::error("HTTP 404"));
        assert_eq!(post.status, Status::error("HTTP 404"));
        assert_eq!(get.status, Status::Unset);
        assert_eq!(get.span_kind, SpanKind::Client);

        assert_eq!(
            search.end_time.duration_since(search.start_time).unwrap(),
            Duration::from_millis(45)
        );
        assert!(get.start_time >= search.start_time && get.end_time <= search.end_time);

        for span in [cell_span, search, get] {
            assert_eq!(
                attribute(span, "centaur.thread_key").map(ToString::to_string),
                Some("slack:C1:1.2".to_owned())
            );
            assert_eq!(
                attribute(span, "centaur.execution_id").map(ToString::to_string),
                Some("exe_1".to_owned())
            );
        }
        assert_eq!(
            attribute(cell_span, "centaur.codemode.failed_calls"),
            Some(&opentelemetry::Value::I64(1))
        );
        assert_eq!(
            attribute(issue, "tool.status").map(ToString::to_string),
            Some("failed".to_owned())
        );
        assert_eq!(
            attribute(issue, "centaur.codemode.files"),
            Some(&opentelemetry::Value::I64(2))
        );
        assert_eq!(
            attribute(get, "http.response.status_code"),
            Some(&opentelemetry::Value::I64(200))
        );
    }

    #[test]
    fn bounds_error_text() {
        let long = "x".repeat(MAX_ERROR_CHARS + 10);
        assert!(bounded(&long).ends_with("…[truncated]"));
        assert_eq!(bounded("short"), "short");
    }
}
