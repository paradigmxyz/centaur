use std::io;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command as ProcessCommand};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use codex_app_server_protocol::{ThreadStartParams, Turn, UserInput};
use serde_json::Value;
use uuid::Uuid;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    Codex,
    ClaudeCode,
    Amp,
}

pub struct ThreadState {
    pub id: String,
    pub cwd: PathBuf,
    pub model: String,
    pub model_provider: String,
    pub service_tier: Option<String>,
    pub harness_session_id: Option<String>,
    pub completed_turns: Vec<Turn>,
    pub process: Option<HarnessChild>,
    pub thread_started_sent: bool,
}

pub struct HarnessChild {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: Receiver<io::Result<String>>,
}

impl Drop for HarnessChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl HarnessChild {
    pub fn kill_and_wait(&mut self) -> io::Result<()> {
        let _ = self.child.kill();
        self.child.wait().map(|_| ())
    }
}

pub trait AppServerRuntime {
    fn run_stdio(&self) -> Result<()>;
}

pub trait HarnessServer {
    type Event;
    type EventNormalizer: Default;

    fn kind(&self) -> HarnessKind;
    fn cli_version(&self) -> &'static str;
    fn default_model(&self) -> String;
    fn default_model_provider(&self) -> &'static str;
    fn command_for_turn(&self, state: &ThreadState) -> ProcessCommand;
    fn stdin_for_turn(&self, input: &[UserInput]) -> Result<Vec<u8>>;
    fn stdin_for_steer(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        self.stdin_for_turn(input)
    }
    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event>;
    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>>;
    /// How to treat an assistant message that stops with a terminal stop
    /// reason (`end_turn`, ...) when no native terminal event has arrived.
    /// `None` keeps the turn open until a native result/error (the default).
    /// `Some(window)` completes the turn once the stream stays quiet for
    /// `window` after the stop: a zero window completes immediately (for
    /// streams with no native result event), a nonzero window gives the
    /// harness's own `result` a chance to settle the turn first — and keeps
    /// that trailing `result` from being read as the *next* turn's terminal —
    /// while still completing when the result never comes.
    fn terminal_assistant_stop_settle(&self) -> Option<Duration> {
        None
    }

    fn thread_state(&self, params: &ThreadStartParams, cwd: PathBuf) -> ThreadState {
        let model = params.model.clone().unwrap_or_else(|| self.default_model());
        let model_provider = params
            .model_provider
            .clone()
            .unwrap_or_else(|| self.default_model_provider().to_string());
        ThreadState {
            id: Uuid::new_v4().to_string(),
            cwd,
            model,
            model_provider,
            service_tier: params.service_tier.clone().flatten(),
            harness_session_id: None,
            completed_turns: Vec::new(),
            process: None,
            thread_started_sent: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum NormalizedEvent {
    SessionStarted {
        session_id: Option<String>,
    },
    /// Announces an agent message before its text deltas so the item starts with
    /// the phase implied by `stop_reason` (deltas carry no phase of their own).
    AgentMessageStarted {
        item_id: String,
        stop_reason: Option<String>,
    },
    AssistantMessage {
        partial: bool,
        stop_reason: Option<String>,
        content: Vec<NormalizedContent>,
    },
    AgentTextDelta {
        item_id: String,
        delta: String,
    },
    ReasoningTextDelta {
        item_id: String,
        delta: String,
    },
    ToolResults(Vec<NormalizedToolResult>),
    TokenUsage {
        usage: NormalizedTokenUsage,
    },
    Result {
        error: Option<String>,
    },
    Error {
        message: String,
    },
    Ignored,
}

impl NormalizedEvent {
    pub(crate) fn session_id(&self) -> Option<&str> {
        match self {
            Self::SessionStarted {
                session_id: Some(session_id),
            } => Some(session_id),
            _ => None,
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Result { .. } | Self::Error { .. })
    }

    pub(crate) fn token_usage(&self) -> Option<&NormalizedTokenUsage> {
        match self {
            Self::TokenUsage { usage } => Some(usage),
            _ => None,
        }
    }

    pub(crate) fn is_terminal_assistant_stop(&self) -> bool {
        matches!(
            self,
            Self::AssistantMessage {
                partial: false,
                stop_reason: Some(stop_reason),
                ..
            } if is_terminal_assistant_stop_reason(stop_reason)
        )
    }
}

fn is_terminal_assistant_stop_reason(reason: &str) -> bool {
    matches!(
        reason,
        "end_turn" | "stop_sequence" | "max_tokens" | "refusal"
    )
}

#[derive(Debug, Clone)]
pub enum NormalizedContent {
    AgentText {
        item_id: String,
        text: String,
    },
    ReasoningText {
        item_id: String,
        text: String,
    },
    ToolUse {
        raw_id: String,
        tool: String,
        arguments: Value,
    },
}

#[derive(Debug, Clone)]
pub struct NormalizedToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedTokenUsage {
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

impl NormalizedTokenUsage {
    pub(crate) fn has_counts(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_creation_input_tokens.is_some()
            || self.cache_read_input_tokens.is_some()
            || self.reasoning_output_tokens.is_some()
            || self.total_tokens.is_some()
    }
}

#[derive(Debug)]
pub struct AppServerNormalizer<D> {
    harness: D,
}

impl<D> AppServerNormalizer<D> {
    pub fn new(harness: D) -> Self {
        Self { harness }
    }
}

impl<D> AppServerRuntime for AppServerNormalizer<D>
where
    D: HarnessServer,
{
    fn run_stdio(&self) -> Result<()> {
        crate::server::run_app_server(&self.harness)
    }
}
