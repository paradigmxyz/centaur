//! Pi harness: drives `pi --mode rpc`, Pi's long-lived JSONL protocol.
//!
//! One `pi` process serves the thread across turns. A turn is a `prompt`
//! command and ends on `agent_settled`, after Pi's own retries, compaction,
//! and queued steers. Pi persists the session on disk under an id derived from
//! the thread key, so a respawn (after an interrupt, crash, or model switch)
//! resumes it.
//!
//! Codemode is on by default: the model can write a script that calls the
//! other tools, and each nested call renders as its own tool item.
//!
//! Pi reads provider API keys from the environment, where the sandbox holds
//! only iron-proxy placeholders that the proxy rewrites on the wire.

use std::env;
use std::process::Command as ProcessCommand;

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};

use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, NormalizedTokenUsage,
    NormalizedToolResult, Result, ThreadState, user_input_to_anthropic_content,
};

/// Pi's default tools plus codemode.
const TOOLS: &str = "read,bash,edit,write,codemode";
/// Thinking level for turns without a per-turn override.
const DEFAULT_THINKING_LEVEL: &str = "medium";
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

/// Renders each assistant message from `message_end`, ignoring the streamed
/// deltas. Only then is it known whether the text was commentary before a
/// tool call or the final answer (downstream renderers treat unphased text as
/// the answer), and whether the attempt failed, in which case Pi may retry and
/// the partial text must not render at all.
#[derive(Debug, Default)]
pub struct PiEventNormalizer {
    messages: u64,
    error: Option<String>,
}

impl PiEventNormalizer {
    fn normalize(&mut self, event: Value) -> Vec<NormalizedEvent> {
        match event["type"].as_str() {
            Some("response") => self.response(&event),
            Some("message_start") if event["message"]["role"] == "assistant" => {
                self.messages += 1;
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
                    out.push(NormalizedEvent::AgentMessageStarted {
                        item_id: item_id.clone(),
                        stop_reason: stop_reason.clone(),
                    });
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

/// Accepts `provider/id` or a bare `id` from [`MODELS`]. Pi itself also
/// fuzzy-matches, which would make a typo silently run some other model.
fn check_model(model: &str) -> std::result::Result<(), String> {
    if MODELS
        .iter()
        .any(|(provider, id)| model == *id || model == format!("{provider}/{id}"))
    {
        return Ok(());
    }
    // "unsupported model" is one of the phrases Slack clears a thread's sticky
    // model on, so a rejected model doesn't fail every later turn.
    Err(format!(
        "unsupported model `{model}` for Pi; see the supported models in the Pi harness docs"
    ))
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
        let bin = env::var("CENTAUR_PI_BIN").unwrap_or_else(|_| "pi".to_string());
        let mut command = ProcessCommand::new(bin);
        // `--approve` trusts the workspace's project resources, such as the
        // skills the sandbox installs under `.agents/skills`; RPC mode cannot
        // prompt for trust and would skip them.
        command.args(["--mode", "rpc", "--approve", "--session-id"]);
        command.arg(session_id(state));
        if !state.model.is_empty() {
            command.args(["--model", &state.model]);
        }
        // A resumed session keeps its last thinking level; start from the default.
        command.args(["--thinking", DEFAULT_THINKING_LEVEL, "--tools", TOOLS]);
        command
    }

    /// `--model` only applies at startup; the session resumes in the new process.
    fn restart_on_model_change(&self) -> bool {
        true
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
        let level = effort.unwrap_or(DEFAULT_THINKING_LEVEL);
        command_line(json!({"type": "set_thinking_level", "level": level}))
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
            "openai/gpt-5.5",
        ] {
            assert_eq!(check_model(model), Ok(()), "{model}");
        }
        for model in [
            "anthropic/sonnet-5",
            "openai/claude-sonnet-5",
            "openai/o3",
            "openai/gpt-5.5:high",
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
