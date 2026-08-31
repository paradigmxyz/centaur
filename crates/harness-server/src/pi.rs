//! Pi harness — drives the official Pi CLI's long-lived RPC mode and
//! translates its native events into Centaur's blocks protocol.

use std::env;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::server::{BlocksCommand, BlocksState, parse_blocks_line_with_state, write_blocks_error};
use crate::traits::{
    NormalizedContent, NormalizedEvent, NormalizedTokenUsage, NormalizedToolResult,
};
use crate::turn::{BridgeConfig, CodexTurnNormalizer};
use crate::util::write_value;
use crate::wire::notification_to_wire_value;
use crate::{HarnessServerError, Result};

const RPC_TIMEOUT: Duration = Duration::from_secs(180);
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn run_pi_blocks_server() -> Result<()> {
    let (command_tx, command_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut state = BlocksState::default();
        for raw in io::stdin().lock().lines() {
            let Ok(line) = raw else { break };
            if line.trim().is_empty() {
                continue;
            }
            let command =
                parse_blocks_line_with_state(&line, &mut state).map_err(|error| error.to_string());
            if command_tx.send(command).is_err() {
                break;
            }
        }
    });

    let mut stdout = io::stdout().lock();
    let mut pi: Option<PiChild> = None;
    let mut turn = 0u64;
    while let Ok(command) = command_rx.recv() {
        let thread_id = pi
            .as_ref()
            .map_or_else(|| "pi".to_string(), |child| child.session_id.clone());
        match command {
            Ok(BlocksCommand::User {
                input,
                client_user_message_id,
                model,
                provider,
                reasoning,
                trace_context,
            }) => {
                turn += 1;
                let result = ensure_child(
                    &mut pi,
                    model.as_deref(),
                    provider.as_deref(),
                    reasoning.as_deref(),
                    trace_context.thread_key.as_deref(),
                )
                .and_then(|child| {
                    run_pi_turn(
                        child,
                        &mut stdout,
                        PiTurn {
                            input,
                            client_user_message_id,
                            model,
                            provider,
                            reasoning,
                            number: turn,
                        },
                        &command_rx,
                    )
                });
                if let Err(error) = result {
                    eprintln!("Pi blocks turn failed: {error:#}");
                    write_blocks_error(&mut stdout, &thread_id, "turn", error.to_string())?;
                    if pi.as_mut().is_some_and(|child| !child.is_alive()) {
                        pi = None;
                    }
                }
            }
            Ok(BlocksCommand::Interrupt) => {
                eprintln!("Pi blocks interrupt ignored: no active turn runs")
            }
            Ok(BlocksCommand::AttachmentChunk) => {}
            Err(error) => write_blocks_error(&mut stdout, &thread_id, "input", error)?,
        }
    }
    Ok(())
}

fn ensure_child<'a>(
    pi: &'a mut Option<PiChild>,
    model: Option<&str>,
    provider: Option<&str>,
    reasoning: Option<&str>,
    thread_key: Option<&str>,
) -> Result<&'a mut PiChild> {
    if pi.is_none() {
        *pi = Some(PiChild::start(model, provider, reasoning, thread_key)?);
    }
    Ok(pi.as_mut().expect("Pi started"))
}

struct PiChild {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<io::Result<String>>,
    session_id: String,
    provider: String,
    model: Option<String>,
    reasoning: Option<String>,
    next_id: u64,
}

