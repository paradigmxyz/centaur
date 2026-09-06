//! Resident OMP RPC host adapter.
//!
//! One `omp --mode rpc` process per Centaur session, reused across
//! sequential turns. The process continuously drains unsolicited
//! session/agent lifecycle frames from its stdout while ordinary
//! commands (prompt/steer/abort) are correlated by request id. This
//! mirrors the Codex App Server V2 resident pattern (`codex::run_codex_blocks_server`,
//! `CodexJsonRpcChild`) but against the OMP RPC wire contract rather than the
//! Codex JSON-RPC app-server protocol.
//!
//! # Process lifetime vs session resume
//! Process reuse is within one resident host lifetime (one `OmpRpcChild`).
//! Across resident lifetimes (child death / re-acquire), set
//! `CENTAUR_OMP_SESSION_NAME` so respawn passes `--resume <name>` and the
//! prior JSONL session is continued instead of starting an anonymous one.
//!
//! # Slash commands
//! In RPC mode omp executes its builtin slash commands locally and streams
//! their output as `command_output` frames instead of starting a model turn.
//! The host forwards only [`OMP_SLASH_COMMAND_ALLOWLIST`] (commands with no
//! effect beyond the current omp session) and surfaces that output as the
//! turn's agent message; every other `/command` is answered by the host
//! without reaching omp. The command is read from the user's own text block
//! (the last one): api-rs prepends a chat-surface note for console and Slack
//! threads, and forwarding that note along would hide the leading `/` from
//! omp's parser, so an allowed command is sent to omp alone.

use std::env;
use std::io::{self, BufRead, Write};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::omp::OmpStreamEvent;
use crate::server::BlocksCommand;
use crate::turn::{BridgeConfig, CodexTurnNormalizer};
use crate::util::write_value;
use crate::{HarnessServerError, Result};
const DEFAULT_OMP_TURN_TIMEOUT: Duration = Duration::from_secs(300);
const OMP_TURN_TIMEOUT_GRACE: Duration = Duration::from_secs(5);
const OMP_CONFIG_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const OMP_THINKING_LEVELS: [&str; 8] = [
    "inherit", "off", "minimal", "low", "medium", "high", "xhigh", "max",
];

#[derive(Debug, PartialEq, Eq)]
struct OmpModelSelector<'a> {
    provider: &'a str,
    model_id: &'a str,
    thinking_level: Option<&'a str>,
}

fn parse_omp_model_selector(selector: &str) -> Result<OmpModelSelector<'_>> {
    let (provider, model_selector) =
        selector
            .split_once('/')
            .ok_or_else(|| HarnessServerError::InvalidBlocksInput {
                message: format!(
                    "OMP RPC model override must use provider/model form, got {selector:?}"
                ),
            })?;
    if provider.is_empty() || model_selector.is_empty() {
        return Err(HarnessServerError::InvalidBlocksInput {
            message: format!(
                "OMP RPC model override must use provider/model form, got {selector:?}"
            ),
        });
    }

    let (model_id, thinking_level) = match model_selector.rsplit_once(':') {
        Some((model_id, level)) if OMP_THINKING_LEVELS.contains(&level) => (model_id, Some(level)),
        _ => (model_selector, None),
    };
    if model_id.is_empty() {
        return Err(HarnessServerError::InvalidBlocksInput {
            message: format!("OMP RPC model override has an empty model id: {selector:?}"),
        });
    }

    Ok(OmpModelSelector {
        provider,
        model_id,
        thinking_level,
    })
}

fn validate_omp_thinking_level(level: &str) -> Result<()> {
    if OMP_THINKING_LEVELS.contains(&level) {
        return Ok(());
    }
    Err(HarnessServerError::InvalidBlocksInput {
        message: format!("unsupported OMP thinking level: {level:?}"),
    })
}

fn turn_timeout_from_trace_context(trace_context: &crate::otel::TraceContext) -> Duration {
    trace_context
        .metadata
        .get("max_duration_ms")
        .and_then(Value::as_u64)
        .filter(|duration_ms| *duration_ms > 0)
        .map(Duration::from_millis)
        .and_then(|duration| duration.checked_add(OMP_TURN_TIMEOUT_GRACE))
        .unwrap_or(DEFAULT_OMP_TURN_TIMEOUT)
}

/// One frame demultiplexed from the resident `omp --mode rpc` stdout stream.
/// The adapter distinguishes correlated command responses (matched by `id`)
/// from unsolicited session/agent lifecycle frames.
#[derive(Debug, Clone)]
pub enum OmpRpcFrame {
    /// Emitted once at startup before any command is accepted.
    Ready,
    /// A correlated command response. `id` echoes the request `id`; `None`
    /// when the request had no id (or for parse/unknown-command errors).
    Response {
        id: Option<String>,
        command: String,
        success: bool,
        data: Option<Value>,
        error: Option<String>,
    },
    /// An `AgentSessionEvent` (`agent_start`, `message_update`, `agent_end`,
    /// …). Reuses the one-shot parser so the normalized event surface is
    /// identical across the one-shot and resident paths.
    Event(OmpStreamEvent),
    /// A prompt that was accepted immediately but later resolves as local-only
    /// (no agent turn). `agent_invoked == false` is a completion signal.
    PromptResult {
        #[allow(dead_code)]
        id: Option<String>,
        agent_invoked: bool,
    },
    /// Output of a builtin slash command omp ran locally (no model turn).
    CommandOutput { text: String },
    /// Any other unsolicited frame the adapter does not demultiplex into a
    /// normalized event (extension_error, available_commands_update,
    /// host_tool_*, subagent_*). Forwarded verbatim to the host log.
    Other(Value),
}

