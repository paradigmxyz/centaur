use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use opentelemetry::trace::{
    Span as _, SpanBuilder, SpanContext, SpanId, SpanKind, Status, TraceContextExt as _,
    TraceFlags, TraceId, TraceState, TracerProvider as _,
};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider, Span as SdkSpan};
use serde_json::{Value, json};

use crate::{HarnessKind, NormalizedContent, NormalizedEvent, NormalizedTokenUsage};

const LAMINAR_METADATA_PREFIX: &str = "lmnr.association.properties.metadata.";
const MAX_SPAN_IO_BYTES: usize = 60 * 1024;
const MAX_TOOL_COMMAND_BYTES: usize = 8 * 1024;
const TRUNCATION_SUFFIX: &str = "…[truncated]";
/// Transcript capture is opt-in: LLM prompt/response text and shell commands
/// are only exported when this env var is set to a truthy value.
const TRANSCRIPT_CAPTURE_ENV: &str = "CENTAUR_TELEMETRY_CAPTURE_TRANSCRIPTS";

static TELEMETRY: OnceLock<Option<TelemetryRuntime>> = OnceLock::new();

struct TelemetryRuntime {
    _provider: SdkTracerProvider,
    tracer: SdkTracer,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceContext {
    pub(crate) thread_key: Option<String>,
    pub(crate) traceparent: Option<String>,
    pub(crate) metadata: BTreeMap<String, Value>,
}

impl TraceContext {
    pub(crate) fn effective_traceparent(&self) -> Option<String> {
        validate_traceparent(self.traceparent.as_deref()?).map(str::to_owned)
    }