impl Drop for PiChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PiChild {
    fn start(
        model: Option<&str>,
        provider: Option<&str>,
        reasoning: Option<&str>,
        thread_key: Option<&str>,
    ) -> Result<Self> {
        let provider = clean(provider)
            .map(str::to_owned)
            .or_else(|| env_string("PI_PROVIDER"))
            .unwrap_or_else(default_provider);
        let model = clean(model)
            .map(str::to_owned)
            .or_else(|| env_string("PI_MODEL"))
            .or_else(|| env_string("CODEX_MODEL"));
        let reasoning = clean(reasoning)
            .map(normalize_reasoning)
            .or_else(|| env_string("PI_THINKING").map(|value| normalize_reasoning(&value)))
            .or_else(|| {
                env_string("CODEX_MODEL_REASONING_EFFORT").map(|value| normalize_reasoning(&value))
            });
        let session_id = env_string("PI_CONTINUE_SESSION_ID").unwrap_or_else(|| {
            thread_key.map_or_else(
                || Uuid::new_v4().to_string(),
                |key| Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).to_string(),
            )
        });
        let bin = env::var("PI_BIN").unwrap_or_else(|_| "pi".to_string());
        let mut command = ProcessCommand::new(bin);
        command.args([
            "--mode",
            "rpc",
            "--session-id",
            &session_id,
            "--provider",
            &provider,
            "--approve",
        ]);
        if let Some(model) = &model {
            command.args(["--model", model]);
        }
        if let Some(reasoning) = &reasoning {
            command.args(["--thinking", reasoning]);
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| HarnessServerError::SpawnHarness {
                cwd: env::current_dir().unwrap_or_default(),
                source,
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(HarnessServerError::HarnessStdoutUnavailable)?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or(HarnessServerError::HarnessStderrUnavailable)?;
        thread::spawn(move || {
            let _ = io::copy(&mut stderr, &mut io::stderr());
        });
        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            for line in io::BufReader::new(stdout).lines() {
                let stop = line.is_err();
                if stdout_tx.send(line).is_err() || stop {
                    break;
                }
            }
        });

        let mut this = Self {
            child,
            stdin,
            stdout: stdout_rx,
            session_id,
            provider,
            model,
            reasoning,
            next_id: 0,
        };
        this.rpc("get_state", json!({}))?;
        Ok(this)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn configure(
        &mut self,
        provider: Option<&str>,
        model: Option<&str>,
        reasoning: Option<&str>,
    ) -> Result<()> {
        let provider = clean(provider).unwrap_or(&self.provider).to_string();
        let model = clean(model)
            .map(str::to_owned)
            .or_else(|| self.model.clone());
        if provider != self.provider || model != self.model {
            let model = model.ok_or_else(|| {
                HarnessServerError::Protocol("Pi model override requires a model id".to_string())
            })?;
            self.rpc("set_model", json!({"provider": provider, "modelId": model}))?;
            self.provider = provider;
            self.model = Some(model);
        }

        if let Some(reasoning) = clean(reasoning).map(normalize_reasoning)
            && self.reasoning.as_deref() != Some(&reasoning)
        {
            self.rpc("set_thinking_level", json!({"level": reasoning}))?;
            self.reasoning = Some(reasoning);
        }
        Ok(())
    }

    fn send(&mut self, kind: &str, mut body: Value) -> Result<u64> {
        self.next_id += 1;
        body["id"] = Value::String(format!("centaur-{}", self.next_id));
        body["type"] = Value::String(kind.to_string());
        serde_json::to_writer(&mut self.stdin, &body)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(self.next_id)
    }

    fn rpc(&mut self, kind: &str, body: Value) -> Result<Value> {
        let id = format!("centaur-{}", self.send(kind, body)?);
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let frame = self.read_until(deadline)?;
            if frame.get("type").and_then(Value::as_str) != Some("response")
                || frame.get("id").and_then(Value::as_str) != Some(&id)
            {
                continue;
            }
            if frame.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(HarnessServerError::Protocol(
                    frame
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Pi RPC request failed")
                        .to_string(),
                ));
            }
            return Ok(frame.get("data").cloned().unwrap_or(Value::Null));
        }
    }

    fn read_until(&mut self, deadline: Instant) -> Result<Value> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(rpc_timeout)?;
            match self.stdout.recv_timeout(remaining) {
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<Value>(line?.trim()) {
                        return Ok(value);
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(rpc_timeout()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HarnessServerError::Protocol(format!(
                        "Pi exited before replying: {}",
                        self.child.wait()?
                    )));
                }
            }
        }
    }
}

struct PiTurn {
    input: Vec<UserInput>,
    client_user_message_id: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    reasoning: Option<String>,
    number: u64,
}

