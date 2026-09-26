use serde_json::Value;

use crate::Result;
use crate::omp::protocol::{frame_type, is_state_notification, protocol_error};
use crate::traits::{
    NormalizedContent, NormalizedEvent, NormalizedTokenUsage, NormalizedToolResult,
};

pub(super) const MAX_RENDERED_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct OmpEventNormalizer {
    command_output_index: usize,
}

impl OmpEventNormalizer {
    pub(crate) fn normalize(&mut self, frame: &Value) -> Result<Vec<NormalizedEvent>> {
        let kind = frame_type(frame)?;
        let mut events = Vec::new();
        match kind {
            "agent_start" | "turn_start" | "turn_end" | "agent_end" => {}
            "message_start" => {
                // Items start with their first text, as for Claude and Hermes, so an
                // assistant message with only thinking or tool calls opens no item.
                if assistant_message(frame).is_some() {
                    assistant_item_id(frame)?;
                }
            }
            "message_update" => {
                if assistant_message(frame).is_none() {
                    return Ok(events);
                }
                let item_id = assistant_item_id(frame)?;
                let update = frame
                    .get("assistantMessageEvent")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        protocol_error("OMP message_update omitted assistantMessageEvent")
                    })?;
                match update.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(delta) = update.get("delta").and_then(Value::as_str) {
                            events.push(NormalizedEvent::AgentTextDelta {
                                item_id,
                                delta: delta.to_string(),
                            });
                        }
                    }
                    Some("thinking_delta" | "reasoning_delta") => {
                        if let Some(delta) = update.get("delta").and_then(Value::as_str) {
                            let index = update
                                .get("contentIndex")
                                .and_then(Value::as_u64)
                                .and_then(|index| usize::try_from(index).ok())
                                .ok_or_else(|| {
                                    protocol_error("OMP reasoning update omitted contentIndex")
                                })?;
                            events.push(NormalizedEvent::ReasoningTextDelta {
                                item_id: reasoning_item_id(&item_id, index),
                                delta: delta.to_string(),
                            });
                        }
                    }
                    Some(
                        "start" | "text_start" | "text_end" | "thinking_start" | "thinking_end"
                        | "image_end" | "toolcall_start" | "toolcall_delta" | "toolcall_end"
                        | "done" | "error",
                    ) => {}
                    _ => return Err(protocol_error("unknown OMP assistant message update")),
                }
            }
            "message_end" => {
                if let Some(message) = assistant_message(frame) {
                    let item_id = assistant_item_id(frame)?;
                    let content = normalized_content(message, &item_id);
                    if !content.is_empty() {
                        events.push(NormalizedEvent::AssistantMessage {
                            partial: false,
                            stop_reason: message
                                .get("stopReason")
                                .or_else(|| message.get("stop_reason"))
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            content,
                        });
                    }
                    if let Some(usage) = normalized_usage(message) {
                        events.push(NormalizedEvent::TokenUsage { usage });
                    }
                }
            }
            "tool_execution_start" => {
                let raw_id = required_frame_string(frame, "toolCallId")?.to_string();
                let tool = required_frame_string(frame, "toolName")?.to_string();
                events.push(NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: Some("tool_use".to_string()),
                    content: vec![NormalizedContent::ToolUse {
                        raw_id,
                        tool,
                        arguments: frame.get("args").cloned().unwrap_or(Value::Null),
                    }],
                });
            }
            "tool_execution_update" => {}
            "tool_execution_end" => {
                let tool_use_id = required_frame_string(frame, "toolCallId")?.to_string();
                let result = frame.get("result").cloned().unwrap_or(Value::Null);
                events.push(NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                    tool_use_id,
                    content: render_result(&result),
                    is_error: frame
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    exit_code: result
                        .get("exitCode")
                        .or_else(|| result.get("exit_code"))
                        .and_then(Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                }]));
            }
            "command_output" => {
                events = self.command_output(required_frame_string(frame, "text")?, false);
            }
            "notice" => {
                if frame.get("level").and_then(Value::as_str) == Some("error") {
                    let message = frame
                        .get("message")
                        .and_then(Value::as_str)
                        .map(bounded_text)
                        .unwrap_or_else(|| "OMP reported an error notice".to_string());
                    events.push(NormalizedEvent::Error { message });
                }
            }
            "extension_error" => {
                let message = frame
                    .get("message")
                    .or_else(|| frame.get("error"))
                    .and_then(Value::as_str)
                    .map(bounded_text)
                    .unwrap_or_else(|| "OMP extension error".to_string());
                events.push(NormalizedEvent::Error { message });
            }
            "tool_stream_update"
            | "auto_compaction_start"
            | "auto_compaction_end"
            | "auto_retry_start"
            | "retry_fallback_applied"
            | "retry_fallback_succeeded"
            | "ttsr_triggered"
            | "todo_reminder"
            | "todo_auto_clear"
            | "irc_message"
            | "goal_updated"
            | "advisor_yielded" => {}
            _ if is_state_notification(kind) => {}
            _ => return Err(protocol_error(format!("unknown OMP event kind {kind}"))),
        }
        Ok(events)
    }

    pub(crate) fn command_output(
        &mut self,
        output: &str,
        local_only: bool,
    ) -> Vec<NormalizedEvent> {
        if output.is_empty() {
            return Vec::new();
        }
        self.command_output_index += 1;
        let item_id = format!("omp-command-output-{}", self.command_output_index);
        if local_only {
            vec![
                NormalizedEvent::AgentMessageStarted {
                    item_id: item_id.clone(),
                    stop_reason: Some("end_turn".to_string()),
                },
                NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: Some("end_turn".to_string()),
                    content: vec![NormalizedContent::AgentText {
                        item_id,
                        text: output.to_string(),
                    }],
                },
            ]
        } else {
            vec![
                NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: Some("tool_use".to_string()),
                    content: vec![NormalizedContent::ToolUse {
                        raw_id: item_id.clone(),
                        tool: "omp_command".to_string(),
                        arguments: Value::Null,
                    }],
                },
                NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                    tool_use_id: item_id,
                    content: output.to_string(),
                    is_error: false,
                    exit_code: None,
                }]),
            ]
        }
    }
}

