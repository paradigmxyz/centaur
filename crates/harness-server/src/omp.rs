//! OMP harness: drives `omp --mode rpc --no-ui`, OMP's long-lived JSONL protocol.
//!
//! Thinking streams as it arrives; text renders from `message_end`, because only
//! then is it known whether it is commentary or the final answer.
//!
//! One process serves the thread across turns. A `prompt` ends on `prompt_result`,
//! or on `session_settled` when background work still owes a follow-up. OMP saves
//! the session in a per-thread directory, so a respawn after an interrupt, crash,
//! or model switch resumes the conversation.
//!
//! OMP reads provider API keys from the environment, where the sandbox holds
//! iron-proxy placeholders that the proxy replaces on the wire.

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_app_server_protocol::UserInput;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pi::{command_line, message_text, session_id, thinking_level, token_usage, tool_result};
use crate::{
    HarnessChild, HarnessKind, HarnessServer, HarnessServerError, NormalizedContent,
    NormalizedEvent, NormalizedToolResult, Result, ThreadState, TurnHold,
};

const DEFAULT_THINKING_LEVEL: &str = "high";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REASSEMBLED_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Default)]
pub struct OmpHarness;

#[derive(Debug, Default)]
pub struct OmpEventNormalizer {
    chunks: Option<PendingChunks>,
    waiting_for_settle: bool,
    /// Reasoning items opened by streamed thinking, closed by their message's end.
    streamed_reasoning: Vec<String>,
}

impl HarnessServer for OmpHarness {
    type Event = Value;
    type EventNormalizer = OmpEventNormalizer;

    fn kind(&self) -> HarnessKind {
        HarnessKind::Omp
    }
    fn cli_version(&self) -> &'static str {
        "omp"
    }
    fn default_model(&self) -> String {
        env::var("CENTAUR_OMP_MODEL").unwrap_or_default()
    }
    fn default_model_provider(&self) -> &'static str {
        "omp"
    }

    fn command_for_turn(&self, _state: &ThreadState) -> ProcessCommand {
        let bin = env::var("CENTAUR_OMP_BIN").unwrap_or_else(|_| "omp".to_string());
        let mut command = ProcessCommand::new(bin);
        command.args(["--mode", "rpc", "--no-ui"]);
        command
    }

    fn on_process_start(&self, state: &ThreadState, process: &mut HarnessChild) -> Result<()> {
        let root = env::var_os("CENTAUR_OMP_SESSION_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| state.cwd.clone())
                    .join(".omp/centaur-sessions")
            });
        let dir = root.join(session_id(state));
        fs::create_dir_all(&dir)?;
        let mut normalizer = OmpEventNormalizer::default();
        let mut command = |kind: &str, fields: Value| -> Result<Value> {
            let mut response = startup_command(process, &mut normalizer, kind, fields)?;
            if response["success"] != true {
                let error = response["error"].as_str().unwrap_or("command failed");
                // open_session checks a requested model as set_model does.
                if kind == "open_session" && error.starts_with("Model not found:") {
                    return Err(HarnessServerError::UnknownModel {
                        message: format!("unsupported model `{}` for OMP: {error}", state.model),
                    });
                }
                return Err(protocol_error(format!("{kind}: {error}")));
            }
            Ok(response["data"].take())
        };
        let negotiated = command("negotiate_protocol", json!({"protocolVersion": 2}))?;
        if negotiated["protocolVersion"] != 2 {
            return Err(protocol_error(
                "negotiate_protocol: expected protocolVersion 2",
            ));
        }
        command(
            "set_event_filter",
            json!({"events": ["message_update", "message_end", "tool_execution_start", "tool_execution_end"], "messageUpdates": "delta"}),
        )?;
        // OMP resumes the session on its saved model, and refuses to when that
        // model is no longer available, so the turn's model goes in the same call.
        let mut open = json!({"sessionDir": dir});
        if let Some((provider, model_id)) = split_model(&state.model) {
            open["provider"] = json!(provider);
            open["modelId"] = json!(model_id);
        }
        let opened = command("open_session", open)?;
        if opened["cancelled"] == true {
            return Err(protocol_error("open_session: cancelled"));
        }
        command("set_cache_warming", json!({"mode": "off"}))?;
        command(
            "set_thinking_level",
            json!({"level": DEFAULT_THINKING_LEVEL}),
        )?;
        Ok(())
    }

    fn restart_on_model_change(&self) -> bool {
        true
    }

    fn validate_model(&self, model: &str) -> std::result::Result<(), String> {
        split_model(model).map(|_| ()).ok_or_else(|| {
            format!("unsupported model `{model}` for OMP: expected provider/modelId")
        })
    }

    fn stdin_for_turn(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        prompt_command("prompt", input)
    }
    fn stdin_for_steer(&self, input: &[UserInput]) -> Result<Vec<u8>> {
        prompt_command("steer", input)
    }

    fn reasoning_effort(&self, requested: &str) -> Option<String> {
        if let Some(level) = thinking_level(requested) {
            return Some(level.to_string());
        }
        eprintln!("ignoring unsupported OMP thinking level {requested:?}");
        None
    }

    fn stdin_for_reasoning_effort(&self, effort: Option<&str>) -> Result<Vec<u8>> {
        let level = effort.unwrap_or(DEFAULT_THINKING_LEVEL);
        command_line(json!({"type": "set_thinking_level", "level": level}))
    }

    fn parse_stdout_line(&self, line: &str) -> Result<Value> {
        Ok(serde_json::from_str(line)?)
    }

    fn normalize_events(
        &self,
        normalizer: &mut OmpEventNormalizer,
        event: Value,
    ) -> Result<Vec<NormalizedEvent>> {
        normalizer.normalize(event)
    }

    fn turn_hold(&self, normalizer: &OmpEventNormalizer) -> TurnHold {
        if normalizer.waiting_for_settle {
            TurnHold::Waiting
        } else {
            TurnHold::Released
        }
    }
}