fn run_pi_turn<W: Write>(
    child: &mut PiChild,
    stdout: &mut W,
    turn: PiTurn,
    commands: &Receiver<std::result::Result<BlocksCommand, String>>,
) -> Result<()> {
    child.configure(
        turn.provider.as_deref(),
        turn.model.as_deref(),
        turn.reasoning.as_deref(),
    )?;
    let mut config = BridgeConfig::new(child.session_id.clone(), format!("turn-{}", turn.number));
    config.cli_version = "pi".to_string();
    config.model_provider = child.provider.clone();
    let mut normalizer = CodexTurnNormalizer::new(config);
    for notification in normalizer.start_notifications(turn.number == 1)? {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    for notification in
        normalizer.emit_user_message(turn.client_user_message_id, turn.input.clone())?
    {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    child.send("prompt", prompt_body(&turn.input, None)?)?;

    let mut events = PiEventNormalizer::new(turn.number);
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                Ok(BlocksCommand::User {
                    input,
                    client_user_message_id,
                    ..
                }) => {
                    for notification in
                        normalizer.emit_user_message(client_user_message_id, input.clone())?
                    {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                    child.send("prompt", prompt_body(&input, Some("steer"))?)?;
                }
                Ok(BlocksCommand::Interrupt) => {
                    child.send("abort", json!({}))?;
                    drain_interrupted_turn(child);
                    if let Some(notification) = normalizer.finish_turn_interrupted()? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                    return Ok(());
                }
                Ok(BlocksCommand::AttachmentChunk) => {}
                Err(error) => write_blocks_error(
                    stdout,
                    &child.session_id,
                    &format!("turn-{}", turn.number),
                    error,
                )?,
            }
        }

        match child.stdout.recv_timeout(Duration::from_millis(25)) {
            Ok(line) => {
                let frame: Value = match serde_json::from_str(line?.trim()) {
                    Ok(frame) => frame,
                    Err(_) => continue,
                };
                let normalized = events.normalize(&frame);
                let terminal = normalized.iter().any(NormalizedEvent::is_terminal);
                for event in normalized {
                    for notification in normalizer.process_event(&event)? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                }
                if terminal {
                    if let Some(notification) = normalizer.finish_turn(None)? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(HarnessServerError::Protocol(format!(
                    "Pi exited during a turn: {}",
                    child.child.wait()?
                )));
            }
        }
    }
}

fn drain_interrupted_turn(child: &mut PiChild) {
    let deadline = Instant::now() + INTERRUPT_TIMEOUT;
    while let Ok(frame) = child.read_until(deadline) {
        if frame.get("type").and_then(Value::as_str) == Some("agent_settled") {
            break;
        }
    }
}

struct PiEventNormalizer {
    turn: u64,
    assistant_message: u64,
    error: Option<String>,
}

impl PiEventNormalizer {
    fn new(turn: u64) -> Self {
        Self {
            turn,
            assistant_message: 0,
            error: None,
        }
    }

    fn normalize(&mut self, frame: &Value) -> Vec<NormalizedEvent> {
        match frame.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start"
                if frame.pointer("/message/role").and_then(Value::as_str) == Some("assistant") =>
            {
                self.assistant_message += 1;
                Vec::new()
            }
            "message_update" => self.normalize_delta(frame),
            "message_end"
                if frame.pointer("/message/role").and_then(Value::as_str) == Some("assistant") =>
            {
                self.normalize_assistant(frame.pointer("/message").unwrap_or(&Value::Null))
            }
            "tool_execution_end" => {
                vec![NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                    tool_use_id: string_at(frame, "/toolCallId")
                        .unwrap_or_else(|| "tool".to_string()),
                    content: content_text(frame.pointer("/result/content").unwrap_or(&Value::Null)),
                    is_error: frame
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    exit_code: frame
                        .pointer("/result/details/exitCode")
                        .and_then(Value::as_i64)
                        .map(|value| value as i32),
                }])]
            }
            "agent_settled" => vec![NormalizedEvent::Result {
                error: self.error.take(),
            }],
            "response" if frame.get("success").and_then(Value::as_bool) == Some(false) => {
                vec![NormalizedEvent::Result {
                    error: Some(
                        frame
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("Pi RPC request failed")
                            .to_string(),
                    ),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn normalize_delta(&self, frame: &Value) -> Vec<NormalizedEvent> {
        let event = frame
            .pointer("/assistantMessageEvent")
            .unwrap_or(&Value::Null);
        let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
        if delta.is_empty() {
            return Vec::new();
        }
        match event.get("type").and_then(Value::as_str) {
            Some("text_delta") => vec![NormalizedEvent::AgentTextDelta {
                item_id: self.content_id("message", event),
                delta: delta.to_string(),
            }],
            Some("thinking_delta") => vec![NormalizedEvent::ReasoningTextDelta {
                item_id: self.content_id("reasoning", event),
                delta: delta.to_string(),
            }],
            _ => Vec::new(),
        }
    }

    fn normalize_assistant(&mut self, message: &Value) -> Vec<NormalizedEvent> {
        let mut out = Vec::new();
        if let Some(usage) = token_usage(message) {
            out.push(NormalizedEvent::TokenUsage { usage });
        }
        let stop_reason = message
            .get("stopReason")
            .and_then(Value::as_str)
            .map(normalized_stop_reason);
        if matches!(
            message.get("stopReason").and_then(Value::as_str),
            Some("error")
        ) {
            self.error = Some(
                message
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("Pi turn failed")
                    .to_string(),
            );
        }
        out.push(NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason,
            content: normalized_content(message, self.turn, self.assistant_message),
        });
        out
    }

    fn content_id(&self, kind: &str, event: &Value) -> String {
        format!(
            "pi-{kind}-{}-{}-{}",
            self.turn,
            self.assistant_message,
            event
                .get("contentIndex")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )
    }
}

fn prompt_body(input: &[UserInput], streaming_behavior: Option<&str>) -> Result<Value> {
    let mut message = Vec::new();
    let mut images = Vec::new();
    for item in input {
        match item {
            UserInput::Text { text, .. } => message.push(text.clone()),
            UserInput::LocalImage { path, .. } => images.push(local_image(path)?),
            UserInput::Image { url, .. } => message.push(format!("[image: {url}]")),
            UserInput::Skill { name, path } => {
                message.push(format!("[skill: {name} at {}]", path.display()))
            }
            UserInput::Mention { name, path } => {
                message.push(format!("[mention: {name} at {path}]"))
            }
        }
    }
    let mut body = json!({"message": message.join("\n")});
    if !images.is_empty() {
        body["images"] = Value::Array(images);
    }
    if let Some(behavior) = streaming_behavior {
        body["streamingBehavior"] = Value::String(behavior.to_string());
    }
    Ok(body)
}

fn local_image(path: &Path) -> Result<Value> {
    let mime_type = match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };
    Ok(json!({
        "type": "image",
        "data": BASE64_STANDARD.encode(std::fs::read(path)?),
        "mimeType": mime_type,
    }))
}