impl OmpRpcFrame {
    /// Parse one JSON line from `omp --mode rpc` stdout into a demultiplexed
    /// frame. Unknown shapes degrade to [`OmpRpcFrame::Other`] rather than
    /// erroring so the drain loop never blocks on a novel frame.
    pub fn parse_json_line(line: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(line)?;
        Self::from_value(value)
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let Some(kind) = value.get("type").and_then(Value::as_str) else {
            return Ok(Self::Other(value));
        };
        match kind {
            "ready" => Ok(Self::Ready),
            "response" => {
                let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
                let command = value
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let success = value
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let data = value.get("data").cloned().filter(|v| !v.is_null());
                let error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Ok(Self::Response {
                    id,
                    command,
                    success,
                    data,
                    error,
                })
            }
            "prompt_result" => {
                let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
                let agent_invoked = value
                    .get("agentInvoked")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(Self::PromptResult { id, agent_invoked })
            }
            "command_output" => Ok(Self::CommandOutput {
                text: value
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            // AgentSessionEvent frames reuse the one-shot stream parser so the
            // normalized event surface is identical across paths.
            "session"
            | "agent_start"
            | "agent_end"
            | "turn_start"
            | "turn_end"
            | "message_start"
            | "message_update"
            | "message_end"
            | "tool_execution_start"
            | "tool_execution_update"
            | "tool_execution_end"
            | "error" => Ok(Self::Event(OmpStreamEvent::parse_json_line(
                &value.to_string(),
            )?)),
            _ => Ok(Self::Other(value)),
        }
    }
}

/// A resident `omp --mode rpc` child process. stdout is drained continuously
/// by a background thread into an mpsc channel so unsolicited lifecycle frames
/// never block a pending command. Command responses are correlated by `id` and
/// handed to the waiting caller via a one-shot slot.
pub struct OmpRpcChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<io::Result<String>>,
    /// Monotonically increasing request id for commands that need correlation.
    next_id: u64,
}

impl OmpRpcChild {
    /// Spawn `omp --mode rpc` (or the override at `CENTAUR_OMP_RPC_BRIDGE_COMMAND`)
    /// with piped stdio. The caller drives the ready handshake and drain loop.
    pub fn spawn() -> Result<Self> {
        let mut command = omp_rpc_command();
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
            // Unlocked handle on purpose: the child outlives each turn, so
            // holding the StderrLock for the copy's lifetime would block every
            // eprintln! in the server until the child exits.
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

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: stdout_rx,
            next_id: 1,
        })
    }

    /// Allocate the next request id. OMP RPC accepts optional string ids; the
    /// adapter always sends one so responses can be correlated.
    pub fn next_request_id(&mut self) -> String {
        let id = self.next_id.to_string();
        self.next_id += 1;
        id
    }

    /// Send a JSON command on stdin. The caller supplies the full command
    /// object (including `id` and `type`).
    pub fn send_command(&mut self, command: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
        serde_json::to_writer(&mut *stdin, command)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Read the next raw stdout line. `Err` on EOF/child exit.
    pub fn read_line(&mut self) -> Result<String> {
        loop {
            let line: io::Result<String> = match self.stdout.recv() {
                Ok(line) => line,
                Err(_) => {
                    let status = self.child.wait()?;
                    return Err(HarnessServerError::HarnessExited {
                        kind: crate::traits::HarnessKind::Omp,
                        status,
                        stderr: String::new(),
                    });
                }
            };
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Ok(trimmed.to_owned());
        }
    }

    /// Try to read the next raw stdout line without blocking longer than
    /// `timeout` wall-clock. Blank lines do not reset the budget: one absolute
    /// deadline covers the whole call, and each recv uses only the remaining
    /// time. `Ok(None)` on timeout; `Err` on EOF/child exit.
    pub fn read_line_timeout(&mut self, timeout: Duration) -> Result<Option<String>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let line: io::Result<String> = match self.stdout.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait()?;
                    return Err(HarnessServerError::HarnessExited {
                        kind: crate::traits::HarnessKind::Omp,
                        status,
                        stderr: String::new(),
                    });
                }
            };
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // Consume blank and continue under the same absolute deadline.
                continue;
            }
            return Ok(Some(trimmed.to_owned()));
        }
    }

    /// Wait for the process to exit and return its status. Used by clean
    /// shutdown after stdin is closed.
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        // Closing stdin tells the RPC server to drain pending side-channel
        // requests and exit cleanly (code 0). Bounded: if the child ignores
        // stdin EOF, kill after the timeout rather than hang forever.
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.flush();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait()? {
                Some(status) => return Ok(status),
                None if std::time::Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    return self.child.wait().map_err(Into::into);
                }
                None => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

impl OmpRpcChild {}

impl Drop for OmpRpcChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn omp_rpc_command() -> ProcessCommand {
    if let Some(command) = crate::command_from_override("CENTAUR_OMP_RPC_BRIDGE_COMMAND") {
        return command;
    }
    let bin = env::var("OMP_BIN").unwrap_or_else(|_| "omp".to_string());
    let mut command = ProcessCommand::new(bin);
    command.args([
        "--mode",
        "rpc",
        "--auto-approve",
        "--session-dir",
        &crate::omp::omp_session_dir().display().to_string(),
    ]);
    // Resume prior session: read the actual session id written by the first
    // spawn's get_state response. The release resolves --resume by JSONL
    // filename prefix matching.
    let session_marker = crate::omp::omp_session_dir().join(".resident_session_id");
    if let Ok(id) = std::fs::read_to_string(&session_marker)
        && !id.trim().is_empty()
    {
        command.args(["--resume", id.trim()]);
    }
    if let Ok(model) = env::var("OMP_MODEL")
        && !model.is_empty()
    {
        command.args(["--model", &model]);
    }
    command
}

fn set_model_command(id: &str, provider: &str, model_id: &str) -> Value {
    json!({
        "id": id,
        "type": "set_model",
        "provider": provider,
        "modelId": model_id,
    })
}

fn set_thinking_level_command(id: &str, level: &str) -> Value {
    json!({
        "id": id,
        "type": "set_thinking_level",
        "level": level,
    })
}

fn drive_omp_config_response(
    child: &mut OmpRpcChild,
    expected_id: &str,
    expected_command: &str,
) -> Result<()> {
    let deadline = std::time::Instant::now() + OMP_CONFIG_COMMAND_TIMEOUT;
    while std::time::Instant::now() < deadline {
        let Some(line) = child.read_line_timeout(Duration::from_millis(100))? else {
            continue;
        };
        let frame = OmpRpcFrame::parse_json_line(&line)?;
        if let OmpRpcFrame::Response {
            id,
            command,
            success,
            error,
            ..
        } = frame
            && id.as_deref() == Some(expected_id)
            && command == expected_command
        {
            if success {
                return Ok(());
            }
            return Err(HarnessServerError::InvalidBlocksInput {
                message: format!(
                    "OMP RPC {expected_command} failed: {}",
                    error.unwrap_or_default()
                ),
            });
        }
    }
    Err(HarnessServerError::InvalidBlocksInput {
        message: format!("OMP RPC {expected_command} timed out"),
    })
}

fn apply_omp_turn_configuration(
    child: &mut OmpRpcChild,
    model: Option<&str>,
    reasoning: Option<&str>,
) -> Result<()> {
    let model_thinking_level = if let Some(selector) = model {
        let selection = parse_omp_model_selector(selector)?;
        let id = child.next_request_id();
        child.send_command(&set_model_command(
            &id,
            selection.provider,
            selection.model_id,
        ))?;
        drive_omp_config_response(child, &id, "set_model")?;
        selection.thinking_level
    } else {
        None
    };

    if let Some(level) = reasoning.or(model_thinking_level) {
        validate_omp_thinking_level(level)?;
        let id = child.next_request_id();
        child.send_command(&set_thinking_level_command(&id, level))?;
        drive_omp_config_response(child, &id, "set_thinking_level")?;
    }
    Ok(())
}

