use async_trait::async_trait;

use crate::{
    ObservedSandbox, SandboxHandle, SandboxId, SandboxIo, SandboxResult, SandboxSpec, SandboxStatus,
};

#[async_trait]
/// Backend-neutral lifecycle and byte-I/O operations for one sandbox runtime.
///
/// This trait intentionally models only the isolated workload primitive. Higher
/// layers decide why the sandbox exists and how stdin/stdout bytes should be
/// framed.
pub trait SandboxBackend: Send + Sync {
    /// Stable backend name used in handles, observations, and diagnostics.
    fn name(&self) -> &'static str;

    /// Create a sandbox from the supplied workload spec and return its handle.
    async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle>;

    /// Open owned stdin/stdout/stderr handles for a running sandbox.
    async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo>;

    /// Read the sandbox workload's recorded stdout history since `since`.
    ///
    /// Live io streams only deliver output from attach time forward; output
    /// emitted while no reader was attached (for example across a control
    /// plane restart) is otherwise lost. Backends whose runtime records the
    /// workload's stdout (such as Kubernetes pod logs) can replay it here so
    /// orphaned executions can be adopted instead of failed.
    async fn read_output_since(
        &self,
        _id: &SandboxId,
        _since: Option<std::time::SystemTime>,
    ) -> SandboxResult<Vec<String>> {
        Err(crate::SandboxError::Unsupported {
            backend: self.name(),
            operation: "read_output_since",
        })
    }

    /// Return the portable, cheap lifecycle status for a sandbox.
    async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus>;

    /// Return the full observed runtime snapshot for one sandbox.
    ///
    /// Unlike [`SandboxBackend::status`], this can include backend-owned
    /// diagnostic context used by reconcilers.
    async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox>;

    /// List all sandbox observations owned by this backend/control plane.
    async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>>;

    /// Stop the sandbox and clean up backend-owned runtime resources.
    async fn stop(&self, id: &SandboxId) -> SandboxResult<()>;

    /// Rebind a running sandbox's managed iron-proxy to a different
    /// iron-control principal.
    async fn assign_iron_control_proxy_principal(
        &self,
        _id: &SandboxId,
        _principal_id: &str,
    ) -> SandboxResult<()> {
        Err(crate::SandboxError::Unsupported {
            backend: self.name(),
            operation: "assign_iron_control_proxy_principal",
        })
    }

    /// Ensure a running sandbox's managed iron-proxy resources are present and
    /// usable for the supplied iron-control principal without otherwise
    /// changing the sandbox lifecycle.
    async fn ensure_iron_control_proxy_resources(
        &self,
        _id: &SandboxId,
        _principal_id: &str,
    ) -> SandboxResult<()> {
        Ok(())
    }

    /// Suspend the sandbox while preserving any backend-supported runtime state.
    async fn pause(&self, id: &SandboxId) -> SandboxResult<()>;

    /// Resume a previously suspended sandbox and wait until it can serve I/O.
    async fn resume(&self, id: &SandboxId) -> SandboxResult<()>;
}
