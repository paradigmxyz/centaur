use std::{sync::Arc, time::Duration};

use centaur_sandbox_core::{SandboxError, SandboxId, SandboxSpec, SandboxStatus};
use centaur_session_sqlx::{PgSessionStore, SessionStoreError};
use thiserror::Error;
use tokio::time::{MissedTickBehavior, interval};
use tracing::warn;

use crate::SandboxManager;

pub type WarmSandboxSpecFactory = Arc<dyn Fn() -> SandboxSpec + Send + Sync>;

pub struct WarmPoolConfig {
    pub target_size: usize,
    pub replenish_interval: Duration,
    pub bootstrap_iron_control_principal: Option<String>,
}

pub struct WarmPoolManager {
    manager: Arc<SandboxManager>,
    store: PgSessionStore,
    spec_factory: WarmSandboxSpecFactory,
    workload_key: String,
    config: WarmPoolConfig,
}

impl WarmPoolManager {
    pub fn new(
        manager: Arc<SandboxManager>,
        store: PgSessionStore,
        spec_factory: WarmSandboxSpecFactory,
        workload_key: impl Into<String>,
        config: WarmPoolConfig,
    ) -> Self {
        Self {
            manager,
            store,
            spec_factory,
            workload_key: workload_key.into(),
            config,
        }
    }

    pub fn workload_key(&self) -> &str {
        &self.workload_key
    }

