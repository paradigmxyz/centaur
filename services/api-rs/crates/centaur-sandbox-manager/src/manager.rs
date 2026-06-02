use std::sync::Arc;

use centaur_sandbox_core::{
    DesiredSandboxState, ObservedSandbox, SandboxBackend, SandboxHandle, SandboxId, SandboxIo,
    SandboxResult, SandboxSpec, SandboxStatus,
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

    pub async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        self.backend.open_io(id).await
    }

    pub async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        self.backend.status(id).await
    }

    pub async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        self.backend.observe(id).await
    }

    /// List every sandbox observation the backend currently owns.
    pub async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
        self.backend.list_observed().await
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
