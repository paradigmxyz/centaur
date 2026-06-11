pub mod amp;
pub mod anthropic;
pub mod claude;
pub mod codex;
mod error;
mod server;
mod traits;
mod turn;
mod util;
mod validation;
pub mod wire;

pub use error::{HarnessServerError, Result};
pub use server::{run_blocks_server, run_harness_server, run_validate_jsonrpc, server_for};
pub use traits::{
    AppServerNormalizer, AppServerRuntime, HarnessKind, HarnessServer, NormalizedContent,
    NormalizedEvent, NormalizedToolResult, ThreadState,
};
pub use turn::{BridgeConfig, CodexTurnNormalizer};
pub use validation::run_validate_agent_deltas;
pub use wire::{
    is_known_untyped_server_notification, notification_to_jsonrpc, notification_to_wire_value,
};

pub(crate) use util::{command_from_override, stable_id, user_input_to_anthropic_content};