fn normalized_content(
    message: &Value,
    turn: u64,
    assistant_message: u64,
) -> Vec<NormalizedContent> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(
            |(index, block)| match block.get("type").and_then(Value::as_str) {
                Some("text") => Some(NormalizedContent::AgentText {
                    item_id: format!("pi-message-{turn}-{assistant_message}-{index}"),
                    text: block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
                Some("thinking") => Some(NormalizedContent::ReasoningText {
                    item_id: format!("pi-reasoning-{turn}-{assistant_message}-{index}"),
                    text: block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
                Some("toolCall") => Some(NormalizedContent::ToolUse {
                    raw_id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    tool: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    arguments: block.get("arguments").cloned().unwrap_or_else(|| json!({})),
                }),
                _ => None,
            },
        )
        .collect()
}

fn token_usage(message: &Value) -> Option<NormalizedTokenUsage> {
    let usage = message.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_i64);
    let normalized = NormalizedTokenUsage {
        model: message
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_tokens: count("input"),
        output_tokens: count("output"),
        cache_creation_input_tokens: count("cacheWrite"),
        cache_read_input_tokens: count("cacheRead"),
        reasoning_output_tokens: count("reasoning"),
        total_tokens: count("totalTokens"),
    };
    normalized.has_counts().then_some(normalized)
}

fn content_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalized_stop_reason(reason: &str) -> String {
    match reason {
        "stop" => "end_turn",
        "toolUse" => "tool_use",
        "length" => "max_tokens",
        other => other,
    }
    .to_string()
}

fn default_provider() -> String {
    if env::var("CODEX_AUTH_MODE").as_deref() == Ok("access_token") {
        "openai-codex"
    } else {
        "openai"
    }
    .to_string()
}

fn normalize_reasoning(value: &str) -> String {
    if value.eq_ignore_ascii_case("none") {
        "off".to_string()
    } else {
        value.trim().to_ascii_lowercase()
    }
}

fn env_string(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn rpc_timeout() -> HarnessServerError {
    HarnessServerError::Protocol("timed out waiting for Pi RPC response".to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PiEventNormalizer, prompt_body};
    use crate::traits::{NormalizedContent, NormalizedEvent};

    #[test]
    fn normalizes_streaming_text_and_terminal_usage() {
        let mut normalizer = PiEventNormalizer::new(1);
        let events = normalizer.normalize(&json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "hello"}
        }));
        assert!(matches!(
            &events[..],
            [NormalizedEvent::AgentTextDelta { delta, .. }] if delta == "hello"
        ));

        let events = normalizer.normalize(&json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "hello"}],
                "model": "gpt-5.6-luna",
                "usage": {"input": 10, "output": 2, "totalTokens": 12},
                "stopReason": "stop"
            }
        }));
        assert!(matches!(events[0], NormalizedEvent::TokenUsage { .. }));
        assert!(matches!(
            &events[1],
            NormalizedEvent::AssistantMessage { stop_reason: Some(reason), content, .. }
                if reason == "end_turn" && matches!(
                    &content[..],
                    [NormalizedContent::AgentText { text, .. }] if text == "hello"
                )
        ));
        assert!(matches!(
            normalizer
                .normalize(&json!({"type": "agent_settled"}))
                .as_slice(),
            [NormalizedEvent::Result { error: None }]
        ));
    }

    #[test]
    fn normalizes_tool_calls_and_results() {
        let mut normalizer = PiEventNormalizer::new(1);
        normalizer.normalize(&json!({"type": "message_start", "message": {"role": "assistant"}}));
        let call = normalizer.normalize(&json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{"type": "toolCall", "id": "call-1", "name": "bash", "arguments": {"command": "pwd"}}],
                "stopReason": "toolUse"
            }
        }));
        assert!(matches!(
            &call[0],
            NormalizedEvent::AssistantMessage { content, .. }
                if matches!(&content[0], NormalizedContent::ToolUse { raw_id, .. } if raw_id == "call-1")
        ));
        let result = normalizer.normalize(&json!({
            "type": "tool_execution_end",
            "toolCallId": "call-1",
            "result": {"content": [{"type": "text", "text": "/repo"}], "details": {"exitCode": 0}},
            "isError": false
        }));
        assert!(matches!(
            &result[0],
            NormalizedEvent::ToolResults(results)
                if results[0].tool_use_id == "call-1" && results[0].content == "/repo"
        ));
    }

    #[test]
    fn gives_each_assistant_message_distinct_item_ids() {
        let mut normalizer = PiEventNormalizer::new(1);
        normalizer.normalize(&json!({"type": "message_start", "message": {"role": "assistant"}}));
        let first = normalizer.normalize(&json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "before tool"}
        }));
        normalizer.normalize(&json!({"type": "message_start", "message": {"role": "assistant"}}));
        let second = normalizer.normalize(&json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "after tool"}
        }));
        let item_id = |event: &NormalizedEvent| match event {
            NormalizedEvent::AgentTextDelta { item_id, .. } => item_id.clone(),
            _ => panic!("expected text delta"),
        };
        assert_ne!(item_id(&first[0]), item_id(&second[0]));
    }

    #[test]
    fn preserves_terminal_provider_errors() {
        let mut normalizer = PiEventNormalizer::new(1);
        normalizer.normalize(&json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [],
                "stopReason": "error",
                "errorMessage": "provider rejected request"
            }
        }));
        assert!(matches!(
            normalizer.normalize(&json!({"type": "agent_settled"})).as_slice(),
            [NormalizedEvent::Result { error: Some(error) }] if error == "provider rejected request"
        ));
    }

    #[test]
    fn builds_rpc_prompt_from_centaur_text() {
        let body = prompt_body(
            &[codex_app_server_protocol::UserInput::Text {
                text: "inspect this".to_string(),
                text_elements: Vec::new(),
            }],
            Some("steer"),
        )
        .unwrap();
        assert_eq!(body["message"], "inspect this");
        assert_eq!(body["streamingBehavior"], "steer");
    }
}
