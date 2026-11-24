//! Sandbox backend for the out-of-sandbox `nanocode` brain.
//!
//! The brain is a long-lived service that runs `nanocode` turns; a "brain
//! sandbox" is a cheap logical handle whose stdin/stdout is a WebSocket to the
//! brain. API-rs treats it exactly like any other [`SandboxBackend`], so the
//! stdout pump, durable events, terminal detection, and rendering are reused
//! unchanged.

mod backend;
mod routing;

pub use backend::{BrainSandboxBackend, BrainSandboxConfig};
pub use routing::RoutingSandboxBackend;

/// Label naming the harness a session sandbox runs.
pub const HARNESS_LABEL: &str = "centaur.ai/harness";
/// Label value that routes a sandbox to the brain.
pub const NANOCODE_HARNESS: &str = "nanocode";
/// Prefix of every brain-owned sandbox id, used to route id-based calls.
pub const BRAIN_SANDBOX_ID_PREFIX: &str = "brain-";