fn startup_command(
    process: &mut HarnessChild,
    normalizer: &mut OmpEventNormalizer,
    kind: &str,
    mut fields: Value,
) -> Result<Value> {
    let id = format!("centaur-{kind}");
    fields["id"] = json!(id);
    fields["type"] = json!(kind);
    process.stdin.write_all(&command_line(fields)?)?;
    process.stdin.flush()?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(protocol_error(format!("{kind} acknowledgement timed out")));
        }
        let line = process
            .stdout
            .recv_timeout(remaining)
            .map_err(|error| protocol_error(format!("{kind} acknowledgement failed: {error}")))??;
        if line.trim().is_empty() {
            continue;
        }
        let Some(frame) = normalizer.frame(serde_json::from_str(line.trim())?)? else {
            continue;
        };
        if frame["type"] == "response" && frame["id"] == id && frame["command"] == kind {
            return Ok(frame);
        }
    }
}

impl OmpEventNormalizer {
    fn normalize(&mut self, event: Value) -> Result<Vec<NormalizedEvent>> {
        let Some(event) = self.frame(event)? else {
            return Ok(Vec::new());
        };
        Ok(match event["type"].as_str() {
            Some("response") => self.response(&event),
            Some("message_update")
                if event["assistantMessageEvent"]["type"] == "thinking_delta" =>
            {
                let update = &event["assistantMessageEvent"];
                let message_id = event["messageId"].as_str().unwrap_or_default();
                let index = update["contentIndex"].as_u64().unwrap_or_default();
                let item_id = format!("omp-{message_id}-{index}");
                if !self.streamed_reasoning.contains(&item_id) {
                    self.streamed_reasoning.push(item_id.clone());
                }
                vec![NormalizedEvent::ReasoningTextDelta {
                    item_id,
                    delta: update["delta"].as_str().unwrap_or_default().to_string(),
                }]
            }
            Some("message_end") if event["message"]["role"] == "assistant" => {
                self.assistant_message(&event)
            }
            Some("tool_execution_start") => vec![NormalizedEvent::AssistantMessage {
                partial: false,
                stop_reason: None,
                content: vec![NormalizedContent::ToolUse {
                    raw_id: event["toolCallId"].as_str().unwrap_or_default().to_string(),
                    tool: event["toolName"].as_str().unwrap_or_default().to_string(),
                    arguments: event["args"].clone(),
                }],
            }],
            Some("tool_execution_end") => {
                vec![NormalizedEvent::ToolResults(vec![NormalizedToolResult {
                    exit_code: event["result"]["details"]["exitCode"]
                        .as_i64()
                        .and_then(|code| i32::try_from(code).ok()),
                    ..tool_result(&event)
                }])]
            }
            Some("prompt_result") => {
                self.waiting_for_settle = event["sessionSettled"] == false;
                let error = match event["status"].as_str() {
                    Some("completed") if self.waiting_for_settle => return Ok(Vec::new()),
                    Some("completed") => None,
                    Some("error") => Some(
                        event["error"]["message"]
                            .as_str()
                            .unwrap_or("omp prompt failed")
                            .to_string(),
                    ),
                    Some("aborted") => Some("omp prompt aborted".to_string()),
                    _ => Some(format!(
                        "omp prompt failed: unknown status {}",
                        event["status"]
                    )),
                };
                vec![NormalizedEvent::Result { error }]
            }
            Some("session_settled") if self.waiting_for_settle => {
                self.waiting_for_settle = false;
                vec![NormalizedEvent::Result { error: None }]
            }
            _ => Vec::new(),
        })
    }

