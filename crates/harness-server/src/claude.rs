use std::env;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use codex_app_server_protocol::UserInput;
use serde_json::json;

use crate::{
    HarnessKind, HarnessServer, NormalizedEvent, Result, ThreadState,
    anthropic::{AnthropicEventNormalizer, AnthropicStreamEvent},
    command_from_override, user_input_to_anthropic_content,
};

const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-4-8";

#[derive(Debug, Default)]
pub struct ClaudeCodeHarness;

impl HarnessServer for ClaudeCodeHarness {
    type Event = AnthropicStreamEvent;
    type EventNormalizer = AnthropicEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    fn cli_version(&self) -> &'static str {
        "claude-code"
    }

    fn default_model(&self) -> String {
        env::var("CLAUDE_MODEL").unwrap_or_else(|_| DEFAULT_CLAUDE_MODEL.to_string())
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
            "--model",
            &state.model,
        ]);
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

    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event> {
        AnthropicStreamEvent::parse_json_line(line)
    }

    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>> {
        Ok(vec![normalizer.normalize(event)])
    }
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::UserInput;
    use serde_json::Value;

    use crate::HarnessServer;

    use super::ClaudeCodeHarness;

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
}
