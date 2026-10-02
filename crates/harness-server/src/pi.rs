//! Pi harness: drives `pi --mode rpc`, Pi's long-lived JSONL protocol.
//!
//! One `pi` process serves the thread across turns. A turn is a `prompt`
//! command and ends on `agent_settled`, after Pi's own retries, compaction,
//! and queued steers. Pi persists the session on disk and the process starts
//! with `--continue`, so a respawn (after an interrupt, crash, or model
//! switch) resumes the same conversation.
//!
//! Pi reads provider API keys from the environment. The sandbox holds only
//! iron-proxy placeholders, which the proxy rewrites on the wire.

use std::env;
use std::process::Command as ProcessCommand;

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};

use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, NormalizedTokenUsage,
    NormalizedToolResult, Result, ThreadState, command_from_override,
    user_input_to_anthropic_content,
};

/// Placeholder API keys iron-proxy replaces with the real credential.
const PLACEHOLDER_API_KEYS: &[&str] = &["ANTHROPIC_API_KEY"];
const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Default)]
pub struct PiHarness;

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
            Some("message_update") => self.message_update(&event["assistantMessageEvent"]),
            Some("message_end") if event["message"]["role"] == "assistant" => {
                self.assistant_message(&event["message"])
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

    fn message_update(&self, update: &Value) -> Vec<NormalizedEvent> {
        let delta = update["delta"].as_str().unwrap_or_default().to_string();
        let item_id = self.item_id(&update["contentIndex"]);
        match update["type"].as_str() {
            Some("text_delta") => vec![NormalizedEvent::AgentTextDelta { item_id, delta }],
            Some("thinking_delta") => vec![NormalizedEvent::ReasoningTextDelta { item_id, delta }],
            _ => Vec::new(),
        }
    }

    fn assistant_message(&mut self, message: &Value) -> Vec<NormalizedEvent> {
        let stop_reason = message["stopReason"].as_str().unwrap_or_default();
        // Pi retries some provider errors itself; only the last message's
        // outcome reaches `agent_settled`.
        self.error = matches!(stop_reason, "error" | "aborted").then(|| {
            message["errorMessage"]
                .as_str()
                .unwrap_or(stop_reason)
                .to_string()
        });

        let content = message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, block)| {
                let item_id = self.item_id(&json!(index));
                let text = || block["text"].as_str().unwrap_or_default().to_string();
                match block["type"].as_str()? {
                    "text" => Some(NormalizedContent::AgentText {
                        item_id,
                        text: text(),
                    }),
                    "thinking" => Some(NormalizedContent::ReasoningText {
                        item_id,
                        text: block["thinking"].as_str().unwrap_or_default().to_string(),
                    }),
                    "toolCall" => Some(NormalizedContent::ToolUse {
                        raw_id: block["id"].as_str()?.to_string(),
                        tool: block["name"].as_str()?.to_string(),
                        arguments: block["arguments"].clone(),
                    }),
                    _ => None,
                }
            })
            .collect();

        let usage = &message["usage"];
        let count = |key: &str| usage[key].as_i64();
        vec![
            NormalizedEvent::TokenUsage {
                usage: NormalizedTokenUsage {
                    model: message["model"].as_str().map(str::to_string),
                    input_tokens: count("input"),
                    output_tokens: count("output"),
                    cache_creation_input_tokens: count("cacheWrite"),
                    cache_read_input_tokens: count("cacheRead"),
                    reasoning_output_tokens: count("reasoning"),
                    total_tokens: count("totalTokens"),
                },
            },
            NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason: match stop_reason {
                    "toolUse" => Some("tool_use".to_string()),
                    "stop" => Some("end_turn".to_string()),
                    _ => None,
                },
                content,
            },
        ]
    }

    fn item_id(&self, content_index: &Value) -> String {
        format!(
            "pi-msg-{}-{}",
            self.messages,
            content_index.as_u64().unwrap_or(0)
        )
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

fn default_thinking_level() -> String {
    env::var("PI_THINKING").unwrap_or_else(|_| "medium".to_string())
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
        env::var("PI_MODEL").unwrap_or_default()
    }

    fn default_model_provider(&self) -> &'static str {
        "pi"
    }

    fn command_for_turn(&self, state: &ThreadState) -> ProcessCommand {
        if let Some(command) = command_from_override("CENTAUR_PI_APP_BRIDGE_COMMAND") {
            return command;
        }

        let bin = env::var("PI_BIN").unwrap_or_else(|_| "pi".to_string());
        let mut command = ProcessCommand::new(bin);
        command.args(["--mode", "rpc", "--continue"]);
        if !state.model.is_empty() {
            command.args(["--model", &state.model]);
        }
        // A resumed session keeps its last thinking level; start from the default.
        command.args(["--thinking", &default_thinking_level()]);
        for key in PLACEHOLDER_API_KEYS {
            if env::var_os(key).is_none() {
                command.env(key, key);
            }
        }
        command
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

    use super::PiEventNormalizer;
    use crate::NormalizedEvent;

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
