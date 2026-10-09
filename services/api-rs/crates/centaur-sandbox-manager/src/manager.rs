use std::{collections::BTreeMap, sync::Arc, time::Duration};

use centaur_sandbox_core::{
    ObservedSandbox, SandboxBackend, SandboxHandle, SandboxId, SandboxIo, SandboxResult,
    SandboxSpec, SandboxStatus,
};
use centaur_telemetry::{record_sandbox_operation, record_sandbox_startup_duration};
use tokio::time::Instant;
use tracing::{Instrument, error, info, info_span};

pub struct SandboxManager {
    backend: Arc<dyn SandboxBackend>,
}

impl SandboxManager {
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        Self { backend }
    }

    pub async fn create_running(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        let backend = self.backend.name();
        let span = info_span!(
            "centaur.api_rs.sandbox.create",
            component = "sandbox_manager",
            event = "sandbox_create",
            "centaur.sandbox.backend" = backend,
            "centaur.sandbox_id" = tracing::field::Empty,
            sandbox_id = tracing::field::Empty,
        );

        async {
            let started_at = Instant::now();
            info!(
                component = "sandbox_manager",
                event = "sandbox_create_started",
                backend,
                "creating sandbox"
            );
            let handle = match self.backend.create(spec).await {
                Ok(handle) => handle,
                Err(error) => {
                    let startup_duration = started_at.elapsed();
                    record_sandbox_operation(backend, "create", "error");
                    record_sandbox_startup_duration(backend, "error", startup_duration);
                    error!(
                        component = "sandbox_manager",
                        event = "sandbox_create_failed",
                        backend,
                        startup_duration_ms = duration_millis_u64(startup_duration),
                        startup_duration_seconds = startup_duration.as_secs_f64(),
                        %error,
                        "failed to create sandbox"
                    );
                    return Err(error);
                }
            };
            span.record("centaur.sandbox_id", handle.id.as_str());
            span.record("sandbox_id", handle.id.as_str());
            let startup_duration = started_at.elapsed();
            record_sandbox_operation(backend, "create", "success");
            record_sandbox_startup_duration(backend, "success", startup_duration);
            info!(
                component = "sandbox_manager",
                event = "sandbox_create_completed",
                backend,
                sandbox_id = %handle.id.as_str(),
                startup_duration_ms = duration_millis_u64(startup_duration),
                startup_duration_seconds = startup_duration.as_secs_f64(),
                "sandbox created"
            );
            Ok(handle)
        }
        .instrument(span.clone())
        .await
    }

    pub async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        let backend = self.backend.name();
        async {
            info!(
                component = "sandbox_manager",
                event = "sandbox_open_io_started",
                backend,
                sandbox_id = %id.as_str(),
                "opening sandbox I/O"
            );
            let io = match self.backend.open_io(id).await {
                Ok(io) => io,
                Err(error) => {
                    record_sandbox_operation(backend, "open_io", "error");
                    error!(
                        component = "sandbox_manager",
                        event = "sandbox_open_io_failed",
                        backend,
                        sandbox_id = %id.as_str(),
                        %error,
                        "failed to open sandbox I/O"
                    );
                    return Err(error);
                }
            };
            record_sandbox_operation(backend, "open_io", "success");
            info!(
                component = "sandbox_manager",
                event = "sandbox_open_io_completed",
                backend,
                sandbox_id = %id.as_str(),
                "sandbox I/O opened"
            );
            Ok(io)
        }
        .instrument(info_span!(
            "centaur.api_rs.sandbox.open_io",
            component = "sandbox_manager",
            event = "sandbox_open_io",
            "centaur.sandbox.backend" = backend,
            "centaur.sandbox_id" = id.as_str(),
            sandbox_id = %id.as_str(),
        ))
        .await
    }

    pub async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        self.backend.status(id).await
    }

    /// Read the sandbox workload's recorded stdout history since `since`.
    /// Backends without recorded output return `SandboxError::Unsupported`.
    pub async fn read_output_since(
        &self,
        id: &SandboxId,
        since: Option<std::time::SystemTime>,
    ) -> SandboxResult<Vec<String>> {
        self.backend.read_output_since(id, since).await
    }

    pub async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        self.backend.observe(id).await
    }

    /// List every sandbox observation the backend currently owns.
    pub async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
        self.backend.list_observed().await
    }

    pub async fn reap_orphan_iron_proxy_resources(
        &self,
        grace: Duration,
    ) -> SandboxResult<BTreeMap<String, u32>> {
        self.backend.reap_orphan_iron_proxy_resources(grace).await
    }

    pub async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        let backend = self.backend.name();
        match self.backend.pause(id).await {
            Ok(()) => record_sandbox_operation(backend, "pause", "success"),
            Err(error) => {
                record_sandbox_operation(backend, "pause", "error");
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
        let backend = self.backend.name();
        match self.backend.resume(id).await {
            Ok(()) => record_sandbox_operation(backend, "resume", "success"),
            Err(error) => {
                record_sandbox_operation(backend, "resume", "error");
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        let backend = self.backend.name();
        async {
            match self.backend.stop(id).await {
                Ok(()) => record_sandbox_operation(backend, "stop", "success"),
                Err(error) => {
                    record_sandbox_operation(backend, "stop", "error");
                    return Err(error);
                }
            }
            info!(
                component = "sandbox_manager",
                event = "sandbox_stop_completed",
                backend,
                sandbox_id = %id.as_str(),
                "sandbox stopped"
            );
            Ok(())
        }
        .instrument(info_span!(
            "centaur.api_rs.sandbox.stop",
            component = "sandbox_manager",
            event = "sandbox_stop",
            "centaur.sandbox.backend" = backend,
            "centaur.sandbox_id" = id.as_str(),
            sandbox_id = %id.as_str(),
        ))
        .await
    }

    pub async fn assign_iron_control_proxy_principal(
        &self,
        id: &SandboxId,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.backend
            .assign_iron_control_proxy_principal(id, principal_id, requester_principal_id, labels)
            .await
    }

    pub async fn ensure_iron_control_proxy_resources(
        &self,
        id: &SandboxId,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.backend
            .ensure_iron_control_proxy_resources(id, principal_id, requester_principal_id, labels)
            .await
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
