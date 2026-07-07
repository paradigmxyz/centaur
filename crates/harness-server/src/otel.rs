use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, span};
use prost::Message as _;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::{HarnessKind, HarnessServerError, NormalizedTokenUsage, Result};

const CODEX_SPAN_PREFIX: &str = "codex.";
const LAMINAR_METADATA_PREFIX: &str = "lmnr.association.properties.metadata.";

static OTLP_PROXY_ENDPOINT: OnceLock<String> = OnceLock::new();
static OTLP_TRACE_METADATA: OnceLock<BTreeMap<String, Value>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceContext {
    pub(crate) thread_key: Option<String>,
    pub(crate) trace_id: Option<String>,
    pub(crate) traceparent: Option<String>,
    pub(crate) metadata: BTreeMap<String, Value>,
}

impl TraceContext {
    pub(crate) fn effective_trace_id(&self) -> Option<String> {
        self.trace_id
            .clone()
            .or_else(|| {
                self.traceparent
                    .as_deref()
                    .and_then(trace_id_from_traceparent)
            })
            .or_else(|| clean_optional(env::var("CENTAUR_TRACE_ID").ok().as_deref()))
    }

    pub(crate) fn effective_traceparent(&self) -> Option<String> {
        let trace_id = self.effective_trace_id()?;
        if let Some(traceparent) = self.traceparent.as_deref().and_then(validate_traceparent)
            && trace_id_from_traceparent(traceparent).as_deref() == Some(trace_id.as_str())
        {
            return Some(traceparent.to_owned());
        }
        traceparent_from_trace_id(&trace_id)
    }
}

pub(crate) fn configure_codex_otel_for_startup(trace: &TraceContext) -> Result<()> {
    let Some(trace_id) = trace.effective_trace_id() else {
        return Ok(());
    };
    let Some(endpoint) = otlp_traces_endpoint() else {
        return Ok(());
    };
    if !trace.metadata.is_empty() {
        let _ = OTLP_TRACE_METADATA.set(trace.metadata.clone());
    }
    let proxy_endpoint = start_otlp_proxy(&endpoint)?;
    let config_path = codex_config_path();
    let base = config_path
        .as_ref()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|contents| strip_otel_toml_sections(&contents))
        .unwrap_or_default();
    let environment = otel_environment();
    let api_key = otel_authorization_token();
    let next = codex_otel_config_contents(
        &base,
        &proxy_endpoint,
        &trace_id,
        trace.thread_key.as_deref(),
        api_key.as_deref(),
        &environment,
    );
    let Some(config_path) = config_path else {
        return Err(HarnessServerError::Protocol(
            "CODEX_HOME/HOME unavailable; cannot write Codex OTEL config".to_string(),
        ));
    };
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(config_path, next)?;
    Ok(())
}

pub(crate) fn unix_time_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Debug)]
pub(crate) struct HarnessUsageSpan<'a> {
    pub(crate) harness: HarnessKind,
    pub(crate) model: &'a str,
    pub(crate) model_provider: &'a str,
    pub(crate) turn_id: &'a str,
    pub(crate) input: Option<&'a str>,
    pub(crate) output: Option<&'a str>,
    pub(crate) start_unix_nano: u64,
    pub(crate) end_unix_nano: u64,
}

pub(crate) fn export_harness_usage_span(
    trace: &TraceContext,
    span: HarnessUsageSpan<'_>,
    usage: &NormalizedTokenUsage,
) -> Result<()> {
    if !usage.has_counts() {
        return Ok(());
    }
    let Some(endpoint) = otlp_traces_endpoint() else {
        return Ok(());
    };
    let request = harness_usage_trace_request(trace, span, usage)?;
    let mut headers = otel_forward_headers();
    if let Some(trace_id) = trace.effective_trace_id() {
        headers.insert("x-trace-id".to_string(), trace_id);
    }
    if let Some(thread_key) = clean_optional(trace.thread_key.as_deref()) {
        headers.insert("x-centaur-thread-key".to_string(), thread_key);
    }
    post_otlp_trace_payload(&endpoint, &headers, &request.encode_to_vec())
}

fn codex_config_path() -> Option<PathBuf> {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .map(|home| home.join("config.toml"))
}

fn otlp_traces_endpoint() -> Option<String> {
    let traces_endpoint = clean_optional(
        env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .ok()
            .as_deref(),
    );
    if traces_endpoint.is_some() {
        return traces_endpoint;
    }
    let base = clean_optional(env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok().as_deref())?;
    if base.ends_with("/v1/traces") {
        Some(base)
    } else {
        Some(format!("{}/v1/traces", base.trim_end_matches('/')))
    }
}

fn clean_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn strip_otel_toml_sections(contents: &str) -> String {
    let mut kept = Vec::new();
    let mut skipping = false;
    for line in contents.lines() {
        let stripped = line.trim();
        if stripped.starts_with('[') && stripped.ends_with(']') {
            let section = stripped.trim_matches(['[', ']']).trim();
            skipping = section == "otel" || section.starts_with("otel.");
        }
        if !skipping {
            kept.push(line);
        }
    }
    kept.join("\n").trim_end().to_owned()
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn codex_otel_config_contents(
    base: &str,
    endpoint: &str,
    trace_id: &str,
    thread_key: Option<&str>,
    api_key: Option<&str>,
    environment: &str,
) -> String {
    let mut headers = vec![format!("x-trace-id = {}", toml_string(trace_id))];
    if let Some(thread_key) = clean_optional(thread_key) {
        headers.push(format!(
            "x-centaur-thread-key = {}",
            toml_string(&thread_key)
        ));
    }
    if let Some(api_key) = clean_optional(api_key) {
        headers.push(format!(
            "authorization = {}",
            toml_string(&format!("Bearer {api_key}"))
        ));
    }
    let otel_block = [
        "[otel]".to_string(),
        format!("environment = {}", toml_string(environment)),
        "log_user_prompt = true".to_string(),
        format!(
            "span_attributes = {{ \"service.name\" = {}, \"centaur.span_prefix\" = {} }}",
            toml_string("codex"),
            toml_string(CODEX_SPAN_PREFIX)
        ),
        format!(
            "trace_exporter = {{ otlp-http = {{ endpoint = {}, protocol = \"binary\", headers = {{ {} }} }} }}",
            toml_string(endpoint),
            headers.join(", ")
        ),
    ]
    .join("\n");
    if base.trim().is_empty() {
        format!("{otel_block}\n")
    } else {
        format!("{}\n\n{otel_block}\n", base.trim_end())
    }
}

fn otel_headers() -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    let Some(raw) = clean_optional(env::var("OTEL_EXPORTER_OTLP_HEADERS").ok().as_deref()) else {
        return headers;
    };
    for item in raw.split(',') {
        let Some((key, value)) = item.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if !key.is_empty() {
            headers.insert(key, percent_decode(value.trim()));
        }
    }
    headers
}

