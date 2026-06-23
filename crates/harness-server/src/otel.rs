use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::trace::v1::Span;
use prost::Message as _;
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::{HarnessServerError, Result};

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
        self.traceparent
            .as_deref()
            .and_then(trace_id_from_traceparent)
            .or_else(|| self.trace_id.clone())
            .or_else(|| clean_optional(env::var("CENTAUR_TRACE_ID").ok().as_deref()))
    }

    pub(crate) fn effective_traceparent(&self) -> Option<String> {
        self.traceparent
            .as_deref()
            .and_then(|value| validate_traceparent(value).map(str::to_owned))
            .or_else(|| {
                self.effective_trace_id()
                    .and_then(|trace_id| traceparent_from_trace_id(&trace_id))
            })
    }
}

pub(crate) fn configure_codex_otel_for_startup(trace: &TraceContext) -> Result<()> {
    let Some(trace_id) = trace.effective_trace_id() else {
        return Ok(());
    };
    let Some(endpoint) = codex_otel_endpoint() else {
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

fn codex_config_path() -> Option<PathBuf> {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .map(|home| home.join("config.toml"))
}

fn codex_otel_endpoint() -> Option<String> {
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
}