/// After ready, drain the initial frames to find the session_info_update
/// (emitted by the release binary on startup). This yields the actual session
/// id and name that the release assigned, so a respawn can resume the prior
/// session by its JSONL path rather than a display name the release cannot
/// resolve. Frames are forwarded through the normalizer if possible.
/// After `ready`, send `get_state` to obtain the authoritative session id.
/// Returns `Err` if the response is missing, empty, failed, or times out.
/// The id is persisted to `$OMP_SESSION_DIR/.resident_session_id` so a respawn
/// can resume the prior JSONL.
fn query_and_persist_session_state(
    child: &mut OmpRpcChild,
    event_normalizer: &mut crate::omp::OmpEventNormalizer,
) -> Result<String> {
    let id = child.next_request_id();
    child.send_command(&json!({ "id": id, "type": "get_state" }))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let Some(line) = child.read_line_timeout(Duration::from_millis(100))? else {
            continue;
        };
        let frame = OmpRpcFrame::parse_json_line(&line)?;
        match frame {
            OmpRpcFrame::Response {
                id: resp_id,
                command,
                success,
                data,
                error,
            } if resp_id.as_deref() == Some(id.as_str()) && command == "get_state" => {
                if !success {
                    return Err(HarnessServerError::InvalidBlocksInput {
                        message: format!("get_state failed: {}", error.unwrap_or_default()),
                    });
                }
                let Some(d) = data else {
                    return Err(HarnessServerError::InvalidBlocksInput {
                        message: "get_state returned no data".to_string(),
                    });
                };
                let Some(sid) = d.get("sessionId").and_then(Value::as_str) else {
                    return Err(HarnessServerError::InvalidBlocksInput {
                        message: "get_state missing sessionId".to_string(),
                    });
                };
                if sid.is_empty() {
                    return Err(HarnessServerError::InvalidBlocksInput {
                        message: "get_state returned empty sessionId".to_string(),
                    });
                }
                eprintln!("omp rpc: resident session id={sid}");
                let dir = crate::omp::omp_session_dir();
                std::fs::create_dir_all(&dir)?;
                let marker = dir.join(".resident_session_id");
                std::fs::write(&marker, sid)?;
                return Ok(sid.to_owned());
            }
            OmpRpcFrame::Response { .. } => {}
            OmpRpcFrame::Event(event) => {
                use crate::traits::HarnessServer;
                let _events = crate::omp::OmpHarness.normalize_events(event_normalizer, event)?;
            }
            _ => {}
        }
    }
    Err(HarnessServerError::InvalidBlocksInput {
        message: "get_state timed out".to_string(),
    })
}

/// Build a `prompt` command. During active streaming, `streaming_behavior`
/// must be `"steer"` or `"followUp"` or the prompt fails.
pub fn prompt_command(id: &str, message: &str, streaming_behavior: Option<&str>) -> Value {
    let mut cmd = json!({ "id": id, "type": "prompt", "message": message });
    if let Some(behavior) = streaming_behavior {
        cmd["streamingBehavior"] = Value::String(behavior.to_owned());
    }
    cmd
}

/// Build a `steer` command (queues a steering message during active streaming).
pub fn steer_command(id: &str, message: &str) -> Value {
    json!({ "id": id, "type": "steer", "message": message })
}

/// Build an `abort` command (interrupts the active turn).
pub fn abort_command(id: &str) -> Value {
    json!({ "id": id, "type": "abort" })
}

/// Slash commands the resident host forwards to omp. omp runs its builtin
/// slash commands locally in RPC mode, so every entry must leave no trace
/// beyond the current omp session: no settings, plugin, MCP, SSH, or memory
/// writes, no session rename, move, or delete, no upload. `None` admits every
/// first argument; `Some(list)` admits only those (`""` is the bare command).
const OMP_SLASH_COMMAND_ALLOWLIST: &[(&str, Option<&[&str]>)] = &[
    ("changelog", None),
    ("compact", None),
    ("context", None),
    ("dump", None),
    ("export", None),
    ("fast", None),
    ("force", None),
    ("fresh", None),
    ("jobs", None),
    ("model", None),
    ("prewalk", None),
    ("reload-plugins", None),
    ("shake", None),
    ("todo", None),
    ("tools", None),
    (
        "advisor",
        Some(&["", "toggle", "on", "off", "status", "dump"]),
    ),
    ("browser", Some(&["status"])),
    (
        "marketplace",
        Some(&["list", "installed", "discover", "help"]),
    ),
    ("mcp", Some(&["list", "test", "reconnect"])),
    ("memory", Some(&["", "view", "stats", "diagnose"])),
    ("plugins", Some(&["", "list"])),
    ("session", Some(&["", "info"])),
    ("ssh", Some(&["list"])),
    ("usage", Some(&["", "show"])),
];

/// A prompt omp would execute as a slash command rather than send to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SlashCommand<'a> {
    name: &'a str,
    /// First argument token, empty when the command is bare.
    first_arg: &'a str,
}

/// Recognizes a prompt that starts with `/name`, where `name` is a bare word
/// (letters, digits, `-`) terminated by whitespace, `:` (omp's name/argument
/// separator), or the end of the text. Paths such as `/etc/hosts` are not
/// commands and flow to the model unchanged.
fn parse_slash_command(text: &str) -> Option<SlashCommand<'_>> {
    let rest = text.trim_start().strip_prefix('/')?;
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let tail = &rest[name_end..];
    if !(tail.is_empty() || tail.starts_with(char::is_whitespace) || tail.starts_with(':')) {
        return None;
    }
    let first_arg = tail
        .trim_start_matches(':')
        .split_whitespace()
        .next()
        .unwrap_or("");
    Some(SlashCommand { name, first_arg })
}

fn slash_command_allowed(command: &SlashCommand<'_>) -> bool {
    let name = command.name.to_ascii_lowercase();
    let first_arg = command.first_arg.to_ascii_lowercase();
    OMP_SLASH_COMMAND_ALLOWLIST.iter().any(|(allowed, args)| {
        *allowed == name && args.is_none_or(|args| args.contains(&first_arg.as_str()))
    })
}