fn assistant_message(frame: &Value) -> Option<&serde_json::Map<String, Value>> {
    let message = frame.get("message")?.as_object()?;
    (message.get("role").and_then(Value::as_str) == Some("assistant")).then_some(message)
}

fn assistant_item_id(frame: &Value) -> Result<String> {
    Ok(format!(
        "omp-{}",
        required_frame_string(frame, "messageId")?
    ))
}

/// Streamed reasoning and the final message content share one item per
/// thinking block, keyed by the block's content index.
fn reasoning_item_id(message_item_id: &str, content_index: usize) -> String {
    format!("{message_item_id}-reasoning-{content_index}")
}

fn normalized_content(
    message: &serde_json::Map<String, Value>,
    fallback_id: &str,
) -> Vec<NormalizedContent> {
    let mut normalized = Vec::new();
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return normalized;
    };
    for (index, block) in content.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    normalized.push(NormalizedContent::AgentText {
                        item_id: fallback_id.to_string(),
                        text: text.to_string(),
                    });
                }
            }
            Some("thinking" | "reasoning") => {
                if let Some(text) = block
                    .get("thinking")
                    .or_else(|| block.get("text"))
                    .and_then(Value::as_str)
                {
                    normalized.push(NormalizedContent::ReasoningText {
                        item_id: reasoning_item_id(fallback_id, index),
                        text: text.to_string(),
                    });
                }
            }
            Some("toolCall" | "tool_call") => {
                let raw_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("omp-tool")
                    .to_string();
                let tool = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let arguments = block
                    .get("arguments")
                    .or_else(|| block.get("args"))
                    .cloned()
                    .unwrap_or(Value::Null);
                normalized.push(NormalizedContent::ToolUse {
                    raw_id,
                    tool,
                    arguments,
                });
            }
            _ => {}
        }
    }
    normalized
}