    fn parent_context(&self) -> Option<Context> {
        let traceparent = validate_traceparent(self.traceparent.as_deref()?)?;
        let mut fields = traceparent.split('-');
        let _version = fields.next()?;
        let trace_id = TraceId::from_hex(fields.next()?).ok()?;
        let span_id = SpanId::from_hex(fields.next()?).ok()?;
        let flags = u8::from_str_radix(fields.next()?, 16).ok()?;
        let span_context = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::new(flags),
            true,
            TraceState::default(),
        );
        Some(Context::new().with_remote_span_context(span_context))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnStatus {
    Completed,
    Failed,
    Cancelled,
}

impl TurnStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn otel_status(self) -> Status {
        match self {
            Self::Completed => Status::Ok,
            Self::Failed => Status::error("turn failed"),
            Self::Cancelled => Status::error("turn cancelled"),
        }
    }
}

pub(crate) struct TurnTelemetry {
    tracer: Option<SdkTracer>,
    parent: Option<Context>,
    trace: TraceContext,
    harness: HarnessKind,
    model: String,
    model_provider: String,
    turn_id: String,
    input: Option<String>,
    output: SpanOutput,
    usage: Option<NormalizedTokenUsage>,
    started_at: SystemTime,
    tools: HashMap<String, ActiveTool>,
    centaur_tool_names: BTreeSet<String>,
    capture_transcripts: bool,
    observed_status: Option<TurnStatus>,
    finished: bool,
}

struct ActiveTool {
    span: SdkSpan,
}

#[derive(Clone, Debug)]
struct ToolLabels {
    kind: String,
    name: String,
    method: String,
}

#[derive(Debug)]
struct ToolCommandDetails {
    executable: String,
    command: Option<String>,
    cwd: Option<String>,
}

impl TurnTelemetry {
    pub(crate) fn new(
        trace: Option<&TraceContext>,
        harness: HarnessKind,
        model: impl Into<String>,
        model_provider: impl Into<String>,
        turn_id: impl Into<String>,
        input: Option<String>,
    ) -> Self {
        let trace = trace.cloned().unwrap_or_default();
        let parent = trace.parent_context();
        let tracer = parent.as_ref().and_then(|_| telemetry_tracer());
        Self::with_tracer(
            trace,
            parent,
            tracer,
            harness,
            model.into(),
            model_provider.into(),
            turn_id.into(),
            input,
            transcript_capture_enabled(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_tracer(
        trace: TraceContext,
        parent: Option<Context>,
        tracer: Option<SdkTracer>,
        harness: HarnessKind,
        model: String,
        model_provider: String,
        turn_id: String,
        input: Option<String>,
        capture_transcripts: bool,
    ) -> Self {
        Self {
            tracer,
            parent,
            trace,
            harness,
            model,
            model_provider,
            turn_id,
            input: if capture_transcripts {
                input.map(|value| bounded_span_value(&value))
            } else {
                None
            },
            output: SpanOutput::default(),
            usage: None,
            started_at: SystemTime::now(),
            tools: HashMap::new(),
            centaur_tool_names: load_centaur_tool_names(),
            capture_transcripts,
            observed_status: None,
            finished: false,
        }
    }

    pub(crate) fn observe_normalized(&mut self, event: &NormalizedEvent) {
        if let Some(usage) = event.token_usage() {
            self.usage = Some(usage.clone());
        }
        if !self.capture_transcripts {
            return;
        }
        match event {
            NormalizedEvent::AssistantMessage { content, .. } => {
                for item in content {
                    if let NormalizedContent::AgentText { item_id, text } = item {
                        self.output.set_item_text(item_id, text);
                    }
                }
            }
            NormalizedEvent::AgentTextDelta { item_id, delta } => {
                self.output.append_delta(item_id, delta);
            }
            NormalizedEvent::Error { message } => self.output.set_fallback_if_empty(message),
            _ => {}
        }
    }

    pub(crate) fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub(crate) fn observe_wire_value(&mut self, value: &Value) {
        self.observe_tool_notification(value);
        self.remember_turn_id(value);
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            return;
        };
        match method {
            "item/completed" | "item.completed" => {
                if self.capture_transcripts
                    && let Some(item) = protocol_item(value)
                {
                    self.observe_completed_output(item);
                }
            }
            "item/agentMessage/delta" => {
                if self.capture_transcripts
                    && let (Some(item_id), Some(delta)) = (
                        string_at_path(value, &["params", "itemId"]),
                        string_at_path(value, &["params", "delta"]),
                    )
                {
                    self.output.append_delta(&item_id, &delta);
                }
            }
            "thread/tokenUsage/updated" => self.observe_codex_usage(value),
            "turn/completed" => {
                self.observed_status = value
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .and_then(|status| match status {
                        "completed" => Some(TurnStatus::Completed),
                        "interrupted" | "cancelled" => Some(TurnStatus::Cancelled),
                        "failed" => Some(TurnStatus::Failed),
                        _ => None,
                    });
            }
            "turn/failed" | "error" => self.observed_status = Some(TurnStatus::Failed),
            "thread/status/changed"
                if value.pointer("/params/status/type").and_then(Value::as_str)
                    == Some("systemError") =>
            {
                self.observed_status = Some(TurnStatus::Failed);
            }
            _ => {}
        }
    }

    pub(crate) fn observe_tool_notification(&mut self, value: &Value) {
        self.remember_turn_id(value);
        match value.get("method").and_then(Value::as_str) {
            Some("item/started" | "item.started") => {
                if let Some(item) = protocol_item(value)
                    && let Some(id) = string_at_path(item, &["id"])
                    && let Some(labels) = tool_labels_from_item(item, &self.centaur_tool_names)
                {
                    let command = tool_command_details(item, self.capture_transcripts);
                    self.start_tool(id, labels, command);
                }
            }
            Some("item/completed" | "item.completed") => {
                if let Some(item) = protocol_item(value) {
                    self.finish_completed_tool(item);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn finish(&mut self, status: TurnStatus) {
        if self.finished {
            return;
        }
        self.finished = true;
        let status = self.observed_status.unwrap_or(status);

        let dangling_status = match status {
            TurnStatus::Cancelled => "cancelled",
            TurnStatus::Completed | TurnStatus::Failed => "failed",
        };
        for (_id, mut tool) in std::mem::take(&mut self.tools) {
            finish_tool_span(&mut tool.span, dangling_status);
        }
        self.finish_usage_span(status);
    }

    fn remember_turn_id(&mut self, value: &Value) {
        let turn_id = [
            &["params", "turnId"][..],
            &["params", "turn", "id"][..],
            &["result", "turn", "id"][..],
        ]
        .into_iter()
        .find_map(|path| string_at_path(value, path));
        if let Some(turn_id) = turn_id {
            self.turn_id = turn_id;
        }
    }

    fn start_tool(&mut self, id: String, labels: ToolLabels, command: Option<ToolCommandDetails>) {
        let (Some(tracer), Some(parent)) = (self.tracer.as_ref(), self.parent.as_ref()) else {
            return;
        };
        if let Some(mut previous) = self.tools.remove(&id) {
            finish_tool_span(&mut previous.span, "failed");
        }
        let mut span_input = json!({
            "kind": labels.kind,
            "name": labels.name,
            "method": labels.method,
        });
        let mut attributes = self.common_attributes();
        attributes.extend([
            KeyValue::new("lmnr.span.type", "TOOL"),
            KeyValue::new("tool.kind", labels.kind.clone()),
            KeyValue::new("tool.name", labels.name.clone()),
            KeyValue::new("tool.method", labels.method.clone()),
        ]);
        if let Some(command) = command {
            attributes.push(KeyValue::new("tool.executable", command.executable.clone()));
            span_input["executable"] = Value::String(command.executable);
            if let Some(value) = command.command {
                attributes.push(KeyValue::new("tool.command", value.clone()));
                span_input["command"] = Value::String(value);
            }
            if let Some(value) = command.cwd {
                attributes.push(KeyValue::new("tool.cwd", value.clone()));
                span_input["cwd"] = Value::String(value);
            }
        }
        attributes.push(KeyValue::new("lmnr.span.input", span_input.to_string()));
        let span = SpanBuilder::from_name(format!(
            "{}.tool.{}",
            harness_name(self.harness),
            labels.name
        ))
        .with_kind(SpanKind::Internal)
        .with_attributes(attributes)
        .start_with_context(tracer, parent);
        self.tools.insert(id, ActiveTool { span });
    }

    fn observe_completed_output(&mut self, item: &Value) {
        if matches!(
            item.get("type").and_then(Value::as_str),
            Some("agentMessage" | "agent_message")
        ) && let Some(item_id) = string_at_path(item, &["id"])
            && let Some(text) = string_at_path(item, &["text"])
        {
            self.output.set_item_text(&item_id, &text);
        }
    }

    fn finish_completed_tool(&mut self, item: &Value) {
        let Some(id) = string_at_path(item, &["id"]) else {
            return;
        };
        let status = completed_tool_status(item);
        if let Some(mut tool) = self.tools.remove(&id) {
            finish_tool_span(&mut tool.span, status);
            return;
        }
        let Some(labels) = tool_labels_from_item(item, &self.centaur_tool_names) else {
            return;
        };
        let command = tool_command_details(item, self.capture_transcripts);
        self.start_tool(id.clone(), labels, command);
        if let Some(mut tool) = self.tools.remove(&id) {
            finish_tool_span(&mut tool.span, status);
        }
    }

    fn observe_codex_usage(&mut self, value: &Value) {
        let usage = value
            .pointer("/params/tokenUsage/last")
            .or_else(|| value.pointer("/params/token_usage/last"))
            .and_then(normalized_usage_from_value);
        let Some(usage) = usage else {
            return;
        };
        if let Some(total) = self.usage.as_mut() {
            add_usage(total, &usage);
        } else {
            self.usage = Some(usage);
        }
    }

    fn finish_usage_span(&mut self, status: TurnStatus) {
        let (Some(tracer), Some(parent), Some(usage)) = (
            self.tracer.as_ref(),
            self.parent.as_ref(),
            self.usage.as_ref(),
        ) else {
            return;
        };
        if !usage.has_counts() {
            return;
        }

        let harness = harness_name(self.harness);
        let system = gen_ai_system(self.harness, &self.model_provider);
        let model = usage
            .model
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.model);
        let mut attributes = self.common_attributes();
        attributes.extend([
            KeyValue::new("lmnr.span.type", "LLM"),
            KeyValue::new("gen_ai.operation.name", "chat"),
            KeyValue::new("gen_ai.system", system),
            KeyValue::new("gen_ai.request.model", model.to_owned()),
            KeyValue::new("gen_ai.response.model", model.to_owned()),
            KeyValue::new("centaur.harness", harness),
            KeyValue::new("centaur.model_provider", self.model_provider.clone()),
            KeyValue::new("centaur.turn_id", self.turn_id.clone()),
            KeyValue::new("centaur.turn_status", status.as_str()),
        ]);
        add_usage_attributes(&mut attributes, usage);
        add_io_attributes(
            &mut attributes,
            self.input.as_deref(),
            self.output.value().as_deref(),
        );
        if let Some(cost) = estimate_usage_cost(self.harness, system, model, usage) {
            attributes.extend([
                KeyValue::new("gen_ai.usage.input_cost", cost.input_cost),
                KeyValue::new("gen_ai.usage.output_cost", cost.output_cost),
                KeyValue::new("gen_ai.usage.cost", cost.total_cost()),
                KeyValue::new("gen_ai.usage.cost_currency", "USD"),
                KeyValue::new("centaur.usage.input_cost_usd", cost.input_cost),
                KeyValue::new("centaur.usage.output_cost_usd", cost.output_cost),
                KeyValue::new("centaur.usage.estimated_cost_usd", cost.total_cost()),
                KeyValue::new("centaur.usage.cost_source", cost.source),
                KeyValue::new("centaur.usage.cost_estimated", true),
            ]);
        }

        let mut span = SpanBuilder::from_name(format!("{harness}.session_task.turn"))
            .with_kind(SpanKind::Internal)
            .with_start_time(self.started_at)
            .with_attributes(attributes)
            .start_with_context(tracer, parent);
        span.set_status(status.otel_status());
        span.end();
    }

    fn common_attributes(&self) -> Vec<KeyValue> {
        let mut attributes = Vec::new();
        if let Some(thread_key) = self.trace.thread_key.as_deref() {
            attributes.push(KeyValue::new("centaur.thread_key", thread_key.to_owned()));
            attributes.push(KeyValue::new(
                "lmnr.association.properties.session_id",
                thread_key.to_owned(),
            ));
        }
        if let Some(execution_id) = self
            .trace
            .metadata
            .get("execution_id")
            .and_then(Value::as_str)
        {
            attributes.push(KeyValue::new(
                "centaur.execution_id",
                execution_id.to_owned(),
            ));
            attributes.push(KeyValue::new(
                format!("{LAMINAR_METADATA_PREFIX}execution_id"),
                execution_id.to_owned(),
            ));
        }
        attributes
    }
}

impl Drop for TurnTelemetry {
    fn drop(&mut self) {
        self.finish(TurnStatus::Failed);
    }
}

fn telemetry_tracer() -> Option<SdkTracer> {
    TELEMETRY
        .get_or_init(|| match build_telemetry_runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("failed to initialize harness OTLP exporter: {error}");
                None
            }
        })
        .as_ref()
        .map(|runtime| runtime.tracer.clone())
}

fn build_telemetry_runtime() -> Result<Option<TelemetryRuntime>, String> {
    if traces_export_disabled() || otlp_traces_endpoint().is_none() {
        return Ok(None);
    }
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()
        .map_err(|error| error.to_string())?;
    let provider = SdkTracerProvider::builder()
        .with_resource(
            Resource::builder()
                .with_service_name("harness-server")
                .with_attribute(KeyValue::new("deployment.environment", otel_environment()))
                .build(),
        )
        .with_batch_exporter(exporter)
        .build();
    let tracer = provider.tracer("centaur.harness-server");
    Ok(Some(TelemetryRuntime {
        _provider: provider,
        tracer,
    }))
}

fn traces_export_disabled() -> bool {
    matches!(
        env::var("OTEL_TRACES_EXPORTER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "none" | "false" | "0" | "off"
    )
}

fn transcript_capture_enabled() -> bool {
    env::var(TRANSCRIPT_CAPTURE_ENV)
        .ok()
        .as_deref()
        .is_some_and(truthy_env_value)
}

fn truthy_env_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn otlp_traces_endpoint() -> Option<String> {
    clean_optional(
        env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .ok()
            .as_deref(),
    )
    .or_else(|| clean_optional(env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok().as_deref()))
}

fn otel_environment() -> String {
    if let Ok(raw) = env::var("OTEL_RESOURCE_ATTRIBUTES") {
        for item in raw.split(',') {
            let Some((key, value)) = item.split_once('=') else {
                continue;
            };
            if matches!(
                key.trim(),
                "deployment.environment" | "deployment.environment.name"
            ) && let Some(value) = clean_optional(Some(value))
            {
                return value;
            }
        }
    }
    clean_optional(env::var("DEPLOY_ENV").ok().as_deref())
        .or_else(|| clean_optional(env::var("ENVIRONMENT").ok().as_deref()))
        .unwrap_or_else(|| "dev".to_string())
}

fn finish_tool_span(span: &mut SdkSpan, status: &str) {
    span.set_attribute(KeyValue::new("tool.status", status.to_owned()));
    span.set_attribute(KeyValue::new(
        "lmnr.span.output",
        json!({ "status": status }).to_string(),
    ));
    span.set_status(if status == "completed" {
        Status::Ok
    } else {
        Status::error(format!("tool {status}"))
    });
    span.end();
}

fn add_usage_attributes(attributes: &mut Vec<KeyValue>, usage: &NormalizedTokenUsage) {
    push_i64(attributes, "gen_ai.usage.input_tokens", usage.input_tokens);
    push_i64(
        attributes,
        "gen_ai.usage.output_tokens",
        usage.output_tokens,
    );
    push_i64(
        attributes,
        "gen_ai.usage.cache_creation_input_tokens",
        usage.cache_creation_input_tokens,
    );
    push_i64(
        attributes,
        "gen_ai.usage.cache_read_input_tokens",
        usage.cache_read_input_tokens,
    );
    push_i64(
        attributes,
        "gen_ai.usage.reasoning_tokens",
        usage.reasoning_output_tokens,
    );
    let total = usage.total_tokens.or_else(|| {
        [
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            usage.reasoning_output_tokens,
        ]
        .into_iter()
        .flatten()
        .try_fold(0_i64, i64::checked_add)
    });
    push_i64(attributes, "gen_ai.usage.total_tokens", total);
}

fn push_i64(attributes: &mut Vec<KeyValue>, key: &'static str, value: Option<i64>) {
    if let Some(value) = value {
        attributes.push(KeyValue::new(key, value));
    }
}

fn add_io_attributes(attributes: &mut Vec<KeyValue>, input: Option<&str>, output: Option<&str>) {
    if let Some(input) = clean_optional(input) {
        let input = bounded_span_value(&input);
        attributes.extend([
            KeyValue::new("input.value", input.clone()),
            KeyValue::new("lmnr.span.input", legacy_chat_message_json("user", &input)),
            KeyValue::new("gen_ai.input.messages", gen_ai_message_json("user", &input)),
        ]);
    }
    if let Some(output) = clean_optional(output) {
        let output = bounded_span_value(&output);
        attributes.extend([
            KeyValue::new("output.value", output.clone()),
            KeyValue::new(
                "lmnr.span.output",
                legacy_chat_message_json("assistant", &output),
            ),
            KeyValue::new(
                "gen_ai.output.messages",
                gen_ai_message_json("assistant", &output),
            ),
        ]);
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

fn normalized_usage_from_value(value: &Value) -> Option<NormalizedTokenUsage> {
    let usage = NormalizedTokenUsage {
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: integer_at(value, &["inputTokens", "input_tokens"]),
        output_tokens: integer_at(value, &["outputTokens", "output_tokens"]),
        cache_creation_input_tokens: integer_at(
            value,
            &["cacheWriteInputTokens", "cache_write_input_tokens"],
        ),
        cache_read_input_tokens: integer_at(value, &["cachedInputTokens", "cached_input_tokens"]),
        reasoning_output_tokens: integer_at(
            value,
            &["reasoningOutputTokens", "reasoning_output_tokens"],
        ),
        total_tokens: integer_at(value, &["totalTokens", "total_tokens"]),
    };
    usage.has_counts().then_some(usage)
}

fn integer_at(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| value.get(*key)?.as_i64())
}

fn add_usage(total: &mut NormalizedTokenUsage, next: &NormalizedTokenUsage) {
    total.model = next.model.clone().or_else(|| total.model.clone());
    total.input_tokens = add_optional(total.input_tokens, next.input_tokens);
    total.output_tokens = add_optional(total.output_tokens, next.output_tokens);
    total.cache_creation_input_tokens = add_optional(
        total.cache_creation_input_tokens,
        next.cache_creation_input_tokens,
    );
    total.cache_read_input_tokens =
        add_optional(total.cache_read_input_tokens, next.cache_read_input_tokens);
    total.reasoning_output_tokens =
        add_optional(total.reasoning_output_tokens, next.reasoning_output_tokens);
    total.total_tokens = add_optional(total.total_tokens, next.total_tokens);
}

fn add_optional(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => left.checked_add(right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn protocol_item(value: &Value) -> Option<&Value> {
    value
        .get("params")
        .and_then(|params| params.get("item"))
        .or_else(|| value.get("item"))
}

fn tool_labels_from_item(
    item: &Value,
    centaur_tool_names: &BTreeSet<String>,
) -> Option<ToolLabels> {
    let item_type = string_at_path(item, &["type"])?;
    match item_type.as_str() {
        "mcpToolCall" | "mcp_tool_call" => Some(ToolLabels {
            kind: "mcp".to_owned(),
            name: string_at_path(item, &["tool"]).unwrap_or_else(|| "unknown".to_owned()),
            method: string_at_path(item, &["server"]).unwrap_or_else(|| "call".to_owned()),
        }),
        "dynamicToolCall" | "dynamic_tool_call" => Some(ToolLabels {
            kind: "dynamic".to_owned(),
            name: string_at_path(item, &["tool"]).unwrap_or_else(|| "unknown".to_owned()),
            method: string_at_path(item, &["namespace"]).unwrap_or_else(|| "call".to_owned()),
        }),
        "collabAgentToolCall" | "collab_agent_tool_call" => Some(ToolLabels {
            kind: "collab_agent".to_owned(),
            name: string_at_path(item, &["tool"]).unwrap_or_else(|| "agent".to_owned()),
            method: "call".to_owned(),
        }),
        "commandExecution" | "command_execution" => {
            Some(centaur_tool_labels(item, centaur_tool_names).unwrap_or_else(command_tool_labels))
        }
        _ => None,
    }
}

fn command_tool_labels() -> ToolLabels {
    ToolLabels {
        kind: "command".to_owned(),
        name: "command_execution".to_owned(),
        method: "shell".to_owned(),
    }
}

fn centaur_tool_labels(item: &Value, centaur_tool_names: &BTreeSet<String>) -> Option<ToolLabels> {
    let command = string_at_path(item, &["command"])?;
    let words = unwrap_shell_words(&command)?;
    let executable = executable_name(words.first()?);

    let (name, method) = if executable == "centaur-tools" {
        match words.get(1).map(String::as_str) {
            Some("call") => (words.get(2)?, words.get(3).map_or("call", String::as_str)),
            Some("run") => (words.get(2)?, "cli"),
            _ => return None,
        }
    } else {
        (words.first()?, "cli")
    };
    let name = executable_name(name);
    if !centaur_tool_names.contains(name) {
        return None;
    }

    Some(ToolLabels {
        kind: "centaur".to_owned(),
        name: name.to_owned(),
        method: method.to_owned(),
    })
}

fn tool_command_details(item: &Value, capture_transcripts: bool) -> Option<ToolCommandDetails> {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("commandExecution" | "command_execution")
    ) {
        return None;
    }
    let raw_command = string_at_path(item, &["command"])?;
    let command = unwrap_shell_command(&raw_command);
    let words = shell_words::split(&command).ok()?;
    let executable = executable_name(words.first()?).to_owned();
    Some(ToolCommandDetails {
        executable,
        command: capture_transcripts.then(|| bounded_tool_command(&command)),
        cwd: if capture_transcripts {
            workspace_relative_cwd(item)
        } else {
            None
        },
    })
}

fn unwrap_shell_command(command: &str) -> String {
    let Ok(words) = shell_words::split(command) else {
        return command.trim().to_owned();
    };
    let executable = words.first().map(|word| executable_name(word));
    if matches!(executable, Some("bash" | "sh" | "zsh"))
        && matches!(words.get(1).map(String::as_str), Some("-c" | "-lc"))
        && let Some(inner) = words.get(2)
    {
        return inner.trim().to_owned();
    }
    command.trim().to_owned()
}

fn workspace_relative_cwd(item: &Value) -> Option<String> {
    let cwd = string_at_path(item, &["cwd"])?;
    let cwd = Path::new(&cwd);
    let workspace = env::var_os("CENTAUR_WORKSPACE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())?;
    let relative = cwd.strip_prefix(workspace).ok()?;
    if relative.as_os_str().is_empty() {
        Some(".".to_owned())
    } else {
        Some(relative.to_string_lossy().into_owned())
    }
}

fn unwrap_shell_words(command: &str) -> Option<Vec<String>> {
    shell_words::split(&unwrap_shell_command(command)).ok()
}

fn executable_name(value: &str) -> &str {
    value.rsplit('/').next().unwrap_or(value)
}

fn load_centaur_tool_names() -> BTreeSet<String> {
    let Some(catalog_path) = centaur_tool_catalog_path() else {
        return BTreeSet::new();
    };
    let Ok(contents) = fs::read_to_string(catalog_path) else {
        return BTreeSet::new();
    };
    let Ok(entries) = serde_json::from_str::<Vec<Value>>(&contents) else {
        return BTreeSet::new();
    };
    entries
        .iter()
        .filter_map(|entry| string_at_path(entry, &["name"]))
        .collect()
}

fn centaur_tool_catalog_path() -> Option<PathBuf> {
    if let Some(bin_dir) = env::var_os("CENTAUR_TOOL_BIN_DIR") {
        return Some(PathBuf::from(bin_dir).join(".centaur-tools.json"));
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/bin/.centaur-tools.json"))
}

fn completed_tool_status(item: &Value) -> &'static str {
    if item
        .get("success")
        .and_then(Value::as_bool)
        .is_some_and(|success| !success)
        || item.get("error").is_some()
    {
        return "failed";
    }
    if let Some(exit_code) = item.get("exitCode").and_then(Value::as_i64) {
        return if exit_code == 0 {
            "completed"
        } else {
            "failed"
        };
    }
    match item
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
    {
        "failed" | "error" | "cancelled" | "declined" => "failed",
        _ => "completed",
    }
}

fn string_at_path(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    let text = current.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn validate_traceparent(traceparent: &str) -> Option<&str> {
    let traceparent = traceparent.trim();
    let parts = traceparent.split('-').collect::<Vec<_>>();
    if parts.len() == 4
        && parts[0].len() == 2
        && parts[1].len() == 32
        && parts[2].len() == 16
        && parts[3].len() == 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && parts[1] != "00000000000000000000000000000000"
        && parts[2] != "0000000000000000"
    {
        Some(traceparent)
    } else {
        None
    }
}

fn clean_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn bounded_span_value(value: &str) -> String {
    bounded_value(value, MAX_SPAN_IO_BYTES)
}

fn bounded_tool_command(value: &str) -> String {
    bounded_value(value, MAX_TOOL_COMMAND_BYTES)
}

fn bounded_value(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub(TRUNCATION_SUFFIX.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], TRUNCATION_SUFFIX)
}

#[derive(Debug, Default)]
struct SpanOutput {
    item_order: Vec<String>,
    text_by_item_id: HashMap<String, String>,
    fallback: Option<String>,
}

impl SpanOutput {
    fn append_delta(&mut self, item_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.remember_item(item_id);
        self.text_by_item_id
            .entry(item_id.to_owned())
            .or_default()
            .push_str(delta);
    }

    fn set_item_text(&mut self, item_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.remember_item(item_id);
        self.text_by_item_id
            .insert(item_id.to_owned(), text.to_owned());
    }

    fn set_fallback_if_empty(&mut self, value: &str) {
        if self.value().is_none() {
            self.fallback = clean_optional(Some(value));
        }
    }

    fn value(&self) -> Option<String> {
        let mut text = String::new();
        for item_id in &self.item_order {
            if let Some(item_text) = self.text_by_item_id.get(item_id) {
                text.push_str(item_text);
            }
        }
        clean_optional(Some(&text))
            .or_else(|| self.fallback.clone())
            .map(|value| bounded_span_value(&value))
    }

    fn remember_item(&mut self, item_id: &str) {
        if self.text_by_item_id.contains_key(item_id) {
            return;
        }
        self.item_order.push(item_id.to_owned());
        self.text_by_item_id
            .insert(item_id.to_owned(), String::new());
    }
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
    if model.contains("gpt-5-6-sol") {
        return Some(TokenPricing {
            input_per_mtok: 5.0,
            cache_creation_per_mtok: 6.25,
            cache_read_per_mtok: 0.5,
            output_per_mtok: 30.0,
            source: "centaur_estimate:openai:gpt-5.6-sol:standard-short-context",
        });
    }
    if model.contains("gpt-5-6-terra") {
        return Some(TokenPricing {
            input_per_mtok: 2.0,
            cache_creation_per_mtok: 2.5,
            cache_read_per_mtok: 0.2,
            output_per_mtok: 12.0,
            source: "centaur_estimate:openai:gpt-5.6-terra:standard-short-context",
        });
    }
    if model.contains("gpt-5-6-luna") {
        return Some(TokenPricing {
            input_per_mtok: 0.2,
            cache_creation_per_mtok: 0.25,
            cache_read_per_mtok: 0.02,
            output_per_mtok: 1.2,
            source: "centaur_estimate:openai:gpt-5.6-luna:standard-short-context",
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use serde_json::json;

    fn test_telemetry() -> (InMemorySpanExporter, SdkTracerProvider, SdkTracer) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("test");
        (exporter, provider, tracer)
    }

    fn trace_context() -> TraceContext {
        TraceContext {
            thread_key: Some("slack:T:C:1.0".to_owned()),
            traceparent: Some("00-0123456789abcdef0123456789abcdef-1111111111111111-01".to_owned()),
            metadata: BTreeMap::from([(
                "execution_id".to_owned(),
                Value::String("exe-1".to_owned()),
            )]),
        }
    }

    fn test_turn(tracer: SdkTracer) -> TurnTelemetry {
        test_turn_with_transcript_capture(tracer, false)
    }

    fn test_turn_with_transcript_capture(
        tracer: SdkTracer,
        capture_transcripts: bool,
    ) -> TurnTelemetry {
        let trace = trace_context();
        TurnTelemetry::with_tracer(
            trace.clone(),
            trace.parent_context(),
            Some(tracer),
            HarnessKind::Codex,
            "gpt-5.5".to_owned(),
            "openai".to_owned(),
            "turn-1".to_owned(),
            Some("hello".to_owned()),
            capture_transcripts,
        )
    }

    fn test_turn_with_centaur_tools(
        tracer: SdkTracer,
        names: impl IntoIterator<Item = &'static str>,
    ) -> TurnTelemetry {
        test_turn_with_centaur_tools_and_capture(tracer, names, false)
    }

    fn test_turn_with_centaur_tools_and_capture(
        tracer: SdkTracer,
        names: impl IntoIterator<Item = &'static str>,
        capture_transcripts: bool,
    ) -> TurnTelemetry {
        let mut turn = test_turn_with_transcript_capture(tracer, capture_transcripts);
        turn.centaur_tool_names = names.into_iter().map(str::to_owned).collect();
        turn
    }

    #[test]
    fn canonical_turn_exports_usage_and_tool_spans_under_execution_parent() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn(tracer);
        turn.observe_wire_value(&json!({
            "method": "item/started",
            "params": {"item": {
                "id": "tool-1",
                "type": "dynamicToolCall",
                "tool": "list_issues",
                "namespace": "github"
            }}
        }));
        turn.observe_wire_value(&json!({
            "method": "item/completed",
            "params": {"item": {
                "id": "tool-1",
                "type": "dynamicToolCall",
                "tool": "list_issues",
                "namespace": "github",
                "status": "completed"
            }}
        }));
        turn.observe_wire_value(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {"tokenUsage": {"last": {
                "inputTokens": 10,
                "cachedInputTokens": 4,
                "outputTokens": 5,
                "reasoningOutputTokens": 2,
                "totalTokens": 15
            }}}
        }));
        turn.finish(TurnStatus::Completed);
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        assert_eq!(spans.len(), 2);
        assert!(
            spans
                .iter()
                .any(|span| span.name == "codex.tool.list_issues")
        );
        assert!(
            spans
                .iter()
                .any(|span| span.name == "codex.session_task.turn")
        );
        for span in &spans {
            assert_eq!(
                span.span_context.trace_id().to_string(),
                "0123456789abcdef0123456789abcdef"
            );
            assert_eq!(span.parent_span_id.to_string(), "1111111111111111");
        }
        let tool = spans
            .iter()
            .find(|span| span.name == "codex.tool.list_issues")
            .expect("tool span");
        assert_eq!(attribute(tool, "lmnr.span.type").as_deref(), Some("TOOL"));
        assert_eq!(attribute(tool, "tool.status").as_deref(), Some("completed"));
        let usage = spans
            .iter()
            .find(|span| span.name == "codex.session_task.turn")
            .expect("usage span");
        assert_eq!(attribute(usage, "lmnr.span.type").as_deref(), Some("LLM"));
        assert_eq!(
            attribute(usage, "gen_ai.usage.input_tokens").as_deref(),
            Some("10")
        );
        assert_eq!(
            attribute(usage, "lmnr.association.properties.session_id").as_deref(),
            Some("slack:T:C:1.0")
        );
        for key in [
            "gen_ai.usage.input_cost",
            "gen_ai.usage.output_cost",
            "gen_ai.usage.cost",
        ] {
            let cost = attribute(usage, key)
                .unwrap_or_else(|| panic!("missing Laminar cost attribute {key}"))
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("invalid {key}: {error}"));
            assert!(cost > 0.0, "expected positive {key}");
        }
    }

    #[test]
    fn unfinished_tool_is_failed_when_turn_finishes() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn(tracer);
        turn.observe_wire_value(&json!({
            "method": "item/started",
            "params": {"item": {
                "id": "tool-1",
                "type": "commandExecution",
                "command": "redacted"
            }}
        }));
        turn.finish(TurnStatus::Cancelled);
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        assert_eq!(spans.len(), 1);
        assert_eq!(
            attribute(&spans[0], "tool.status").as_deref(),
            Some("cancelled")
        );
        assert_eq!(spans[0].status, Status::error("tool cancelled"));
    }

    #[test]
    fn centaur_cli_command_exports_the_catalog_tool_name() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn_with_centaur_tools(tracer, ["websearch"]);
        for method in ["item/started", "item/completed"] {
            turn.observe_wire_value(&json!({
                "method": method,
                "params": {"item": {
                    "id": "tool-1",
                    "type": "commandExecution",
                    "command": "/bin/bash -lc 'websearch search --query secret-value'",
                    "exitCode": 0
                }}
            }));
        }
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        assert_eq!(spans.len(), 1);
        let tool = &spans[0];
        assert_eq!(tool.name, "codex.tool.websearch");
        assert_eq!(attribute(tool, "tool.kind").as_deref(), Some("centaur"));
        assert_eq!(attribute(tool, "tool.name").as_deref(), Some("websearch"));
        assert_eq!(attribute(tool, "tool.method").as_deref(), Some("cli"));
        assert_eq!(
            attribute(tool, "tool.executable").as_deref(),
            Some("websearch")
        );
        assert_eq!(attribute(tool, "tool.command"), None);
        assert_eq!(attribute(tool, "tool.cwd"), None);
        assert!(
            !attribute(tool, "lmnr.span.input")
                .unwrap_or_default()
                .contains("secret-value")
        );
    }

    #[test]
    fn transcript_capture_exports_bounded_command_and_relative_cwd() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn_with_centaur_tools_and_capture(tracer, ["websearch"], true);
        let cwd = env::current_dir().expect("current dir").join("src");
        for method in ["item/started", "item/completed"] {
            turn.observe_wire_value(&json!({
                "method": method,
                "params": {"item": {
                    "id": "tool-1",
                    "type": "commandExecution",
                    "command": "/bin/bash -lc 'websearch search --query private-query'",
                    "cwd": cwd,
                    "exitCode": 0
                }}
            }));
        }
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        assert_eq!(spans.len(), 1);
        let tool = &spans[0];
        assert_eq!(
            attribute(tool, "tool.command").as_deref(),
            Some("websearch search --query private-query")
        );
        assert_eq!(attribute(tool, "tool.cwd").as_deref(), Some("src"));
        let input = attribute(tool, "lmnr.span.input").expect("tool input");
        assert!(input.contains("private-query"));
        assert!(input.contains("\"cwd\":\"src\""));
    }

    #[test]
    fn captured_tool_commands_are_bounded() {
        let command = format!("printf {}", "x".repeat(MAX_TOOL_COMMAND_BYTES));
        let bounded = bounded_tool_command(&command);

        assert!(bounded.ends_with(TRUNCATION_SUFFIX));
        assert!(bounded.len() <= MAX_TOOL_COMMAND_BYTES);
    }

    #[test]
    fn centaur_tools_call_exports_tool_and_method() {
        let labels = tool_labels_from_item(
            &json!({
                "type": "commandExecution",
                "command": "centaur-tools call vlogs errors '{\"query\":\"secret-value\"}'"
            }),
            &BTreeSet::from(["vlogs".to_owned()]),
        )
        .expect("labels");

        assert_eq!(labels.kind, "centaur");
        assert_eq!(labels.name, "vlogs");
        assert_eq!(labels.method, "errors");
    }

    #[test]
    fn non_catalog_command_remains_a_shell_command() {
        let labels = tool_labels_from_item(
            &json!({
                "type": "commandExecution",
                "command": "/bin/bash -lc 'gh api repos/example/project'"
            }),
            &BTreeSet::from(["websearch".to_owned()]),
        )
        .expect("labels");

        assert_eq!(labels.kind, "command");
        assert_eq!(labels.name, "command_execution");
        assert_eq!(labels.method, "shell");
    }

    #[test]
    fn codex_usage_updates_are_aggregated_per_turn() {
        let (_exporter, _provider, tracer) = test_telemetry();
        let mut turn = test_turn(tracer);
        for usage in [
            json!({"inputTokens": 10, "outputTokens": 2, "totalTokens": 12}),
            json!({"inputTokens": 20, "outputTokens": 3, "totalTokens": 23}),
        ] {
            turn.observe_wire_value(&json!({
                "method": "thread/tokenUsage/updated",
                "params": {"tokenUsage": {"last": usage}}
            }));
        }
        let usage = turn.usage.as_ref().expect("usage");
        assert_eq!(usage.input_tokens, Some(30));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(35));
    }

    #[test]
    fn resolved_codex_model_is_used_for_usage_and_cost() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = TurnTelemetry::with_tracer(
            trace_context(),
            trace_context().parent_context(),
            Some(tracer),
            HarnessKind::Codex,
            String::new(),
            "openai".to_owned(),
            "turn-1".to_owned(),
            None,
            false,
        );
        turn.set_model("gpt-5.4");
        turn.observe_wire_value(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {"tokenUsage": {"last": {
                "inputTokens": 50_000,
                "outputTokens": 1_000,
                "totalTokens": 51_000
            }}}
        }));
        turn.finish(TurnStatus::Completed);
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        let span = spans
            .iter()
            .find(|span| span.name == "codex.session_task.turn")
            .expect("turn span");
        assert_eq!(
            attribute(span, "gen_ai.response.model").as_deref(),
            Some("gpt-5.4")
        );
        let cost = attribute(span, "gen_ai.usage.cost")
            .expect("cost")
            .parse::<f64>()
            .expect("numeric cost");
        assert!(cost > 0.0);
    }

    #[test]
    fn transcript_capture_is_disabled_without_opt_in() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn(tracer);
        turn.observe_wire_value(&json!({
            "method": "item/completed",
            "params": {"item": {
                "id": "message-1",
                "type": "agentMessage",
                "text": "private answer"
            }}
        }));
        turn.observe_wire_value(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {"tokenUsage": {"last": {
                "inputTokens": 10,
                "outputTokens": 2,
                "totalTokens": 12
            }}}
        }));
        turn.finish(TurnStatus::Completed);
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        let span = spans
            .iter()
            .find(|span| span.name == "codex.session_task.turn")
            .expect("turn span");
        for key in [
            "input.value",
            "output.value",
            "lmnr.span.input",
            "lmnr.span.output",
            "gen_ai.input.messages",
            "gen_ai.output.messages",
        ] {
            assert_eq!(attribute(span, key), None, "unexpected {key}");
        }
        assert_eq!(
            attribute(span, "gen_ai.usage.total_tokens").as_deref(),
            Some("12")
        );
    }

    #[test]
    fn transcript_capture_exports_prompt_and_response_when_enabled() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn_with_transcript_capture(tracer, true);
        turn.observe_wire_value(&json!({
            "method": "item/completed",
            "params": {"item": {
                "id": "message-1",
                "type": "agentMessage",
                "text": "captured answer"
            }}
        }));
        turn.observe_wire_value(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {"tokenUsage": {"last": {
                "inputTokens": 10,
                "outputTokens": 2,
                "totalTokens": 12
            }}}
        }));
        turn.finish(TurnStatus::Completed);
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        let span = spans
            .iter()
            .find(|span| span.name == "codex.session_task.turn")
            .expect("turn span");
        assert_eq!(attribute(span, "input.value").as_deref(), Some("hello"));
        assert_eq!(
            attribute(span, "output.value").as_deref(),
            Some("captured answer")
        );
    }

    #[test]
    fn transcript_capture_accepts_only_explicit_truthy_values() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert!(
                truthy_env_value(value),
                "expected {value} to enable capture"
            );
        }
        for value in ["", "0", "false", "no", "off", "enabled"] {
            assert!(
                !truthy_env_value(value),
                "expected {value} to leave capture disabled"
            );
        }
    }

    #[test]
    fn codex_terminal_error_marks_turn_and_dangling_tools_failed() {
        let (exporter, provider, tracer) = test_telemetry();
        let mut turn = test_turn(tracer);
        turn.observe_wire_value(&json!({
            "method": "item/started",
            "params": {"item": {
                "id": "tool-1",
                "type": "commandExecution"
            }}
        }));
        turn.observe_wire_value(&json!({
            "method": "error",
            "params": {"error": {"message": "redacted"}}
        }));
        turn.observe_wire_value(&json!({
            "method": "thread/tokenUsage/updated",
            "params": {"tokenUsage": {"last": {
                "inputTokens": 10,
                "outputTokens": 2,
                "totalTokens": 12
            }}}
        }));
        turn.finish(TurnStatus::Completed);
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        assert_eq!(spans.len(), 2);
        let tool = spans
            .iter()
            .find(|span| span.name == "codex.tool.command_execution")
            .expect("tool span");
        assert_eq!(attribute(tool, "tool.status").as_deref(), Some("failed"));
        let turn = spans
            .iter()
            .find(|span| span.name == "codex.session_task.turn")
            .expect("turn span");
        assert_eq!(turn.status, Status::error("turn failed"));
    }

    #[test]
    fn traceparent_is_the_only_trace_identity() {
        let trace = trace_context();
        assert_eq!(
            trace.effective_traceparent().as_deref(),
            Some("00-0123456789abcdef0123456789abcdef-1111111111111111-01")
        );
        assert!(
            TraceContext {
                traceparent: Some("invalid".to_owned()),
                ..Default::default()
            }
            .effective_traceparent()
            .is_none()
        );
    }

    #[test]
    fn cost_estimate_accounts_for_cached_tokens() {
        let usage = NormalizedTokenUsage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(100_000),
            cache_read_input_tokens: Some(250_000),
            ..Default::default()
        };
        let cost =
            estimate_usage_cost(HarnessKind::Codex, "openai", "gpt-5.5", &usage).expect("cost");
        assert!((cost.input_cost - 3.875).abs() < 1e-9);
        assert!((cost.output_cost - 3.0).abs() < 1e-9);
    }

    #[test]
    fn gpt_5_6_family_cost_uses_standard_short_context_pricing() {
        let usage = NormalizedTokenUsage {
            input_tokens: Some(1_000_000),
            cache_creation_input_tokens: Some(100_000),
            cache_read_input_tokens: Some(200_000),
            output_tokens: Some(100_000),
            ..Default::default()
        };

        for (model, input_cost, output_cost, source) in [
            (
                "gpt-5.6-sol",
                4.225,
                3.0,
                "centaur_estimate:openai:gpt-5.6-sol:standard-short-context",
            ),
            (
                "gpt-5.6-terra",
                1.69,
                1.2,
                "centaur_estimate:openai:gpt-5.6-terra:standard-short-context",
            ),
            (
                "gpt-5.6-luna",
                0.169,
                0.12,
                "centaur_estimate:openai:gpt-5.6-luna:standard-short-context",
            ),
        ] {
            let cost =
                estimate_usage_cost(HarnessKind::Codex, "openai", model, &usage).expect("cost");

            assert!((cost.input_cost - input_cost).abs() < 1e-9, "{model}");
            assert!((cost.output_cost - output_cost).abs() < 1e-9, "{model}");
            assert!(
                (cost.total_cost() - input_cost - output_cost).abs() < 1e-9,
                "{model}"
            );
            assert_eq!(cost.source, source);
        }
    }

    fn attribute(span: &opentelemetry_sdk::trace::SpanData, key: &str) -> Option<String> {
        span.attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == key)
            .map(|attribute| match &attribute.value {
                opentelemetry::Value::String(value) => value.to_string(),
                opentelemetry::Value::I64(value) => value.to_string(),
                opentelemetry::Value::F64(value) => value.to_string(),
                opentelemetry::Value::Bool(value) => value.to_string(),
                _ => String::new(),
            })
    }

    #[test]
    fn bounded_span_value_preserves_utf8_boundary() {
        let value = format!("{}é", "a".repeat(MAX_SPAN_IO_BYTES - 1));
        let bounded = bounded_span_value(&value);
        assert!(bounded.ends_with(TRUNCATION_SUFFIX));
        assert!(bounded.len() <= MAX_SPAN_IO_BYTES);
        assert!(bounded.is_char_boundary(bounded.len()));
    }
}
