use std::env;
use std::io::Write;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use codex_app_server_protocol::UserInput;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::traits::HarnessChild;
use crate::{
    HarnessKind, HarnessServer, NormalizedContent, NormalizedEvent, NormalizedToolResult, Result,
    ThreadState, command_from_override, stable_id, user_input_to_anthropic_content,
};

const ACP_PROTOCOL_VERSION: u64 = 1;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum HermesEvent {
    SessionUpdate(HermesSessionUpdate),
    PromptResponse { result: Value },
    Error { message: String },
    PermissionRequest { id: Value },
    Ignored,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesSessionUpdate {
    pub session_id: String,
    pub update: HermesUpdate,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesUpdate {
    pub session_update: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub status: Option<String>,
}

impl HermesEvent {
    pub fn parse_json_line(line: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(line)?;
        let Some(object) = value.as_object() else {
            return Ok(Self::Ignored);
        };

        if object.get("method").and_then(Value::as_str) == Some("session/update") {
            let params = object.get("params").cloned().ok_or_else(|| {
                crate::HarnessServerError::Protocol("session/update is missing params".to_string())
            })?;
            return Ok(Self::SessionUpdate(serde_json::from_value(params)?));
        }

        if object.get("method").and_then(Value::as_str) == Some("session/request_permission") {
            let id = object.get("id").cloned().ok_or_else(|| {
                crate::HarnessServerError::Protocol("permission request is missing id".to_string())
            })?;
            return Ok(Self::PermissionRequest { id });
        }

        if object.get("id").is_some() {
            if let Some(error) = object.get("error") {
                return Ok(Self::Error {
                    message: error_message(error),
                });
            }
            if let Some(result) = object.get("result") {
                return Ok(Self::PromptResponse {
                    result: result.clone(),
                });
            }
        }

        Ok(Self::Ignored)
    }
}

#[derive(Debug, Default)]
pub struct HermesEventNormalizer {
    session_started: bool,
}

impl HermesEventNormalizer {
    pub fn normalize(&mut self, event: HermesEvent) -> Vec<NormalizedEvent> {
        match event {
            HermesEvent::SessionUpdate(params) => {
                let mut events = Vec::new();
                if !self.session_started {
                    self.session_started = true;
                    events.push(NormalizedEvent::SessionStarted {
                        session_id: Some(params.session_id.clone()),
                    });
                }
                events.extend(normalize_update(&params.session_id, params.update));
                events
            }
            HermesEvent::PromptResponse { result } => {
                let error = result
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .filter(|reason| matches!(*reason, "cancelled" | "refusal"))
                    .map(str::to_owned);
                vec![NormalizedEvent::Result { error }]
            }
            HermesEvent::Error { message } => vec![NormalizedEvent::Error { message }],
            HermesEvent::PermissionRequest { .. } | HermesEvent::Ignored => {
                vec![NormalizedEvent::Ignored]
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct HermesHarness;

impl HarnessServer for HermesHarness {
    type Event = HermesEvent;
    type EventNormalizer = HermesEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::Hermes
    }

    fn cli_version(&self) -> &'static str {
        "hermes-agent"
    }

    fn default_model(&self) -> String {
        env::var("HERMES_MODEL").unwrap_or_default()
    }

    fn default_model_provider(&self) -> &'static str {
        "openrouter"
    }

    fn command_for_turn(&self, _state: &ThreadState) -> ProcessCommand {
        if let Some(command) = command_from_override("CENTAUR_HERMES_COMMAND") {
            return command;
        }
        ProcessCommand::new(env::var("HERMES_BIN").unwrap_or_else(|_| "hermes-acp".to_string()))
    }

    fn prepare_process(&self, process: &mut HarnessChild, state: &mut ThreadState) -> Result<()> {
        let initialize_id = json!("centaur-initialize");
        write_request(
            process,
            initialize_id.clone(),
            "initialize",
            json!({
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "clientCapabilities": {},
                "clientInfo": {
                    "name": "centaur-harness-server",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        wait_for_response(process, &initialize_id)?;

        let session_id_request = json!("centaur-session");
        write_request(
            process,
            session_id_request.clone(),
            "session/new",
            json!({
                "cwd": state.cwd.to_string_lossy(),
                "mcpServers": [],
            }),
        )?;
        let result = wait_for_response(process, &session_id_request)?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                crate::HarnessServerError::Protocol(
                    "Hermes session/new response did not include sessionId".to_string(),
                )
            })?;
        state.harness_session_id = Some(session_id.to_string());
        Ok(())
    }

    fn stdin_for_turn(&self, _input: &[UserInput]) -> Result<Vec<u8>> {
        Err(crate::HarnessServerError::Protocol(
            "Hermes prompts require a native session id".to_string(),
        ))
    }

    fn stdin_for_thread_turn(&self, state: &ThreadState, input: &[UserInput]) -> Result<Vec<u8>> {
        // The ACP server owns the native conversation state. Centaur therefore
        // sends the same session id for every turn instead of replaying history.
        let session_id = state.harness_session_id.as_deref().ok_or_else(|| {
            crate::HarnessServerError::Protocol(
                "Hermes session is not initialized before prompt".to_string(),
            )
        })?;
        session_prompt_stdin(session_id, input)
    }

    fn stdin_for_steer_with_session(
        &self,
        session_id: Option<&str>,
        input: &[UserInput],
    ) -> Result<Vec<u8>> {
        let session_id = session_id.ok_or_else(|| {
            crate::HarnessServerError::Protocol(
                "Hermes steer requires a native session id".to_string(),
            )
        })?;
        session_prompt_stdin(session_id, input)
    }

    fn parse_stdout_line(&self, line: &str) -> Result<Self::Event> {
        HermesEvent::parse_json_line(line)
    }

    fn handle_process_event(&self, process: &mut HarnessChild, event: &Self::Event) -> Result<()> {
        if let HermesEvent::PermissionRequest { id } = event {
            write_value(
                process,
                &json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "outcome": {
                            "outcome": "selected",
                            "optionId": permission_option_id(),
                        },
                    },
                }),
            )?;
        }
        Ok(())
    }

    fn normalize_events(
        &self,
        normalizer: &mut Self::EventNormalizer,
        event: Self::Event,
    ) -> Result<Vec<NormalizedEvent>> {
        Ok(normalizer.normalize(event))
    }
}

fn normalize_update(session_id: &str, update: HermesUpdate) -> Vec<NormalizedEvent> {
    match update.session_update.as_str() {
        "agent_message_chunk" => text_from_content(update.content)
            .filter(|text| !text.is_empty())
            .map(|delta| {
                vec![NormalizedEvent::AgentTextDelta {
                    item_id: stable_id(session_id, "hermes-message"),
                    delta,
                }]
            })
            .unwrap_or_default(),
        "agent_thought_chunk" => text_from_content(update.content)
            .filter(|text| !text.is_empty())
            .map(|delta| {
                vec![NormalizedEvent::ReasoningTextDelta {
                    item_id: stable_id(session_id, "hermes-thought"),
                    delta,
                }]
            })
            .unwrap_or_default(),
        "tool_call" => {
            let Some(tool_call_id) = update.tool_call_id else {
                return Vec::new();
            };
            let tool = update.title.unwrap_or_else(|| "tool".to_string());
            vec![NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason: None,
                content: vec![NormalizedContent::ToolUse {
                    raw_id: tool_call_id,
                    tool,
                    arguments: update.raw_input.unwrap_or_else(|| json!({})),
                }],
            }]
        }
        "tool_call_update" if update.status.as_deref() == Some("completed") => {
            let Some(tool_call_id) = update.tool_call_id else {
                return Vec::new();
            };
            vec![NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                tool_use_id: tool_call_id,
                content: text_from_content(update.content).unwrap_or_default(),
                is_error: false,
                exit_code: None,
            }])]
        }
        _ => Vec::new(),
    }
}

