use std::{sync::Arc, time::Duration};

use centaur_sandbox_core::{
    DesiredSandboxState, ObservedSandbox, SandboxBackend, SandboxHandle, SandboxId, SandboxIo,
    SandboxResult, SandboxSpec, SandboxStatus,
};
use centaur_telemetry::{record_sandbox_operation, record_sandbox_startup_duration};
use tokio::time::Instant;
use tracing::{Instrument, error, info, info_span};

use crate::{
    DesiredStateStore, DriftReason, InMemoryDesiredStateStore, ReconcileAction, ReconcileOutcome,
    ReconcilePlan,
};

pub struct SandboxManager<S = InMemoryDesiredStateStore> {
    backend: Arc<dyn SandboxBackend>,
    store: S,
}

impl SandboxManager<InMemoryDesiredStateStore> {
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        Self::with_store(backend, InMemoryDesiredStateStore::new())
    }
}

impl<S> SandboxManager<S>
where
    S: DesiredStateStore,
{
    pub fn with_store(backend: Arc<dyn SandboxBackend>, store: S) -> Self {
        Self { backend, store }
    }

    pub fn desired_state(&self, id: &SandboxId) -> Option<DesiredSandboxState> {
        self.store.get(id)
    }

    pub fn set_desired_state(&self, id: SandboxId, state: DesiredSandboxState) {
        self.store.set(id, state);
    }

    pub fn desired_states(&self) -> Vec<(SandboxId, DesiredSandboxState)> {
        self.store.list()
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
            let handle = match self.backend.create(spec.clone()).await {
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
            self.store
                .set(handle.id.clone(), DesiredSandboxState::Running(spec));
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

    pub async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        let backend = self.backend.name();
        match self.backend.pause(id).await {
            Ok(()) => record_sandbox_operation(backend, "pause", "success"),
            Err(error) => {
                record_sandbox_operation(backend, "pause", "error");
                return Err(error);
            }
        }
        if let Some(DesiredSandboxState::Running(spec) | DesiredSandboxState::Suspended(spec)) =
            self.store.get(id)
        {
            self.store
                .set(id.clone(), DesiredSandboxState::Suspended(spec));
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
        if let Some(DesiredSandboxState::Running(spec) | DesiredSandboxState::Suspended(spec)) =
            self.store.get(id)
        {
            self.store
                .set(id.clone(), DesiredSandboxState::Running(spec));
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
            self.store.set(id.clone(), DesiredSandboxState::Stopped);
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
    ) -> SandboxResult<()> {
        self.backend
            .assign_iron_control_proxy_principal(id, principal_id)
            .await
    }

    pub async fn reconcile_one(&self, id: &SandboxId) -> SandboxResult<ReconcileOutcome> {
        let Some(desired) = self.store.get(id) else {
            return Ok(ReconcileOutcome::Drift(DriftReason::NoDesiredState));
        };
        let observed = self.backend.observe(id).await?;
        let plan = ReconcilePlan::for_state(&desired, &observed);
        self.apply_plan(id, plan).await
    }

    async fn apply_plan(
        &self,
        id: &SandboxId,
        plan: ReconcilePlan,
    ) -> SandboxResult<ReconcileOutcome> {
        let backend = self.backend.name();
        match plan.action {
            ReconcileAction::None => Ok(ReconcileOutcome::Noop),
            ReconcileAction::Pause => {
                match self.backend.pause(id).await {
                    Ok(()) => record_sandbox_operation(backend, "pause", "success"),
                    Err(error) => {
                        record_sandbox_operation(backend, "pause", "error");
                        return Err(error);
                    }
                }
                Ok(ReconcileOutcome::Paused)
            }
            ReconcileAction::Resume => {
                match self.backend.resume(id).await {
                    Ok(()) => record_sandbox_operation(backend, "resume", "success"),
                    Err(error) => {
                        record_sandbox_operation(backend, "resume", "error");
                        return Err(error);
                    }
                }
                Ok(ReconcileOutcome::Resumed)
            }
            ReconcileAction::Stop => {
                match self.backend.stop(id).await {
                    Ok(()) => record_sandbox_operation(backend, "stop", "success"),
                    Err(error) => {
                        record_sandbox_operation(backend, "stop", "error");
                        return Err(error);
                    }
                }
                Ok(ReconcileOutcome::Stopped)
            }
            ReconcileAction::ReportDrift(reason) => Ok(ReconcileOutcome::Drift(reason)),
        }
    }

    pub async fn reconcile_all(&self) -> SandboxResult<Vec<ManagedSandbox>> {
        let mut reconciled = Vec::new();
        for (id, desired) in self.store.list() {
            let observed = self.backend.observe(&id).await?;
            let plan = ReconcilePlan::for_state(&desired, &observed);
            let outcome = self.apply_plan(&id, plan).await?;
            reconciled.push(ManagedSandbox {
                id,
                desired,
                observed,
                outcome,
            });
        }
        Ok(reconciled)
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedSandbox {
    pub id: SandboxId,
    pub desired: DesiredSandboxState,
    pub observed: ObservedSandbox,
    pub outcome: ReconcileOutcome,
}