/// The host's reply for a slash command it refuses to forward.
fn slash_command_rejection(command: &SlashCommand<'_>) -> String {
    let name = command.name.to_ascii_lowercase();
    if let Some((_, Some(args))) = OMP_SLASH_COMMAND_ALLOWLIST
        .iter()
        .find(|(allowed, args)| *allowed == name && args.is_some())
    {
        let accepted = args
            .iter()
            .map(|arg| {
                if arg.is_empty() {
                    format!("`/{name}`")
                } else {
                    format!("`/{name} {arg}`")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "`/{name} {}` is not available through Centaur because it changes state beyond this omp session. Available: {accepted}.",
            command.first_arg
        );
    }
    let mut names: Vec<&str> = OMP_SLASH_COMMAND_ALLOWLIST
        .iter()
        .map(|(name, _)| *name)
        .collect();
    names.sort_unstable();
    let available = names
        .iter()
        .map(|name| format!("`/{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "`/{}` is not available through Centaur. Slash commands run inside this thread's omp session, so only commands with no effect beyond it are forwarded: {available}.",
        command.name
    )
}

/// Drop ANSI CSI and OSC escape sequences from terminal-oriented command output.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut previous = '\0';
                for c in chars.by_ref() {
                    if c == '\u{07}' || (previous == '\u{1b}' && c == '\\') {
                        break;
                    }
                    previous = c;
                }
            }
            _ => {}
        }
    }
    out
}

fn omp_bridge_config(thread_id: &str, turn_id: &str) -> BridgeConfig {
    let mut config = BridgeConfig::new(thread_id.to_owned(), turn_id.to_owned());
    config.cli_version = "omp".to_string();
    config.model_provider = "omp".to_string();
    config
}

/// Emit a complete turn whose only agent output is `text`, without touching
/// the resident process. Used for slash commands the host refuses to forward.
fn write_local_turn(
    stdout: &mut impl Write,
    thread_id: &str,
    client_user_message_id: Option<String>,
    input: Vec<codex_app_server_protocol::UserInput>,
    text: &str,
) -> Result<()> {
    use crate::traits::{NormalizedContent, NormalizedEvent};

    let turn_id = format!("turn-{}", uuid::Uuid::new_v4().simple());
    let mut normalizer = CodexTurnNormalizer::new(omp_bridge_config(thread_id, &turn_id));
    let mut notifications = normalizer.start_notifications(true)?;
    notifications.extend(normalizer.emit_user_message(client_user_message_id, input)?);
    notifications.extend(
        normalizer.process_event(&NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason: Some("end_turn".to_string()),
            content: vec![NormalizedContent::AgentText {
                item_id: format!("{turn_id}-host"),
                text: text.to_string(),
            }],
        })?,
    );
    notifications.extend(normalizer.finish_turn(None)?);
    for notification in notifications {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    Ok(())
}

/// The resident OMP blocks server. One `omp --mode rpc` process per sandbox,
/// reused across sequential turns. Continuously drains unsolicited
/// session/agent lifecycle frames while ordinary commands are
/// correlated by id. On clean stdin EOF this server waits (bounded) for the
/// child then exits.
pub fn run_omp_blocks_server() -> Result<()> {
    use crate::omp::OmpEventNormalizer;
    use crate::server::{BlocksState, parse_blocks_line_with_state};
    use crate::wire::notification_to_wire_value;
    use std::io::{self, BufRead};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;

    let mut stdout = io::stdout().lock();
    let (input_tx, input_rx) = mpsc::channel::<BlocksCommand>();
    let turn_active = Arc::new(AtomicBool::new(false));

    // stdin reader: parses shared blocks commands (user/interrupt) and sends
    // them through one channel the main loop receives on.
    {
        let turn_active = Arc::clone(&turn_active);
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut blocks_state = BlocksState::default();
            for raw in stdin.lock().lines() {
                let Ok(line) = raw else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match parse_blocks_line_with_state(trimmed, &mut blocks_state) {
                    Ok(BlocksCommand::Interrupt) if turn_active.load(Ordering::SeqCst) => {
                        // Interrupt during an active turn: send as a control
                        // so the turn driver can abort the resident process.
                        if input_tx.send(BlocksCommand::Interrupt).is_err() {
                            break;
                        }
                    }
                    Ok(command @ BlocksCommand::User { .. }) => {
                        turn_active.store(true, Ordering::SeqCst);
                        if input_tx.send(command).is_err() {
                            break;
                        }
                    }
                    Ok(command) => {
                        if input_tx.send(command).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        eprintln!("invalid OMP blocks input: {error}");
                    }
                }
            }
        });
    }

    let mut child: Option<OmpRpcChild> = None;
    let mut event_normalizer = OmpEventNormalizer;
    let thread_id = format!("omp-{}", uuid::Uuid::new_v4().simple());
    let mut harness_session_id: Option<String> = None;

    let mut respawn_child = false;
    let mut outer_pending: std::collections::VecDeque<BlocksCommand> =
        std::collections::VecDeque::new();
    loop {
        let input = if let Some(pending) = outer_pending.pop_front() {
            pending
        } else {
            match input_rx.recv() {
                Ok(input) => input,
                Err(_) => break,
            }
        };
        match input {
            BlocksCommand::User {
                input,
                client_user_message_id,
                model,
                reasoning,
                trace_context,
                ..
            } => {
                // Set turn_active at actual dispatch so concurrent
                // interrupt/steer gates work for pending Users that start
                // after a prior turn cleared the flag.
                turn_active.store(true, Ordering::SeqCst);

                let parts = prompt_text_parts(&input);
                let message = parts.join("\n\n");

                // Detect steering: api-rs sends a user line with
                // trace_metadata.action == "steer_active_execution" to queue
                // additional context during an active turn. Route it as a
                // steer command rather than a new prompt.
                let is_steer = trace_context.metadata.get("action").and_then(Value::as_str)
                    == Some("steer_active_execution");

                // Slash commands execute inside omp itself, so refuse the ones
                // that would outlive this session before the process is even
                // spawned. The refusal is the turn's (or steer's) only output.
                // The user's own text is the last block; api-rs prepends a
                // chat-surface note for console and Slack threads.
                let slash_command = parts.last().and_then(|text| parse_slash_command(text));
                if let Some(command) = &slash_command
                    && !slash_command_allowed(command)
                {
                    let rejection = slash_command_rejection(command);
                    if is_steer {
                        write_blocks_error(&mut stdout, &thread_id, "steer", &rejection)?;
                    } else {
                        write_local_turn(
                            &mut stdout,
                            &thread_id,
                            client_user_message_id,
                            input,
                            &rejection,
                        )?;
                    }
                    turn_active.store(false, Ordering::SeqCst);
                    continue;
                }

                // Spawn or reuse the resident process.
                if child.is_none() {
                    child = Some(OmpRpcChild::spawn()?);
                    drain_ready(child.as_mut().unwrap())?;
                    query_and_persist_session_state(
                        child.as_mut().unwrap(),
                        &mut event_normalizer,
                    )?;
                }
                let child = child.as_mut().unwrap();

                if is_steer {
                    // Steer: send the steer command and wait for its response.
                    // No turn lifecycle — the active turn continues.
                    let id = child.next_request_id();
                    child.send_command(&steer_command(&id, &message))?;
                    drive_omp_steer_response(child, &id, &mut stdout, &thread_id)?;
                    turn_active.store(false, Ordering::SeqCst);
                    continue;
                }
                if let Err(error) =
                    apply_omp_turn_configuration(child, model.as_deref(), reasoning.as_deref())
                {
                    write_blocks_error(
                        &mut stdout,
                        &thread_id,
                        "turn",
                        &format!("OMP turn configuration failed: {error}"),
                    )?;
                    turn_active.store(false, Ordering::SeqCst);
                    continue;
                }

                // Prompt: send and drive the turn to completion. An allowed
                // slash command goes to omp alone: with the chat-surface note
                // in front of it omp's parser would never see the leading `/`.
                let prompt = match (&slash_command, parts.last()) {
                    (Some(_), Some(text)) => text.clone(),
                    _ => message.clone(),
                };
                let id = child.next_request_id();
                child.send_command(&prompt_command(&id, &prompt, None))?;

                let turn_id = format!("turn-{}", uuid::Uuid::new_v4().simple());
                let mut normalizer =
                    CodexTurnNormalizer::new(omp_bridge_config(&thread_id, &turn_id));

                for notification in normalizer.start_notifications(true)? {
                    write_value(&mut stdout, &notification_to_wire_value(&notification)?)?;
                }
                for notification in
                    normalizer.emit_user_message(client_user_message_id, input.clone())?
                {
                    write_value(&mut stdout, &notification_to_wire_value(&notification)?)?;
                }

                let turn_timeout = turn_timeout_from_trace_context(&trace_context);

                let drive_result = drive_omp_turn(
                    child,
                    &mut event_normalizer,
                    &mut normalizer,
                    &mut stdout,
                    &mut harness_session_id,
                    &id,
                    &turn_id,
                    &thread_id,
                    &input_rx,
                    turn_timeout,
                )?;

                turn_active.store(false, Ordering::SeqCst);

                // #4: requeue pending items from the turn driver into the
                // outer VecDeque for FIFO processing before the next recv.
                for input in drive_result.pending {
                    outer_pending.push_back(input);
                }

                // #6: if the child is not reusable, drop it so the next
                // turn spawns a fresh process.
                if !drive_result.child_reusable {
                    // Signal outer scope to respawn. The inner 'child' is
                    // a &mut reference; we kill via Drop by setting a flag.
                    respawn_child = true;
                }
            }
            BlocksCommand::Interrupt => {
                // No turn is active: abort whatever the resident process may still be
                // doing. Without a process there is nothing to abort.
                if let Some(child) = child.as_mut() {
                    let id = child.next_request_id();
                    child.send_command(&abort_command(&id))?;
                }
            }
            BlocksCommand::AttachmentChunk => {}
        }

        // Handle child respawn after a non-reusable turn.
        if respawn_child {
            if let Some(old_child) = child.take() {
                let _ = old_child.wait();
            }
            respawn_child = false;
        }
    }

    // Clean shutdown: close stdin and wait for the process to exit.
    if let Some(child) = child.take() {
        let _ = child.wait();
    }
    Ok(())
}

