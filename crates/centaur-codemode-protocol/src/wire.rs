//! HTTP bodies between the brain and the executor.
//!
//! Every request carries the turn token as `Authorization: Bearer <token>`;
//! the executor refuses anything without a valid one. A W3C `traceparent`
//! header, when present, parents the cell's spans.
//!
//! | Route | Body | Result |
//! |---|---|---|
//! | `POST /v1/exec` | [`ExecRequest`] | the cell response: `{ok, result?, error?, logs, calls, ms}` |
//! | `POST /v1/search` | [`SearchRequest`] | `{results, catalog_revision}` |
//! | `POST /v1/load` | [`LoadRequest`] | `{tool, description, brief}`, or `404` |
//!
//! `401` means a missing, invalid or expired turn token; `503 {"error":
//! "busy", "retryable": true}` means the executor is at capacity.

use serde::{Deserialize, Serialize};

pub const EXEC_PATH: &str = "/v1/exec";
pub const SEARCH_PATH: &str = "/v1/search";
pub const LOAD_PATH: &str = "/v1/load";

/// Run one cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    /// The body of an async function.
    pub code: String,
    /// Identifies the cell in logs; the brain uses the model's tool call id.
    pub cell_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadRequest {
    pub tool: String,
}
