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

pub use backend::SandboxBackend;
pub use error::{BoxedError, SandboxError, SandboxResult};
pub use io::{SandboxIo, SandboxIoGuard, SandboxIoParts, SandboxRead, SandboxWrite};
pub use lifecycle::{
    DesiredSandboxState, ObservedSandbox, SandboxHandle, SandboxId, SandboxStatus,
};
pub use spec::{
    EnvVar, Mount, MountKind, RepoCacheAccess, ResourceLimits, SandboxCapabilities, SandboxSpec,
};
