use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::Deserialize;
use serde_json::Value;

use crate::{HarnessServerError, Result};

// Physical stdout lines remain capped at 1 MiB, with slack for framing.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024 + 1024;
const MAX_REASSEMBLED_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
pub(crate) struct Response {
    pub(crate) id: String,
    pub(crate) command: String,
    pub(crate) success: bool,
    #[serde(default)]
    pub(crate) data: Value,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Model {
    pub(crate) provider: String,
    pub(crate) id: String,
}

#[derive(Deserialize)]
pub(crate) struct State {
    pub(crate) model: Option<Model>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptResult {
    pub(crate) id: String,
    pub(crate) status: PromptStatus,
    pub(crate) error: Option<PromptError>,
    pub(crate) agent_invoked: bool,
    pub(crate) session_settled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptStatus {
    Completed,
    Error,
    Aborted,
}

#[derive(Deserialize)]
pub(crate) struct PromptError {
    pub(crate) message: String,
}

pub(crate) fn parse_ready(frame: &Value) -> Result<usize> {
    if frame_type(frame)? != "ready" {
        return Err(protocol_error("expected OMP ready frame"));
    }
    if !frame
        .get("supportedProtocolVersions")
        .and_then(Value::as_array)
        .is_some_and(|versions| versions.iter().any(|version| version.as_u64() == Some(2)))
    {
        return Err(protocol_error(
            "OMP RPC protocol v2 unavailable: the omp harness requires omp >= 18.3.1",
        ));
    }
    frame
        .get("maxReassembledFrameBytes")
        .and_then(Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit > 0 && *limit <= MAX_REASSEMBLED_BYTES)
        .ok_or_else(|| protocol_error("invalid OMP maxReassembledFrameBytes"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Chunk {
    chunk_id: String,
    index: usize,
    count: usize,
    byte_length: usize,
    data: String,
}

struct PendingChunks {
    chunk_id: String,
    next_index: usize,
    count: usize,
    byte_length: usize,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct FrameDecoder {
    limit: usize,
    pending: Option<PendingChunks>,
}

impl FrameDecoder {
    pub(crate) fn push(&mut self, line: &[u8]) -> Result<Option<Value>> {
        let frame: Value = serde_json::from_slice(line)?;
        if frame_type(&frame)? != "rpc_chunk" {
            self.finish()?;
            if frame_type(&frame)? == "ready" {
                self.limit = parse_ready(&frame)?;
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
            || chunk.byte_length > self.limit
        {
            return Err(protocol_error(
                "invalid OMP rpc_chunk metadata or reassembly limit",
            ));
        }
        if self.pending.is_none() {
            if chunk.index != 0 {
                return Err(protocol_error(
                    "OMP rpc_chunk sequence must start at index 0",
                ));
            }
            self.pending = Some(PendingChunks {
                chunk_id: chunk.chunk_id,
                next_index: 0,
                count: chunk.count,
                byte_length: chunk.byte_length,
                // decode_vec can reserve up to two padding bytes beyond the result.
                bytes: Vec::with_capacity(chunk.byte_length + 3),
            });
        } else if self.pending.as_ref().is_some_and(|pending| {
            pending.chunk_id != chunk.chunk_id
                || pending.count != chunk.count
                || pending.byte_length != chunk.byte_length
                || pending.next_index != chunk.index
        }) {
            return Err(protocol_error("OMP rpc_chunk sequence mismatch"));
        }
        let pending = self.pending.as_mut().expect("chunk sequence initialized");
        if chunk.data.is_empty() || chunk.data.len() > CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(protocol_error("invalid OMP rpc_chunk data length"));
        }
        let before = pending.bytes.len();
        BASE64_STANDARD
            .decode_vec(&chunk.data, &mut pending.bytes)
            .map_err(|_| protocol_error("invalid OMP rpc_chunk base64"))?;
        if pending.bytes.len() - before > CHUNK_BYTES || pending.bytes.len() > pending.byte_length {
            return Err(protocol_error("OMP rpc_chunk exceeds declared length"));
        }
        pending.next_index += 1;
        if pending.next_index < pending.count {
            return Ok(None);
        }
        let pending = self.pending.take().expect("chunk sequence initialized");
        if pending.bytes.len() != pending.byte_length {
            return Err(protocol_error("OMP rpc_chunk byteLength mismatch"));
        }
        let text = std::str::from_utf8(&pending.bytes)
            .map_err(|_| protocol_error("OMP rpc_chunk is not strict UTF-8"))?;
        let frame: Value = serde_json::from_str(text)?;
        if !frame.is_object() {
            return Err(protocol_error("OMP rpc_chunk must contain one JSON object"));
        }
        Ok(Some(frame))
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if self.pending.is_some() {
            Err(protocol_error("OMP rpc_chunk sequence interrupted"))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn frame_type(value: &Value) -> Result<&str> {
    value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error("OMP frame is missing string type"))
}

pub(crate) fn is_state_notification(kind: &str) -> bool {
    matches!(
        kind,
        "available_commands_update"
            | "config_update"
            | "session_info_update"
            | "thinking_level_changed"
            | "model_changed"
            | "config_warnings_changed"
            | "advisor_cost_changed"
    )
}

pub(crate) fn protocol_error(message: impl Into<String>) -> HarnessServerError {
    HarnessServerError::Protocol(format!("OMP protocol error: {}", message.into()))
}