fn otel_authorization_token() -> Option<String> {
    let authorization = clean_optional(otel_headers().get("authorization").map(String::as_str))?;
    authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or(Some(authorization))
}

fn otel_forward_headers() -> BTreeMap<String, String> {
    otel_headers()
        .into_iter()
        .filter(|(name, _)| name == "authorization")
        .collect()
}

fn otel_environment() -> String {
    if let Ok(raw) = env::var("OTEL_RESOURCE_ATTRIBUTES") {
        for item in raw.split(',') {
            let Some((key, value)) = item.split_once('=') else {
                continue;
            };
            if key.trim() == "deployment.environment"
                && let Some(value) = clean_optional(Some(value))
            {
                return value;
            }
        }
    }
    clean_optional(env::var("DEPLOY_ENV").ok().as_deref())
        .or_else(|| clean_optional(env::var("ENVIRONMENT").ok().as_deref()))
        .unwrap_or_else(|| "dev".to_string())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            out.push((hi << 4) | lo);
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

fn start_otlp_proxy(endpoint: &str) -> Result<String> {
    if let Some(existing) = OTLP_PROXY_ENDPOINT.get() {
        return Ok(existing.clone());
    }
    let target = OtlpTarget::parse(endpoint)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let local = listener.local_addr()?;
    thread::spawn(move || run_otlp_proxy(listener, target));
    let endpoint = format!("http://{local}/v1/traces");
    let _ = OTLP_PROXY_ENDPOINT.set(endpoint.clone());
    Ok(endpoint)
}

#[derive(Clone, Debug)]
struct OtlpTarget {
    host: String,
    port: u16,
    path: String,
    host_header: String,
}

impl OtlpTarget {
    fn parse(endpoint: &str) -> Result<Self> {
        let url = Url::parse(endpoint).map_err(|error| {
            HarnessServerError::Protocol(format!("invalid OTLP endpoint: {error}"))
        })?;
        if url.scheme() != "http" {
            return Err(HarnessServerError::Protocol(format!(
                "harness OTLP proxy only supports http endpoints, got {}",
                url.scheme()
            )));
        }
        let host = url
            .host_str()
            .ok_or_else(|| HarnessServerError::Protocol("OTLP endpoint missing host".to_string()))?
            .to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let mut path = url.path().to_string();
        if path.is_empty() {
            path = "/".to_string();
        }
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        let host_header = if url.port().is_some() {
            format!("{host}:{port}")
        } else {
            host.clone()
        };
        Ok(Self {
            host,
            port,
            path,
            host_header,
        })
    }
}

fn post_otlp_trace_payload(
    endpoint: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<()> {
    let target = OtlpTarget::parse(endpoint)?;
    let mut upstream = TcpStream::connect((target.host.as_str(), target.port))?;
    upstream.set_read_timeout(Some(Duration::from_secs(10)))?;
    upstream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        upstream,
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n",
        target.path,
        target.host_header,
        body.len()
    )?;
    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "authorization" | "x-trace-id" | "x-centaur-thread-key"
        ) {
            write!(upstream, "{name}: {value}\r\n")?;
        }
    }
    upstream.write_all(b"\r\n")?;
    upstream.write_all(body)?;
    upstream.flush()?;

    let mut response = Vec::new();
    upstream.read_to_end(&mut response)?;
    let status = http_status_code(&response).unwrap_or(0);
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(HarnessServerError::Protocol(format!(
            "OTLP trace export failed with HTTP status {status}"
        )))
    }
}

fn http_status_code(response: &[u8]) -> Option<u16> {
    let line = String::from_utf8_lossy(response)
        .lines()
        .next()?
        .to_string();
    let mut parts = line.split_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse().ok()
}

fn run_otlp_proxy(listener: TcpListener, target: OtlpTarget) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let target = target.clone();
        thread::spawn(move || {
            let _ = handle_otlp_proxy_connection(stream, &target);
        });
    }
}

