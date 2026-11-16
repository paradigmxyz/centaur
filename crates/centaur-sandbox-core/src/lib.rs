//! Backend-neutral sandbox runtime types.
//!
//! This crate intentionally models only isolated runtime workloads. Centaur
//! concepts such as thread keys, personas, harnesses, model choice, assignment
//! generations, and durable execution rows belong in higher-level crates.

mod backend;
mod error;
mod io;
mod lifecycle;
mod spec;

/// Home directory used by the sandbox agent image.
pub const SANDBOX_AGENT_HOME: &str = "/home/agent";

/// Label naming the kind of workload a sandbox runs.
pub const WORKLOAD_LABEL: &str = "centaur.ai/workload";
/// [`WORKLOAD_LABEL`] value for per-principal MCP Code Mode sandboxes.
pub const CODEMODE_WORKLOAD: &str = "codemode";
/// Label carrying the hashed principal a Code Mode sandbox serves.
pub const CODEMODE_PRINCIPAL_LABEL: &str = "centaur.ai/codemode-principal";
/// Label carrying the generation a Code Mode sandbox was built for: a hash of
/// its engine image, the principal's repo-cache access, and its observability
/// setting. Any change to those yields a new sandbox. The label is the only
/// record of the generation: sandbox labels are authoritative.
pub const CODEMODE_FINGERPRINT_LABEL: &str = "centaur.ai/codemode-fingerprint";
/// Port the `centaur-codemode` server listens on inside a Code Mode sandbox.
/// The Kubernetes readiness probe and the session runtime share this constant.
pub const CODEMODE_PORT: u16 = 8790;

pub use backend::SandboxBackend;
pub use error::{BoxedError, SandboxError, SandboxResult};
pub use io::{SandboxIo, SandboxIoGuard, SandboxIoParts, SandboxRead, SandboxWrite};
pub use lifecycle::{ObservedSandbox, SandboxHandle, SandboxId, SandboxStatus};
pub use spec::{
    EnvVar, Mount, MountKind, RepoCacheAccess, ResourceClaim, ResourceRequirements,
    SandboxCapabilities, SandboxFile, SandboxSpec,
};

/// Whether sandbox labels describe a per-principal MCP Code Mode sandbox.
///
/// Code Mode sandboxes have no session, execution, or warm-pool row behind
/// them, so every generic lifecycle sweep keys off this predicate instead of
/// durable state.
pub fn is_codemode_workload(labels: &std::collections::BTreeMap<String, String>) -> bool {
    labels.get(WORKLOAD_LABEL).map(String::as_str) == Some(CODEMODE_WORKLOAD)
}
