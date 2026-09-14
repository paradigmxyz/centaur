use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use codex_app_server_protocol::UserInput;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, NormalizedTokenUsage,
    NormalizedToolResult, Result, ThreadState, command_from_override, stable_id,
    user_input_to_anthropic_content,
};

#[derive(Debug, Default)]
pub struct OmpHarness;

/// One line of `omp -p --mode json` output. omp runs one agent turn per
/// process: the first line is always `session` (carrying the durable session
/// id, echoed verbatim on `-r` resume), tool loops produce several
/// `turn_start`/`turn_end` pairs, and `agent_end` is the final line before the
/// process exits.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OmpStreamEvent {
    Session {
        id: String,
    },
    AgentStart,
    TurnStart,
    MessageStart,
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: OmpAssistantMessageEvent,
        message: OmpMessageId,
    },
    MessageEnd {
        message: OmpMessage,
    },
    ToolExecutionStart,
    ToolExecutionUpdate,
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        result: Option<OmpToolExecutionResult>,
        #[serde(default, rename = "isError")]
        is_error: bool,
    },
    TurnEnd,
    AgentEnd,
    Error {
        message: Option<String>,
        error: Option<Value>,
    },
    #[serde(other)]
    Unknown,
}

impl OmpStreamEvent {
    pub fn parse_json_line(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line)?)
    }
}

/// The streaming `assistantMessageEvent` payload inside `message_update`.
/// Only text and thinking deltas are consumed; frame boundaries and tool-call
/// streaming carry nothing the normalized events need (the authoritative tool
/// call arrives with the message's `message_end`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OmpAssistantMessageEvent {
    TextStart,
    TextDelta {
        delta: String,
    },
    TextEnd,
    ThinkingStart,
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    ThinkingEnd,
    ToolcallStart,
    ToolcallDelta,
    ToolcallEnd,
    #[serde(other)]
    Unknown,
}

