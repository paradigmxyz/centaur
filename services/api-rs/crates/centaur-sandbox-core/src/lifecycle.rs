use serde::{Deserialize, Serialize};

use crate::SandboxSpec;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// Opaque backend-owned sandbox identifier.
pub struct SandboxId(String);

impl SandboxId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<String> for SandboxId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SandboxId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl AsRef<str> for SandboxId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Handle returned after a successful sandbox create operation.
pub struct SandboxHandle {
    /// Backend-owned sandbox identifier.
    pub id: SandboxId,
    /// Name of the backend that owns this sandbox.
    pub backend: String,
}

impl SandboxHandle {
    pub fn new(id: impl Into<SandboxId>, backend: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            backend: backend.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Portable lifecycle state for a sandbox runtime.
pub enum SandboxStatus {
    /// Runtime resources exist but are not yet ready for byte I/O.
    Created,
    /// Runtime is live and should accept read/write operations.
    Running,
    /// Runtime state may exist, but no live process is serving I/O.
    Suspended,
    /// Runtime was intentionally stopped.
    Stopped,
    /// Backend could not find the runtime.
    Gone,
    /// Backend reported a state that does not map cleanly to the portable enum.
    Unknown(String),
}

impl SandboxStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped | Self::Gone)
    }

    pub fn can_read_write(&self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Backend observation used by reconciliation.
///
/// This is the runtime's current view of the world, not the control plane's
/// desired state. Managers compare this with [`DesiredSandboxState`] to repair
/// drift after crashes, controller actions, or external operator changes.
pub struct ObservedSandbox {
    /// Observed sandbox identifier.
    pub id: SandboxId,
    /// Name of the backend that produced the observation.
    pub backend: String,
    /// Current portable lifecycle status.
    pub status: SandboxStatus,
    /// Opaque backend version token used for freshness/fencing.
    ///
    /// Kubernetes backends may use a resource version, UID, or observed
    /// generation. Local backends may use a counter. Callers should not parse
    /// this value.
    pub generation: Option<String>,
    /// Backend-owned diagnostic reason for the observed status.
    pub reason: Option<String>,
}

impl ObservedSandbox {
    pub fn new(
        id: impl Into<SandboxId>,
        backend: impl Into<String>,
        status: SandboxStatus,
    ) -> Self {
        Self {
            id: id.into(),
            backend: backend.into(),
            status,
            generation: None,
            reason: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Control-plane desired state for reconciliation.
///
/// A manager compares this value with [`ObservedSandbox`] and issues backend
/// operations until the observed runtime converges, or reports ambiguous drift.
pub enum DesiredSandboxState {
    /// The sandbox should exist and serve byte I/O using this spec.
    Running(SandboxSpec),
    /// The sandbox should retain backend-supported state but not serve live I/O.
    Suspended(SandboxSpec),
    /// The sandbox should be stopped and cleaned up.
    Stopped,
}