fn handle_otlp_proxy_connection(mut stream: TcpStream, target: &OtlpTarget) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    match read_http_request(&mut stream) {
        Ok(request) if request.method == "POST" && request.path == "/v1/traces" => {
            match rewrite_otlp_trace_payload(&request.body) {
                Ok(body) => forward_otlp_request(&mut stream, target, &request.headers, &body),
                Err(error) => {
                    write_http_response(&mut stream, 400, "Bad Request", error.as_bytes())
                }
            }
        }
        Ok(_) => write_http_response(&mut stream, 404, "Not Found", b"not found"),
        Err(error) => write_http_response(
            &mut stream,
            400,
            "Bad Request",
            error.to_string().as_bytes(),
        ),
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_http_request(stream: &mut TcpStream) -> std::io::Result<HttpRequest> {
    let mut data = Vec::new();
    let header_end = loop {
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        data.extend_from_slice(&buffer[..read]);
        if let Some(index) = find_header_end(&data) {
            break index;
        }
        if data.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "headers too large",
            ));
        }
    };
    let body_start = header_end + 4;
    let headers_text = String::from_utf8_lossy(&data[..header_end]);
    let mut lines = headers_text.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = data[body_start..].to_vec();
    if body.len() < content_length {
        let mut remaining = vec![0_u8; content_length - body.len()];
        stream.read_exact(&mut remaining)?;
        body.extend_from_slice(&remaining);
    }
    body.truncate(content_length);
    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn forward_otlp_request(
    client: &mut TcpStream,
    target: &OtlpTarget,
    incoming_headers: &BTreeMap<String, String>,
    body: &[u8],
) -> std::io::Result<()> {
    let mut upstream = TcpStream::connect((target.host.as_str(), target.port))?;
    upstream.set_read_timeout(Some(Duration::from_secs(10)))?;
    upstream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        upstream,
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n",
        target.path,
        target.host_header,
        body.len()
    )?;
    for (name, value) in incoming_headers {
        if matches!(
            name.as_str(),
            "authorization" | "x-trace-id" | "x-centaur-thread-key"
        ) {
            write!(upstream, "{name}: {value}\r\n")?;
        }
    }
    upstream.write_all(b"\r\n")?;
    upstream.write_all(body)?;
    upstream.flush()?;

    let mut response = Vec::new();
    upstream.read_to_end(&mut response)?;
    if response.is_empty() {
        write_http_response(client, 502, "Bad Gateway", b"empty upstream response")
    } else {
        client.write_all(&response)
    }
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn harness_usage_trace_request(
    trace: &TraceContext,
    span_context: HarnessUsageSpan<'_>,
    usage: &NormalizedTokenUsage,
) -> Result<ExportTraceServiceRequest> {
    let (trace_id, parent_span_id) = harness_usage_span_trace_ids(trace)?;
    let mut attributes = Vec::new();
    let harness_name = harness_name(span_context.harness);
    let system = gen_ai_system(span_context.harness, span_context.model_provider);
    let model = usage
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(span_context.model);
    let total_tokens = usage.total_tokens.or_else(|| {
        [
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            usage.reasoning_output_tokens,
        ]
        .into_iter()
        .flatten()
        .try_fold(0_i64, |acc, value| acc.checked_add(value))
    });

    set_attribute_string(&mut attributes, "gen_ai.operation.name", "chat");
    set_attribute_string(&mut attributes, "lmnr.span.type", "LLM");
    set_attribute_string(&mut attributes, "gen_ai.system", system);
    set_attribute_string(&mut attributes, "gen_ai.request.model", model);
    set_attribute_string(&mut attributes, "gen_ai.response.model", model);
    set_harness_span_io_attributes(&mut attributes, span_context.input, span_context.output);
    set_attribute_int(
        &mut attributes,
        "gen_ai.usage.input_tokens",
        usage.input_tokens,
    );
    set_attribute_int(
        &mut attributes,
        "gen_ai.usage.output_tokens",
        usage.output_tokens,
    );
    set_attribute_int(
        &mut attributes,
        "gen_ai.usage.cache_creation_input_tokens",
        usage.cache_creation_input_tokens,
    );
    set_attribute_int(
        &mut attributes,
        "gen_ai.usage.cache_read_input_tokens",
        usage.cache_read_input_tokens,
    );
    set_attribute_int(
        &mut attributes,
        "gen_ai.usage.reasoning_tokens",
        usage.reasoning_output_tokens,
    );
    set_attribute_int(&mut attributes, "gen_ai.usage.total_tokens", total_tokens);
    if let Some(cost) = estimate_usage_cost(span_context.harness, system, model, usage) {
        set_attribute_double(&mut attributes, "gen_ai.usage.input_cost", cost.input_cost);
        set_attribute_double(
            &mut attributes,
            "gen_ai.usage.output_cost",
            cost.output_cost,
        );
        set_attribute_double(&mut attributes, "gen_ai.usage.cost", cost.total_cost());
        set_attribute_string(&mut attributes, "gen_ai.usage.cost_currency", "USD");
        set_attribute_double(
            &mut attributes,
            "centaur.usage.input_cost_usd",
            cost.input_cost,
        );
        set_attribute_double(
            &mut attributes,
            "centaur.usage.output_cost_usd",
            cost.output_cost,
        );
        set_attribute_double(
            &mut attributes,
            "centaur.usage.estimated_cost_usd",
            cost.total_cost(),
        );
        set_attribute_string(&mut attributes, "centaur.usage.cost_source", cost.source);
        set_attribute_bool(&mut attributes, "centaur.usage.cost_estimated", true);
    }
    set_attribute_string(&mut attributes, "centaur.harness", harness_name);
    set_attribute_string(
        &mut attributes,
        "centaur.model_provider",
        span_context.model_provider,
    );
    set_attribute_string(&mut attributes, "centaur.turn_id", span_context.turn_id);
    if let Some(thread_key) = trace.thread_key.as_deref() {
        set_attribute_string(&mut attributes, "centaur.thread_key", thread_key);
    }
    apply_laminar_trace_metadata_to_attributes(&mut attributes, &trace.metadata);

    let start = span_context.start_unix_nano.min(span_context.end_unix_nano);
    let end = span_context.end_unix_nano.max(start);
    Ok(ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![
                    kv_string("service.name", "harness-server"),
                    kv_string("deployment.environment", &otel_environment()),
                ],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: "centaur.harness-server".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    ..Default::default()
                }),
                spans: vec![Span {
                    trace_id,
                    span_id: random_span_id(),
                    parent_span_id,
                    name: format!("{harness_name}.session_task.turn"),
                    kind: span::SpanKind::Internal as i32,
                    start_time_unix_nano: start,
                    end_time_unix_nano: end,
                    attributes,
                    flags: 1,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    })
}

