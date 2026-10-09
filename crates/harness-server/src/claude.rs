use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use codex_app_server_protocol::UserInput;
use serde_json::json;
use uuid::Uuid;

use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, NormalizedToolResult, Result,
    ThreadState, TurnHold,
    anthropic::{AnthropicEventNormalizer, AnthropicRawStreamEvent, AnthropicStreamEvent},
    command_from_override, user_input_to_anthropic_content,
};

/// Effort levels Claude Code accepts for its `effortLevel` setting.
const CLAUDE_EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Defers agent text until the owning message's fate is known, so agentMessage
/// items can be emitted with an authoritative stop reason. Claude's per-block
/// `assistant` events leave `stop_reason` null, and downstream renderers treat
/// unphased messages as the final answer, so completing each text block as it
/// arrives makes every interim message render as an extra reply. Text is held
/// until a `message_delta` stop reason, a `tool_use` in the same message, a tool
/// result, a newer message, or the terminal `result` settles whether the text was
/// commentary (`tool_use`) or the final answer (`end_turn`). Flushing replays the
/// original delta chunks after an `AgentMessageStarted` carrying the stop reason,
/// so the item starts with the right phase and native chunking is preserved.
///
/// Background subagents (Agent tool with `run_in_background`) outlive the
/// message that launched them: Claude Code emits that turn's `result`, then
/// runs a follow-up turn on its own once a subagent's `task_notification`
/// lands. While main-chain agents are running — or a notification has not
/// yet been answered by a new main-chain message — the turn is held open:
/// terminal text flushes as commentary and the interim `result` is swallowed,
/// so the follow-up's answer is delivered by this turn instead of being
/// drained as stale output before the next one. Each background agent is
/// surfaced as a tool item that starts on `task_started` and completes with
/// the agent's summary on `task_notification`; a resumed agent starts again
/// and gets a new item.
#[derive(Debug, Default)]
pub struct ClaudeEventNormalizer {
    inner: AnthropicEventNormalizer,
    pending: Vec<PendingAgentMessage>,
    /// Main-chain background agents still owed a `task_notification`, by task id.
    background_agents: HashMap<String, BackgroundAgent>,
    awaiting_follow_up: bool,
    /// Claude Code's own turn ended (its `result` was swallowed) and no
    /// follow-up message has started since.
    result_held: bool,
}

#[derive(Debug)]
struct BackgroundAgent {
    item_raw_id: String,
    /// Listed by the latest `background_tasks_changed`. An agent can drop out
    /// just before its notification lands, so it is still owed one.
    live: bool,
}

/// How long a held turn waits for Claude Code's follow-up once no background
/// agent is live. Any output (including API retry notices) restarts it.
const FOLLOW_UP_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
struct PendingAgentMessage {
    item_id: String,
    chunks: Vec<String>,
    canonical: Option<String>,
}

impl PendingAgentMessage {
    fn text(&self) -> String {
        self.canonical
            .clone()
            .unwrap_or_else(|| self.chunks.concat())
    }
}

