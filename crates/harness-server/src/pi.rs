//! Pi harness: drives `pi --mode rpc`, Pi's long-lived JSONL protocol.
//!
//! One `pi` process serves the thread across turns. A turn is a `prompt`
//! command and ends on `agent_settled`, after Pi's own retries, compaction,
//! and queued steers. An interrupt sends `abort`, which settles the turn and
//! keeps the process. Pi persists the session on disk under an id derived from
//! the thread key, so a respawn (after a crash or model switch) resumes it.
//!
//! Codemode is on by default: the model can write a script that calls the
//! other tools, and each nested call renders as its own tool item.
//!
//! Pi reads provider API keys from the environment, where the sandbox holds
//! only iron-proxy placeholders that the proxy rewrites on the wire.

use std::collections::HashMap;
use std::env;
use std::process::Command as ProcessCommand;

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};

use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, NormalizedTokenUsage,
    NormalizedToolResult, Result, ThreadState, command_from_override,
    user_input_to_anthropic_content,
};

/// Pi's default tools plus codemode; `CENTAUR_PI_TOOLS` replaces the list.
const DEFAULT_TOOLS: &str = "read,bash,edit,write,codemode";
/// Models Centaur supports on Pi, matching the Claude and GPT models the chat
/// ingresses offer. Each needs its provider's `api_key` credential.
const MODELS: &[(&str, &str)] = &[
    ("anthropic", "claude-fable-5"),
    ("anthropic", "claude-haiku-4-5"),
    ("anthropic", "claude-opus-4-7"),
    ("anthropic", "claude-opus-4-8"),
    ("anthropic", "claude-opus-5"),
    ("anthropic", "claude-opus-5-5"),
    ("anthropic", "claude-sonnet-4-6"),
    ("anthropic", "claude-sonnet-5"),
    ("openai", "gpt-5.4"),
    ("openai", "gpt-5.4-mini"),
    ("openai", "gpt-5.4-nano"),
    ("openai", "gpt-5.4-pro"),
    ("openai", "gpt-5.5"),
    ("openai", "gpt-5.5-pro"),
    ("openai", "gpt-5.6-luna"),
    ("openai", "gpt-5.6-sol"),
    ("openai", "gpt-5.6-terra"),
    ("openai", "gpt-6-astra"),
    ("openai", "gpt-6-luna"),
    ("openai", "gpt-6-sol"),
];
const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Default)]
pub struct PiHarness;

/// Holds each assistant message's text until `message_end` reports how it
/// ended. Only then is it known whether the text was commentary before a tool
/// call or the final answer (downstream renderers treat unphased text as the
/// answer), and whether the attempt failed, in which case Pi may retry and the
/// partial text must not render at all.
#[derive(Debug, Default)]
pub struct PiEventNormalizer {
    messages: u64,
    text_chunks: HashMap<u64, Vec<String>>,
    error: Option<String>,
}