/// The slice of a `message_update`'s partial message the normalizer needs:
/// the provider response id keying text deltas to their message item. The
/// full partial (repeated content, usage, cost) is deliberately not parsed —
/// it is re-sent on every delta line.
#[derive(Debug, Clone, Deserialize)]
pub struct OmpMessageId {
    #[serde(default, rename = "responseId")]
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OmpMessage {
    pub role: String,
    #[serde(default)]
    pub content: OmpMessageContent,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<OmpUsage>,
    #[serde(default, rename = "stopReason")]
    pub stop_reason: Option<String>,
    #[serde(default, rename = "errorMessage")]
    pub error_message: Option<String>,
    #[serde(default, rename = "responseId")]
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OmpMessageContent {
    Blocks(Vec<OmpContentBlock>),
    Text(String),
}

impl Default for OmpMessageContent {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

impl OmpMessage {
    fn token_usage(&self) -> Option<NormalizedTokenUsage> {
        let usage = self.usage.as_ref()?;
        let normalized = NormalizedTokenUsage {
            model: self.model.clone(),
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_creation_input_tokens: usage.cache_write,
            cache_read_input_tokens: usage.cache_read,
            reasoning_output_tokens: None,
            total_tokens: usage.total_tokens,
        };
        normalized.has_counts().then_some(normalized)
    }

    fn into_normalized_content(self) -> Vec<NormalizedContent> {
        let item_id = assistant_item_id(self.response_id.as_deref());
        match self.content {
            OmpMessageContent::Blocks(blocks) => blocks
                .into_iter()
                .enumerate()
                .filter_map(|(index, block)| match block {
                    OmpContentBlock::Text { text } => Some(NormalizedContent::AgentText {
                        item_id: item_id.clone(),
                        text,
                    }),
                    OmpContentBlock::Thinking { thinking } => {
                        Some(NormalizedContent::ReasoningText {
                            item_id: reasoning_item_id(&item_id, index),
                            text: thinking,
                        })
                    }
                    OmpContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(NormalizedContent::ToolUse {
                        raw_id: id,
                        tool: name,
                        arguments,
                    }),
                    OmpContentBlock::Unknown => None,
                })
                .collect(),
            OmpMessageContent::Text(text) => vec![NormalizedContent::AgentText { item_id, text }],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum OmpContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        arguments: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OmpUsage {
    #[serde(default)]
    pub input: Option<i64>,
    #[serde(default)]
    pub output: Option<i64>,
    #[serde(default, rename = "cacheRead")]
    pub cache_read: Option<i64>,
    #[serde(default, rename = "cacheWrite")]
    pub cache_write: Option<i64>,
    #[serde(default, rename = "totalTokens")]
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OmpToolExecutionResult {
    #[serde(default)]
    pub content: Vec<OmpContentBlock>,
}

impl OmpToolExecutionResult {
    fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let OmpContentBlock::Text { text } = block {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }
}

#[derive(Debug, Default)]
pub struct OmpEventNormalizer;

impl OmpEventNormalizer {
    fn normalize(&mut self, event: OmpStreamEvent) -> Vec<NormalizedEvent> {
        match event {
            OmpStreamEvent::Session { id } => vec![NormalizedEvent::SessionStarted {
                session_id: Some(id),
            }],
            OmpStreamEvent::MessageUpdate {
                assistant_message_event,
                message,
            } => {
                let item_id = assistant_item_id(message.response_id.as_deref());
                match assistant_message_event {
                    OmpAssistantMessageEvent::TextDelta { delta } if !delta.is_empty() => {
                        vec![NormalizedEvent::AgentTextDelta { item_id, delta }]
                    }
                    OmpAssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta,
                    } if !delta.is_empty() => {
                        vec![NormalizedEvent::ReasoningTextDelta {
                            item_id: reasoning_item_id(&item_id, content_index),
                            delta,
                        }]
                    }
                    OmpAssistantMessageEvent::Unknown => {
                        eprintln!("omp: ignoring unknown assistantMessageEvent kind");
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            OmpStreamEvent::MessageEnd { message } if message.role == "assistant" => {
                let mut out = Vec::new();
                if let Some(usage) = message.token_usage() {
                    out.push(NormalizedEvent::TokenUsage { usage });
                }
                if message.stop_reason.as_deref() == Some("error") {
                    out.push(NormalizedEvent::Error {
                        message: message
                            .error_message
                            .unwrap_or_else(|| "omp assistant message failed".to_owned()),
                    });
                    return out;
                }
                let stop_reason = message.stop_reason.as_deref().map(normalized_stop_reason);
                out.push(NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason,
                    content: message.into_normalized_content(),
                });
                out
            }
            // user echoes and toolResult messages: the tool result is taken
            // from `tool_execution_end` (which also carries `isError`), so the
            // duplicate toolResult message would double-emit it.
            OmpStreamEvent::MessageEnd { .. } => Vec::new(),
            OmpStreamEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
            } => vec![NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                tool_use_id: tool_call_id,
                content: result
                    .as_ref()
                    .map(OmpToolExecutionResult::text)
                    .unwrap_or_default(),
                is_error,
                exit_code: None,
            }])],
            OmpStreamEvent::AgentEnd => vec![NormalizedEvent::Result { error: None }],
            OmpStreamEvent::Error { message, error } => {
                let message = message
                    .or_else(|| error.as_ref().map(ToString::to_string))
                    .unwrap_or_else(|| "harness error".to_string());
                vec![NormalizedEvent::Error { message }]
            }
            OmpStreamEvent::AgentStart
            | OmpStreamEvent::TurnStart
            | OmpStreamEvent::TurnEnd
            | OmpStreamEvent::MessageStart
            | OmpStreamEvent::ToolExecutionStart
            | OmpStreamEvent::ToolExecutionUpdate => Vec::new(),
            OmpStreamEvent::Unknown => {
                eprintln!("omp: ignoring unknown event type");
                Vec::new()
            }
        }
    }
}

impl HarnessServer for OmpHarness {
    type Event = OmpStreamEvent;
    type EventNormalizer = OmpEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::Omp
    }

    fn cli_version(&self) -> &'static str {
        "omp"
    }

    /// Empty when no explicit override exists: the model is owned by the
    /// in-image harness config (harness/omp/config.yml modelRoles), mirroring
    /// how claude reads harness/claude/settings.json. An empty model means
    /// `command_for_turn` omits `--model` so the CLI falls through to the
    /// agent config dir.
    fn default_model(&self) -> String {
        env::var("OMP_MODEL").unwrap_or_default()
    }

    fn default_model_provider(&self) -> &'static str {
        "omp"
    }

    fn command_for_turn(&self, state: &ThreadState, input: &[UserInput]) -> ProcessCommand {
        if let Some(command) = command_from_override("CENTAUR_OMP_APP_BRIDGE_COMMAND") {
            return command;
        }

        let bin = env::var("OMP_BIN").unwrap_or_else(|_| "omp".to_string());
        let mut command = ProcessCommand::new(bin);
        command.args(["-p", "--mode", "json", "--auto-approve", "--session-dir"]);
        command.arg(omp_session_dir());
        if let Some(session_id) = &state.harness_session_id {
            command.args(["-r", session_id]);
        }
        if !state.model.is_empty() {
            command.args(["--model", &state.model]);
        }
        // The prompt rides argv, not stdin; `--` keeps prompts that start
        // with a dash from being read as flags.
        command.arg("--");
        command.arg(prompt_text(input));
        command
    }

    /// omp takes the prompt as a command argument and never reads stdin in
    /// `-p` mode, so there is nothing to write (an empty write is a no-op).
    fn stdin_for_turn(&self, _input: &[UserInput]) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event> {
        OmpStreamEvent::parse_json_line(line)
    }

    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>> {
        Ok(normalizer.normalize(event))
    }

    /// omp's native terminal event (`agent_end`) is the last line before the
    /// process exits, so waiting for it after the final assistant stop only
    /// adds the process's shutdown time to every turn: complete immediately
    /// (amp pattern). The trailing `turn_end`/`agent_end` lines die with the
    /// per-turn process — the next turn spawns a fresh one.
    fn terminal_assistant_stop_settle(&self) -> Option<Duration> {
        Some(Duration::ZERO)
    }

    /// Persist the mapping in `$OMP_SESSION_DIR/thread-map.json` so it rides
    /// the same storage as the session JSONLs it describes: a resume after
    /// the bridge process (or its host) is replaced can still translate the
    /// bridge id into the omp-issued one. Failures are logged, not fatal —
    /// the in-memory mapping still covers the current process lifetime.
    fn record_session_id(&self, thread_id: &str, session_id: &str) {
        let dir = omp_session_dir();
        if let Err(err) = record_thread_session(&dir, thread_id, session_id) {
            eprintln!(
                "omp: failed to persist session map in {}: {err}",
                dir.display()
            );
        }
    }

    fn resume_session_id(&self, thread_id: &str) -> Option<String> {
        read_thread_map(&omp_session_dir()).remove(thread_id)
    }
}

fn assistant_item_id(raw_id: Option<&str>) -> String {
    stable_id(raw_id.unwrap_or("assistant"), "msg")
}

fn reasoning_item_id(message_id: &str, index: usize) -> String {
    format!("{message_id}-reasoning-{index}")
}

/// Map omp stop reasons onto the anthropic-style values the shared
/// terminal-stop logic understands (`stop` ends the agent run, `toolUse`
/// continues the tool loop). Unrecognized reasons pass through and never
/// settle the turn — `agent_end` (or process exit) ends it instead.
fn normalized_stop_reason(reason: &str) -> String {
    match reason {
        "stop" => "end_turn",
        "toolUse" => "tool_use",
        "length" => "max_tokens",
        other => other,
    }
    .to_string()
}

pub(crate) fn omp_session_dir() -> PathBuf {
    env::var_os("OMP_SESSION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".omp-harness-sessions")
        })
}