    pub fn spawn_replenisher(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut tick = interval(self.config.replenish_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            loop {
                tick.tick().await;
                if let Err(error) = self.replenish_once().await {
                    warn!(%error, "session sandbox warm pool replenishment failed");
                }
            }
        });
    }

    pub async fn claim(
        &self,
        thread_key: &str,
        iron_control_principal: Option<&str>,
    ) -> Result<Option<String>, WarmPoolError> {
        loop {
            let Some(sandbox_id) = self
                .store
                .claim_ready_warm_sandbox(self.workload_key.as_str(), thread_key)
                .await?
            else {
                return Ok(None);
            };

            let id = SandboxId::new(sandbox_id.as_str());
            let failure = match self.manager.status(&id).await {
                // Only `Running` accepts `open_io`. `Created` means the
                // runtime regressed after the replenisher saw it running
                // (backends wait for readiness before returning from create),
                // so claiming it would fail at I/O attach.
                Ok(SandboxStatus::Running) => {
                    if let Some(principal_id) = iron_control_principal
                        && let Err(error) = self
                            .manager
                            .assign_iron_control_proxy_principal(&id, principal_id)
                            .await
                    {
                        let error_message = error.to_string();
                        let _ = self
                            .store
                            .mark_warm_sandbox_failed(&sandbox_id, &error_message)
                            .await;
                        return Err(WarmPoolError::Sandbox(error));
                    }
                    return Ok(Some(sandbox_id));
                }
                Ok(status) => format!("claimed warm sandbox was not running: {status:?}"),
                Err(SandboxError::NotFound(_)) => "claimed warm sandbox was not found".to_owned(),
                Err(error) => {
                    let error_message = error.to_string();
                    warn!(%sandbox_id, error = %error_message);
                    let _ = self
                        .store
                        .mark_warm_sandbox_failed(&sandbox_id, &error_message)
                        .await;
                    return Err(WarmPoolError::Sandbox(error));
                }
            };
            warn!(%sandbox_id, error = %failure, thread_key);
            self.store
                .mark_warm_sandbox_failed(&sandbox_id, &failure)
                .await?;
        }
    }

    async fn replenish_once(&self) -> Result<(), WarmPoolError> {
        self.prune_stale_ready_sandboxes().await?;

        let needed = self.config.target_size.saturating_sub(
            self.store
                .count_ready_warm_sandboxes(self.workload_key.as_str())
                .await?
                .max(0) as usize,
        );

        for _ in 0..needed {
            let mut spec = (self.spec_factory)();
            if let Some(principal_id) = &self.config.bootstrap_iron_control_principal {
                spec.iron_control_principal = Some(principal_id.clone());
            }
            let handle = self.manager.create_running(spec).await?;
            if let Err(error) = self
                .store
                .insert_ready_warm_sandbox(handle.id.as_str(), self.workload_key.as_str())
                .await
            {
                let _ = self.manager.stop(&handle.id).await;
                return Err(WarmPoolError::Store(error));
            }
        }

        Ok(())
    }

    async fn prune_stale_ready_sandboxes(&self) -> Result<(), WarmPoolError> {
        for sandbox_id in self.store.list_ready_warm_sandbox_ids().await? {
            let id = SandboxId::new(sandbox_id.as_str());
            let failure = match self.manager.status(&id).await {
                Ok(SandboxStatus::Running) => continue,
                Ok(status) => format!("ready warm sandbox was not running: {status:?}"),
                Err(SandboxError::NotFound(_)) => "ready warm sandbox was not found".to_owned(),
                Err(error) => {
                    let error_message = error.to_string();
                    warn!(%sandbox_id, error = %error_message);
                    return Err(WarmPoolError::Sandbox(error));
                }
            };
            warn!(%sandbox_id, error = %failure, "marking stale ready warm sandbox failed");
            self.store
                .mark_warm_sandbox_failed(&sandbox_id, &failure)
                .await?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum WarmPoolError {
    #[error(transparent)]
    Store(#[from] SessionStoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use async_trait::async_trait;
    use centaur_sandbox_core::{
        ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
        SandboxResult, SandboxSpec, SandboxStatus,
    };

    use super::*;

    #[tokio::test]
    async fn replenisher_prunes_missing_ready_rows_before_counting() {
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let workload_key = format!("test-prune-{suffix}");
        let old_workload_key = format!("test-prune-old-{suffix}");
        let stale_sandbox = format!("stale-{suffix}");
        let old_stale_sandbox = format!("old-stale-{suffix}");
        let fresh_sandbox = format!("fresh-{suffix}");

        store
            .insert_ready_warm_sandbox(&stale_sandbox, &workload_key)
            .await
            .expect("insert stale warm sandbox row");
        store
            .insert_ready_warm_sandbox(&old_stale_sandbox, &old_workload_key)
            .await
            .expect("insert stale warm sandbox row for old workload");
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&workload_key)
                .await
                .expect("count ready warm sandboxes"),
            1
        );
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&old_workload_key)
                .await
                .expect("count ready warm sandboxes for old workload"),
            1
        );

        let backend = Arc::new(TestBackend::new(fresh_sandbox.clone()));
        let pool = WarmPoolManager::new(
            Arc::new(SandboxManager::new(backend.clone())),
            store.clone(),
            Arc::new(|| SandboxSpec::new("image")),
            workload_key.clone(),
            WarmPoolConfig {
                target_size: 1,
                replenish_interval: Duration::from_secs(60),
                bootstrap_iron_control_principal: None,
            },
        );

        pool.replenish_once().await.expect("replenish warm pool");

        assert_eq!(backend.created(), vec![fresh_sandbox.clone()]);
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&workload_key)
                .await
                .expect("count ready warm sandboxes"),
            1
        );
        assert_eq!(
            store
                .claim_ready_warm_sandbox(&workload_key, "test-thread")
                .await
                .expect("claim ready warm sandbox"),
            Some(fresh_sandbox)
        );
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&old_workload_key)
                .await
                .expect("count ready warm sandboxes for old workload"),
            0
        );
    }

    async fn test_store() -> Option<PgSessionStore> {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return None;
        };
        let store = PgSessionStore::connect(&url)
            .await
            .expect("connect test db");
        store.run_migrations().await.expect("run migrations");
        Some(store)
    }

    fn unique_suffix() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
            .to_string()
    }

    struct TestBackend {
        create_id: String,
        statuses: Mutex<BTreeMap<String, SandboxStatus>>,
        created: Mutex<Vec<String>>,
    }

    impl TestBackend {
        fn new(create_id: String) -> Self {
            Self {
                create_id,
                statuses: Mutex::new(BTreeMap::new()),
                created: Mutex::new(Vec::new()),
            }
        }

        fn created(&self) -> Vec<String> {
            self.created.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SandboxBackend for TestBackend {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            self.statuses
                .lock()
                .unwrap()
                .insert(self.create_id.clone(), SandboxStatus::Running);
            self.created.lock().unwrap().push(self.create_id.clone());
            Ok(SandboxHandle::new(
                SandboxId::new(self.create_id.clone()),
                self.name(),
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            Err(SandboxError::Unsupported {
                backend: self.name(),
                operation: "open_io",
            })
        }

        async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
            self.statuses
                .lock()
                .unwrap()
                .get(id.as_str())
                .cloned()
                .ok_or_else(|| SandboxError::NotFound(id.as_str().to_owned()))
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
                .unwrap()
                .iter()
                .map(|(id, status)| ObservedSandbox::new(id.clone(), self.name(), status.clone()))
                .collect())
        }

        async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
            self.statuses
                .lock()
                .unwrap()
                .insert(id.as_str().to_owned(), SandboxStatus::Stopped);
            Ok(())
        }

        async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
            self.statuses
                .lock()
                .unwrap()
                .insert(id.as_str().to_owned(), SandboxStatus::Suspended);
            Ok(())
        }

        async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
            self.statuses
                .lock()
                .unwrap()
                .insert(id.as_str().to_owned(), SandboxStatus::Running);
            Ok(())
        }
    }
}