    fn response(&self, event: &Value) -> Vec<NormalizedEvent> {
        let command = event["command"].as_str().unwrap_or_default();
        if event["success"] == false {
            let error = event["error"].as_str().unwrap_or("command failed");
            if matches!(command, "prompt" | "parse") {
                return vec![NormalizedEvent::Error {
                    message: format!("omp {command}: {error}"),
                }];
            }
            eprintln!("omp {command} failed: {error}");
        }
        Vec::new()
    }

    fn assistant_message(&mut self, event: &Value) -> Vec<NormalizedEvent> {
        let message = &event["message"];
        let mut out = vec![NormalizedEvent::TokenUsage {
            usage: token_usage(message),
        }];
        let streamed = std::mem::take(&mut self.streamed_reasoning);
        let stop_reason = message["stopReason"].as_str().unwrap_or_default();
        if matches!(stop_reason, "error" | "aborted") {
            // A failed attempt renders nothing new, but thinking it already
            // streamed still completes, with the text it streamed.
            if !streamed.is_empty() {
                out.push(NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: None,
                    content: streamed
                        .into_iter()
                        .map(|item_id| NormalizedContent::ReasoningText {
                            item_id,
                            text: String::new(),
                        })
                        .collect(),
                });
            }
            return out;
        }
        let stop_reason = match stop_reason {
            "toolUse" => Some("tool_use".to_string()),
            "stop" => Some("end_turn".to_string()),
            _ => None,
        };
        let message_id = event["messageId"].as_str().unwrap_or_default();
        let mut content = Vec::new();
        for (index, block) in message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let item_id = format!("omp-{message_id}-{index}");
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