fn normalized_usage(message: &serde_json::Map<String, Value>) -> Option<NormalizedTokenUsage> {
    let usage = message.get("usage")?.as_object()?;
    let normalized = NormalizedTokenUsage {
        model: message
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        input_tokens: usage.get("input").and_then(Value::as_i64),
        output_tokens: usage.get("output").and_then(Value::as_i64),
        cache_creation_input_tokens: usage.get("cacheWrite").and_then(Value::as_i64),
        cache_read_input_tokens: usage.get("cacheRead").and_then(Value::as_i64),
        reasoning_output_tokens: usage.get("reasoning").and_then(Value::as_i64),
        total_tokens: usage.get("totalTokens").and_then(Value::as_i64),
    };
    normalized.has_counts().then_some(normalized)
}

fn render_result(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    if let Some(content) = value.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(blocks) = content.as_array() {
            let text = blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return text;
            }
        }
    }
    serde_json::to_string(value).unwrap_or_else(|_| "[unrenderable OMP tool result]".to_string())
}

fn required_frame_string<'a>(frame: &'a Value, field: &str) -> Result<&'a str> {
    frame
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error(format!("OMP frame field {field} must be a string")))
}

fn bounded_text(text: &str) -> String {
    if text.len() <= MAX_RENDERED_TEXT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_RENDERED_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[OMP output truncated]", &text[..end])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::OmpEventNormalizer;
    use crate::traits::{NormalizedContent, NormalizedEvent};

    fn assistant_frame(kind: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut frame = json!({
            "type": kind,
            "messageId": "msg-1",
            "message": {"role": "assistant", "responseId": "msg_provider"}
        });
        if let (Some(frame), Some(extra)) = (frame.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                if key == "message" {
                    frame["message"]
                        .as_object_mut()
                        .unwrap()
                        .extend(value.as_object().unwrap().clone());
                } else {
                    frame.insert(key.clone(), value.clone());
                }
            }
        }
        frame
    }

    fn item_id(event: &NormalizedEvent) -> &str {
        match event {
            NormalizedEvent::AgentTextDelta { item_id, .. } => item_id,
            NormalizedEvent::AssistantMessage { content, .. } => match &content[0] {
                NormalizedContent::AgentText { item_id, .. } => item_id,
                other => panic!("unexpected content {other:?}"),
            },
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[test]
    fn attributes_messages_to_their_rpc_message_id() {
        let mut normalizer = OmpEventNormalizer::default();
        let mut item_ids = Vec::new();
        for (message_id, text) in [("reply", "hello"), ("injected", "again")] {
            normalizer
                .normalize(&assistant_frame(
                    "message_start",
                    json!({"messageId": message_id}),
                ))
                .unwrap();
            let delta = normalizer
                .normalize(&assistant_frame(
                    "message_update",
                    json!({"messageId": message_id, "assistantMessageEvent": {"type": "text_delta", "delta": text}}),
                ))
                .unwrap();
            assert!(
                matches!(&delta[0], NormalizedEvent::AgentTextDelta { delta, .. } if delta == text)
            );
            let ended = normalizer
                .normalize(&assistant_frame(
                    "message_end",
                    json!({"messageId": message_id, "message": {
                        "content": [{"type": "text", "text": text}],
                        "usage": {"input": 3, "output": 2, "cacheRead": 1, "cacheWrite": 0}
                    }}),
                ))
                .unwrap();
            assert!(
                ended
                    .iter()
                    .any(|event| matches!(event, NormalizedEvent::TokenUsage { .. }))
            );
            let id = item_id(&delta[0]).to_owned();
            assert_eq!(id, format!("omp-{message_id}"));
            assert_eq!(item_id(&ended[0]), id);
            item_ids.push(id);
        }
        assert_ne!(item_ids[0], item_ids[1]);
    }

    #[test]
    fn assistant_update_without_message_id_fails_closed() {
        let mut normalizer = OmpEventNormalizer::default();
        let error = normalizer
            .normalize(&assistant_frame(
                "message_update",
                json!({"messageId": null, "assistantMessageEvent": {"type": "text_delta", "delta": "orphan"}}),
            ))
            .unwrap_err();
        assert!(error.to_string().contains("messageId"));
    }
}