impl PiEventNormalizer {
    fn normalize(&mut self, event: Value) -> Vec<NormalizedEvent> {
        match event["type"].as_str() {
            Some("response") => self.response(&event),
            Some("message_start") if event["message"]["role"] == "assistant" => {
                self.messages += 1;
                self.text_chunks.clear();
                Vec::new()
            }
            Some("message_update") => {
                let update = &event["assistantMessageEvent"];
                if update["type"] == "text_delta"
                    && let (Some(index), Some(delta)) =
                        (update["contentIndex"].as_u64(), update["delta"].as_str())
                {
                    self.text_chunks
                        .entry(index)
                        .or_default()
                        .push(delta.to_string());
                }
                Vec::new()
            }
            Some("message_end") if event["message"]["role"] == "assistant" => {
                self.assistant_message(&event["message"])
            }
            // Codemode's nested calls never appear in an assistant message.
            Some("tool_execution_start") if event["parentToolCallId"].is_string() => {
                vec![NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: None,
                    content: vec![NormalizedContent::ToolUse {
                        raw_id: event["toolCallId"].as_str().unwrap_or_default().to_string(),
                        tool: event["toolName"].as_str().unwrap_or_default().to_string(),
                        arguments: event["args"].clone(),
                    }],
                }]
            }
            Some("tool_execution_end") => {
                vec![NormalizedEvent::ToolResults(vec![tool_result(&event)])]
            }
            Some("agent_settled") => vec![NormalizedEvent::Result {
                error: self.error.take(),
            }],
            _ => Vec::new(),
        }
    }

    /// Only the turn's `prompt` decides the turn from its response: a failure
    /// means no run started, and a `handled` prompt runs no agent either.
    fn response(&self, event: &Value) -> Vec<NormalizedEvent> {
        let command = event["command"].as_str().unwrap_or_default();
        if event["success"] == false {
            let error = event["error"].as_str().unwrap_or("command failed");
            if matches!(command, "prompt" | "parse") {
                return vec![NormalizedEvent::Error {
                    message: format!("pi {command}: {error}"),
                }];
            }
            eprintln!("pi {command} failed: {error}");
        } else if command == "prompt" && event["data"]["disposition"] == "handled" {
            return vec![NormalizedEvent::Result { error: None }];
        }
        Vec::new()
    }

    fn assistant_message(&mut self, message: &Value) -> Vec<NormalizedEvent> {
        let usage = &message["usage"];
        let count = |key: &str| usage[key].as_i64();
        let mut out = vec![NormalizedEvent::TokenUsage {
            usage: NormalizedTokenUsage {
                model: message["model"].as_str().map(str::to_string),
                input_tokens: count("input"),
                output_tokens: count("output"),
                cache_creation_input_tokens: count("cacheWrite"),
                cache_read_input_tokens: count("cacheRead"),
                reasoning_output_tokens: count("reasoning"),
                total_tokens: count("totalTokens"),
            },
        }];

        let stop_reason = message["stopReason"].as_str().unwrap_or_default();
        // A failed attempt renders nothing: Pi may retry it, and only the last
        // attempt's outcome reaches `agent_settled`.
        if matches!(stop_reason, "error" | "aborted") {
            self.error = Some(
                message["errorMessage"]
                    .as_str()
                    .unwrap_or(stop_reason)
                    .to_string(),
            );
            return out;
        }
        self.error = None;
        let stop_reason = match stop_reason {
            "toolUse" => Some("tool_use".to_string()),
            "stop" => Some("end_turn".to_string()),
            _ => None,
        };

        let mut content = Vec::new();
        for (index, block) in message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let item_id = format!("pi-msg-{}-{index}", self.messages);
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or_default().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    // Replay the streamed chunks when they match the final text.
                    let chunks = self
                        .text_chunks
                        .remove(&(index as u64))
                        .filter(|chunks| chunks.concat() == text)
                        .unwrap_or_else(|| vec![text.clone()]);
                    out.push(NormalizedEvent::AgentMessageStarted {
                        item_id: item_id.clone(),
                        stop_reason: stop_reason.clone(),
                    });
                    out.extend(
                        chunks
                            .into_iter()
                            .map(|delta| NormalizedEvent::AgentTextDelta {
                                item_id: item_id.clone(),
                                delta,
                            }),
                    );
                    content.push(NormalizedContent::AgentText { item_id, text });
                }
                Some("thinking") => content.push(NormalizedContent::ReasoningText {
                    item_id,
                    text: block["thinking"].as_str().unwrap_or_default().to_string(),
                }),
                Some("toolCall") => {
                    if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                        content.push(NormalizedContent::ToolUse {
                            raw_id: id.to_string(),
                            tool: name.to_string(),
                            arguments: block["arguments"].clone(),
                        });
                    }
                }
                _ => {}
            }
        }
        out.push(NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason,
            content,
        });
        out
    }
}

fn tool_result(event: &Value) -> NormalizedToolResult {
    let result = &event["result"];
    let content = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    NormalizedToolResult {
        tool_use_id: event["toolCallId"].as_str().unwrap_or_default().to_string(),
        content,
        is_error: event["isError"] == true,
        exit_code: result["structuredContent"]["exit_code"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok()),
    }
}

/// The Pi session for this thread: stable across respawns, unlike
/// `--continue`, which would pick up whichever session in the workspace ran
/// last. Pi session ids allow letters, digits, `.`, `_`, and `-`.
fn session_id(state: &ThreadState) -> String {
    let key = state.thread_key.as_deref().unwrap_or(&state.id);
    let sanitized: String = key
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    format!(
        "centaur-{}",
        sanitized.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
    )
}

fn pi_bin() -> String {
    env::var("CENTAUR_PI_BIN").unwrap_or_else(|_| "pi".to_string())
}

/// Accepts `provider/id` or a bare `id` from [`MODELS`], each optionally with
/// Pi's `:<thinking>` suffix. Pi itself also fuzzy-matches, which would make a
/// typo silently run some other model.
fn check_model(model: &str) -> std::result::Result<(), String> {
    let name = model
        .rsplit_once(':')
        .filter(|(_, level)| THINKING_LEVELS.contains(level))
        .map_or(model, |(name, _)| name);
    if MODELS
        .iter()
        .any(|(provider, id)| name == *id || name == format!("{provider}/{id}"))
    {
        return Ok(());
    }
    let needle = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    let suggestions: Vec<String> = MODELS
        .iter()
        .filter(|(_, id)| id.to_ascii_lowercase().contains(&needle))
        .take(5)
        .map(|(provider, id)| format!("{provider}/{id}"))
        .collect();
    // "unsupported model" is one of the phrases Slack clears a thread's sticky
    // model on, so a rejected model doesn't fail every later turn.
    let mut message = format!("unsupported model `{model}` for Pi; use a supported provider/id");
    if !suggestions.is_empty() {
        message.push_str(&format!(", such as {}", suggestions.join(", ")));
    }
    Err(message)
}

fn default_thinking_level() -> String {
    env::var("CENTAUR_PI_THINKING").unwrap_or_else(|_| "medium".to_string())
}