/// Drive a steer command to its correlated response, draining unsolicited
/// frames (agent events) in the meantime. The steer response
/// is an ack; the active turn continues and its events flow through the
/// turn driver's drain loop.
fn drive_omp_steer_response(
    child: &mut OmpRpcChild,
    expected_id: &str,
    stdout: &mut impl Write,
    thread_id: &str,
) -> Result<()> {
    loop {
        let line = match child.read_line_timeout(Duration::from_secs(30))? {
            Some(line) => line,
            None => continue,
        };
        let frame = OmpRpcFrame::parse_json_line(&line)?;
        match frame {
            OmpRpcFrame::Response {
                id, success, error, ..
            } if id.as_deref() == Some(expected_id) => {
                if !success {
                    let msg = error.unwrap_or_else(|| "steer failed".to_owned());
                    write_blocks_error_with_request_id(
                        stdout,
                        thread_id,
                        "steer",
                        &msg,
                        Some(expected_id),
                    )?;
                }
                return Ok(());
            }
            OmpRpcFrame::Response { .. } => {}
            OmpRpcFrame::Event(_)
            | OmpRpcFrame::PromptResult { .. }
            | OmpRpcFrame::CommandOutput { .. }
            | OmpRpcFrame::Ready
            | OmpRpcFrame::Other(_) => {}
        }
    }
}

fn drain_ready(child: &mut OmpRpcChild) -> Result<()> {
    loop {
        let line = child.read_line()?;
        let frame = OmpRpcFrame::parse_json_line(&line)?;
        match frame {
            OmpRpcFrame::Ready => return Ok(()),
            OmpRpcFrame::Event(_)
            | OmpRpcFrame::PromptResult { .. }
            | OmpRpcFrame::CommandOutput { .. }
            | OmpRpcFrame::Other(_) => {}
            OmpRpcFrame::Response { .. } => {}
        }
    }
}

/// Result of driving an OMP turn. `pending` items are returned to the outer
/// loop for FIFO processing. `child_reusable` is false when the child process
/// is in an unrecoverable state (e.g. timeout abort without clean drain).
struct TurnDriveResult {
    pending: std::collections::VecDeque<BlocksCommand>,
    child_reusable: bool,
}