fn session_prompt_stdin(session_id: &str, input: &[UserInput]) -> Result<Vec<u8>> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": format!("centaur-turn-{}", Uuid::new_v4()),
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": user_input_to_anthropic_content(input),
        },
    });
    let mut bytes = serde_json::to_vec(&payload)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn text_from_content(content: Option<Value>) -> Option<String> {
    let value = content?;
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
    })
}

fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Hermes ACP request failed")
        .to_string()
}

fn permission_option_id() -> &'static str {
    match env::var("HERMES_PERMISSION_OUTCOME")
        .unwrap_or_else(|_| "allow_session".to_string())
        .trim()
    {
        "deny" => "deny",
        "allow_once" => "allow_once",
        _ => "allow_session",
    }
}

fn write_request(process: &mut HarnessChild, id: Value, method: &str, params: Value) -> Result<()> {
    write_value(
        process,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }),
    )
}

fn write_value(process: &mut HarnessChild, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut process.stdin, value)?;
    process.stdin.write_all(b"\n")?;
    process.stdin.flush()?;
    Ok(())
}

fn wait_for_response(process: &mut HarnessChild, id: &Value) -> Result<Value> {
    loop {
        let line = process.stdout.recv_timeout(STARTUP_TIMEOUT).map_err(|_| {
            crate::HarnessServerError::Protocol(
                "timed out waiting for Hermes ACP startup response".to_string(),
            )
        })??;
        let value: Value = serde_json::from_str(&line)?;
        if value.get("method").and_then(Value::as_str) == Some("session/request_permission") {
            if let Some(permission_id) = value.get("id") {
                write_value(
                    process,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": permission_id,
                        "result": {
                            "outcome": {
                                "outcome": "selected",
                                "optionId": permission_option_id(),
                            },
                        },
                    }),
                )?;
            }
            continue;
        }
        if value.get("id") != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(crate::HarnessServerError::Protocol(error_message(error)));
        }
        return Ok(value.get("result").cloned().unwrap_or_else(|| json!({})));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_message_chunk_becomes_canonical_text_delta() {
        let mut normalizer = HermesEventNormalizer::default();
        let event = HermesEvent::SessionUpdate(HermesSessionUpdate {
            session_id: "hermes-session".to_string(),
            update: HermesUpdate {
                session_update: "agent_message_chunk".to_string(),
                content: Some(json!({"type": "text", "text": "hello"})),
                tool_call_id: None,
                title: None,
                raw_input: None,
                status: None,
            },
        });

        let normalized = normalizer.normalize(event);
        assert!(matches!(
            normalized.first(),
            Some(NormalizedEvent::SessionStarted { .. })
        ));
        assert!(matches!(
            normalized.get(1),
            Some(NormalizedEvent::AgentTextDelta { delta, .. }) if delta == "hello"
        ));
    }

    #[test]
    fn prompt_response_is_terminal() {
        let mut normalizer = HermesEventNormalizer::default();
        let normalized = normalizer.normalize(HermesEvent::PromptResponse {
            result: json!({"stopReason": "end_turn"}),
        });
        assert!(matches!(
            normalized.as_slice(),
            [NormalizedEvent::Result { error: None }]
        ));
    }

    #[test]
    fn parses_acp_session_update_wire_shape() {
        let event = HermesEvent::parse_json_line(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ok"}}}}"#,
        )
        .unwrap();
        assert!(matches!(event, HermesEvent::SessionUpdate(_)));
    }

    #[test]
    fn tool_call_events_become_canonical_tool_use_and_result() {
        let mut normalizer = HermesEventNormalizer::default();
        let tool_start = HermesEvent::SessionUpdate(HermesSessionUpdate {
            session_id: "hermes-session".to_string(),
            update: HermesUpdate {
                session_update: "tool_call".to_string(),
                content: None,
                tool_call_id: Some("call-1".to_string()),
                title: Some("read_file".to_string()),
                raw_input: Some(json!({"path": "README.md"})),
                status: None,
            },
        });
        let tool_done = HermesEvent::SessionUpdate(HermesSessionUpdate {
            session_id: "hermes-session".to_string(),
            update: HermesUpdate {
                session_update: "tool_call_update".to_string(),
                content: Some(json!({"type": "text", "text": "file body"})),
                tool_call_id: Some("call-1".to_string()),
                title: None,
                raw_input: None,
                status: Some("completed".to_string()),
            },
        });

        let started = normalizer.normalize(tool_start);
        assert!(matches!(
            started.get(1),
            Some(NormalizedEvent::AssistantMessage { content, .. })
                if matches!(
                    content.as_slice(),
                    [NormalizedContent::ToolUse { raw_id, tool, arguments }]
                        if raw_id == "call-1"
                            && tool == "read_file"
                            && arguments == &json!({"path": "README.md"})
                )
        ));

        let finished = normalizer.normalize(tool_done);
        assert!(matches!(
            finished.as_slice(),
            [NormalizedEvent::ToolResults(results)]
                if results.len() == 1
                    && results[0].tool_use_id == "call-1"
                    && results[0].content == "file body"
                    && !results[0].is_error
        ));
    }

    #[test]
    fn prompt_payload_reuses_native_session_id() {
        let bytes = session_prompt_stdin(
            "session-123",
            &[UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(value["method"], "session/prompt");
        assert_eq!(value["params"]["sessionId"], "session-123");
        assert_eq!(value["params"]["prompt"][0]["text"], "hello");
    }
}