const THREAD_MAP_FILE: &str = "thread-map.json";

/// On-disk `thread-map.json` schema: `{"version":1,"threads":{bridge_id:
/// omp_session_id}}`. Versioned so a future shape change can migrate rather
/// than silently misread.
#[derive(Debug, Serialize, Deserialize)]
struct ThreadMapFile {
    version: u32,
    threads: BTreeMap<String, String>,
}

/// Missing, unreadable, or corrupt maps read as empty: the map is a resume
/// optimization, never a reason to fail a turn.
fn read_thread_map(dir: &Path) -> BTreeMap<String, String> {
    let Ok(bytes) = fs::read(dir.join(THREAD_MAP_FILE)) else {
        return BTreeMap::new();
    };
    serde_json::from_slice::<ThreadMapFile>(&bytes)
        .map(|file| file.threads)
        .unwrap_or_default()
}

/// Read-modify-write with an atomic tmp+rename in the same directory, so a
/// crash mid-write can never leave a truncated map for the next reader.
fn record_thread_session(dir: &Path, thread_id: &str, session_id: &str) -> io::Result<()> {
    let mut threads = read_thread_map(dir);
    if threads.get(thread_id).map(String::as_str) == Some(session_id) {
        return Ok(());
    }
    threads.insert(thread_id.to_string(), session_id.to_string());
    fs::create_dir_all(dir)?;
    let payload = serde_json::to_vec_pretty(&ThreadMapFile {
        version: 1,
        threads,
    })
    .map_err(io::Error::other)?;
    let tmp = dir.join(format!("{THREAD_MAP_FILE}.tmp.{}", std::process::id()));
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, dir.join(THREAD_MAP_FILE))
}

