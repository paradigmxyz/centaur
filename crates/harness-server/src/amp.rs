use std::collections::HashSet;
use std::env;
use std::process::Command as ProcessCommand;

use codex_app_server_protocol::UserInput;
use serde_json::json;

use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, Result, ThreadState,
    anthropic::{AnthropicEventNormalizer, AnthropicStreamEvent},
    command_from_override, user_input_to_anthropic_content,
};

const FINAL_TEXT_DELTA_BYTES: usize = 96;

#[derive(Debug, Default)]
pub struct AmpHarness;

#[derive(Debug, Default)]
pub struct AmpEventNormalizer {
    anthropic: AnthropicEventNormalizer,
    pre_final_text_items: HashSet<String>,
}

impl AmpEventNormalizer {
    fn normalize(&mut self, event: AnthropicStreamEvent) -> Vec<NormalizedEvent> {
        let normalized = self.anthropic.normalize(event);
        match normalized {
            NormalizedEvent::AgentTextDelta { ref item_id, delta } => {
                if !delta.is_empty() {
                    self.pre_final_text_items.insert(item_id.clone());
                }
                vec![NormalizedEvent::AgentTextDelta {
                    item_id: item_id.clone(),
                    delta,
                }]
            }
            NormalizedEvent::AssistantMessage {
                partial: true,
                ref content,
                ..
            } => {
                self.record_pre_final_text_items(content);
                vec![normalized]
            }
            NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason,
                content,
            } => self.chunk_final_assistant_message(stop_reason, content),
            event => vec![event],
        }
    }

    fn chunk_final_assistant_message(
        &self,
        stop_reason: Option<String>,
        content: Vec<NormalizedContent>,
    ) -> Vec<NormalizedEvent> {
        let mut out = Vec::new();
        for item in &content {
            if let NormalizedContent::AgentText { item_id, text } = item {
                if self.pre_final_text_items.contains(item_id) {
                    continue;
                }
                out.extend(
                    text_chunks(text).map(|delta| NormalizedEvent::AgentTextDelta {
                        item_id: item_id.clone(),
                        delta,
                    }),
                );
            }
        }
        out.push(NormalizedEvent::AssistantMessage {
            partial: false,
            stop_reason,
            content,
        });
        out
    }

    fn record_pre_final_text_items(&mut self, content: &[NormalizedContent]) {
        for item in content {
            if let NormalizedContent::AgentText { item_id, text } = item
                && !text.is_empty()
            {
                self.pre_final_text_items.insert(item_id.clone());
            }
        }
    }
}

impl HarnessServer for AmpHarness {
    type Event = AnthropicStreamEvent;
    type EventNormalizer = AmpEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::Amp
    }

    fn cli_version(&self) -> &'static str {
        "amp"
    }

    fn default_model(&self) -> String {
        env::var("AMP_MODE").unwrap_or_else(|_| "deep".to_string())
    }

    fn default_model_provider(&self) -> &'static str {
        "amp"
    }

    fn command_for_turn(&self, state: &ThreadState) -> ProcessCommand {
        if let Some(command) = command_from_override("CENTAUR_AMP_APP_BRIDGE_COMMAND") {
            return command;
        }

        let bin = env::var("AMP_BIN").unwrap_or_else(|_| "amp".to_string());
        let mut command = ProcessCommand::new(bin);
        command.args([
            "--no-ide",
            "--no-notifications",
            "--no-color",
            "--dangerously-allow-all",
            "--execute",
            "--stream-json",
            "--stream-json-input",
            "--stream-json-thinking",
            "--mode",
            &state.model,
        ]);
        if let Ok(visibility) = env::var("AMP_THREAD_VISIBILITY")
            && !visibility.trim().is_empty()
        {
            command.args(["--visibility", visibility.trim()]);
        }
        if let Some(session_id) = &state.harness_session_id {
            command.args(["threads", "continue", session_id]);
        }
        command
    }

    fn stdin_for_turn(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        amp_user_stdin(input, false)
    }

    fn stdin_for_steer(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        amp_user_stdin(input, true)
    }

    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event> {
        AnthropicStreamEvent::parse_json_line(line)
    }

    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>> {
        Ok(normalizer.normalize(event))
    }

    fn finish_turn_on_assistant_end_turn(&self) -> bool {
        true
    }
}

fn text_chunks(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for ch in text.chars() {
        chunk.push(ch);
        if chunk.len() >= FINAL_TEXT_DELTA_BYTES {
            chunks.push(std::mem::take(&mut chunk));
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks.into_iter()
}

fn amp_user_stdin(input: &[UserInput], steer: bool) -> Result<Vec<u8>> {
    let payload = if steer {
        json!({
            "type": "user",
            "steer": true,
            "message": {
                "role": "user",
                "content": user_input_to_anthropic_content(input),
            },
        })
    } else {
        let payload = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": user_input_to_anthropic_content(input),
            },
        });
        payload
    };
    let mut bytes = serde_json::to_vec(&payload)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::UserInput;
    use serde_json::Value;

    use crate::HarnessServer;

    use super::AmpHarness;

    #[test]
    fn turn_stdin_is_plain_user_message() {
        let bytes = AmpHarness
            .stdin_for_turn(&[UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }])
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(value["type"], "user");
        assert!(value.get("steer").is_none());
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn steer_stdin_sets_amp_native_steer_flag() {
        let bytes = AmpHarness
            .stdin_for_steer(&[UserInput::Text {
                text: "new guidance".to_string(),
                text_elements: Vec::new(),
            }])
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(value["type"], "user");
        assert_eq!(value["steer"], true);
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "new guidance");
    }
}