#[allow(
    clippy::too_many_arguments,
    clippy::single_match,
    clippy::collapsible_if
)]
fn drive_omp_turn(
    child: &mut OmpRpcChild,
    event_normalizer: &mut crate::omp::OmpEventNormalizer,
    normalizer: &mut CodexTurnNormalizer,
    stdout: &mut impl Write,
    harness_session_id: &mut Option<String>,
    expected_prompt_id: &str,
    turn_id: &str,
    thread_id: &str,
    active_rx: &mpsc::Receiver<BlocksCommand>,
    turn_timeout: Duration,
) -> Result<TurnDriveResult> {
    use crate::omp::OmpHarness;
    use crate::traits::{HarnessServer, NormalizedContent, NormalizedEvent};

    let mut pending = std::collections::VecDeque::new();
    let mut terminal = false;
    let mut failed = false;
    let mut aborted = false;
    let mut child_reusable = true;
    let mut prompt_error: Option<String> = None;
    // Output of a builtin slash command omp ran locally: (item id, text so far).
    let mut command_item: Option<(String, String)> = None;
    // Settle window: arm only after a terminal assistant stop.
    let mut settle_deadline: Option<std::time::Instant> = None;
    let absolute_deadline = std::time::Instant::now().checked_add(turn_timeout);

    while !terminal {
        // #5: check deadlines unconditionally, regardless of frame flow.
        let now = std::time::Instant::now();
        if absolute_deadline.is_some_and(|deadline| now >= deadline) {
            eprintln!("omp rpc: absolute turn timeout, terminating");
            let abort_id = child.next_request_id();
            child.send_command(&abort_command(&abort_id))?;
            // #6: drain correlated abort response + agent_end (up to 2s).
            let drain_deadline = std::time::Instant::now() + Duration::from_secs(2);
            let mut got_abort_ack = false;
            let mut got_terminal = false;
            while std::time::Instant::now() < drain_deadline {
                match child.read_line_timeout(Duration::from_millis(50))? {
                    Some(line) => {
                        let frame = OmpRpcFrame::parse_json_line(&line)?;
                        match frame {
                            OmpRpcFrame::Response { id, command, .. }
                                if id.as_deref() == Some(abort_id.as_str())
                                    && command == "abort" =>
                            {
                                got_abort_ack = true;
                            }
                            OmpRpcFrame::Event(event) => {
                                let events =
                                    OmpHarness.normalize_events(event_normalizer, event)?;
                                for normalized in events {
                                    // Require Result (agent_end), not Error.
                                    if matches!(&normalized, NormalizedEvent::Result { .. }) {
                                        got_terminal = true;
                                    }
                                    let _ = normalized;
                                }
                            }
                            _ => {}
                        }
                    }
                    None => {}
                }
                if got_abort_ack && got_terminal {
                    break;
                }
            }
            if !got_abort_ack || !got_terminal {
                child_reusable = false;
            }
            failed = true;
            prompt_error = Some("turn timed out".to_string());
            break;
        }
        if let Some(deadline) = settle_deadline
            && now >= deadline
        {
            eprintln!("omp rpc: settle window expired after terminal stop");
            break;
        }

        // Drain concurrent controls: an interrupt aborts the turn, a steer line
        // queues its text on the active turn, and anything else is preserved in
        // FIFO order for the outer loop.
        loop {
            match active_rx.try_recv() {
                Ok(BlocksCommand::Interrupt) => {
                    let id = child.next_request_id();
                    child.send_command(&abort_command(&id))?;
                    aborted = true;
                }
                Ok(BlocksCommand::User {
                    input,
                    trace_context,
                    ..
                }) if trace_context.metadata.get("action").and_then(Value::as_str)
                    == Some("steer_active_execution") =>
                {
                    let steer_msg = prompt_text(&input);
                    if !steer_msg.is_empty() {
                        let id = child.next_request_id();
                        child.send_command(&steer_command(&id, &steer_msg))?;
                    }
                }
                Ok(other) => {
                    // Preserve unmatched input in FIFO order.
                    pending.push_back(other);
                }
                Err(_) => break,
            }
        }

        let line = match child.read_line_timeout(Duration::from_millis(50))? {
            Some(line) => line,
            None => continue,
        };
        let frame = OmpRpcFrame::parse_json_line(&line)?;
        match frame {
            OmpRpcFrame::Response {
                id,
                command: _,
                success,
                data,
                error,
            } if id.as_deref() == Some(expected_prompt_id) => {
                if !success {
                    // #8: preserve actual error message across scope.
                    prompt_error = error
                        .filter(|e| !e.is_empty())
                        .or(Some("omp prompt failed".to_string()));
                    write_blocks_error(
                        stdout,
                        thread_id,
                        turn_id,
                        prompt_error.as_deref().unwrap(),
                    )?;
                    failed = true;
                    terminal = true;
                    continue;
                }
                let agent_invoked = data
                    .as_ref()
                    .and_then(|d| d.get("agentInvoked").and_then(Value::as_bool))
                    .unwrap_or(true);
                if !agent_invoked {
                    terminal = true;
                }
            }
            OmpRpcFrame::Response { success, error, .. } => {
                if !success {
                    let msg = error.unwrap_or_else(|| "omp command failed".to_owned());
                    eprintln!("omp rpc command failed: {msg}");
                }
            }
            OmpRpcFrame::Event(event) => {
                let events = OmpHarness.normalize_events(event_normalizer, event)?;
                for normalized in events {
                    if let Some(sid) = normalized.session_id() {
                        *harness_session_id = Some(sid.to_string());
                    }
                    for notification in normalizer.process_event(&normalized)? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                    if normalized.is_terminal_assistant_stop() {
                        if settle_deadline.is_none() {
                            settle_deadline =
                                Some(std::time::Instant::now() + Duration::from_secs(5));
                        }
                    }
                    if normalized.is_terminal() {
                        terminal = true;
                    }
                }
            }
            OmpRpcFrame::CommandOutput { text } => {
                let text = strip_ansi(&text);
                let (item_id, buffer) = command_item
                    .get_or_insert_with(|| (format!("{turn_id}-command"), String::new()));
                let delta = if buffer.is_empty() || buffer.ends_with('\n') {
                    text
                } else {
                    format!("\n{text}")
                };
                buffer.push_str(&delta);
                let event = NormalizedEvent::AgentTextDelta {
                    item_id: item_id.clone(),
                    delta,
                };
                for notification in normalizer.process_event(&event)? {
                    write_value(stdout, &notification_to_wire_value(&notification)?)?;
                }
            }
            OmpRpcFrame::PromptResult { agent_invoked, .. } if !agent_invoked => {
                terminal = true;
            }
            OmpRpcFrame::PromptResult { .. } => {}
            OmpRpcFrame::Ready => {}
            OmpRpcFrame::Other(value) => {
                if let Some(kind) = value.get("type").and_then(Value::as_str) {
                    eprintln!("omp rpc: unsolicited {kind} frame");
                }
            }
        }
    }

    // Close the slash-command output item before the turn ends.
    if let Some((item_id, text)) = command_item.take() {
        let event = NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason: Some("end_turn".to_string()),
            content: vec![NormalizedContent::AgentText { item_id, text }],
        };
        for notification in normalizer.process_event(&event)? {
            write_value(stdout, &notification_to_wire_value(&notification)?)?;
        }
    }

    // #7: finish with correct status.
    if aborted && !failed {
        if let Some(notification) = normalizer.finish_turn_interrupted()? {
            write_value(stdout, &notification_to_wire_value(&notification)?)?;
        }
    } else if failed {
        let reason = prompt_error.unwrap_or_else(|| "omp prompt failed".to_string());
        if let Some(notification) = normalizer.finish_turn(Some(reason))? {
            write_value(stdout, &notification_to_wire_value(&notification)?)?;
        }
    } else if let Some(notification) = normalizer.finish_turn(None)? {
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }

    Ok(TurnDriveResult {
        pending,
        child_reusable,
    })
}