fn harness_usage_span_trace_ids(trace: &TraceContext) -> Result<(Vec<u8>, Vec<u8>)> {
    let trace_id = trace.effective_trace_id().ok_or_else(|| {
        HarnessServerError::Protocol("missing trace id for harness usage span".to_string())
    })?;
    let trace_id = trace_id_to_bytes(&trace_id).ok_or_else(|| {
        HarnessServerError::Protocol("invalid trace id for harness usage span".to_string())
    })?;
    let parent_span_id = trace
        .traceparent
        .as_deref()
        .and_then(trace_ids_from_traceparent)
        .and_then(|(parent_trace_id, parent_span_id)| {
            if parent_trace_id == trace_id {
                Some(parent_span_id)
            } else {
                None
            }
        })
        .unwrap_or_default();
    Ok((trace_id, parent_span_id))
}

fn set_harness_span_io_attributes(
    attributes: &mut Vec<KeyValue>,
    input: Option<&str>,
    output: Option<&str>,
) {
    if let Some(input) = clean_optional(input) {
        set_attribute_string(attributes, "input.value", &input);
        set_attribute_string(
            attributes,
            "lmnr.span.input",
            &legacy_chat_message_json("user", &input),
        );
        set_attribute_string(
            attributes,
            "gen_ai.input.messages",
            &gen_ai_message_json("user", &input),
        );
    }
    if let Some(output) = clean_optional(output) {
        set_attribute_string(attributes, "output.value", &output);
        set_attribute_string(
            attributes,
            "lmnr.span.output",
            &legacy_chat_message_json("assistant", &output),
        );
        set_attribute_string(
            attributes,
            "gen_ai.output.messages",
            &gen_ai_message_json("assistant", &output),
        );
    }
}

fn legacy_chat_message_json(role: &str, content: &str) -> String {
    serde_json::to_string(&json!([{ "role": role, "content": content }]))
        .unwrap_or_else(|_| "[]".to_string())
}

fn gen_ai_message_json(role: &str, content: &str) -> String {
    serde_json::to_string(&json!([
        {
            "role": role,
            "parts": [{ "type": "text", "content": content }]
        }
    ]))
    .unwrap_or_else(|_| "[]".to_string())
}

fn apply_laminar_trace_metadata_to_attributes(
    attributes: &mut Vec<KeyValue>,
    metadata: &BTreeMap<String, Value>,
) {
    for (key, value) in metadata {
        let key = key.trim();
        if !key.is_empty() {
            set_attribute_json(
                attributes,
                &format!("{LAMINAR_METADATA_PREFIX}{key}"),
                value,
            );
        }
    }
}

fn trace_ids_from_traceparent(traceparent: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let parts = validate_traceparent(traceparent)?
        .split('-')
        .collect::<Vec<_>>();
    Some((hex_bytes(parts[1])?, hex_bytes(parts[2])?))
}

fn trace_id_to_bytes(trace_id: &str) -> Option<Vec<u8>> {
    Some(Uuid::parse_str(trace_id).ok()?.as_bytes().to_vec())
}

fn hex_bytes(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(value.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn random_span_id() -> Vec<u8> {
    Uuid::new_v4().as_bytes()[..8].to_vec()
}

fn harness_name(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Codex => "codex",
        HarnessKind::ClaudeCode => "claude",
        HarnessKind::Amp => "amp",
    }
}

fn gen_ai_system(kind: HarnessKind, model_provider: &str) -> &'static str {
    let provider = model_provider.trim().to_ascii_lowercase();
    if provider.contains("anthropic") || matches!(kind, HarnessKind::ClaudeCode) {
        "anthropic"
    } else if provider.contains("openai") || matches!(kind, HarnessKind::Codex) {
        "openai"
    } else if provider.contains("amp") || matches!(kind, HarnessKind::Amp) {
        "amp"
    } else {
        "unknown"
    }
}

#[derive(Clone, Copy, Debug)]
struct TokenPricing {
    input_per_mtok: f64,
    cache_creation_per_mtok: f64,
    cache_read_per_mtok: f64,
    output_per_mtok: f64,
    source: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct UsageCost {
    input_cost: f64,
    output_cost: f64,
    source: &'static str,
}

impl UsageCost {
    fn total_cost(self) -> f64 {
        self.input_cost + self.output_cost
    }
}

fn estimate_usage_cost(
    harness: HarnessKind,
    system: &str,
    model: &str,
    usage: &NormalizedTokenUsage,
) -> Option<UsageCost> {
    let pricing = pricing_for_usage(harness, system, model)?;
    let input_tokens = positive_tokens(usage.input_tokens);
    let cache_creation_tokens = positive_tokens(usage.cache_creation_input_tokens);
    let cache_read_tokens = positive_tokens(usage.cache_read_input_tokens);
    let output_tokens = positive_tokens(usage.output_tokens);
    let cache_tokens = cache_creation_tokens + cache_read_tokens;
    let non_cached_input_tokens = if input_tokens >= cache_tokens {
        input_tokens - cache_tokens
    } else {
        input_tokens
    };

    let input_cost = mtok_cost(non_cached_input_tokens, pricing.input_per_mtok)
        + mtok_cost(cache_creation_tokens, pricing.cache_creation_per_mtok)
        + mtok_cost(cache_read_tokens, pricing.cache_read_per_mtok);
    let output_cost = mtok_cost(output_tokens, pricing.output_per_mtok);

    Some(UsageCost {
        input_cost,
        output_cost,
        source: pricing.source,
    })
}

fn pricing_for_usage(harness: HarnessKind, system: &str, model: &str) -> Option<TokenPricing> {
    let normalized = normalize_model_name(model);
    match system {
        "anthropic" => anthropic_pricing(&normalized),
        "openai" => openai_pricing(&normalized),
        "amp" => amp_pricing(harness, &normalized),
        _ => None,
    }
}

fn normalize_model_name(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['_', '.'], "-")
}