impl ClaudeEventNormalizer {
    pub fn normalize(&mut self, event: AnthropicStreamEvent) -> Vec<NormalizedEvent> {
        let token_usage = event.token_usage();
        let message_stop_reason = event.message_stop_reason().map(str::to_string);
        let mut out = Vec::new();
        self.track_background_agents(&event, &mut out);
        let normalized = self.inner.normalize(event);
        if let Some(usage) = token_usage {
            out.push(NormalizedEvent::TokenUsage { usage });
        }
        if let Some(stop_reason) = message_stop_reason {
            self.flush_pending(Some(stop_reason), &mut out);
        }

        match normalized {
            NormalizedEvent::AgentTextDelta { item_id, delta } => {
                self.flush_other_messages(&item_id, &mut out);
                self.pending_for(item_id).chunks.push(delta);
            }
            NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason,
                content,
            } => self.defer_assistant_message(stop_reason, content, &mut out),
            NormalizedEvent::ToolResults(results) => {
                self.flush_pending(Some("tool_use".to_string()), &mut out);
                out.push(NormalizedEvent::ToolResults(results));
            }
            NormalizedEvent::Result { error: None } if self.holding_turn() => {
                self.flush_pending(Some("tool_use".to_string()), &mut out);
                self.result_held = true;
            }
            event @ NormalizedEvent::Result { .. } => {
                self.flush_pending(Some("end_turn".to_string()), &mut out);
                out.push(event);
            }
            event @ NormalizedEvent::Error { .. } => {
                self.flush_pending(None, &mut out);
                out.push(event);
            }
            NormalizedEvent::TokenUsage { .. } => {}
            NormalizedEvent::Ignored => {}
            event => out.push(event),
        }
        out
    }

    fn defer_assistant_message(
        &mut self,
        stop_reason: Option<String>,
        content: Vec<NormalizedContent>,
        out: &mut Vec<NormalizedEvent>,
    ) {
        let mut passthrough = Vec::new();
        let mut has_tool_use = false;
        for part in content {
            match part {
                NormalizedContent::AgentText { item_id, text } => {
                    self.flush_other_messages(&item_id, out);
                    self.pending_for(item_id).canonical = Some(text);
                }
                part @ NormalizedContent::ToolUse { .. } => {
                    has_tool_use = true;
                    passthrough.push(part);
                }
                part @ NormalizedContent::ReasoningText { .. } => passthrough.push(part),
            }
        }
        if has_tool_use {
            self.flush_pending(Some("tool_use".to_string()), out);
        }
        if let Some(stop_reason) = stop_reason {
            self.flush_pending(Some(stop_reason), out);
        }
        if !passthrough.is_empty() {
            out.push(NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason: None,
                content: passthrough,
            });
        }
    }

    fn pending_for(&mut self, item_id: String) -> &mut PendingAgentMessage {
        if let Some(index) = self
            .pending
            .iter()
            .position(|message| message.item_id == item_id)
        {
            return &mut self.pending[index];
        }
        self.pending.push(PendingAgentMessage {
            item_id,
            chunks: Vec::new(),
            canonical: None,
        });
        self.pending.last_mut().expect("just pushed")
    }

    /// Text from an older message still pending when a newer message produces
    /// text means the older message ended mid-turn: commentary.
    fn flush_other_messages(&mut self, current_item_id: &str, out: &mut Vec<NormalizedEvent>) {
        if self
            .pending
            .iter()
            .all(|message| message.item_id == current_item_id)
        {
            return;
        }
        let (current, others) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|message| message.item_id == current_item_id);
        self.pending = current;
        flush_messages(others, Some("tool_use".to_string()), out);
    }

    fn flush_pending(&mut self, stop_reason: Option<String>, out: &mut Vec<NormalizedEvent>) {
        // A message that ends the model's turn while background agents still
        // owe a follow-up is commentary, not the final answer; flushing it as
        // `end_turn` would also arm the terminal-stop fallback.
        let stop_reason = match stop_reason {
            Some(reason) if reason != "tool_use" && self.holding_turn() => {
                Some("tool_use".to_string())
            }
            reason => reason,
        };
        flush_messages(std::mem::take(&mut self.pending), stop_reason, out);
    }

    fn holding_turn(&self) -> bool {
        self.awaiting_follow_up || !self.background_agents.is_empty()
    }

    pub fn turn_hold(&self) -> TurnHold {
        if !self.holding_turn() {
            TurnHold::Released
        } else if self.result_held && !self.background_agents.values().any(|agent| agent.live) {
            TurnHold::Idle(FOLLOW_UP_IDLE_TIMEOUT)
        } else {
            TurnHold::Waiting
        }
    }

    fn track_background_agents(
        &mut self,
        event: &AnthropicStreamEvent,
        out: &mut Vec<NormalizedEvent>,
    ) {
        let (subtype, task) = match event {
            AnthropicStreamEvent::System { subtype, task, .. } => (subtype.as_deref(), task),
            // The model is answering with every delivered notification in
            // context, whether mid-turn or in Claude Code's follow-up turn.
            AnthropicStreamEvent::StreamEvent {
                event: AnthropicRawStreamEvent::MessageStart { .. },
                ..
            } => {
                self.awaiting_follow_up = false;
                self.result_held = false;
                return;
            }
            _ => return,
        };
        match subtype {
            Some("task_started") if task.is_backgrounded && task.is_main_chain_agent() => {
                let Some(task_id) = &task.task_id else { return };
                // Unique per run: a resumed agent reuses its task id.
                let item_raw_id = format!("background-agent-{}", Uuid::new_v4().simple());
                self.background_agents.insert(
                    task_id.clone(),
                    BackgroundAgent {
                        item_raw_id: item_raw_id.clone(),
                        live: true,
                    },
                );
                out.push(NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: None,
                    content: vec![NormalizedContent::ToolUse {
                        raw_id: item_raw_id,
                        tool: BACKGROUND_AGENT_TOOL.to_string(),
                        arguments: json!({
                            "description": task.description,
                            "subagent_type": task.subagent_type,
                        }),
                    }],
                });
            }
            Some("task_notification") => {
                // Notifications carry no task type; match the launch by id.
                let Some(task_id) = &task.task_id else { return };
                let Some(agent) = self.background_agents.remove(task_id) else {
                    return;
                };
                self.awaiting_follow_up = true;
                out.push(NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                    tool_use_id: agent.item_raw_id,
                    content: task.summary.clone().unwrap_or_default(),
                    is_error: task.status.as_deref() != Some("completed"),
                    exit_code: None,
                }]));
            }
            Some("background_tasks_changed") => {
                let Some(tasks) = &task.tasks else { return };
                for (task_id, agent) in &mut self.background_agents {
                    agent.live = tasks
                        .iter()
                        .any(|task| task.task_id.as_ref() == Some(task_id));
                }
            }
            _ => {}
        }
    }
}