fn command_line(command: Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&command)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn message_text(input: &[UserInput]) -> String {
    user_input_to_anthropic_content(input)
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

impl HarnessServer for PiHarness {
    type Event = Value;
    type EventNormalizer = PiEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::Pi
    }

    fn cli_version(&self) -> &'static str {
        "pi"
    }

    /// `provider/id`. Empty lets Pi choose from the providers it has keys for.
    fn default_model(&self) -> String {
        env::var("CENTAUR_PI_MODEL").unwrap_or_default()
    }

    fn default_model_provider(&self) -> &'static str {
        "pi"
    }

    fn command_for_turn(&self, state: &ThreadState) -> ProcessCommand {
        if let Some(command) = command_from_override("CENTAUR_PI_APP_BRIDGE_COMMAND") {
            return command;
        }

        let mut command = ProcessCommand::new(pi_bin());
        // `--approve` trusts the workspace's project resources, such as the
        // skills the sandbox installs under `.agents/skills`; RPC mode cannot
        // prompt for trust and would skip them.
        command.args(["--mode", "rpc", "--approve", "--session-id"]);
        command.arg(session_id(state));
        if !state.model.is_empty() {
            command.args(["--model", &state.model]);
        }
        // A resumed session keeps its last thinking level; start from the default.
        command.args(["--thinking", &default_thinking_level()]);
        let tools = env::var("CENTAUR_PI_TOOLS").unwrap_or_else(|_| DEFAULT_TOOLS.to_string());
        command.args(["--tools", &tools]);
        command.env("PI_TELEMETRY", "0");
        command.env("PI_SKIP_VERSION_CHECK", "1");
        command
    }

    fn validate_model(&self, model: &str) -> std::result::Result<(), String> {
        check_model(model)
    }

    fn stdin_for_turn(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        command_line(json!({"type": "prompt", "message": message_text(input)}))
    }

    fn stdin_for_steer(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        command_line(json!({"type": "steer", "message": message_text(input)}))
    }

    fn reasoning_effort(&self, requested: &str) -> Option<String> {
        let level = match requested.trim().to_ascii_lowercase().as_str() {
            "none" => "off".to_string(),
            level => level.to_string(),
        };
        if THINKING_LEVELS.contains(&level.as_str()) {
            return Some(level);
        }
        eprintln!("ignoring unsupported Pi thinking level {requested:?}");
        None
    }

    /// Pi clamps the level to the model. The level persists in the session, so
    /// a turn without an override restores the default explicitly.
    fn stdin_for_reasoning_effort(&self, effort: Option<&str>) -> Result<Vec<u8>> {
        let level = effort.map_or_else(default_thinking_level, str::to_string);
        command_line(json!({"type": "set_thinking_level", "level": level}))
    }

    /// `abort` settles the turn (ending with `agent_settled`) and keeps the
    /// process, its session, and the tool processes' cleanup in Pi's hands.
    fn stdin_for_interrupt(&self) -> Option<Vec<u8>> {
        command_line(json!({"type": "abort"})).ok()
    }

    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event> {
        Ok(serde_json::from_str(line)?)
    }

    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>> {
        Ok(normalizer.normalize(event))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{PiEventNormalizer, check_model};
    use crate::NormalizedEvent;

    #[test]
    fn only_supported_models_are_accepted() {
        for model in [
            "anthropic/claude-sonnet-5",
            "claude-sonnet-5",
            "openai/gpt-5.5:high",
        ] {
            assert_eq!(check_model(model), Ok(()), "{model}");
        }
        assert_eq!(
            check_model("anthropic/sonnet-5"),
            Err(
                "unsupported model `anthropic/sonnet-5` for Pi; use a supported provider/id, \
                 such as anthropic/claude-sonnet-5"
                    .to_string()
            )
        );
        for model in [
            "openai/gpt-5.5:ultra",
            "openai/claude-sonnet-5",
            "openai/o3",
        ] {
            assert!(check_model(model).is_err(), "{model}");
        }
    }

    #[test]
    fn retried_provider_error_does_not_fail_the_settled_turn() {
        let mut normalizer = PiEventNormalizer::default();
        let failed = json!({"type": "message_end", "message": {
            "role": "assistant", "content": [], "stopReason": "error",
            "errorMessage": "529 overloaded",
        }});
        let succeeded = json!({"type": "message_end", "message": {
            "role": "assistant", "content": [{"type": "text", "text": "ok"}],
            "stopReason": "stop",
        }});
        let settled = json!({"type": "agent_settled"});

        normalizer.normalize(failed.clone());
        normalizer.normalize(succeeded);
        assert!(matches!(
            normalizer.normalize(settled.clone()).as_slice(),
            [NormalizedEvent::Result { error: None }]
        ));

        normalizer.normalize(failed);
        assert!(matches!(
            normalizer.normalize(settled).as_slice(),
            [NormalizedEvent::Result { error: Some(error) }] if error == "529 overloaded"
        ));
    }
}