fn anthropic_pricing(model: &str) -> Option<TokenPricing> {
    if model.contains("fable-5") || model.contains("mythos-5") {
        return Some(TokenPricing {
            input_per_mtok: 10.0,
            cache_creation_per_mtok: 12.5,
            cache_read_per_mtok: 1.0,
            output_per_mtok: 50.0,
            source: "centaur_estimate:anthropic:fable-mythos-5:5m-cache-write",
        });
    }
    if model.contains("opus-4-8")
        || model.contains("opus-4-7")
        || model.contains("opus-4-6")
        || model.contains("opus-4-5")
    {
        return Some(TokenPricing {
            input_per_mtok: 5.0,
            cache_creation_per_mtok: 6.25,
            cache_read_per_mtok: 0.5,
            output_per_mtok: 25.0,
            source: "centaur_estimate:anthropic:opus-4.5-plus:5m-cache-write",
        });
    }
    if model.contains("opus-4-1") || model.contains("opus-4") {
        return Some(TokenPricing {
            input_per_mtok: 15.0,
            cache_creation_per_mtok: 18.75,
            cache_read_per_mtok: 1.5,
            output_per_mtok: 75.0,
            source: "centaur_estimate:anthropic:opus-4-deprecated:5m-cache-write",
        });
    }
    if model.contains("sonnet-4-6") || model.contains("sonnet-4-5") || model.contains("sonnet-4") {
        return Some(TokenPricing {
            input_per_mtok: 3.0,
            cache_creation_per_mtok: 3.75,
            cache_read_per_mtok: 0.3,
            output_per_mtok: 15.0,
            source: "centaur_estimate:anthropic:sonnet-4:5m-cache-write",
        });
    }
    if model.contains("haiku-4-5") {
        return Some(TokenPricing {
            input_per_mtok: 1.0,
            cache_creation_per_mtok: 1.25,
            cache_read_per_mtok: 0.1,
            output_per_mtok: 5.0,
            source: "centaur_estimate:anthropic:haiku-4.5:5m-cache-write",
        });
    }
    None
}

fn openai_pricing(model: &str) -> Option<TokenPricing> {
    if model.contains("gpt-5-5") {
        return Some(TokenPricing {
            input_per_mtok: 5.0,
            cache_creation_per_mtok: 5.0,
            cache_read_per_mtok: 0.5,
            output_per_mtok: 30.0,
            source: "centaur_estimate:openai:gpt-5.5",
        });
    }
    if model.contains("gpt-5-4") {
        return Some(TokenPricing {
            input_per_mtok: 2.5,
            cache_creation_per_mtok: 2.5,
            cache_read_per_mtok: 0.25,
            output_per_mtok: 15.0,
            source: "centaur_estimate:openai:gpt-5.4",
        });
    }
    None
}

fn amp_pricing(_harness: HarnessKind, model: &str) -> Option<TokenPricing> {
    if model == "deep"
        || model.starts_with("deep-")
        || model == "rush"
        || model.starts_with("rush-")
    {
        return Some(TokenPricing {
            input_per_mtok: 5.0,
            cache_creation_per_mtok: 5.0,
            cache_read_per_mtok: 0.5,
            output_per_mtok: 30.0,
            source: "centaur_estimate:amp:gpt-5.5",
        });
    }
    if model == "smart" || model.starts_with("smart-") {
        return Some(TokenPricing {
            input_per_mtok: 5.0,
            cache_creation_per_mtok: 6.25,
            cache_read_per_mtok: 0.5,
            output_per_mtok: 25.0,
            source: "centaur_estimate:amp:claude-opus-4.8:5m-cache-write",
        });
    }
    openai_pricing(model).or_else(|| anthropic_pricing(model))
}

fn positive_tokens(value: Option<i64>) -> f64 {
    value.unwrap_or_default().max(0) as f64
}

fn mtok_cost(tokens: f64, price_per_mtok: f64) -> f64 {
    tokens * price_per_mtok / 1_000_000.0
}

fn kv_string(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
        ..Default::default()
    }
}

pub(crate) fn rewrite_otlp_trace_payload(payload: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut request = ExportTraceServiceRequest::decode(payload)
        .map_err(|error| format!("invalid OTLP trace payload: {error}"))?;
    for resource_span in &mut request.resource_spans {
        for scope_span in &mut resource_span.scope_spans {
            for span in &mut scope_span.spans {
                if !span.name.is_empty() && !span.name.starts_with(CODEX_SPAN_PREFIX) {
                    span.name = format!("{}{}", CODEX_SPAN_PREFIX, span.name);
                }
                normalize_codex_llm_span(span);
            }
        }
    }
    Ok(request.encode_to_vec())
}

fn normalize_codex_llm_span(span: &mut Span) {
    if span.name != "codex.session_task.turn" {
        return;
    }
    let model = attribute_string(&span.attributes, "model");
    let input_tokens = attribute_int(&span.attributes, "codex.turn.token_usage.input_tokens");
    let output_tokens = attribute_int(&span.attributes, "codex.turn.token_usage.output_tokens");
    let cached_tokens = attribute_int(
        &span.attributes,
        "codex.turn.token_usage.cached_input_tokens",
    );
    let reasoning_tokens = attribute_int(
        &span.attributes,
        "codex.turn.token_usage.reasoning_output_tokens",
    );
    let total_tokens = attribute_int(&span.attributes, "codex.turn.token_usage.total_tokens");

    if let Some(metadata) = OTLP_TRACE_METADATA.get() {
        apply_laminar_trace_metadata(span, metadata);
    }
    set_attribute_string(&mut span.attributes, "gen_ai.operation.name", "chat");
    set_attribute_string(&mut span.attributes, "gen_ai.system", "openai");
    set_attribute_string(&mut span.attributes, "gen_ai.request.model", &model);
    set_attribute_string(&mut span.attributes, "gen_ai.response.model", &model);
    set_attribute_int(
        &mut span.attributes,
        "gen_ai.usage.input_tokens",
        input_tokens,
    );
    set_attribute_int(
        &mut span.attributes,
        "gen_ai.usage.output_tokens",
        output_tokens,
    );
    set_attribute_int(
        &mut span.attributes,
        "gen_ai.usage.cache_read_input_tokens",
        cached_tokens,
    );
    set_attribute_int(
        &mut span.attributes,
        "gen_ai.usage.reasoning_tokens",
        reasoning_tokens,
    );
    set_attribute_int(
        &mut span.attributes,
        "gen_ai.usage.total_tokens",
        total_tokens,
    );
}

