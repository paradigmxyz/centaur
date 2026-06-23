//! Thin orchestration over a sandbox backend.
//!
//! The manager owns desired state and transition policy. Backends own the
//! runtime-specific work needed to make those transitions happen.

mod manager;
mod reaper;
mod reconcile;
mod store;
mod warm_pool;

pub use manager::{ManagedSandbox, SandboxManager};
pub use reaper::{SandboxReaper, SandboxReaperConfig};
pub use reconcile::{DriftReason, ReconcileAction, ReconcileOutcome, ReconcilePlan};
pub use store::{DesiredStateStore, InMemoryDesiredStateStore};
pub use warm_pool::{WarmPoolConfig, WarmPoolError, WarmPoolManager, WarmSandboxSpecFactory};

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use centaur_sandbox_core::{
        DesiredSandboxState, ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle,
        SandboxId, SandboxIo, SandboxResult, SandboxSpec, SandboxStatus,
    };

    use super::{DriftReason, ReconcileOutcome, SandboxManager};

    #[tokio::test]
    async fn reconcile_resumes_suspended_sandbox_desired_running() {
        let backend = Arc::new(FakeBackend::new([("sandbox-1", SandboxStatus::Suspended)]));
        let manager = SandboxManager::new(backend.clone());
        manager.set_desired_state(
            "sandbox-1".into(),
            DesiredSandboxState::Running(SandboxSpec::new("image")),
        );

        let outcome = manager.reconcile_one(&"sandbox-1".into()).await.unwrap();

        assert_eq!(outcome, ReconcileOutcome::Resumed);
        assert_eq!(backend.status_of("sandbox-1"), Some(SandboxStatus::Running));
        assert_eq!(backend.operations(), ["resume:sandbox-1"]);
    }

    #[tokio::test]
    async fn reconcile_pauses_running_sandbox_desired_suspended() {
        let backend = Arc::new(FakeBackend::new([("sandbox-1", SandboxStatus::Running)]));
        let manager = SandboxManager::new(backend.clone());
        manager.set_desired_state(
            "sandbox-1".into(),
            DesiredSandboxState::Suspended(SandboxSpec::new("image")),
        );

        let outcome = manager.reconcile_one(&"sandbox-1".into()).await.unwrap();

        assert_eq!(outcome, ReconcileOutcome::Paused);
        assert_eq!(
            backend.status_of("sandbox-1"),
            Some(SandboxStatus::Suspended)
        );
        assert_eq!(backend.operations(), ["pause:sandbox-1"]);
    }

    #[tokio::test]
    async fn reconcile_stops_live_sandbox_desired_stopped() {
        let backend = Arc::new(FakeBackend::new([("sandbox-1", SandboxStatus::Running)]));
        let manager = SandboxManager::new(backend.clone());
        manager.set_desired_state("sandbox-1".into(), DesiredSandboxState::Stopped);

        let outcome = manager.reconcile_one(&"sandbox-1".into()).await.unwrap();

        assert_eq!(outcome, ReconcileOutcome::Stopped);
        assert_eq!(backend.status_of("sandbox-1"), Some(SandboxStatus::Stopped));
        assert_eq!(backend.operations(), ["stop:sandbox-1"]);
    }

    #[tokio::test]
    async fn reconcile_reports_drift_when_running_sandbox_disappears() {
        let backend = Arc::new(FakeBackend::new([]));
        let manager = SandboxManager::new(backend.clone());
        manager.set_desired_state(
            "sandbox-1".into(),
            DesiredSandboxState::Running(SandboxSpec::new("image")),
        );

        let outcome = manager.reconcile_one(&"sandbox-1".into()).await.unwrap();

        assert_eq!(
            outcome,
            ReconcileOutcome::Drift(DriftReason::MissingWhileRunning)
        );
        assert_eq!(backend.operations(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn pause_keeps_desired_state_when_backend_pause_fails() {
        let backend = Arc::new(FakeBackend::new([("sandbox-1", SandboxStatus::Running)]));
        backend.fail_operation("pause");
        let manager = SandboxManager::new(backend.clone());
        manager.set_desired_state(
            "sandbox-1".into(),
            DesiredSandboxState::Running(SandboxSpec::new("image")),
        );

        let err = manager.pause(&"sandbox-1".into()).await.unwrap_err();

        assert!(matches!(err, SandboxError::Backend { .. }));
        assert!(matches!(
            manager.desired_state(&"sandbox-1".into()),
            Some(DesiredSandboxState::Running(_))
        ));
        assert_eq!(backend.status_of("sandbox-1"), Some(SandboxStatus::Running));
        assert_eq!(backend.operations(), ["pause:sandbox-1"]);
    }

    #[tokio::test]
    async fn reconcile_all_reports_mixed_outcomes_after_partial_backend_loss() {
        let backend = Arc::new(FakeBackend::new([
            ("running-but-suspended", SandboxStatus::Suspended),
            ("stopped-but-running", SandboxStatus::Running),
        ]));
        let manager = SandboxManager::new(backend.clone());
        manager.set_desired_state(
            "running-but-suspended".into(),
            DesiredSandboxState::Running(SandboxSpec::new("image")),
        );
        manager.set_desired_state(
            "missing-running".into(),
            DesiredSandboxState::Running(SandboxSpec::new("image")),
        );
        manager.set_desired_state("stopped-but-running".into(), DesiredSandboxState::Stopped);

        let mut outcomes = manager
            .reconcile_all()
            .await
            .unwrap()
            .into_iter()
            .map(|managed| (managed.id.into_string(), managed.outcome))
            .collect::<Vec<_>>();
        outcomes.sort_by(|left, right| left.0.cmp(&right.0));

        assert_eq!(
            outcomes,
            [
                (
                    "missing-running".to_owned(),
                    ReconcileOutcome::Drift(DriftReason::MissingWhileRunning)
                ),
                (
                    "running-but-suspended".to_owned(),
                    ReconcileOutcome::Resumed
                ),
                ("stopped-but-running".to_owned(), ReconcileOutcome::Stopped),
            ]
        );
        let mut operations = backend.operations();
        operations.sort();
        assert_eq!(
            operations,
            ["resume:running-but-suspended", "stop:stopped-but-running"]
        );
    }

    struct FakeBackend {
        statuses: Mutex<HashMap<SandboxId, SandboxStatus>>,
        operations: Mutex<Vec<String>>,
        fail_operations: Mutex<Vec<&'static str>>,
    }

    impl FakeBackend {
        fn new<const N: usize>(statuses: [(&str, SandboxStatus); N]) -> Self {
            Self {
                statuses: Mutex::new(
                    statuses
                        .into_iter()
                        .map(|(id, status)| (SandboxId::from(id), status))
                        .collect(),
                ),
                operations: Mutex::new(Vec::new()),
                fail_operations: Mutex::new(Vec::new()),
            }
        }

        fn status_of(&self, id: &str) -> Option<SandboxStatus> {
            self.statuses
                .lock()
                .expect("status lock poisoned")
                .get(&SandboxId::from(id))
                .cloned()
        }

        fn operations(&self) -> Vec<String> {
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .clone()
        }

        fn set_status(&self, id: &SandboxId, status: SandboxStatus) {
            self.statuses
                .lock()
                .expect("status lock poisoned")
                .insert(id.clone(), status);
        }

        fn push_operation(&self, operation: &str, id: &SandboxId) {
            self.operations
                .lock()
                .expect("operations lock poisoned")
                .push(format!("{operation}:{}", id.as_str()));
        }

        fn fail_operation(&self, operation: &'static str) {
            self.fail_operations
                .lock()
                .expect("fail operation lock poisoned")
                .push(operation);
        }

        fn maybe_fail(&self, operation: &'static str) -> SandboxResult<()> {
            if self
                .fail_operations
                .lock()
                .expect("fail operation lock poisoned")
                .contains(&operation)
            {
                return Err(SandboxError::backend(format!(
                    "injected {operation} failure"
                )));
            }
            Ok(())
        }
    }

    #[async_trait]
    impl SandboxBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            unreachable!("manager reconciliation should not create in this slice")
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            unreachable!("reconciliation should not open I/O")
        }

        async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(self.status_of(id.as_str()).unwrap_or(SandboxStatus::Gone))
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            Ok(ObservedSandbox::new(
                id.clone(),
                self.name(),
                self.status(id).await?,
            ))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(self
                .statuses
                .lock()
                .expect("status lock poisoned")
                .iter()
                .map(|(id, status)| ObservedSandbox::new(id.clone(), self.name(), status.clone()))
                .collect())
        }

        async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
            self.push_operation("stop", id);
            self.maybe_fail("stop")?;
            self.set_status(id, SandboxStatus::Stopped);
            Ok(())
        }

        async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
            self.push_operation("pause", id);
            self.maybe_fail("pause")?;
            self.set_status(id, SandboxStatus::Suspended);
            Ok(())
        }

        async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
            self.push_operation("resume", id);
            self.maybe_fail("resume")?;
            self.set_status(id, SandboxStatus::Running);
            Ok(())
        }
    }
}
