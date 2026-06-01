use std::sync::Arc;

use bytes::Bytes;
use centaur_sandbox_core::{
    DesiredSandboxState, ExecCommand, ExecResult, ObservedSandbox, ReadOptions, ReadResult,
    SandboxBackend, SandboxHandle, SandboxId, SandboxResult, SandboxSpec, SandboxStatus, WriteAck,
};

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
        let handle = self.backend.create(spec.clone()).await?;
        self.store
            .set(handle.id.clone(), DesiredSandboxState::Running(spec));
        Ok(handle)
    }

    pub async fn read_bytes(&self, id: &SandboxId, opts: ReadOptions) -> SandboxResult<ReadResult> {
        self.backend.read_bytes(id, opts).await
    }

    pub async fn write_bytes(&self, id: &SandboxId, bytes: Bytes) -> SandboxResult<WriteAck> {
        self.backend.write_bytes(id, bytes).await
    }

    pub async fn close_stdin(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend.close_stdin(id).await
    }

    pub async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        self.backend.status(id).await
    }

    pub async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        self.backend.observe(id).await
    }

    pub async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend.pause(id).await?;
        if let Some(DesiredSandboxState::Running(spec) | DesiredSandboxState::Suspended(spec)) =
            self.store.get(id)
        {
            self.store
                .set(id.clone(), DesiredSandboxState::Suspended(spec));
        }
        Ok(())
    }

    pub async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend.resume(id).await?;
        if let Some(DesiredSandboxState::Running(spec) | DesiredSandboxState::Suspended(spec)) =
            self.store.get(id)
        {
            self.store
                .set(id.clone(), DesiredSandboxState::Running(spec));
        }
        Ok(())
    }

    pub async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend.stop(id).await?;
        self.store.set(id.clone(), DesiredSandboxState::Stopped);
        Ok(())
    }

    pub async fn exec(&self, id: &SandboxId, command: ExecCommand) -> SandboxResult<ExecResult> {
        self.backend.exec(id, command).await
    }

    pub async fn interrupt(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend.interrupt(id).await
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
        match plan.action {
            ReconcileAction::None => Ok(ReconcileOutcome::Noop),
            ReconcileAction::Pause => {
                self.backend.pause(id).await?;
                Ok(ReconcileOutcome::Paused)
            }
            ReconcileAction::Resume => {
                self.backend.resume(id).await?;
                Ok(ReconcileOutcome::Resumed)
            }
            ReconcileAction::Stop => {
                self.backend.stop(id).await?;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedSandbox {
    pub id: SandboxId,
    pub desired: DesiredSandboxState,
    pub observed: ObservedSandbox,
    pub outcome: ReconcileOutcome,
}
