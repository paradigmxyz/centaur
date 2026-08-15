//! Hermes Agent harness — drives `hermes-agent`'s `tui_gateway` JSON-RPC
//! stdio wire as a Centaur harness.
//!
//! Unlike claude/amp (spawn-per-turn stream-json CLIs), Hermes ships a
//! long-lived JSON-RPC gateway (`python -m tui_gateway.entry`) that owns the
//! agent loop, durable session store, skills, persistent memory, background
//! self-improvement reviews, and the cron scheduler. This runtime therefore
//! mirrors the codex runtime shape — one persistent child per sandbox, a
//! handshake (`gateway.ready` → `session.create`), then one `prompt.submit`
//! per turn with events pumped into the shared `CodexTurnNormalizer`:
//!
//! - `message.delta`                       → AgentTextDelta
//! - `reasoning.delta` / `thinking.delta`  → ReasoningTextDelta
//! - `tool.start`                          → AssistantMessage(ToolUse)
//! - `tool.complete`                       → ToolResults
//! - `turn.usage`                          → TokenUsage
//! - `message.complete`                    → AssistantMessage(final) + Result
//!
//! Because the gateway (not this process) owns the agent loop, Hermes's
//! session history, prompt cache, learning loop, and cron jobs all survive
//! across turns. `HERMES_CONTINUE_SESSION_ID` resumes the durable session
//! after a sandbox restart (the hermes counterpart of
//! `CODEX_CONTINUE_THREAD_ID`), and Centaur's `interrupt` maps to Hermes's
//! `session.interrupt` — the turn ends Interrupted while the session lives on.

use std::env;
use std::io::{self, BufRead, Write};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};

use crate::server::{BlocksCommand, BlocksState, parse_blocks_line_with_state, write_blocks_error};
use crate::traits::{
    NormalizedContent, NormalizedEvent, NormalizedTokenUsage, NormalizedToolResult,
};
use crate::turn::{BridgeConfig, CodexTurnNormalizer};
use crate::util::write_value;
use crate::wire::notification_to_wire_value;
use crate::{HarnessServerError, Result};

/// Gateway startup + RPC response budget. Generous because a cold Hermes
/// start imports its agent stack and may run MCP discovery first.
const RPC_TIMEOUT: Duration = Duration::from_secs(180);
/// How long an interrupted turn may take to deliver its terminal
/// `message.complete` before we stop draining and move on.
const INTERRUPT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_CRON_TICK_SECONDS: u64 = 60;

/// Entry point for `harness-server hermes`.
pub fn run_hermes_blocks_server() -> Result<()> {
    let mut stdout = io::stdout().lock();
    let mut hermes: Option<HermesChild> = None;
    let (command_tx, command_rx) = mpsc::channel();
    let (interrupt_tx, interrupt_rx) = mpsc::channel();

    spawn_cron_ticker();

    thread::spawn(move || {
        let stdin = io::stdin();
        let mut blocks_state = BlocksState::default();
        for raw in stdin.lock().lines() {
            let Ok(line) = raw else { break };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let sent = match parse_blocks_line_with_state(trimmed, &mut blocks_state) {
                Ok(BlocksCommand::Interrupt) => interrupt_tx.send(()).is_ok(),
                Ok(command) => command_tx.send(Ok(command)).is_ok(),
                Err(error) => command_tx.send(Err(error.to_string())).is_ok(),
            };
            if !sent {
                break;
            }
        }
    });

    let mut turn = 0u64;
    while let Ok(input) = command_rx.recv() {
        let thread_id = hermes
            .as_ref()
            .map_or("hermes", HermesChild::thread_id)
            .to_owned();
        match input {
            Ok(BlocksCommand::User {
                input,
                client_user_message_id,
                model,
                provider: _,
                reasoning,
                trace_context: _,
            }) => {
                turn += 1;
                let result = ensure_child(&mut hermes, model).and_then(|child| {
                    run_hermes_turn(
                        child,
                        &mut stdout,
                        input,
                        client_user_message_id,
                        reasoning,
                        turn,
                        &interrupt_rx,
                    )
                });
                if let Err(error) = result {
                    eprintln!("Hermes blocks turn failed: {error:#}");
                    write_blocks_error(&mut stdout, &thread_id, "turn", error.to_string())?;
                    // A dead gateway cannot serve the next turn; drop it so the
                    // next message restarts Hermes and resumes the durable
                    // session via HERMES_CONTINUE_SESSION_ID.
                    if hermes.as_mut().is_some_and(|child| !child.is_alive()) {
                        hermes = None;
                    }
                }
            }
            Ok(BlocksCommand::Interrupt) => {
                eprintln!("Hermes blocks interrupt ignored: no active turn runs");
            }
            Ok(BlocksCommand::AttachmentChunk) => {}
            Err(error) => {
                eprintln!("invalid Hermes blocks input: {error}");
                write_blocks_error(&mut stdout, &thread_id, "input", error)?;
            }
        }
        // Drain interrupts that arrived between turns so a stale one cannot
        // instantly cancel the next turn.
        while interrupt_rx.try_recv().is_ok() {}
    }
    Ok(())
}

