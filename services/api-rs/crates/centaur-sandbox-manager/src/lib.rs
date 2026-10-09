//! Thin orchestration over a sandbox backend.
//!
//! The manager records lifecycle telemetry around sandbox operations. Backends
//! own the runtime-specific work needed to make those transitions happen.

mod manager;
mod reaper;
mod warm_pool;

pub use manager::SandboxManager;
pub use reaper::{SandboxReaper, SandboxReaperConfig};
pub use warm_pool::{WarmPoolConfig, WarmPoolError, WarmPoolManager, WarmSandboxSpecFactory};