fn prompt_text(input: &[UserInput]) -> String {
    let mut prompt = String::new();
    for part in user_input_to_anthropic_content(input) {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if !prompt.is_empty() {
                prompt.push_str("\n\n");
            }
            prompt.push_str(text);
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::UserInput;

    use crate::{HarnessServer, NormalizedContent, NormalizedEvent};

    use super::{
        OmpEventNormalizer, OmpHarness, THREAD_MAP_FILE, read_thread_map, record_thread_session,
    };

    fn normalize(normalizer: &mut OmpEventNormalizer, line: &str) -> Vec<NormalizedEvent> {
        let event = OmpHarness.parse_stdout_line(line).unwrap();
        OmpHarness.normalize_events(normalizer, event).unwrap()
    }

    #[test]
    fn thread_map_reads_empty_when_missing_or_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_thread_map(dir.path()).is_empty());

        std::fs::write(dir.path().join(THREAD_MAP_FILE), b"not json{").unwrap();
        assert!(read_thread_map(dir.path()).is_empty());
    }

    #[test]
    fn thread_map_round_trips_and_accumulates() {
        let dir = tempfile::tempdir().unwrap();
        // Directory creation is part of the write path: point at a child
        // that does not exist yet.
        let map_dir = dir.path().join("omp-sessions");

        record_thread_session(&map_dir, "bridge-a", "omp-1").unwrap();
        record_thread_session(&map_dir, "bridge-b", "omp-2").unwrap();
        // Re-recording the same mapping is a no-op, not an error.
        record_thread_session(&map_dir, "bridge-a", "omp-1").unwrap();

        let map = read_thread_map(&map_dir);
        assert_eq!(map.get("bridge-a").map(String::as_str), Some("omp-1"));
        assert_eq!(map.get("bridge-b").map(String::as_str), Some("omp-2"));
        assert_eq!(map.len(), 2);

        // A remapped thread id overwrites in place.
        record_thread_session(&map_dir, "bridge-a", "omp-3").unwrap();
        assert_eq!(
            read_thread_map(&map_dir)
                .get("bridge-a")
                .map(String::as_str),
            Some("omp-3")
        );

        // On-disk shape matches the documented schema.
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(map_dir.join(THREAD_MAP_FILE)).unwrap()).unwrap();
        assert_eq!(raw["version"], 1);
        assert_eq!(raw["threads"]["bridge-b"], "omp-2");
    }

    #[test]
    fn corrupt_map_is_replaced_on_next_record() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(THREAD_MAP_FILE), b"{\"threads\":42}").unwrap();

        record_thread_session(dir.path(), "bridge-a", "omp-1").unwrap();
        assert_eq!(
            read_thread_map(dir.path())
                .get("bridge-a")
                .map(String::as_str),
            Some("omp-1")
        );
    }

    #[test]
    fn session_line_yields_harness_session_id() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"session","version":3,"id":"019f3bb2-40aa-7000-b7e7-5414136e3b18","timestamp":"2026-07-07T08:29:25.547Z","cwd":"/tmp/omp-p03-test"}"#,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].session_id(),
            Some("019f3bb2-40aa-7000-b7e7-5414136e3b18")
        );
    }

    #[test]
    fn text_delta_streams_agent_text_keyed_by_response_id() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"ONE"},"message":{"role":"assistant","content":[{"type":"text","text":"DONE"}],"responseId":"msg_011CcnPBnPUWzpXCn7915U1v"}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::AgentTextDelta { item_id, delta }]
                if item_id == "msg_011CcnPBnPUWzpXCn7915U1v" && delta == "ONE"
        ));
    }

    #[test]
    fn assistant_message_end_emits_usage_and_tool_use() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"toolu_01CnTRcRztUD24oiuqg4wQq8","name":"bash","arguments":{"command":"echo centaur-tool-test","i":"Running test echo"}}],"provider":"anthropic","model":"claude-opus-4-8","usage":{"input":2,"output":80,"cacheRead":0,"cacheWrite":41021,"totalTokens":41103},"stopReason":"toolUse","responseId":"msg_011CcnPBYXnmZhM1otcyNfq5"}}"#,
        );
        assert!(matches!(
            &events[0],
            NormalizedEvent::TokenUsage { usage }
                if usage.input_tokens == Some(2)
                    && usage.output_tokens == Some(80)
                    && usage.total_tokens == Some(41103)
                    && usage.model.as_deref() == Some("claude-opus-4-8")
        ));
        assert!(matches!(
            &events[1],
            NormalizedEvent::AssistantMessage { partial: false, stop_reason: Some(reason), content }
                if reason == "tool_use"
                    && matches!(
                        content.as_slice(),
                        [NormalizedContent::ToolUse { raw_id, tool, .. }]
                            if raw_id == "toolu_01CnTRcRztUD24oiuqg4wQq8" && tool == "bash"
                    )
        ));
    }

    #[test]
    fn assistant_error_message_end_is_terminal_failure() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_end","message":{"role":"assistant","content":[],"provider":"litellm","model":"glm-5.2-fp8","stopReason":"error","errorStatus":401,"errorId":16781312,"errorMessage":"401 LiteLLM Virtual Key expected"}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::Error { message }]
                if message == "401 LiteLLM Virtual Key expected"
        ));
    }

    #[test]
    fn tool_execution_end_maps_tool_result() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"tool_execution_end","toolCallId":"toolu_01CnTRcRztUD24oiuqg4wQq8","toolName":"bash","result":{"content":[{"type":"text","text":"centaur-tool-test\n\n\nWall time: 0.07 seconds"}],"details":{"timeoutSeconds":300,"wallTimeMs":70.62383399999817}},"isError":false}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::ToolResults(results)]
                if results.len() == 1
                    && results[0].tool_use_id == "toolu_01CnTRcRztUD24oiuqg4wQq8"
                    && results[0].content.starts_with("centaur-tool-test")
                    && !results[0].is_error
        ));
    }

    #[test]
    fn final_assistant_stop_is_terminal_and_agent_end_yields_result() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"DONE"}],"usage":{"input":2,"output":5,"cacheRead":0,"cacheWrite":41333,"totalTokens":41340},"stopReason":"stop","responseId":"msg_011CcnPBnPUWzpXCn7915U1v"}}"#,
        );
        let assistant = events
            .iter()
            .find(|event| matches!(event, NormalizedEvent::AssistantMessage { .. }))
            .expect("assistant message");
        assert!(assistant.is_terminal_assistant_stop());

        let events = normalize(&mut normalizer, r#"{"type":"agent_end","messages":[]}"#);
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::Result { error: None }]
        ));
    }

    #[test]
    fn tool_result_message_end_is_ignored_to_avoid_double_emission() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_end","message":{"role":"toolResult","toolCallId":"toolu_01CnTRcRztUD24oiuqg4wQq8","toolName":"bash","content":[{"type":"text","text":"centaur-tool-test"}],"isError":false}}"#,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn user_message_end_with_string_content_is_ignored() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_end","message":{"role":"user","content":"<system-notice>workflow instructions</system-notice>"}}"#,
        );
        assert!(events.is_empty());
    }

    #[test]
    fn assistant_message_end_with_string_content_emits_text() {
        let mut normalizer = OmpEventNormalizer;
        let events = normalize(
            &mut normalizer,
            r#"{"type":"message_end","message":{"role":"assistant","content":"DONE","stopReason":"stop","responseId":"msg_string_content"}}"#,
        );
        assert!(matches!(
            events.as_slice(),
            [NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason: Some(reason),
                content,
            }] if reason == "end_turn"
                && matches!(
                    content.as_slice(),
                    [NormalizedContent::AgentText { item_id, text }]
                        if item_id == "msg_string_content" && text == "DONE"
                )
        ));
    }

    #[test]
    fn turn_stdin_is_empty() {
        let bytes = OmpHarness
            .stdin_for_turn(&[UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }])
            .unwrap();
        assert!(bytes.is_empty());
    }
}