fn ensure_child(
    hermes: &mut Option<HermesChild>,
    model: Option<String>,
) -> Result<&mut HermesChild> {
    if hermes.is_none() {
        *hermes = Some(HermesChild::start(model)?);
    }
    Ok(hermes.as_mut().expect("hermes started"))
}

/// Tick `hermes cron tick` on an interval so cron jobs created inside the
/// conversation fire while the sandbox lives. Hermes serializes ticks
/// cross-process with a file lock, so this is safe alongside any other Hermes
/// process on the same HERMES_HOME. `HERMES_CRON_TICK_SECONDS=0` disables it.
fn spawn_cron_ticker() {
    let interval = env::var("HERMES_CRON_TICK_SECONDS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_CRON_TICK_SECONDS);
    if interval == 0 {
        return;
    }
    let bin = env::var("HERMES_BIN").unwrap_or_else(|_| "hermes".to_string());
    thread::spawn(move || {
        let mut warned = false;
        loop {
            thread::sleep(Duration::from_secs(interval));
            let status = ProcessCommand::new(&bin)
                .args(["cron", "tick"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Err(error) = status
                && !warned
            {
                eprintln!("hermes cron ticker disabled: {bin} cron tick failed: {error}");
                warned = true;
            }
        }
    });
}

struct HermesChild {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<io::Result<String>>,
    session_id: String,
    next_rpc_id: i64,
}

impl Drop for HermesChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl HermesChild {
    fn start(model: Option<String>) -> Result<Self> {
        // The JSON-RPC gateway is a Python module, not a `hermes` subcommand;
        // HERMES_PYTHON points at the interpreter whose env has hermes-agent.
        let python = env::var("HERMES_PYTHON").unwrap_or_else(|_| "python3".to_string());
        let mut child = ProcessCommand::new(python)
            .args(["-m", "tui_gateway.entry"])
            .env("HERMES_QUIET", "1")
            // Centaur owns approval policy at the sandbox boundary (isolated
            // sandbox, iron-proxy egress); inside it Hermes runs unattended.
            .env(
                "HERMES_APPROVAL_MODE",
                env::var("HERMES_APPROVAL_MODE").unwrap_or_else(|_| "off".to_string()),
            )
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
            // Unlocked handle on purpose: the child lives across turns, so
            // holding the StderrLock for the copy's lifetime would block
            // every eprintln! in the server until the child exits.
            let mut parent_stderr = io::stderr();
            let _ = io::copy(&mut stderr, &mut parent_stderr);
        });
        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = io::BufReader::new(stdout);
            for raw in reader.lines() {
                let should_stop = raw.is_err();
                if stdout_tx.send(raw).is_err() || should_stop {
                    break;
                }
            }
        });

        let mut this = Self {
            child,
            stdin,
            stdout: stdout_rx,
            session_id: String::new(),
            next_rpc_id: 0,
        };
        this.wait_for_gateway_ready()?;
        this.create_or_resume_session(model)?;
        Ok(this)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn thread_id(&self) -> &str {
        if self.session_id.is_empty() {
            "hermes"
        } else {
            &self.session_id
        }
    }

    fn wait_for_gateway_ready(&mut self) -> Result<()> {
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            if event_type(&self.read_frame_until(deadline)?) == Some("gateway.ready".to_string()) {
                return Ok(());
            }
        }
    }

    /// Create the thread's Hermes session, or resume the durable one after a
    /// sandbox restart.
    fn create_or_resume_session(&mut self, model: Option<String>) -> Result<()> {
        let resume = env::var("HERMES_CONTINUE_SESSION_ID").unwrap_or_default();
        let resume = resume.trim();
        if !resume.is_empty()
            && let Ok(result) =
                self.rpc("session.resume", json!({"session_id": resume, "cols": 200}))
            && let Some(sid) = result.get("session_id").and_then(Value::as_str)
        {
            self.session_id = sid.to_string();
            return Ok(());
        }

        let mut params = json!({
            "cols": 200,
            "cwd": env::current_dir().unwrap_or_default().to_string_lossy(),
            "title": "Centaur thread",
            "source": "centaur",
        });
        if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
            params["model"] = Value::String(model);
        }
        let result = self.rpc("session.create", params)?;
        self.session_id = result
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HarnessServerError::Protocol(
                    "session.create response missing session_id".to_string(),
                )
            })?
            .to_string();
        Ok(())
    }

    /// Send a request frame without waiting for the response (the caller
    /// pumps the stream itself, as `run_hermes_turn` does for prompt.submit).
    fn send_request(&mut self, method: &str, params: Value) -> Result<()> {
        self.next_rpc_id += 1;
        self.write_frame(&json!({
            "jsonrpc": "2.0",
            "id": self.next_rpc_id,
            "method": method,
            "params": params,
        }))
    }

    /// Send a request and block for its response, dropping unrelated frames.
    /// Only for out-of-turn RPCs (handshake, interrupt) — events skipped here
    /// would be lost to the turn pump.
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.send_request(method, params)?;
        let id = self.next_rpc_id;
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let frame = self.read_frame_until(deadline)?;
            if frame.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = frame.get("error") {
                return Err(HarnessServerError::Protocol(format!(
                    "hermes {method} failed: {error}"
                )));
            }
            return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn write_frame(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_frame_until(&mut self, deadline: Instant) -> Result<Value> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(gateway_timeout)?;
            match self.stdout.recv_timeout(remaining) {
                Ok(line) => {
                    // Hermes keeps stdout clean of non-JSON in quiet mode;
                    // tolerate stray lines anyway.
                    if let Ok(value) = serde_json::from_str::<Value>(line?.trim()) {
                        return Ok(value);
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Err(gateway_timeout()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HarnessServerError::HermesExited {
                        status: self.child.wait()?,
                    });
                }
            }
        }
    }
}