    /// Mirrors OMP v18.8.6 packages/coding-agent/src/modes/rpc/rpc-frame.ts:
    /// one contiguous base64 chunk group, bounded size, exact metadata and JSON.
    fn frame(&mut self, frame: Value) -> Result<Option<Value>> {
        if frame["type"] != "rpc_chunk" {
            if self.chunks.is_some() {
                return Err(protocol_error("rpc_chunk sequence interrupted"));
            }
            if !frame.is_object() {
                return Err(protocol_error("frame must be an object"));
            }
            return Ok(Some(frame));
        }
        let chunk: Chunk = serde_json::from_value(frame)?;
        if chunk.chunk_id.is_empty()
            || chunk.chunk_id.len() > 128
            || chunk.count < 2
            || chunk.count > MAX_REASSEMBLED_BYTES / CHUNK_BYTES
            || chunk.index >= chunk.count
            || chunk.byte_length < 1024 * 1024
            || chunk.byte_length > MAX_REASSEMBLED_BYTES
        {
            return Err(protocol_error("invalid rpc_chunk metadata"));
        }
        if self.chunks.is_none() {
            if chunk.index != 0 {
                return Err(protocol_error("rpc_chunk sequence must start at index 0"));
            }
            self.chunks = Some(PendingChunks {
                chunk_id: chunk.chunk_id,
                next_index: 0,
                count: chunk.count,
                byte_length: chunk.byte_length,
                // decode_vec may reserve two padding bytes beyond the result.
                bytes: Vec::with_capacity(chunk.byte_length + 3),
            });
        } else if self.chunks.as_ref().is_some_and(|pending| {
            pending.chunk_id != chunk.chunk_id
                || pending.next_index != chunk.index
                || pending.count != chunk.count
                || pending.byte_length != chunk.byte_length
        }) {
            return Err(protocol_error("rpc_chunk sequence mismatch"));
        }
        let pending = self.chunks.as_mut().expect("chunk sequence initialized");
        if chunk.data.is_empty() || chunk.data.len() > CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(protocol_error("invalid rpc_chunk data length"));
        }
        let before = pending.bytes.len();
        BASE64_STANDARD
            .decode_vec(&chunk.data, &mut pending.bytes)
            .map_err(|_| protocol_error("invalid rpc_chunk base64"))?;
        if pending.bytes.len() - before > CHUNK_BYTES || pending.bytes.len() > pending.byte_length {
            return Err(protocol_error("rpc_chunk exceeds declared length"));
        }
        pending.next_index += 1;
        if pending.next_index < pending.count {
            return Ok(None);
        }
        let pending = self.chunks.take().expect("chunk sequence initialized");
        if pending.bytes.len() != pending.byte_length {
            return Err(protocol_error("rpc_chunk byteLength mismatch"));
        }
        let frame: Value = serde_json::from_slice(&pending.bytes)?;
        if !frame.is_object() {
            return Err(protocol_error("rpc_chunk must contain an object"));
        }
        Ok(Some(frame))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Chunk {
    chunk_id: String,
    index: usize,
    count: usize,
    byte_length: usize,
    data: String,
}

#[derive(Debug)]
struct PendingChunks {
    chunk_id: String,
    next_index: usize,
    count: usize,
    byte_length: usize,
    bytes: Vec<u8>,
}

fn protocol_error(message: impl std::fmt::Display) -> HarnessServerError {
    HarnessServerError::Protocol(format!("omp {message}"))
}

fn split_model(model: &str) -> Option<(&str, &str)> {
    let (provider, id) = model.split_once('/')?;
    (!provider.is_empty() && !id.is_empty() && !model.chars().any(char::is_whitespace))
        .then_some((provider, id))
}

fn prompt_command(kind: &str, input: &[UserInput]) -> Result<Vec<u8>> {
    let mut command = json!({"type": kind, "message": message_text(input)});
    let images: Vec<Value> = input.iter().filter_map(image).collect();
    if !images.is_empty() {
        command["images"] = Value::Array(images);
    }
    command_line(command)
}

fn image(input: &UserInput) -> Option<Value> {
    let (data, mime) = match input {
        UserInput::LocalImage { path, .. } => {
            let mime = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                "gif" => "image/gif",
                _ => return None,
            };
            if !fs::metadata(path).ok()?.is_file() {
                return None;
            }
            (
                BASE64_STANDARD.encode(fs::read(path).ok()?),
                mime.to_string(),
            )
        }
        UserInput::Image { url, .. } => {
            let (header, data) = url.strip_prefix("data:")?.split_once(',')?;
            let mime = header.strip_suffix(";base64")?;
            if !matches!(
                mime,
                "image/png" | "image/jpeg" | "image/webp" | "image/gif"
            ) {
                return None;
            }
            let mut decoded = base64::read::DecoderReader::new(data.as_bytes(), &BASE64_STANDARD);
            std::io::copy(&mut decoded, &mut std::io::sink()).ok()?;
            (data.to_string(), mime.to_string())
        }
        _ => return None,
    };
    Some(json!({"type": "image", "data": data, "mimeType": mime}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_thinking_deltas_stream_with_the_final_block_id() {
        let mut normalizer = OmpEventNormalizer::default();
        let mut update = json!({"type": "message_update", "messageId": "msg-7",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 2, "delta": "visible"}});
        assert!(normalizer.normalize(update.clone()).unwrap().is_empty());
        update["assistantMessageEvent"]["type"] = json!("thinking_delta");
        let events = normalizer.normalize(update).unwrap();
        assert!(
            matches!(events.as_slice(), [NormalizedEvent::ReasoningTextDelta { item_id, delta }]
            if item_id == "omp-msg-7-2" && delta == "visible")
        );
    }

    #[test]
    fn models_are_structural_not_an_allowlist() {
        assert_eq!(
            split_model("custom/new-model"),
            Some(("custom", "new-model"))
        );
        assert_eq!(
            split_model("openrouter/anthropic/model"),
            Some(("openrouter", "anthropic/model"))
        );
        for invalid in [
            "bare",
            "/model",
            "provider/",
            " provider/model",
            "p/model id",
        ] {
            assert!(OmpHarness.validate_model(invalid).is_err(), "{invalid}");
        }
    }

    fn chunks(frame: &Value) -> Vec<Value> {
        let bytes = serde_json::to_vec(frame).unwrap();
        let count = bytes.len().div_ceil(CHUNK_BYTES);
        bytes
            .chunks(CHUNK_BYTES)
            .enumerate()
            .map(|(index, part)| {
                json!({
                    "type": "rpc_chunk", "chunkId": "frame", "index": index, "count": count,
                    "byteLength": bytes.len(), "data": BASE64_STANDARD.encode(part),
                })
            })
            .collect()
    }

    #[test]
    fn chunks_reassemble_utf8_across_byte_boundaries() {
        let frame = json!({"type": "message_end", "text": "é".repeat(600_000)});
        let parts = chunks(&frame);
        let mut normalizer = OmpEventNormalizer::default();
        for part in &parts[..parts.len() - 1] {
            assert_eq!(normalizer.frame(part.clone()).unwrap(), None);
        }
        assert_eq!(
            normalizer.frame(parts.last().unwrap().clone()).unwrap(),
            Some(frame)
        );
    }

    #[test]
    fn chunks_reject_invalid_order_metadata_encoding_and_truncation() {
        let parts = chunks(&json!({"type": "message_end", "text": "x".repeat(1_100_000)}));
        for field in ["count", "byteLength", "chunkId", "index", "data"] {
            let mut normalizer = OmpEventNormalizer::default();
            normalizer.frame(parts[0].clone()).unwrap();
            let mut next = parts[1].clone();
            next[field] = match field {
                "count" => json!(999),
                "byteLength" => json!(MAX_REASSEMBLED_BYTES + 1),
                "chunkId" => json!("other"),
                "index" => json!(0),
                _ => json!("!!!!"),
            };
            assert!(normalizer.frame(next).is_err(), "{field}");
        }
        let mut normalizer = OmpEventNormalizer::default();
        assert!(normalizer.frame(parts[1].clone()).is_err());
        normalizer.frame(parts[0].clone()).unwrap();
        assert!(normalizer.frame(json!({"type": "prompt_result"})).is_err());
        let mut oversized = parts[0].clone();
        oversized["byteLength"] = json!(MAX_REASSEMBLED_BYTES + 1);
        assert!(OmpEventNormalizer::default().frame(oversized).is_err());
    }
}