pub(crate) fn apply_laminar_trace_metadata(span: &mut Span, metadata: &BTreeMap<String, Value>) {
    for (key, value) in metadata {
        let key = key.trim();
        if !key.is_empty() {
            set_attribute_json(
                &mut span.attributes,
                &format!("{LAMINAR_METADATA_PREFIX}{key}"),
                value,
            );
        }
    }
}

fn attribute_string(attributes: &[KeyValue], key: &str) -> String {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref())
        .and_then(|value| match value.value.as_ref()? {
            any_value::Value::StringValue(value) => Some(value.clone()),
            any_value::Value::IntValue(value) => Some(value.to_string()),
            any_value::Value::DoubleValue(value) => Some(value.to_string()),
            any_value::Value::BoolValue(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn attribute_int(attributes: &[KeyValue], key: &str) -> Option<i64> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| attribute.value.as_ref())
        .and_then(|value| match value.value.as_ref()? {
            any_value::Value::IntValue(value) => Some(*value),
            any_value::Value::DoubleValue(value) => Some(*value as i64),
            any_value::Value::StringValue(value) => value.parse().ok(),
            _ => None,
        })
}

fn set_attribute_string(attributes: &mut Vec<KeyValue>, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    set_attribute_value(
        attributes,
        key,
        AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        },
    );
}

fn set_attribute_int(attributes: &mut Vec<KeyValue>, key: &str, value: Option<i64>) {
    let Some(value) = value else {
        return;
    };
    set_attribute_value(
        attributes,
        key,
        AnyValue {
            value: Some(any_value::Value::IntValue(value)),
        },
    );
}

fn set_attribute_double(attributes: &mut Vec<KeyValue>, key: &str, value: f64) {
    if !value.is_finite() {
        return;
    }
    set_attribute_value(
        attributes,
        key,
        AnyValue {
            value: Some(any_value::Value::DoubleValue(value)),
        },
    );
}

fn set_attribute_bool(attributes: &mut Vec<KeyValue>, key: &str, value: bool) {
    set_attribute_value(
        attributes,
        key,
        AnyValue {
            value: Some(any_value::Value::BoolValue(value)),
        },
    );
}

fn set_attribute_json(attributes: &mut Vec<KeyValue>, key: &str, value: &Value) {
    let any_value = match value {
        Value::Bool(value) => AnyValue {
            value: Some(any_value::Value::BoolValue(*value)),
        },
        Value::Number(value) => {
            if let Some(int) = value.as_i64() {
                AnyValue {
                    value: Some(any_value::Value::IntValue(int)),
                }
            } else if let Some(float) = value.as_f64() {
                AnyValue {
                    value: Some(any_value::Value::DoubleValue(float)),
                }
            } else {
                AnyValue {
                    value: Some(any_value::Value::StringValue(value.to_string())),
                }
            }
        }
        Value::String(value) => AnyValue {
            value: Some(any_value::Value::StringValue(value.clone())),
        },
        _ => AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        },
    };
    set_attribute_value(attributes, key, any_value);
}

fn set_attribute_value(attributes: &mut Vec<KeyValue>, key: &str, value: AnyValue) {
    if let Some(attribute) = attributes.iter_mut().find(|attribute| attribute.key == key) {
        attribute.value = Some(value);
        return;
    }
    attributes.push(KeyValue {
        key: key.to_string(),
        value: Some(value),
        ..Default::default()
    });
}

fn trace_id_from_traceparent(traceparent: &str) -> Option<String> {
    let parts = validate_traceparent(traceparent)?
        .split('-')
        .collect::<Vec<_>>();
    Uuid::parse_str(parts[1]).ok().map(|uuid| uuid.to_string())
}

fn traceparent_from_trace_id(trace_id: &str) -> Option<String> {
    let trace_hex = Uuid::parse_str(trace_id).ok()?.simple().to_string();
    if trace_hex == "0".repeat(32) {
        return None;
    }
    let span_id = Uuid::new_v4().simple().to_string()[..16].to_string();
    Some(format!("00-{trace_hex}-{span_id}-01"))
}