fn gateway_timeout() -> HarnessServerError {
    HarnessServerError::Protocol("timed out waiting for hermes gateway".to_string())
}

/// The `params.type` of an event frame, or None for responses/other frames.
fn event_type(frame: &Value) -> Option<String> {
    (frame.get("method").and_then(Value::as_str) == Some("event"))
        .then(|| frame.pointer("/params/type").and_then(Value::as_str))
        .flatten()
        .map(str::to_string)
}

/// Translate one Hermes gateway frame into normalized events. Pure: the text
/// item id is deterministic per turn, and a failed turn's error rides the
/// terminal `Result` event (which `CodexTurnNormalizer` latches into
/// `last_error` for `finish_turn`). Terminal frames are recognized with the
/// standard `NormalizedEvent::is_terminal()`.
fn normalize_hermes_frame(turn: u64, frame: &Value) -> Vec<NormalizedEvent> {
    let Some(kind) = event_type(frame) else {
        return Vec::new();
    };
    let empty = json!({});
    let payload = frame.pointer("/params/payload").unwrap_or(&empty);
    let text_of = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or("");

    match kind.as_str() {
        "message.delta" if !text_of("text").is_empty() => vec![NormalizedEvent::AgentTextDelta {
            item_id: format!("hermes-msg-{turn}"),
            delta: text_of("text").to_string(),
        }],
        "reasoning.delta" | "thinking.delta" if !text_of("text").is_empty() => {
            vec![NormalizedEvent::ReasoningTextDelta {
                item_id: format!("hermes-reasoning-{turn}"),
                delta: text_of("text").to_string(),
            }]
        }
        "tool.start" => vec![NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason: None,
            content: vec![NormalizedContent::ToolUse {
                raw_id: nonempty_or(text_of("tool_id"), "tool"),
                tool: nonempty_or(text_of("name"), "tool"),
                arguments: payload.get("args").cloned().unwrap_or(json!({})),
            }],
        }],
        "tool.complete" => {
            let result = payload.get("result");
            let is_error = result.is_some_and(|result| {
                result.get("success").and_then(Value::as_bool) == Some(false)
                    || result.get("error").is_some_and(|error| !error.is_null())
            });
            vec![NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                tool_use_id: nonempty_or(text_of("tool_id"), "tool"),
                content: tool_result_text(payload),
                is_error,
                exit_code: payload
                    .pointer("/result/exit_code")
                    .and_then(Value::as_i64)
                    .map(|code| code as i32),
            }])]
        }
        "turn.usage" | "session.usage" => {
            let count = |key: &str| payload.get(key).and_then(Value::as_i64);
            let usage = NormalizedTokenUsage {
                model: payload
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_tokens: count("input_tokens"),
                output_tokens: count("output_tokens"),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: count("cached_tokens"),
                reasoning_output_tokens: None,
                total_tokens: count("total_tokens"),
            };
            if usage.has_counts() {
                vec![NormalizedEvent::TokenUsage { usage }]
            } else {
                Vec::new()
            }
        }
        "message.complete" => {
            let text = text_of("text");
            if text_of("status") == "error" {
                let error = [text_of("error"), text, "hermes turn failed"]
                    .into_iter()
                    .find(|candidate| !candidate.is_empty())
                    .expect("last candidate is non-empty")
                    .to_string();
                return vec![NormalizedEvent::Result { error: Some(error) }];
            }
            let mut events = Vec::new();
            if !text.is_empty() {
                // The final text repeats the streamed deltas; the normalizer's
                // suffix-delta reconciliation prevents double emission.
                events.push(NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: Some("end_turn".to_string()),
                    content: vec![NormalizedContent::AgentText {
                        item_id: format!("hermes-msg-{turn}"),
                        text: text.to_string(),
                    }],
                });
            }
            events.push(NormalizedEvent::Result { error: None });
            events
        }
        _ => Vec::new(),
    }
}