const BACKGROUND_AGENT_TOOL: &str = "BackgroundAgent";

fn flush_messages(
    pending: Vec<PendingAgentMessage>,
    stop_reason: Option<String>,
    out: &mut Vec<NormalizedEvent>,
) {
    for message in pending {
        let text = message.text();
        if text.is_empty() {
            continue;
        }
        out.push(NormalizedEvent::AgentMessageStarted {
            item_id: message.item_id.clone(),
            stop_reason: stop_reason.clone(),
        });
        out.extend(
            message
                .chunks
                .into_iter()
                .map(|delta| NormalizedEvent::AgentTextDelta {
                    item_id: message.item_id.clone(),
                    delta,
                }),
        );
        out.push(NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason: stop_reason.clone(),
            content: vec![NormalizedContent::AgentText {
                item_id: message.item_id,
                text,
            }],
        });
    }
}

#[derive(Debug, Default)]
pub struct ClaudeCodeHarness;

impl HarnessServer for ClaudeCodeHarness {
    type Event = AnthropicStreamEvent;
    type EventNormalizer = ClaudeEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    fn cli_version(&self) -> &'static str {
        "claude-code"
    }

    /// Empty when no explicit override exists: the model is owned by the
    /// in-image harness config (harness/claude/settings.json), mirroring how
    /// codex reads harness/codex/config.toml. An empty model means
    /// `command_for_turn` omits `--model` so the CLI falls through to
    /// settings.json.
    fn default_model(&self) -> String {
        env::var("CLAUDE_MODEL").unwrap_or_default()
    }

    fn default_model_provider(&self) -> &'static str {
        "anthropic"
    }

    fn command_for_turn(&self, state: &ThreadState) -> ProcessCommand {
        if let Some(command) = command_from_override("CENTAUR_CLAUDE_APP_BRIDGE_COMMAND") {
            return command;
        }

        let bin = env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
        let mut command = ProcessCommand::new(bin);
        command.args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--dangerously-skip-permissions",
            "--permission-mode",
            "bypassPermissions",
        ]);
        if !state.model.is_empty() {
            command.args(["--model", &state.model]);
        }
        if PathBuf::from("AGENTS.md").is_file() {
            command.args(["--append-system-prompt-file", "AGENTS.md"]);
        }
        if let Some(session_id) = &state.harness_session_id {
            command.args(["--resume", session_id]);
        } else {
            command.args(["--session-id", &state.id]);
        }
        command
    }

    fn stdin_for_turn(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        let payload = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": user_input_to_anthropic_content(input),
            },
        });
        let mut bytes = serde_json::to_vec(&payload)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn reasoning_effort(&self, requested: &str) -> Option<String> {
        let effort = requested.trim().to_ascii_lowercase();
        if CLAUDE_EFFORT_LEVELS.contains(&effort.as_str()) {
            return Some(effort);
        }
        eprintln!("ignoring unsupported Claude Code effort level {requested:?}");
        None
    }

    /// The Claude process outlives each turn, so effort is applied in-band
    /// with an `apply_flag_settings` control request ahead of the turn's user
    /// message. A `null` effortLevel restores the configured default.
    fn stdin_for_reasoning_effort(&self, effort: Option<&str>) -> Result<Vec<u8>> {
        let payload = json!({
            "type": "control_request",
            "request_id": format!("effort-{}", Uuid::new_v4().simple()),
            "request": {
                "subtype": "apply_flag_settings",
                "settings": { "effortLevel": effort },
            },
        });
        let mut bytes = serde_json::to_vec(&payload)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event> {
        AnthropicStreamEvent::parse_json_line(line)
    }

    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>> {
        // Subagent sidechains (Task tool) interleave their own messages into
        // the stream, ending with their own `end_turn` while the parent turn
        // keeps running. Letting them through corrupts the pending-text state
        // (their message ids clobber the main chain's) and their stop reasons
        // would settle — and with the stop fallback, terminate — the parent
        // turn. The subagent's report reaches the turn through the main
        // chain's Task tool result.
        if event.is_sidechain() {
            return Ok(Vec::new());
        }
        Ok(normalizer.normalize(event))
    }

    /// Claude Code normally ends a turn with a native `result` line, but
    /// streams have been observed to stop at `message_delta.stop_reason`
    /// without one (leaving the execution hung as "thinking" forever). Wait a
    /// short window for the native result before completing on the stop, so
    /// the trailing `result` is consumed by this turn instead of instantly
    /// terminating the next one.
    fn terminal_assistant_stop_settle(&self) -> Option<Duration> {
        Some(Duration::from_secs(2))
    }

    fn turn_hold(&self, normalizer: &Self::EventNormalizer) -> TurnHold {
        normalizer.turn_hold()
    }
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::UserInput;
    use serde_json::{Value, json};

    use crate::{
        HarnessServer, NormalizedContent, NormalizedEvent, TurnHold,
        anthropic::AnthropicStreamEvent,
    };

    use super::{ClaudeCodeHarness, ClaudeEventNormalizer};

    fn normalize(normalizer: &mut ClaudeEventNormalizer, event: Value) -> Vec<NormalizedEvent> {
        normalizer.normalize(serde_json::from_value(event).unwrap())
    }

    fn agent_texts(events: &[NormalizedEvent]) -> Vec<(Option<String>, String)> {
        events
            .iter()
            .filter_map(|event| match event {
                NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason,
                    content,
                } => {
                    let text = content
                        .iter()
                        .filter_map(|part| match part {
                            NormalizedContent::AgentText { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    (!text.is_empty()).then(|| (stop_reason.clone(), text))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn defers_streamed_text_until_message_delta_settles_stop_reason() {
        let mut normalizer = ClaudeEventNormalizer::default();

        let events = normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_1", "stop_reason": null, "content": []}}}),
        );
        assert!(events.is_empty());

        let events = normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}),
        );
        assert!(events.is_empty());

        // Text deltas are buffered, not forwarded.
        let events = normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Let me check."}}}),
        );
        assert!(events.is_empty());

        // The per-block assistant event records canonical text but stays deferred.
        let events = normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "stop_reason": null, "content": [{"type": "text", "text": "Let me check."}]}}),
        );
        assert!(events.is_empty());

        // The tool_use block settles the message as commentary.
        let events = normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "stop_reason": null, "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "echo hello"}}]}}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("tool_use".to_string()), "Let me check.".to_string())]
        );
        assert!(matches!(
            events.last(),
            Some(NormalizedEvent::AssistantMessage { content, .. })
                if matches!(content.as_slice(), [NormalizedContent::ToolUse { .. }])
        ));

        // message_delta arrives after the blocks; nothing left to flush.
        let events = normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_delta", "delta": {"stop_reason": "tool_use"}}}),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn final_message_flushes_as_end_turn_on_message_delta() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_2", "stop_reason": null, "content": []}}}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "DONE"}}}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_2", "stop_reason": null, "content": [{"type": "text", "text": "DONE"}]}}),
        );

        let events = normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_delta", "delta": {"stop_reason": "end_turn"}}}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("end_turn".to_string()), "DONE".to_string())]
        );
    }

    #[test]
    fn pending_text_flushes_as_final_answer_when_result_arrives_first() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "stop_reason": null, "content": [{"type": "text", "text": "final answer"}]}}),
        );

        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "final answer"}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("end_turn".to_string()), "final answer".to_string())]
        );
        assert!(matches!(
            events.last(),
            Some(NormalizedEvent::Result { error: None })
        ));
    }

    #[test]
    fn tool_result_flushes_pending_text_as_commentary() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "stop_reason": null, "content": [{"type": "text", "text": "Let me check."}]}}),
        );

        let events = normalize(
            &mut normalizer,
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok", "is_error": false}]}}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("tool_use".to_string()), "Let me check.".to_string())]
        );
        assert!(matches!(
            events.last(),
            Some(NormalizedEvent::ToolResults(results)) if results.len() == 1
        ));
    }

    #[test]
    fn newer_message_text_flushes_older_pending_message_as_commentary() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "stop_reason": null, "content": [{"type": "text", "text": "first"}]}}),
        );

        let events = normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_2", "stop_reason": null, "content": [{"type": "text", "text": "second"}]}}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("tool_use".to_string()), "first".to_string())]
        );

        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "second"}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("end_turn".to_string()), "second".to_string())]
        );
    }

    #[test]
    fn flush_replays_original_delta_chunks_behind_phased_item_start() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_1", "stop_reason": null, "content": []}}}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}),
        );
        for chunk in ["hel", "lo ", "world"] {
            normalize(
                &mut normalizer,
                json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": chunk}}}),
            );
        }

        let events = normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_delta", "delta": {"stop_reason": "end_turn"}}}),
        );
        assert!(matches!(
            &events[0],
            NormalizedEvent::AgentMessageStarted { item_id, stop_reason: Some(reason) }
                if item_id == "msg_1" && reason == "end_turn"
        ));
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                NormalizedEvent::AgentTextDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["hel", "lo ", "world"]);
        assert_eq!(
            agent_texts(&events),
            vec![(Some("end_turn".to_string()), "hello world".to_string())]
        );
    }

    #[test]
    fn explicit_assistant_stop_reason_flushes_immediately() {
        let mut normalizer = ClaudeEventNormalizer::default();
        let events = normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "stop_reason": "end_turn", "content": [{"type": "text", "text": "hello"}]}}),
        );
        assert_eq!(
            agent_texts(&events),
            vec![(Some("end_turn".to_string()), "hello".to_string())]
        );
    }

    /// Replays the shape Claude Code 2.1 emits for a background subagent: the
    /// launching turn ends with its own `result`, then the CLI runs a
    /// follow-up turn once the agent's `task_notification` lands.
    #[test]
    fn background_agent_holds_turn_open_until_follow_up_result() {
        let harness = ClaudeCodeHarness;
        let mut normalizer = ClaudeEventNormalizer::default();
        let mut feed = |event: Value| {
            harness
                .normalize_events(&mut normalizer, serde_json::from_value(event).unwrap())
                .unwrap()
        };
        let msg = |id: &str, text: &str| {
            [
                json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": id, "content": []}}}),
                json!({"type": "assistant", "message": {"id": id, "content": [{"type": "text", "text": text}]}}),
                json!({"type": "stream_event", "event": {"type": "message_delta", "delta": {"stop_reason": "end_turn"}}}),
            ]
        };

        feed(
            json!({"type": "assistant", "message": {"id": "msg_1", "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent", "input": {"run_in_background": true}}]}}),
        );
        feed(
            json!({"type": "system", "subtype": "background_tasks_changed", "tasks": [{"task_id": "a1", "task_type": "local_agent"}]}),
        );
        let started = feed(
            json!({"type": "system", "subtype": "task_started", "task_id": "a1", "tool_use_id": "toolu_agent", "description": "Research prices", "subagent_type": "general-purpose", "is_backgrounded": true, "task_type": "local_agent"}),
        );
        let item_raw_id = background_agent_raw_id(&started);
        feed(
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_agent", "content": "Async agent launched successfully."}]}}),
        );

        // The launching turn's closing text is commentary and its result is
        // swallowed, so neither settles the turn.
        let mut events = Vec::new();
        for event in msg("msg_2", "Started research.") {
            events.extend(feed(event));
        }
        events.extend(feed(
            json!({"type": "result", "subtype": "success", "result": "Started research."}),
        ));
        assert_eq!(
            agent_texts(&events),
            vec![(
                Some("tool_use".to_string()),
                "Started research.".to_string()
            )]
        );
        assert!(
            !events
                .iter()
                .any(|event| event.is_terminal() || event.is_terminal_assistant_stop())
        );

        // Sidechain chatter stays invisible.
        assert!(feed(json!({"type": "assistant", "parent_tool_use_id": "toolu_agent", "message": {"id": "msg_side", "stop_reason": "end_turn", "content": [{"type": "text", "text": "PONG"}]}})).is_empty());

        // The live set drops the agent just before its notification lands.
        feed(json!({"type": "system", "subtype": "background_tasks_changed", "tasks": []}));
        let notified = feed(
            json!({"type": "system", "subtype": "task_notification", "task_id": "a1", "tool_use_id": "toolu_agent", "status": "completed", "summary": "Prices are up 4%."}),
        );
        assert!(matches!(
            notified.as_slice(),
            [NormalizedEvent::ToolResults(results)]
                if results[0].tool_use_id == item_raw_id
                    && results[0].content == "Prices are up 4%."
                    && !results[0].is_error
        ));

        // Claude Code's follow-up turn carries the real answer and terminates.
        feed(json!({"type": "system", "subtype": "init", "session_id": "s1"}));
        let mut events = Vec::new();
        for event in msg("msg_3", "Prices are up 4%.") {
            events.extend(feed(event));
        }
        assert_eq!(
            agent_texts(&events),
            vec![(
                Some("end_turn".to_string()),
                "Prices are up 4%.".to_string()
            )]
        );
        let done =
            feed(json!({"type": "result", "subtype": "success", "result": "Prices are up 4%."}));
        assert!(done.iter().any(NormalizedEvent::is_terminal));
    }

    fn background_agent_raw_id(events: &[NormalizedEvent]) -> String {
        match events {
            [NormalizedEvent::AssistantMessage { content, .. }] => match content.as_slice() {
                [NormalizedContent::ToolUse { raw_id, tool, .. }] if tool == "BackgroundAgent" => {
                    raw_id.clone()
                }
                other => panic!("expected a BackgroundAgent tool use, got {other:?}"),
            },
            other => panic!("expected a BackgroundAgent tool use, got {other:?}"),
        }
    }

    #[test]
    fn held_turn_waits_on_live_agents_and_times_out_only_when_idle() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "task_started", "task_id": "a1", "is_backgrounded": true, "task_type": "local_agent"}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "started"}),
        );
        assert_eq!(normalizer.turn_hold(), TurnHold::Waiting);

        // Dropped from the live set without a notification: the follow-up may
        // never come, so the hold becomes bounded.
        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "background_tasks_changed", "tasks": []}),
        );
        assert!(matches!(normalizer.turn_hold(), TurnHold::Idle(_)));

        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "task_notification", "task_id": "a1", "status": "completed"}),
        );
        assert!(matches!(normalizer.turn_hold(), TurnHold::Idle(_)));

        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_2", "content": []}}}),
        );
        assert_eq!(normalizer.turn_hold(), TurnHold::Released);
    }

    #[test]
    fn resumed_agent_holds_again_with_a_new_item_and_duplicate_notifications_are_ignored() {
        let mut normalizer = ClaudeEventNormalizer::default();
        let started = json!({"type": "system", "subtype": "task_started", "task_id": "a1", "is_backgrounded": true, "task_type": "local_agent"});
        let notified = json!({"type": "system", "subtype": "task_notification", "task_id": "a1", "status": "completed"});
        let message_start = json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_2", "content": []}}});

        let first = background_agent_raw_id(&normalize(&mut normalizer, started.clone()));
        normalize(&mut normalizer, notified.clone());
        normalize(&mut normalizer, message_start.clone());
        assert!(normalize(&mut normalizer, notified.clone()).is_empty());
        assert_eq!(normalizer.turn_hold(), TurnHold::Released);

        let resumed = background_agent_raw_id(&normalize(&mut normalizer, started));
        assert_ne!(first, resumed);
        assert_eq!(normalizer.turn_hold(), TurnHold::Waiting);
        assert!(matches!(
            normalize(&mut normalizer, notified).as_slice(),
            [NormalizedEvent::ToolResults(results)] if results[0].tool_use_id == resumed
        ));
    }

    #[test]
    fn subagent_owned_agents_do_not_hold_the_turn() {
        let mut normalizer = ClaudeEventNormalizer::default();
        assert!(
            normalize(
                &mut normalizer,
                json!({"type": "system", "subtype": "task_started", "task_id": "a2", "is_backgrounded": true, "task_type": "local_agent", "owned_by_subagent": true}),
            )
            .is_empty()
        );
        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "done"}),
        );
        assert!(events.iter().any(NormalizedEvent::is_terminal));
    }

    #[test]
    fn unexpected_system_field_types_do_not_fail_the_line() {
        let event = ClaudeCodeHarness
            .parse_stdout_line(r#"{"type":"system","subtype":"status","is_backgrounded":null,"status":{"state":"busy"},"tasks":"none"}"#)
            .unwrap();
        assert!(matches!(event, AnthropicStreamEvent::System { .. }));
    }

    #[test]
    fn background_agent_finishing_before_final_message_still_waits_for_follow_up() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "task_started", "task_id": "a1", "is_backgrounded": true, "task_type": "local_agent"}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_1", "content": []}}}),
        );
        // The notification lands while the final message is generating, so
        // the model has not seen it yet: the CLI will run a follow-up turn.
        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "task_notification", "task_id": "a1", "status": "completed"}),
        );
        normalize(
            &mut normalizer,
            json!({"type": "assistant", "message": {"id": "msg_1", "content": [{"type": "text", "text": "waiting"}]}}),
        );
        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "waiting"}),
        );
        assert!(!events.iter().any(NormalizedEvent::is_terminal));

        normalize(
            &mut normalizer,
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_2", "content": []}}}),
        );
        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "done"}),
        );
        assert!(events.iter().any(NormalizedEvent::is_terminal));
    }

    #[test]
    fn background_shell_tasks_do_not_hold_the_turn() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "task_started", "task_id": "b1", "is_backgrounded": true, "task_type": "local_bash"}),
        );
        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "success", "result": "server started"}),
        );
        assert!(events.iter().any(NormalizedEvent::is_terminal));
    }

    #[test]
    fn failed_result_ends_turn_even_with_background_agents_running() {
        let mut normalizer = ClaudeEventNormalizer::default();
        normalize(
            &mut normalizer,
            json!({"type": "system", "subtype": "task_started", "task_id": "a1", "is_backgrounded": true, "task_type": "local_agent"}),
        );
        let events = normalize(
            &mut normalizer,
            json!({"type": "result", "subtype": "error_during_execution", "is_error": true}),
        );
        assert!(events.iter().any(NormalizedEvent::is_terminal));
        // The agent still owes output, so the server stops it with the process.
        assert_ne!(normalizer.turn_hold(), TurnHold::Released);
    }

    #[test]
    fn steer_stdin_uses_claude_streaming_user_message_shape() {
        let bytes = ClaudeCodeHarness
            .stdin_for_steer(&[UserInput::Text {
                text: "new guidance".to_string(),
                text_elements: Vec::new(),
            }])
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(value["type"], "user");
        assert!(value.get("steer").is_none());
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "new guidance");
    }

    #[test]
    fn normalizes_claude_code_effort_levels() {
        assert_eq!(
            ClaudeCodeHarness.reasoning_effort(" High ").as_deref(),
            Some("high")
        );
        assert_eq!(
            ClaudeCodeHarness.reasoning_effort("max").as_deref(),
            Some("max")
        );
        for effort in ["minimal", "none", "ultra"] {
            assert_eq!(ClaudeCodeHarness.reasoning_effort(effort), None, "{effort}");
        }
    }

    #[test]
    fn effort_is_applied_with_a_flag_settings_control_request() {
        for (effort, level) in [(Some("max"), json!("max")), (None, Value::Null)] {
            let bytes = ClaudeCodeHarness
                .stdin_for_reasoning_effort(effort)
                .unwrap();
            assert_eq!(bytes.last(), Some(&b'\n'));
            let value: Value = serde_json::from_slice(&bytes).unwrap();

            assert_eq!(value["type"], "control_request");
            assert!(value["request_id"].as_str().unwrap().starts_with("effort-"));
            assert_eq!(
                value["request"],
                json!({"subtype": "apply_flag_settings", "settings": {"effortLevel": level}})
            );
        }
    }
}