fn validate_traceparent(traceparent: &str) -> Option<&str> {
    let traceparent = traceparent.trim();
    let parts = traceparent.split('-').collect::<Vec<_>>();
    if parts.len() == 4
        && parts[0] == "00"
        && parts[1].len() == 32
        && parts[2].len() == 16
        && parts[1].chars().all(|char| char.is_ascii_hexdigit())
        && parts[2].chars().all(|char| char.is_ascii_hexdigit())
    {
        Some(traceparent)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans};

    #[test]
    fn config_contents_replace_otel_section() {
        let base = strip_otel_toml_sections(
            r#"model = "gpt-5.5"

[otel]
environment = "old"

[projects."/"]
trust_level = "trusted"
"#,
        );

        let config = codex_otel_config_contents(
            &base,
            "http://127.0.0.1:1234/v1/traces",
            "01234567-89ab-cdef-0123-456789abcdef",
            Some("slack:T:C:1.0"),
            Some("secret"),
            "production",
        );

        assert!(config.contains("model = \"gpt-5.5\""));
        assert!(config.contains("[projects.\"/\"]"));
        assert!(config.contains("[otel]"));
        assert!(config.contains("environment = \"production\""));
        assert!(config.contains("x-trace-id = \"01234567-89ab-cdef-0123-456789abcdef\""));
        assert!(config.contains("x-centaur-thread-key = \"slack:T:C:1.0\""));
        assert!(config.contains("authorization = \"Bearer secret\""));
        assert!(!config.contains("environment = \"old\""));
    }

    #[test]
    fn explicit_thread_trace_id_wins_over_foreign_traceparent() {
        let thread_trace_id = "01234567-89ab-cdef-0123-456789abcdef";
        let execution_traceparent = "00-fedcba9876543210fedcba9876543210-0123456789abcdef-01";
        let trace = TraceContext {
            thread_key: Some("slack:T:C:1.0".to_string()),
            trace_id: Some(thread_trace_id.to_string()),
            traceparent: Some(execution_traceparent.to_string()),
            metadata: BTreeMap::new(),
        };

        assert_eq!(trace.effective_trace_id().as_deref(), Some(thread_trace_id));
        let effective_traceparent = trace.effective_traceparent().expect("traceparent");
        assert_ne!(effective_traceparent, execution_traceparent);
        assert_eq!(
            trace_id_from_traceparent(&effective_traceparent).as_deref(),
            Some(thread_trace_id)
        );
    }

    #[test]
    fn rewrite_otlp_trace_payload_prefixes_and_normalizes_codex_turn_span() {
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "session_task.turn".to_string(),
                        attributes: vec![
                            kv_string("model", "gpt-5.5"),
                            kv_int("codex.turn.token_usage.input_tokens", 10),
                            kv_int("codex.turn.token_usage.output_tokens", 20),
                            kv_int("codex.turn.token_usage.cached_input_tokens", 7),
                            kv_int("codex.turn.token_usage.reasoning_output_tokens", 3),
                            kv_int("codex.turn.token_usage.total_tokens", 30),
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let rewritten = rewrite_otlp_trace_payload(&request.encode_to_vec()).expect("rewrite");
        let decoded = ExportTraceServiceRequest::decode(rewritten.as_slice()).expect("decode");
        let span = &decoded.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(span.name, "codex.session_task.turn");
        assert_eq!(
            attribute_string(&span.attributes, "gen_ai.request.model"),
            "gpt-5.5"
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.input_tokens"),
            Some(10)
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.output_tokens"),
            Some(20)
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.cache_read_input_tokens"),
            Some(7)
        );
    }

    #[test]
    fn harness_usage_trace_request_builds_laminar_priced_span() {
        let trace = TraceContext {
            thread_key: Some("slack:C123:123.456".to_string()),
            trace_id: None,
            traceparent: Some(
                "00-0123456789abcdef0123456789abcdef-1111111111111111-01".to_string(),
            ),
            metadata: BTreeMap::from([(
                "execution_id".to_string(),
                Value::String("exe_123".to_string()),
            )]),
        };
        let usage = NormalizedTokenUsage {
            model: Some("claude-fable-5".to_string()),
            input_tokens: Some(2),
            output_tokens: Some(7),
            cache_creation_input_tokens: Some(3),
            cache_read_input_tokens: Some(5),
            reasoning_output_tokens: None,
            total_tokens: None,
        };
        let request = harness_usage_trace_request(
            &trace,
            HarnessUsageSpan {
                harness: HarnessKind::ClaudeCode,
                model: "fallback-model",
                model_provider: "anthropic",
                turn_id: "turn-1",
                input: Some("say hi"),
                output: Some("hi there"),
                start_unix_nano: 100,
                end_unix_nano: 200,
            },
            &usage,
        )
        .expect("usage trace request");
        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(span.name, "claude.session_task.turn");
        assert_eq!(span.trace_id.len(), 16);
        assert_eq!(span.parent_span_id.len(), 8);
        assert_eq!(
            attribute_string(&span.attributes, "gen_ai.system"),
            "anthropic"
        );
        assert_eq!(
            attribute_string(&span.attributes, "gen_ai.response.model"),
            "claude-fable-5"
        );
        assert_eq!(attribute_string(&span.attributes, "lmnr.span.type"), "LLM");
        assert_eq!(attribute_string(&span.attributes, "input.value"), "say hi");
        assert_eq!(
            serde_json::from_str::<Value>(&attribute_string(
                &span.attributes,
                "gen_ai.input.messages"
            ))
            .expect("input messages JSON"),
            json!([{
                "role": "user",
                "parts": [{ "type": "text", "content": "say hi" }]
            }])
        );
        assert_eq!(
            attribute_string(&span.attributes, "output.value"),
            "hi there"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&attribute_string(
                &span.attributes,
                "gen_ai.output.messages"
            ))
            .expect("output messages JSON"),
            json!([{
                "role": "assistant",
                "parts": [{ "type": "text", "content": "hi there" }]
            }])
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.input_tokens"),
            Some(2)
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.cache_creation_input_tokens"),
            Some(3)
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.cache_read_input_tokens"),
            Some(5)
        );
        assert_eq!(
            attribute_int(&span.attributes, "gen_ai.usage.total_tokens"),
            Some(17)
        );
        assert_eq!(
            attribute_double(&span.attributes, "gen_ai.usage.input_cost"),
            Some(0.0000625)
        );
        assert_eq!(
            attribute_double(&span.attributes, "gen_ai.usage.output_cost"),
            Some(0.00035)
        );
        assert_eq!(
            attribute_double(&span.attributes, "gen_ai.usage.cost"),
            Some(0.0004125)
        );
        assert_eq!(
            attribute_double(&span.attributes, "centaur.usage.estimated_cost_usd"),
            Some(0.0004125)
        );
        assert_eq!(
            attribute_string(&span.attributes, "gen_ai.usage.cost_currency"),
            "USD"
        );
        assert_eq!(
            attribute_string(&span.attributes, "centaur.usage.cost_source"),
            "centaur_estimate:anthropic:fable-mythos-5:5m-cache-write"
        );
        assert_eq!(
            attribute_string(
                &span.attributes,
                "lmnr.association.properties.metadata.execution_id"
            ),
            "exe_123"
        );
    }

    #[test]
    fn harness_usage_trace_request_uses_matching_traceparent_for_api_parentage() {
        let thread_trace_id = "01234567-89ab-cdef-0123-456789abcdef";
        let trace = TraceContext {
            thread_key: Some("slack:C123:123.456".to_string()),
            trace_id: Some(thread_trace_id.to_string()),
            traceparent: Some(
                "00-0123456789abcdef0123456789abcdef-1111111111111111-01".to_string(),
            ),
            metadata: BTreeMap::new(),
        };
        let usage = NormalizedTokenUsage {
            model: Some("claude-opus-4-8".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(2),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            reasoning_output_tokens: None,
            total_tokens: None,
        };
        let request = harness_usage_trace_request(
            &trace,
            HarnessUsageSpan {
                harness: HarnessKind::ClaudeCode,
                model: "fallback-model",
                model_provider: "anthropic",
                turn_id: "turn-1",
                input: None,
                output: None,
                start_unix_nano: 100,
                end_unix_nano: 200,
            },
            &usage,
        )
        .expect("usage trace request");
        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(
            span.trace_id,
            Uuid::parse_str(thread_trace_id)
                .expect("thread trace uuid")
                .as_bytes()
                .to_vec()
        );
        assert_eq!(
            span.parent_span_id,
            vec![0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11]
        );
    }

    #[test]
    fn harness_usage_trace_request_ignores_foreign_traceparent_parentage() {
        let thread_trace_id = "01234567-89ab-cdef-0123-456789abcdef";
        let trace = TraceContext {
            thread_key: Some("slack:C123:123.456".to_string()),
            trace_id: Some(thread_trace_id.to_string()),
            traceparent: Some(
                "00-fedcba9876543210fedcba9876543210-1111111111111111-01".to_string(),
            ),
            metadata: BTreeMap::new(),
        };
        let usage = NormalizedTokenUsage {
            model: Some("claude-opus-4-8".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(2),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            reasoning_output_tokens: None,
            total_tokens: None,
        };
        let request = harness_usage_trace_request(
            &trace,
            HarnessUsageSpan {
                harness: HarnessKind::ClaudeCode,
                model: "fallback-model",
                model_provider: "anthropic",
                turn_id: "turn-1",
                input: None,
                output: None,
                start_unix_nano: 100,
                end_unix_nano: 200,
            },
            &usage,
        )
        .expect("usage trace request");
        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(
            span.trace_id,
            Uuid::parse_str(thread_trace_id)
                .expect("thread trace uuid")
                .as_bytes()
                .to_vec()
        );
        assert!(span.parent_span_id.is_empty());
    }

    #[test]
    fn harness_usage_trace_request_falls_back_to_explicit_thread_trace_id() {
        let thread_trace_id = "01234567-89ab-cdef-0123-456789abcdef";
        let trace = TraceContext {
            thread_key: Some("slack:C123:123.456".to_string()),
            trace_id: Some(thread_trace_id.to_string()),
            traceparent: None,
            metadata: BTreeMap::new(),
        };
        let usage = NormalizedTokenUsage {
            model: Some("claude-opus-4-8".to_string()),
            input_tokens: Some(10),
            output_tokens: Some(2),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            reasoning_output_tokens: None,
            total_tokens: None,
        };
        let request = harness_usage_trace_request(
            &trace,
            HarnessUsageSpan {
                harness: HarnessKind::ClaudeCode,
                model: "fallback-model",
                model_provider: "anthropic",
                turn_id: "turn-1",
                input: None,
                output: None,
                start_unix_nano: 100,
                end_unix_nano: 200,
            },
            &usage,
        )
        .expect("usage trace request");
        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(
            span.trace_id,
            Uuid::parse_str(thread_trace_id)
                .expect("thread trace uuid")
                .as_bytes()
                .to_vec()
        );
        assert!(span.parent_span_id.is_empty());
    }

    #[test]
    fn harness_usage_trace_request_estimates_amp_deep_cost() {
        let trace = TraceContext {
            thread_key: Some("slack:C123:123.456".to_string()),
            trace_id: None,
            traceparent: Some(
                "00-0123456789abcdef0123456789abcdef-1111111111111111-01".to_string(),
            ),
            metadata: BTreeMap::new(),
        };
        let usage = NormalizedTokenUsage {
            model: Some("deep".to_string()),
            input_tokens: Some(150_681),
            output_tokens: Some(1_456),
            cache_creation_input_tokens: Some(27_289),
            cache_read_input_tokens: Some(123_392),
            reasoning_output_tokens: None,
            total_tokens: Some(152_137),
        };
        let request = harness_usage_trace_request(
            &trace,
            HarnessUsageSpan {
                harness: HarnessKind::Amp,
                model: "deep",
                model_provider: "amp",
                turn_id: "turn-1",
                input: None,
                output: None,
                start_unix_nano: 100,
                end_unix_nano: 200,
            },
            &usage,
        )
        .expect("usage trace request");
        let span = &request.resource_spans[0].scope_spans[0].spans[0];

        assert_eq!(span.name, "amp.session_task.turn");
        assert_eq!(attribute_string(&span.attributes, "gen_ai.system"), "amp");
        assert_eq!(
            attribute_double(&span.attributes, "gen_ai.usage.input_cost"),
            Some(0.198141)
        );
        assert_eq!(
            attribute_double(&span.attributes, "gen_ai.usage.output_cost"),
            Some(0.04368)
        );
        assert_eq!(
            attribute_double(&span.attributes, "gen_ai.usage.cost"),
            Some(0.241821)
        );
        assert_eq!(
            attribute_double(&span.attributes, "centaur.usage.estimated_cost_usd"),
            Some(0.241821)
        );
        assert_eq!(
            attribute_string(&span.attributes, "centaur.usage.cost_source"),
            "centaur_estimate:amp:gpt-5.5"
        );
    }

    fn kv_string(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.to_string())),
            }),
            ..Default::default()
        }
    }

    fn kv_int(key: &str, value: i64) -> KeyValue {
        KeyValue {
            key: key.to_string(),
            value: Some(AnyValue {
                value: Some(any_value::Value::IntValue(value)),
            }),
            ..Default::default()
        }
    }

    fn attribute_double(attributes: &[KeyValue], key: &str) -> Option<f64> {
        attributes
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| attribute.value.as_ref())
            .and_then(|value| match value.value.as_ref()? {
                any_value::Value::DoubleValue(value) => Some(*value),
                any_value::Value::IntValue(value) => Some(*value as f64),
                any_value::Value::StringValue(value) => value.parse().ok(),
                _ => None,
            })
    }
}