fn nonempty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() { fallback } else { value }.to_string()
}

fn tool_result_text(payload: &Value) -> String {
    if let Some(text) = payload.get("result_text").and_then(Value::as_str) {
        return text.to_string();
    }
    match payload.get("result") {
        Some(Value::String(text)) => text.clone(),
        Some(value) if !value.is_null() => serde_json::to_string(value).unwrap_or_default(),
        _ => payload
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn run_hermes_turn<W: Write>(
    child: &mut HermesChild,
    stdout: &mut W,
    input: Vec<UserInput>,
    client_user_message_id: Option<String>,
    reasoning: Option<String>,
    turn: u64,
    interrupt_rx: &Receiver<()>,
) -> Result<()> {
    let mut config = BridgeConfig::new(child.thread_id().to_string(), format!("turn-{turn}"));
    config.cli_version = "hermes".to_string();
    config.model_provider = "hermes".to_string();
    let mut normalizer = CodexTurnNormalizer::new(config);

    for notification in normalizer.start_notifications(turn == 1)? {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    for notification in normalizer.emit_user_message(client_user_message_id, input.clone())? {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }

    let mut params = json!({
        "session_id": child.session_id,
        "text": user_input_text(&input),
    });
    if let Some(reasoning) = reasoning.filter(|value| !value.trim().is_empty()) {
        params["reasoning_effort"] = Value::String(reasoning);
    }
    child.send_request("prompt.submit", params)?;

    loop {
        if interrupt_rx.try_recv().is_ok() {
            let params = json!({"session_id": child.session_id});
            let _ = child.rpc("session.interrupt", params);
            // Hermes ends the interrupted turn with its own terminal frame;
            // drain until it arrives (bounded) so it can't leak into the
            // next turn as an instant terminal.
            let deadline = Instant::now() + INTERRUPT_DRAIN_TIMEOUT;
            while let Ok(frame) = child.read_frame_until(deadline) {
                if normalize_hermes_frame(turn, &frame)
                    .iter()
                    .any(NormalizedEvent::is_terminal)
                {
                    break;
                }
            }
            if let Some(notification) = normalizer.finish_turn_interrupted()? {
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            return Ok(());
        }

        match child.stdout.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => {
                let Ok(frame) = serde_json::from_str::<Value>(line?.trim()) else {
                    continue;
                };
                let mut terminal = false;
                for event in normalize_hermes_frame(turn, &frame) {
                    terminal |= event.is_terminal();
                    for notification in normalizer.process_event(&event)? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                }
                if terminal {
                    // A failed turn's error was latched from the Result event.
                    if let Some(notification) = normalizer.finish_turn(None)? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(HarnessServerError::HermesExited {
                    status: child.child.wait()?,
                });
            }
        }
    }
}

fn user_input_text(input: &[UserInput]) -> String {
    let mut parts = Vec::new();
    for item in input {
        match item {
            UserInput::Text { text, .. } => parts.push(text.clone()),
            UserInput::Image { url, .. } => parts.push(format!("[image: {url}]")),
            UserInput::LocalImage { path, .. } => {
                parts.push(format!("[image file: {}]", path.display()))
            }
            UserInput::Skill { name, path } => {
                parts.push(format!("[skill: {name} at {}]", path.display()))
            }
            UserInput::Mention { name, path } => parts.push(format!("[mention: {name} at {path}]")),
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::traits::{NormalizedContent, NormalizedEvent};

    use super::normalize_hermes_frame;

    fn frame(kind: &str, payload: serde_json::Value) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {"type": kind, "session_id": "abc", "payload": payload},
        })
    }

    fn is_terminal(events: &[NormalizedEvent]) -> bool {
        events.iter().any(NormalizedEvent::is_terminal)
    }

    #[test]
    fn message_delta_becomes_agent_text_delta() {
        let events = normalize_hermes_frame(1, &frame("message.delta", json!({"text": "hi"})));
        assert!(!is_terminal(&events));
        assert!(matches!(
            &events[..],
            [NormalizedEvent::AgentTextDelta { delta, .. }] if delta == "hi"
        ));
    }

    #[test]
    fn reasoning_delta_becomes_reasoning_text_delta() {
        let events =
            normalize_hermes_frame(1, &frame("reasoning.delta", json!({"text": "thinking"})));
        assert!(matches!(
            &events[..],
            [NormalizedEvent::ReasoningTextDelta { delta, .. }] if delta == "thinking"
        ));
    }

    #[test]
    fn tool_start_and_complete_round_trip() {
        let start_events = normalize_hermes_frame(
            1,
            &frame(
                "tool.start",
                json!({"tool_id": "t1", "name": "terminal", "args": {"command": "ls"}}),
            ),
        );
        let [NormalizedEvent::AssistantMessage { content, .. }] = &start_events[..] else {
            panic!("expected assistant message, got {start_events:?}");
        };
        assert!(matches!(
            &content[..],
            [NormalizedContent::ToolUse { raw_id, tool, .. }]
                if raw_id == "t1" && tool == "terminal"
        ));

        let complete_events = normalize_hermes_frame(
            1,
            &frame(
                "tool.complete",
                json!({"tool_id": "t1", "name": "terminal", "result": {"output": "ok", "exit_code": 0}}),
            ),
        );
        let [NormalizedEvent::ToolResults(results)] = &complete_events[..] else {
            panic!("expected tool results, got {complete_events:?}");
        };
        assert_eq!(results[0].tool_use_id, "t1");
        assert!(!results[0].is_error);
        assert_eq!(results[0].exit_code, Some(0));
    }

    #[test]
    fn message_complete_finishes_turn_with_canonical_text() {
        let events = normalize_hermes_frame(
            1,
            &frame("message.complete", json!({"text": "partial then final"})),
        );
        assert!(is_terminal(&events));
        assert!(matches!(
            &events[..],
            [
                NormalizedEvent::AssistantMessage { partial: false, content, .. },
                NormalizedEvent::Result { error: None },
            ] if matches!(
                &content[..],
                [NormalizedContent::AgentText { text, .. }] if text == "partial then final"
            )
        ));
    }

    #[test]
    fn message_complete_error_becomes_failed_result() {
        let events = normalize_hermes_frame(
            1,
            &frame(
                "message.complete",
                json!({"text": "boom", "status": "error", "error": "provider 500"}),
            ),
        );
        assert!(is_terminal(&events));
        assert!(matches!(
            &events[..],
            [NormalizedEvent::Result { error: Some(error) }] if error == "provider 500"
        ));
    }

    #[test]
    fn message_complete_error_falls_back_to_text() {
        let events = normalize_hermes_frame(
            1,
            &frame(
                "message.complete",
                json!({"text": "boom", "status": "error"}),
            ),
        );
        assert!(matches!(
            &events[..],
            [NormalizedEvent::Result { error: Some(error) }] if error == "boom"
        ));
    }

    #[test]
    fn tool_complete_marks_failures() {
        let events = normalize_hermes_frame(
            1,
            &frame(
                "tool.complete",
                json!({"tool_id": "t2", "result": {"success": false, "error": "denied"}}),
            ),
        );
        let [NormalizedEvent::ToolResults(results)] = &events[..] else {
            panic!("expected tool results");
        };
        assert!(results[0].is_error);
    }

    #[test]
    fn unknown_events_are_ignored() {
        let events = normalize_hermes_frame(1, &frame("session.info", json!({"model": "x"})));
        assert!(events.is_empty());
    }

    #[test]
    fn non_event_frames_are_ignored() {
        let events = normalize_hermes_frame(
            1,
            &json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}}),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn usage_event_maps_token_counts() {
        let events = normalize_hermes_frame(
            1,
            &frame(
                "turn.usage",
                json!({"input_tokens": 100, "output_tokens": 20, "total_tokens": 120}),
            ),
        );
        let [NormalizedEvent::TokenUsage { usage }] = &events[..] else {
            panic!("expected token usage");
        };
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.total_tokens, Some(120));
    }
}
