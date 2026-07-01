use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    NormalizedContent, NormalizedEvent, NormalizedTokenUsage, NormalizedToolResult, Result,
    stable_id,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamEvent {
    System {
        subtype: Option<String>,
        session_id: Option<String>,
    },
    Assistant {
        #[serde(default)]
        is_partial: bool,
        message: AnthropicMessage,
    },
    User {
        message: AnthropicMessage,
        tool_use_result: Option<Value>,
    },
    StreamEvent {
        event: AnthropicRawStreamEvent,
    },
    Result {
        subtype: Option<String>,
        result: Option<String>,
        #[serde(default)]
        is_error: bool,
        error: Option<Value>,
        message: Option<String>,
        usage: Option<Value>,
    },
    Error {
        error: Option<Value>,
        message: Option<String>,
        result: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

impl AnthropicStreamEvent {
    pub fn parse_json_line(line: &str) -> Result<Self> {
        Ok(serde_json::from_str(line)?)
    }

    /// The authoritative end-of-message stop reason, carried by the raw
    /// `message_delta` stream event (`tool_use`, `end_turn`, ...). The per-block
    /// `assistant` events leave `stop_reason` null, so this is the only in-stream
    /// signal of whether a message is interim commentary or the final answer.
    pub fn message_stop_reason(&self) -> Option<&str> {
        match self {
            Self::StreamEvent {
                event:
                    AnthropicRawStreamEvent::MessageDelta {
                        delta: Some(delta), ..
                    },
            } => delta.stop_reason.as_deref(),
            _ => None,
        }
    }

    pub fn token_usage(&self) -> Option<NormalizedTokenUsage> {
        match self {
            Self::Assistant { message, .. } => {
                token_usage_from_value(message.usage.as_ref(), message.model.clone())
            }
            Self::Result { usage, .. } => token_usage_from_value(usage.as_ref(), None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicRawStreamEvent {
    MessageStart {
        message: AnthropicMessage,
    },
    ContentBlockStart {
        index: usize,
        content_block: AnthropicContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: AnthropicContentDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageStop,
    MessageDelta {
        #[serde(default)]
        delta: Option<AnthropicMessageDeltaBody>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessageDeltaBody {
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentDelta {
    TextDelta {
        #[serde(default)]
        text: String,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone)]
enum StreamBlockKind {
    Text,
    Reasoning,
    Other,
}

#[derive(Debug, Default)]
pub struct AnthropicEventNormalizer {
    current_message_item_id: Option<String>,
    content_blocks: HashMap<usize, StreamBlockKind>,
}

impl AnthropicEventNormalizer {
    pub fn normalize(&mut self, event: AnthropicStreamEvent) -> NormalizedEvent {
        match event {
            AnthropicStreamEvent::StreamEvent { event } => self.normalize_stream_event(event),
            event => self.normalize_message_event(event),
        }
    }

    fn normalize_stream_event(&mut self, event: AnthropicRawStreamEvent) -> NormalizedEvent {
        match event {
            AnthropicRawStreamEvent::MessageStart { message } => {
                self.current_message_item_id = Some(assistant_item_id(message.id.as_deref()));
                self.content_blocks.clear();
                NormalizedEvent::Ignored
            }
            AnthropicRawStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                self.content_blocks
                    .insert(index, stream_block_kind(content_block));
                NormalizedEvent::Ignored
            }
            AnthropicRawStreamEvent::ContentBlockDelta { index, delta } => {
                let message_id = self
                    .current_message_item_id
                    .clone()
                    .unwrap_or_else(|| assistant_item_id(None));
                match delta {
                    AnthropicContentDelta::TextDelta { text } => match self
                        .content_blocks
                        .get(&index)
                    {
                        Some(StreamBlockKind::Reasoning) => NormalizedEvent::ReasoningTextDelta {
                            item_id: reasoning_item_id(&message_id, index),
                            delta: text,
                        },
                        _ => NormalizedEvent::AgentTextDelta {
                            item_id: message_id,
                            delta: text,
                        },
                    },
                    AnthropicContentDelta::ThinkingDelta { thinking } => {
                        NormalizedEvent::ReasoningTextDelta {
                            item_id: reasoning_item_id(&message_id, index),
                            delta: thinking,
                        }
                    }
                    AnthropicContentDelta::Unknown => NormalizedEvent::Ignored,
                }
            }
            AnthropicRawStreamEvent::ContentBlockStop { index } => {
                self.content_blocks.remove(&index);
                NormalizedEvent::Ignored
            }
            AnthropicRawStreamEvent::MessageStop => {
                self.current_message_item_id = None;
                self.content_blocks.clear();
                NormalizedEvent::Ignored
            }
            AnthropicRawStreamEvent::MessageDelta { .. } | AnthropicRawStreamEvent::Unknown => {
                NormalizedEvent::Ignored
            }
        }
    }

    fn normalize_message_event(&mut self, event: AnthropicStreamEvent) -> NormalizedEvent {
        match event {
            AnthropicStreamEvent::System {
                subtype,
                session_id,
            } => {
                if subtype.as_deref() == Some("init") {
                    NormalizedEvent::SessionStarted { session_id }
                } else {
                    NormalizedEvent::Ignored
                }
            }
            AnthropicStreamEvent::Assistant {
                is_partial,
                message,
            } => NormalizedEvent::AssistantMessage {
                partial: is_partial,
                stop_reason: message.stop_reason.clone(),
                content: message.into_normalized_content(),
            },
            AnthropicStreamEvent::User {
                message,
                tool_use_result,
            } => {
                let tool_use_result = tool_use_result.as_ref();
                let results = message
                    .content
                    .into_iter()
                    .filter_map(|block| block.into_tool_result(tool_use_result))
                    .collect();
                NormalizedEvent::ToolResults(results)
            }
            AnthropicStreamEvent::Result {
                subtype,
                result,
                is_error,
                error,
                message,
                usage: _,
            } => NormalizedEvent::Result {
                error: result_error_text(subtype, is_error, error, message, result),
            },
            AnthropicStreamEvent::Error {
                error,
                message,
                result,
            } => NormalizedEvent::Error {
                message: event_error_text(error, message, result)
                    .unwrap_or_else(|| "harness error".to_string()),
            },
            AnthropicStreamEvent::StreamEvent { .. } | AnthropicStreamEvent::Unknown => {
                NormalizedEvent::Ignored
            }
        }
    }
}

impl From<AnthropicStreamEvent> for NormalizedEvent {
    fn from(event: AnthropicStreamEvent) -> Self {
        AnthropicEventNormalizer::default().normalize(event)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicMessage {
    pub id: Option<String>,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub usage: Option<Value>,
    #[serde(default)]
    pub content: Vec<AnthropicContentBlock>,
}

impl AnthropicMessage {
    fn into_normalized_content(self) -> Vec<NormalizedContent> {
        let message_id = assistant_item_id(self.id.as_deref());
        self.content
            .into_iter()
            .enumerate()
            .filter_map(|(index, block)| block.into_normalized_content(&message_id, index))
            .collect()
    }
}

fn assistant_item_id(raw_id: Option<&str>) -> String {
    stable_id(raw_id.unwrap_or("assistant"), "msg")
}

fn reasoning_item_id(message_id: &str, index: usize) -> String {
    format!("{message_id}-reasoning-{index}")
}

fn stream_block_kind(block: AnthropicContentBlock) -> StreamBlockKind {
    match block {
        AnthropicContentBlock::Text { .. } => StreamBlockKind::Text,
        AnthropicContentBlock::Thinking { .. } | AnthropicContentBlock::Reasoning { .. } => {
            StreamBlockKind::Reasoning
        }
        AnthropicContentBlock::ToolUse { .. }
        | AnthropicContentBlock::ToolResult { .. }
        | AnthropicContentBlock::Unknown => StreamBlockKind::Other,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        thinking: Option<String>,
        text: Option<String>,
    },
    Reasoning {
        thinking: Option<String>,
        text: Option<String>,
    },
    ToolUse {
        id: Option<String>,
        name: Option<String>,
        input: Option<Value>,
    },
    ToolResult {
        tool_use_id: Option<String>,
        content: Option<Value>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Unknown,
}

impl AnthropicContentBlock {
    fn into_normalized_content(self, message_id: &str, index: usize) -> Option<NormalizedContent> {
        match self {
            Self::Text { text } => Some(NormalizedContent::AgentText {
                item_id: message_id.to_string(),
                text,
            }),
            Self::Thinking { thinking, text } | Self::Reasoning { thinking, text } => {
                Some(NormalizedContent::ReasoningText {
                    item_id: format!("{message_id}-reasoning-{index}"),
                    text: thinking.or(text).unwrap_or_default(),
                })
            }
            Self::ToolUse { id, name, input } => Some(NormalizedContent::ToolUse {
                raw_id: id.unwrap_or_else(|| "tool".to_string()),
                tool: name.unwrap_or_else(|| "tool".to_string()),
                arguments: input.unwrap_or_else(|| serde_json::json!({})),
            }),
            Self::ToolResult { .. } | Self::Unknown => None,
        }
    }

    fn into_tool_result(self, runtime_result: Option<&Value>) -> Option<NormalizedToolResult> {
        match self {
            Self::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let content_text = content_text(content.as_ref());
                let (content_text, parsed_exit_code) =
                    parse_json_tool_result(&content_text).unwrap_or((content_text, None));
                Some(NormalizedToolResult {
                    tool_use_id: tool_use_id.unwrap_or_else(|| "tool".to_string()),
                    content: runtime_output_text(runtime_result).unwrap_or(content_text.clone()),
                    is_error,
                    exit_code: runtime_exit_code(runtime_result)
                        .or(parsed_exit_code)
                        .or_else(|| exit_code_from_prefix(&content_text)),
                })
            }
            _ => None,
        }
    }
}

fn result_error_text(
    subtype: Option<String>,
    is_error: bool,
    error: Option<Value>,
    message: Option<String>,
    result: Option<String>,
) -> Option<String> {
    let subtype_is_error = subtype
        .as_deref()
        .is_some_and(|subtype| !matches!(subtype, "" | "success"));
    if is_error || subtype_is_error {
        event_error_text(error, message, result)
            .or_else(|| Some("harness reported an error".to_string()))
    } else {
        None
    }
}

fn event_error_text(
    error: Option<Value>,
    message: Option<String>,
    result: Option<String>,
) -> Option<String> {
    error
        .and_then(|error| {
            error.as_str().map(str::to_string).or_else(|| {
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
        .or(message)
        .or(result)
}

fn content_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn runtime_output_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let stdout = value.get("stdout").and_then(Value::as_str);
    let stderr = value.get("stderr").and_then(Value::as_str);
    if stdout.is_some() || stderr.is_some() {
        Some(format!("{}{}", stdout.unwrap_or(""), stderr.unwrap_or("")))
    } else {
        None
    }
}

fn runtime_exit_code(value: Option<&Value>) -> Option<i32> {
    let value = value?;
    value
        .get("exit_code")
        .or_else(|| value.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

fn token_usage_from_value(
    value: Option<&Value>,
    model: Option<String>,
) -> Option<NormalizedTokenUsage> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let usage = NormalizedTokenUsage {
        model: model.or_else(|| token_string(value, &["model"])),
        input_tokens: token_count(value, &["input_tokens", "inputTokens", "inputTokenCount"]),
        output_tokens: token_count(
            value,
            &["output_tokens", "outputTokens", "outputTokenCount"],
        ),
        cache_creation_input_tokens: token_count(
            value,
            &[
                "cache_creation_input_tokens",
                "cacheCreationInputTokens",
                "cacheCreationTokens",
            ],
        ),
        cache_read_input_tokens: token_count(
            value,
            &[
                "cache_read_input_tokens",
                "cached_input_tokens",
                "cacheReadInputTokens",
                "cachedInputTokens",
            ],
        ),
        reasoning_output_tokens: token_count(
            value,
            &[
                "reasoning_output_tokens",
                "reasoning_tokens",
                "reasoningOutputTokens",
                "reasoningTokens",
            ],
        ),
        total_tokens: token_count(value, &["total_tokens", "totalTokens", "totalTokenCount"]),
    };
    usage.has_counts().then_some(usage)
}

fn token_count(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(value_as_i64)
}

fn token_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|value| *value >= 0)
}

fn parse_json_tool_result(content: &str) -> Option<(String, Option<i32>)> {
    let value: Value = serde_json::from_str(content).ok()?;
    let object = value.as_object()?;
    let output = object
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_string)?;
    let exit_code = object
        .get("exitCode")
        .or_else(|| object.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    Some((output, exit_code))
}

fn exit_code_from_prefix(content: &str) -> Option<i32> {
    let rest = content.strip_prefix("Exit code ")?;
    let code_text = rest.split_once('\n').map_or(rest, |(code, _)| code);
    code_text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_event_exposes_token_usage() {
        let event: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"assistant","message":{"model":"claude-fable-5","id":"msg_1","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":3,"cache_read_input_tokens":5,"output_tokens":7}}}"#,
        )
        .expect("assistant event");
        let usage = event.token_usage().expect("usage");

        assert_eq!(usage.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(usage.input_tokens, Some(2));
        assert_eq!(usage.cache_creation_input_tokens, Some(3));
        assert_eq!(usage.cache_read_input_tokens, Some(5));
        assert_eq!(usage.output_tokens, Some(7));
    }

    #[test]
    fn result_event_exposes_camel_case_token_usage() {
        let event: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"result","subtype":"success","usage":{"inputTokens":11,"cachedInputTokens":13,"outputTokens":17,"totalTokens":41}}"#,
        )
        .expect("result event");
        let usage = event.token_usage().expect("usage");

        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.cache_read_input_tokens, Some(13));
        assert_eq!(usage.output_tokens, Some(17));
        assert_eq!(usage.total_tokens, Some(41));
    }
}