/// The text blocks of a user line, one per content block, in order.
fn prompt_text_parts(input: &[codex_app_server_protocol::UserInput]) -> Vec<String> {
    crate::util::user_input_to_anthropic_content(input)
        .into_iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn prompt_text(input: &[codex_app_server_protocol::UserInput]) -> String {
    prompt_text_parts(input).join("\n\n")
}

fn write_blocks_error(
    stdout: &mut impl Write,
    thread_id: &str,
    turn_id: &str,
    message: &str,
) -> Result<()> {
    write_blocks_error_with_request_id(stdout, thread_id, turn_id, message, None)
}

fn write_blocks_error_with_request_id(
    stdout: &mut impl Write,
    thread_id: &str,
    turn_id: &str,
    message: &str,
    request_id: Option<&str>,
) -> Result<()> {
    let mut params = serde_json::json!({
        "error": { "message": message, "codexErrorInfo": null, "additionalDetails": null },
        "willRetry": false,
        "threadId": thread_id,
        "turnId": turn_id,
    });
    if let Some(rid) = request_id {
        params["request_id"] = Value::String(rid.to_owned());
    }
    write_value(
        stdout,
        &serde_json::json!({
            "method": "error",
            "params": params,
        }),
    )
}

fn notification_to_wire_value(
    notification: &codex_app_server_protocol::ServerNotification,
) -> Result<Value> {
    crate::wire::notification_to_wire_value(notification)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omp::{OmpEventNormalizer, OmpHarness};
    use crate::traits::{HarnessServer, NormalizedEvent};

    // --- Frame demultiplexing ---------------------------------------------

    #[test]
    fn ready_frame_parses() {
        let frame = OmpRpcFrame::parse_json_line(r#"{"type":"ready"}"#).unwrap();
        assert!(matches!(frame, OmpRpcFrame::Ready));
    }

    #[test]
    fn response_frame_correlates_by_id() {
        let frame = OmpRpcFrame::parse_json_line(
            r#"{"id":"req_1","type":"response","command":"prompt","success":true,"data":{"agentInvoked":false}}"#,
        )
        .unwrap();
        match frame {
            OmpRpcFrame::Response {
                id,
                command,
                success,
                data,
                error,
            } => {
                assert_eq!(id.as_deref(), Some("req_1"));
                assert_eq!(command, "prompt");
                assert!(success);
                assert!(error.is_none());
                assert_eq!(
                    data.as_ref()
                        .and_then(|d| d.get("agentInvoked").and_then(Value::as_bool)),
                    Some(false)
                );
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn failed_response_carries_error() {
        let frame = OmpRpcFrame::parse_json_line(
            r#"{"id":"req_2","type":"response","command":"set_model","success":false,"error":"Model not found: provider/model"}"#,
        )
        .unwrap();
        match frame {
            OmpRpcFrame::Response { success, error, .. } => {
                assert!(!success);
                assert_eq!(error.as_deref(), Some("Model not found: provider/model"));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn response_without_id_is_still_a_response() {
        // Unknown-command responses echo id: undefined on the wire.
        let frame = OmpRpcFrame::parse_json_line(
            r#"{"type":"response","command":"parse","success":false,"error":"unknown command"}"#,
        )
        .unwrap();
        match frame {
            OmpRpcFrame::Response { id, error, .. } => {
                assert!(id.is_none());
                assert_eq!(error.as_deref(), Some("unknown command"));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn agent_end_frame_routes_through_one_shot_parser() {
        let frame = OmpRpcFrame::parse_json_line(r#"{"type":"agent_end","messages":[]}"#).unwrap();
        match frame {
            OmpRpcFrame::Event(event) => {
                let mut normalizer = OmpEventNormalizer;
                let events = OmpHarness.normalize_events(&mut normalizer, event).unwrap();
                assert!(matches!(
                    events.as_slice(),
                    [NormalizedEvent::Result { error: None }]
                ));
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn message_update_event_normalizes_text_delta() {
        let frame = OmpRpcFrame::parse_json_line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"ONE"},"message":{"role":"assistant","content":[{"type":"text","text":"DONE"}],"responseId":"msg_011CcnPBnPUWzpXCn7915U1v"}}"#,
        )
        .unwrap();
        match frame {
            OmpRpcFrame::Event(event) => {
                let mut normalizer = OmpEventNormalizer;
                let events = OmpHarness.normalize_events(&mut normalizer, event).unwrap();
                assert!(matches!(
                    events.as_slice(),
                    [NormalizedEvent::AgentTextDelta { item_id, delta }]
                        if item_id == "msg_011CcnPBnPUWzpXCn7915U1v" && delta == "ONE"
                ));
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn prompt_result_frame_demultiplexes() {
        let frame = OmpRpcFrame::parse_json_line(
            r#"{"type":"prompt_result","id":"req_1","agentInvoked":false}"#,
        )
        .unwrap();
        match frame {
            OmpRpcFrame::PromptResult { id, agent_invoked } => {
                assert_eq!(id.as_deref(), Some("req_1"));
                assert!(!agent_invoked);
            }
            other => panic!("expected PromptResult, got {other:?}"),
        }
    }

    #[test]
    fn unknown_frame_degrades_to_other_without_erroring() {
        let frame =
            OmpRpcFrame::parse_json_line(r#"{"type":"available_commands_update","commands":[]}"#)
                .unwrap();
        assert!(matches!(frame, OmpRpcFrame::Other(_)));
    }

    #[test]
    fn host_tool_call_frame_degrades_to_other() {
        let frame = OmpRpcFrame::parse_json_line(
            r#"{"type":"host_tool_call","id":"host_1","toolCallId":"toolu_123","toolName":"echo_host","arguments":{"message":"hi"}}"#,
        )
        .unwrap();
        assert!(matches!(frame, OmpRpcFrame::Other(_)));
    }

    // --- Command builders -------------------------------------------------

    #[test]
    fn model_selector_extracts_provider_model_and_thinking_level() {
        assert_eq!(
            parse_omp_model_selector("openai-codex/gpt-5.6-sol:max").unwrap(),
            OmpModelSelector {
                provider: "openai-codex",
                model_id: "gpt-5.6-sol",
                thinking_level: Some("max"),
            }
        );
        assert_eq!(
            parse_omp_model_selector("openrouter/vendor/model:free")
                .unwrap()
                .model_id,
            "vendor/model:free"
        );
    }

    #[test]
    fn model_configuration_commands_match_omp_rpc_contract() {
        assert_eq!(
            set_model_command("model-1", "openai-codex", "gpt-5.6-sol"),
            json!({
                "id": "model-1",
                "type": "set_model",
                "provider": "openai-codex",
                "modelId": "gpt-5.6-sol",
            })
        );
        assert_eq!(
            set_thinking_level_command("thinking-1", "max"),
            json!({
                "id": "thinking-1",
                "type": "set_thinking_level",
                "level": "max",
            })
        );
    }

    #[test]
    fn prompt_command_carries_id_and_optional_streaming_behavior() {
        let cmd = prompt_command("req_1", "hello", None);
        assert_eq!(cmd["type"], "prompt");
        assert_eq!(cmd["id"], "req_1");
        assert_eq!(cmd["message"], "hello");
        assert!(cmd.get("streamingBehavior").is_none());

        let cmd = prompt_command("req_1", "more", Some("steer"));
        assert_eq!(cmd["streamingBehavior"], "steer");
    }

    #[test]
    fn steer_command_builds() {
        let cmd = steer_command("req_2", "also include risks");
        assert_eq!(cmd["type"], "steer");
        assert_eq!(cmd["id"], "req_2");
        assert_eq!(cmd["message"], "also include risks");
    }

    #[test]
    fn abort_command_builds() {
        let cmd = abort_command("req_3");
        assert_eq!(cmd["type"], "abort");
        assert_eq!(cmd["id"], "req_3");
    }

    // --- Room state parsing and API projection ----------------------------

    #[test]
    fn configured_max_duration_extends_the_omp_turn_deadline() {
        let trace_context = crate::otel::TraceContext {
            metadata: std::collections::BTreeMap::from([(
                "max_duration_ms".to_owned(),
                json!(2_700_000),
            )]),
            ..Default::default()
        };

        assert_eq!(
            turn_timeout_from_trace_context(&trace_context),
            Duration::from_millis(2_705_000)
        );
    }

    #[test]
    fn missing_invalid_or_zero_max_duration_keeps_the_default_deadline() {
        assert_eq!(
            turn_timeout_from_trace_context(&crate::otel::TraceContext::default()),
            DEFAULT_OMP_TURN_TIMEOUT
        );
        let invalid = crate::otel::TraceContext {
            metadata: std::collections::BTreeMap::from([(
                "max_duration_ms".to_owned(),
                json!("2700000"),
            )]),
            ..Default::default()
        };
        assert_eq!(
            turn_timeout_from_trace_context(&invalid),
            DEFAULT_OMP_TURN_TIMEOUT
        );
        let zero = crate::otel::TraceContext {
            metadata: std::collections::BTreeMap::from([("max_duration_ms".to_owned(), json!(0))]),
            ..Default::default()
        };
        assert_eq!(
            turn_timeout_from_trace_context(&zero),
            DEFAULT_OMP_TURN_TIMEOUT
        );
    }

    #[test]
    fn read_line_timeout_blank_stream_respects_absolute_deadline() {
        // Bridge emits ready then continuous blank lines. A naive per-line
        // recv_timeout reset would hang forever; absolute remaining budget
        // must return Ok(None) within the caller's slice.
        let bridge =
            std::env::temp_dir().join(format!("omp-blank-bridge-{}.sh", std::process::id()));
        std::fs::write(
            &bridge,
            r#"#!/bin/sh
printf '%s\n' '{"type":"ready"}'
# Flood blank lines forever (no non-empty frames).
while :; do
  printf '\n'
done
"#,
        )
        .expect("write bridge");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bridge).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bridge, perms).unwrap();
        }
        let prev = std::env::var_os("CENTAUR_OMP_RPC_BRIDGE_COMMAND");
        unsafe {
            std::env::set_var("CENTAUR_OMP_RPC_BRIDGE_COMMAND", &bridge);
        }
        let mut child = OmpRpcChild::spawn().expect("spawn blank bridge");
        drain_ready(&mut child).expect("ready");
        let started = std::time::Instant::now();
        let result = child
            .read_line_timeout(Duration::from_millis(150))
            .expect("timeout path is Ok");
        let elapsed = started.elapsed();
        assert!(
            result.is_none(),
            "blank stream must time out, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(800),
            "absolute deadline must bound the call, elapsed={elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(100),
            "should wait near the requested slice, elapsed={elapsed:?}"
        );
        drop(child);
        let _ = std::fs::remove_file(&bridge);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CENTAUR_OMP_RPC_BRIDGE_COMMAND", v),
                None => std::env::remove_var("CENTAUR_OMP_RPC_BRIDGE_COMMAND"),
            }
        }
    }

    // --- Slash commands -----------------------------------------------------

    #[test]
    fn slash_command_parses_bare_word_names_only() {
        assert_eq!(
            parse_slash_command("/context"),
            Some(SlashCommand {
                name: "context",
                first_arg: ""
            })
        );
        assert_eq!(
            parse_slash_command("  /usage reset now"),
            Some(SlashCommand {
                name: "usage",
                first_arg: "reset"
            })
        );
        assert_eq!(
            parse_slash_command("/model:litellm/glm"),
            Some(SlashCommand {
                name: "model",
                first_arg: "litellm/glm"
            })
        );
        assert_eq!(parse_slash_command("/etc/hosts is empty"), None);
        assert_eq!(parse_slash_command("/ context"), None);
        assert_eq!(parse_slash_command("/2fast"), None);
        assert_eq!(parse_slash_command("look at /context"), None);
    }

    #[test]
    fn slash_command_allowlist_admits_session_scoped_commands_only() {
        let allowed = |text: &str| slash_command_allowed(&parse_slash_command(text).unwrap());
        assert!(allowed("/context"));
        assert!(allowed("/compact safe"));
        assert!(allowed("/model litellm/glm-5.2-fp8"));
        assert!(allowed("/mcp list"));
        assert!(allowed("/session"));
        assert!(allowed("/Usage show"));
        assert!(!allowed("/share"));
        assert!(!allowed("/mcp add foo"));
        assert!(!allowed("/session delete"));
        assert!(!allowed("/usage reset"));
        assert!(!allowed("/browser"));
        assert!(!allowed("/rename x"));
        assert!(!allowed("/move /tmp"));
        assert!(!allowed("/new"));
    }

    #[test]
    fn slash_command_rejection_names_the_alternatives() {
        let unknown = slash_command_rejection(&parse_slash_command("/share").unwrap());
        assert!(unknown.starts_with("`/share` is not available through Centaur"));
        assert!(unknown.contains("`/context`") && unknown.contains("`/compact`"));
        assert!(!unknown.contains("`/share`,"));

        let subcommand = slash_command_rejection(&parse_slash_command("/mcp add foo").unwrap());
        assert!(subcommand.starts_with("`/mcp add` is not available through Centaur"));
        assert!(subcommand.contains("`/mcp list`") && subcommand.contains("`/mcp test`"));
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc_sequences() {
        assert_eq!(
            strip_ansi(
                "\u{1b}[1mContext\u{1b}[0m: 12k \u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{7}"
            ),
            "Context: 12k link"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn command_output_frames_are_demultiplexed() {
        let frame =
            OmpRpcFrame::parse_json_line(r#"{"type":"command_output","text":"Context: 12k"}"#)
                .unwrap();
        match frame {
            OmpRpcFrame::CommandOutput { text } => assert_eq!(text, "Context: 12k"),
            other => panic!("expected CommandOutput, got {other:?}"),
        }
    }
}
