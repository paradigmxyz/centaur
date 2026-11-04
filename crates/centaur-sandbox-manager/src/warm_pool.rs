use std::{collections::BTreeMap, sync::Arc, time::Duration};

use centaur_sandbox_core::{SandboxError, SandboxId, SandboxResult, SandboxSpec, SandboxStatus};
use centaur_session_sqlx::{PgSessionStore, SessionStoreError};
use thiserror::Error;
use tokio::time::{MissedTickBehavior, interval};
use tracing::warn;

use crate::SandboxManager;

pub type WarmSandboxSpecFactory = Arc<dyn Fn() -> SandboxSpec + Send + Sync>;
const STALE_EVICTING_WARM_SANDBOX_AGE: Duration = Duration::from_secs(300);
/// How long a ready warm sandbox from an unconfigured pool survives. Covers a
/// rolling deploy, where a replica with the previous configuration may still
/// be filling its pools.
const UNCONFIGURED_WARM_SANDBOX_GRACE: Duration = Duration::from_secs(900);

/// One warm pool: interchangeable sandboxes booted from the same spec and
/// claimed by its workload key.
#[derive(Clone)]
pub struct WarmPool {
    pub workload_key: String,
    pub spec_factory: WarmSandboxSpecFactory,
    pub target_size: usize,
}

pub struct WarmPoolConfig {
    pub replenish_interval: Duration,
    pub bootstrap_iron_control_principal: String,
    pub max_running_sandboxes: Option<usize>,
}

/// Per-claim binding applied to a warm sandbox before it is handed out.
pub struct WarmClaim<'a> {
    pub thread_key: &'a str,
    pub iron_control_principal: Option<&'a str>,
    pub requester_principal_id: Option<&'a str>,
    pub proxy_labels: &'a BTreeMap<String, String>,
    /// Warm sandboxes boot without observability; grant it at claim when the
    /// session has the capability.
    pub observability_enabled: bool,
}

pub struct WarmPoolManager {
    manager: Arc<SandboxManager>,
    store: PgSessionStore,
    pools: Vec<WarmPool>,
    config: WarmPoolConfig,
}

impl WarmPoolManager {
    pub fn new(
        manager: Arc<SandboxManager>,
        store: PgSessionStore,
        pools: Vec<WarmPool>,
        config: WarmPoolConfig,
    ) -> Self {
        Self {
            manager,
            store,
            pools,
            config,
        }
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
        workload_key: &str,
        claim: WarmClaim<'_>,
    ) -> Result<Option<String>, WarmPoolError> {
        loop {
            let Some(sandbox_id) = self
                .store
                .claim_ready_warm_sandbox(workload_key, claim.thread_key)
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
                    if let Err(error) = self.bind_claimed(&id, &claim).await {
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
            warn!(%sandbox_id, error = %failure, thread_key = claim.thread_key);
            self.store
                .mark_warm_sandbox_failed(&sandbox_id, &failure)
                .await?;
        }
    }

    async fn bind_claimed(&self, id: &SandboxId, claim: &WarmClaim<'_>) -> SandboxResult<()> {
        // Grant observability first: a proxy repaired during assignment is
        // rebuilt from the sandbox's recorded capabilities.
        if claim.observability_enabled {
            self.manager.enable_observability(id).await?;
        }
        if let Some(principal_id) = claim.iron_control_principal {
            self.manager
                .assign_iron_control_proxy_principal(
                    id,
                    principal_id,
                    claim.requester_principal_id,
                    claim.proxy_labels,
                )
                .await?;
        }
        Ok(())
    }

    async fn replenish_once(&self) -> Result<(), WarmPoolError> {
        self.prune_stale_ready_sandboxes().await?;
        self.prune_stale_evicting_sandboxes().await?;
        self.evict_unconfigured_sandboxes().await?;

        let mut available = self.available_running_slots().await?;
        for pool in &self.pools {
            let ready = self
                .store
                .count_ready_warm_sandboxes(pool.workload_key.as_str())
                .await?
                .max(0) as usize;
            let needed = pool.target_size.saturating_sub(ready).min(available);
            available -= needed;
            // One failing pool must not starve the others.
            if let Err(error) = self.fill(pool, needed).await {
                warn!(
                    %error,
                    workload_key = pool.workload_key.as_str(),
                    "session sandbox warm pool replenishment failed"
                );
            }
        }

        Ok(())
    }

    async fn fill(&self, pool: &WarmPool, count: usize) -> Result<(), WarmPoolError> {
        for _ in 0..count {
            let mut spec = (pool.spec_factory)();
            spec.iron_control_principal =
                Some(self.config.bootstrap_iron_control_principal.clone());
            let handle = self.manager.create_running(spec).await?;
            if let Err(error) = self
                .store
                .insert_ready_warm_sandbox(handle.id.as_str(), pool.workload_key.as_str())
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

    /// Stop ready warm sandboxes no configured pool can claim, so a removed
    /// pool or a changed sandbox spec does not hold running capacity until the
    /// max-lifetime reaper. The evicted rows are finalized once the sandbox
    /// is gone, like reaper evictions.
    async fn evict_unconfigured_sandboxes(&self) -> Result<(), WarmPoolError> {
        let workload_keys: Vec<String> = self
            .pools
            .iter()
            .map(|pool| pool.workload_key.clone())
            .collect();
        for sandbox_id in self
            .store
            .list_unclaimable_ready_warm_sandbox_ids(
                &workload_keys,
                UNCONFIGURED_WARM_SANDBOX_GRACE,
            )
            .await?
        {
            if !self.store.evict_unclaimed_warm_sandbox(&sandbox_id).await? {
                continue;
            }
            match self
                .manager
                .stop(&SandboxId::new(sandbox_id.as_str()))
                .await
            {
                Ok(()) | Err(SandboxError::NotFound(_)) => {}
                Err(error) => {
                    warn!(%sandbox_id, %error, "failed to stop unconfigured warm sandbox");
                }
            }
        }
        Ok(())
    }

    async fn prune_stale_evicting_sandboxes(&self) -> Result<(), WarmPoolError> {
        for sandbox_id in self
            .store
            .list_stale_evicting_warm_sandbox_ids(STALE_EVICTING_WARM_SANDBOX_AGE)
            .await?
        {
            let id = SandboxId::new(sandbox_id.as_str());
            let failure = match self.manager.status(&id).await {
                Ok(status) if status_consumes_running_slot(&status) => {
                    match self.manager.stop(&id).await {
                        Ok(()) | Err(SandboxError::NotFound(_)) => {
                            "stale evicting warm sandbox stopped".to_owned()
                        }
                        Err(error) => {
                            let error_message = error.to_string();
                            warn!(%sandbox_id, error = %error_message);
                            return Err(WarmPoolError::Sandbox(error));
                        }
                    }
                }
                Ok(status) => format!("stale evicting warm sandbox was not running: {status:?}"),
                Err(SandboxError::NotFound(_)) => {
                    "stale evicting warm sandbox was not found".to_owned()
                }
                Err(error) => {
                    let error_message = error.to_string();
                    warn!(%sandbox_id, error = %error_message);
                    return Err(WarmPoolError::Sandbox(error));
                }
            };
            warn!(%sandbox_id, reason = %failure, "marking stale evicting warm sandbox failed");
            self.store
                .mark_warm_sandbox_failed(&sandbox_id, &failure)
                .await?;
        }
        Ok(())
    }

    async fn available_running_slots(&self) -> Result<usize, WarmPoolError> {
        let Some(max_running) = self.config.max_running_sandboxes else {
            return Ok(usize::MAX);
        };
        let running = self
            .manager
            .list_observed()
            .await?
            .into_iter()
            .filter(|observed| status_consumes_running_slot(&observed.status))
            .count();
        Ok(max_running.saturating_sub(running))
    }
}

fn status_consumes_running_slot(status: &SandboxStatus) -> bool {
    matches!(status, SandboxStatus::Running)
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

    /// Replenishment prunes ready and stale warm rows database-wide, so
    /// concurrently running tests would fail each other's sandboxes.
    /// Serialize the database-backed tests in this module.
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn replenisher_prunes_missing_ready_rows_before_counting() {
        let _serial = TEST_LOCK.lock().await;
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let workload_key = format!("test-prune-{suffix}");
        let old_workload_key = format!("test-prune-old-{suffix}");
        let stale_sandbox = format!("stale-{suffix}");
        let old_stale_sandbox = format!("old-stale-{suffix}");

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

        let backend = Arc::new(TestBackend::new(format!("fresh-{suffix}")));
        let pool = test_pool(backend.clone(), &store, &[(&workload_key, 1)]);

        pool.replenish_once().await.expect("replenish warm pool");

        let created = backend.created();
        assert_eq!(created.len(), 1);
        let fresh_sandbox = created[0].clone();
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&workload_key)
                .await
                .expect("count ready warm sandboxes"),
            1
        );
        let claim_thread = format!("test:prune-claim-{suffix}");
        insert_session_row(&store, &claim_thread).await;
        assert_eq!(
            store
                .claim_ready_warm_sandbox(&workload_key, &claim_thread)
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

    #[tokio::test]
    async fn replenisher_prunes_stale_evicting_rows() {
        let _serial = TEST_LOCK.lock().await;
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let workload_key = format!("test-evicting-{suffix}");
        let stale_sandbox = format!("stale-evicting-{suffix}");

        store
            .insert_ready_warm_sandbox(&stale_sandbox, &workload_key)
            .await
            .expect("insert stale evicting warm sandbox row");
        sqlx::query(
            r#"
            update session_warm_sandboxes
            set status = 'evicting', updated_at = now() - interval '10 minutes'
            where sandbox_id = $1
            "#,
        )
        .bind(&stale_sandbox)
        .execute(store.pool())
        .await
        .expect("make warm sandbox eviction stale");

        let backend = Arc::new(TestBackend::new(format!("fresh-{suffix}")));
        backend.set_status(&stale_sandbox, SandboxStatus::Running);
        let pool = test_pool(backend.clone(), &store, &[(&workload_key, 0)]);

        pool.replenish_once().await.expect("replenish warm pool");

        assert_eq!(
            backend
                .status(&SandboxId::new(&stale_sandbox))
                .await
                .unwrap(),
            SandboxStatus::Stopped
        );
        assert!(
            !store
                .list_referenced_sandbox_ids()
                .await
                .expect("list referenced sandboxes")
                .contains(&stale_sandbox)
        );
    }

    #[tokio::test]
    async fn claim_fails_unusable_warm_sandboxes_and_claims_the_next() {
        let _serial = TEST_LOCK.lock().await;
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let workload_key = format!("test-claim-skip-{suffix}");
        // Claims take the oldest ready row, tie-broken by sandbox id.
        let missing_sandbox = format!("a-missing-{suffix}");
        let stopped_sandbox = format!("b-stopped-{suffix}");
        let live_sandbox = format!("c-live-{suffix}");
        let thread_key = format!("test:claim-skip-{suffix}");
        insert_session_row(&store, &thread_key).await;

        let backend = Arc::new(TestBackend::new(format!("fresh-{suffix}")));
        for sandbox_id in [&missing_sandbox, &stopped_sandbox, &live_sandbox] {
            store
                .insert_ready_warm_sandbox(sandbox_id, &workload_key)
                .await
                .expect("insert warm sandbox");
        }
        backend.set_status(&stopped_sandbox, SandboxStatus::Stopped);
        backend.set_status(&live_sandbox, SandboxStatus::Running);
        let pool = test_pool(backend, &store, &[(&workload_key, 0)]);

        let claimed = pool
            .claim(
                &workload_key,
                test_claim(&thread_key, None, &BTreeMap::new(), false),
            )
            .await
            .expect("claim warm sandbox");

        assert_eq!(claimed, Some(live_sandbox));
        for sandbox_id in [&missing_sandbox, &stopped_sandbox] {
            let status: String = sqlx::query_scalar(
                "select status from session_warm_sandboxes where sandbox_id = $1",
            )
            .bind(sandbox_id)
            .fetch_one(store.pool())
            .await
            .expect("load warm sandbox status");
            assert_eq!(status, "failed", "{sandbox_id}");
        }
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&workload_key)
                .await
                .expect("count ready warm sandboxes"),
            0
        );
    }

    #[tokio::test]
    async fn claim_binds_each_turns_requester_and_observability() {
        let _serial = TEST_LOCK.lock().await;
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let workload_key = format!("test-claim-requester-{suffix}");
        let first_sandbox = format!("claim-first-{suffix}");
        let second_sandbox = format!("claim-second-{suffix}");
        let first_thread = format!("test:claim-req-a-{suffix}");
        let second_thread = format!("test:claim-req-b-{suffix}");
        for thread_key in [&first_thread, &second_thread] {
            insert_session_row(&store, thread_key).await;
        }

        let backend = Arc::new(TestBackend::new(format!("fresh-{suffix}")));
        let pool = test_pool(backend.clone(), &store, &[(&workload_key, 0)]);
        let labels = BTreeMap::from([("centaur.slack_channel_id".to_owned(), "C1".to_owned())]);

        insert_running(&store, &backend, &first_sandbox, &workload_key).await;
        let claimed = pool
            .claim(
                &workload_key,
                test_claim(&first_thread, Some("prn_req"), &labels, false),
            )
            .await
            .expect("claim first warm sandbox");
        assert_eq!(claimed, Some(first_sandbox.clone()));

        insert_running(&store, &backend, &second_sandbox, &workload_key).await;
        let claimed = pool
            .claim(
                &workload_key,
                test_claim(&second_thread, None, &labels, true),
            )
            .await
            .expect("claim second warm sandbox");
        assert_eq!(claimed, Some(second_sandbox.clone()));

        // The proxy scopes credentials to the requester, so each claim must
        // bind its own turn's requester, and no requester when there is none.
        assert_eq!(
            backend.assigned(),
            vec![
                (
                    first_sandbox,
                    "prn_conv".to_owned(),
                    Some("prn_req".to_owned()),
                    labels.clone()
                ),
                (second_sandbox.clone(), "prn_conv".to_owned(), None, labels),
            ]
        );
        // Warm sandboxes boot without observability; only the claim that
        // asks for it grants it.
        assert_eq!(backend.observability_enabled(), vec![second_sandbox]);
    }

    #[tokio::test]
    async fn replenisher_fills_each_pool_to_its_own_target() {
        let _serial = TEST_LOCK.lock().await;
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let small_pool = format!("test-pools-small-{suffix}");
        let large_pool = format!("test-pools-large-{suffix}");
        let backend = Arc::new(TestBackend::new(format!("fresh-{suffix}")));
        let pool = test_pool(
            backend.clone(),
            &store,
            &[(&small_pool, 1), (&large_pool, 2)],
        );

        pool.replenish_once().await.expect("replenish warm pools");
        pool.replenish_once()
            .await
            .expect("replenish full warm pools");

        assert_eq!(backend.created().len(), 3);
        for (workload_key, expected) in [(&small_pool, 1), (&large_pool, 2)] {
            assert_eq!(
                store
                    .count_ready_warm_sandboxes(workload_key)
                    .await
                    .expect("count ready warm sandboxes"),
                expected,
                "{workload_key}"
            );
        }
    }

    #[tokio::test]
    async fn replenisher_evicts_ready_rows_no_pool_can_claim() {
        let _serial = TEST_LOCK.lock().await;
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = unique_suffix();
        let configured_key = format!("test-configured-{suffix}");
        let removed_key = format!("test-removed-{suffix}");
        let configured_sandbox = format!("configured-{suffix}");
        let old_removed_sandbox = format!("removed-old-{suffix}");
        let new_removed_sandbox = format!("removed-new-{suffix}");
        let backend = Arc::new(TestBackend::new(format!("fresh-{suffix}")));
        for (sandbox_id, workload_key) in [
            (&configured_sandbox, &configured_key),
            (&old_removed_sandbox, &removed_key),
            (&new_removed_sandbox, &removed_key),
        ] {
            insert_running(&store, &backend, sandbox_id, workload_key).await;
        }
        sqlx::query(
            "update session_warm_sandboxes set created_at = now() - interval '1 hour' \
             where sandbox_id = any($1)",
        )
        .bind(vec![
            configured_sandbox.clone(),
            old_removed_sandbox.clone(),
        ])
        .execute(store.pool())
        .await
        .expect("age warm sandboxes");
        let pool = test_pool(backend.clone(), &store, &[(&configured_key, 1)]);

        pool.replenish_once().await.expect("replenish warm pools");

        let status = |sandbox_id: String| {
            let backend = backend.clone();
            async move { backend.status(&SandboxId::new(sandbox_id)).await.unwrap() }
        };
        assert_eq!(status(old_removed_sandbox).await, SandboxStatus::Stopped);
        // A recent row may belong to a replica still rolling out.
        assert_eq!(status(new_removed_sandbox).await, SandboxStatus::Running);
        assert_eq!(status(configured_sandbox).await, SandboxStatus::Running);
        assert_eq!(
            store
                .count_ready_warm_sandboxes(&removed_key)
                .await
                .expect("count ready warm sandboxes"),
            1
        );
        assert!(backend.created().is_empty());
    }

    async fn insert_running(
        store: &PgSessionStore,
        backend: &TestBackend,
        sandbox_id: &str,
        workload_key: &str,
    ) {
        store
            .insert_ready_warm_sandbox(sandbox_id, workload_key)
            .await
            .expect("insert warm sandbox");
        backend.set_status(sandbox_id, SandboxStatus::Running);
    }

    fn test_pool(
        backend: Arc<TestBackend>,
        store: &PgSessionStore,
        pools: &[(&String, usize)],
    ) -> WarmPoolManager {
        WarmPoolManager::new(
            Arc::new(SandboxManager::new(backend)),
            store.clone(),
            pools
                .iter()
                .map(|(workload_key, target_size)| WarmPool {
                    workload_key: (*workload_key).clone(),
                    spec_factory: Arc::new(|| SandboxSpec::new("image")),
                    target_size: *target_size,
                })
                .collect(),
            WarmPoolConfig {
                replenish_interval: Duration::from_secs(60),
                bootstrap_iron_control_principal: "prn_test_bootstrap".to_owned(),
                max_running_sandboxes: None,
            },
        )
    }

    fn test_claim<'a>(
        thread_key: &'a str,
        requester_principal_id: Option<&'a str>,
        proxy_labels: &'a BTreeMap<String, String>,
        observability_enabled: bool,
    ) -> WarmClaim<'a> {
        WarmClaim {
            thread_key,
            iron_control_principal: Some("prn_conv"),
            requester_principal_id,
            proxy_labels,
            observability_enabled,
        }
    }

    async fn test_store() -> Option<PgSessionStore> {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return None;
        };
        let store = PgSessionStore::connect(&url)
            .await
            .expect("connect test db");
        store
            .run_migrations(centaur_session_sqlx::TextSearchBackend::Postgres)
            .await
            .expect("run migrations");
        Some(store)
    }

    /// Claimed warm rows reference `sessions.thread_key`, so a claim test must
    /// create the session row first.
    async fn insert_session_row(store: &PgSessionStore, thread_key: &str) {
        sqlx::query(
            "insert into sessions (thread_key, harness_type, status) \
             values ($1, 'codex', 'idle') on conflict (thread_key) do nothing",
        )
        .bind(thread_key)
        .execute(store.pool())
        .await
        .expect("insert session row");
    }

    fn unique_suffix() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
            .to_string()
    }

    type RecordedAssignment = (String, String, Option<String>, BTreeMap<String, String>);

    struct TestBackend {
        create_prefix: String,
        statuses: Mutex<BTreeMap<String, SandboxStatus>>,
        created: Mutex<Vec<String>>,
        assigned: Mutex<Vec<RecordedAssignment>>,
        observability_enabled: Mutex<Vec<String>>,
    }

    impl TestBackend {
        fn new(create_prefix: String) -> Self {
            Self {
                create_prefix,
                statuses: Mutex::new(BTreeMap::new()),
                created: Mutex::new(Vec::new()),
                assigned: Mutex::new(Vec::new()),
                observability_enabled: Mutex::new(Vec::new()),
            }
        }

        fn created(&self) -> Vec<String> {
            self.created.lock().unwrap().clone()
        }

        fn observability_enabled(&self) -> Vec<String> {
            self.observability_enabled.lock().unwrap().clone()
        }

        fn assigned(&self) -> Vec<RecordedAssignment> {
            self.assigned.lock().unwrap().clone()
        }

        fn set_status(&self, sandbox_id: &str, status: SandboxStatus) {
            self.statuses
                .lock()
                .unwrap()
                .insert(sandbox_id.to_owned(), status);
        }
    }

    #[async_trait]
    impl SandboxBackend for TestBackend {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            let mut created = self.created.lock().unwrap();
            let id = format!("{}-{}", self.create_prefix, created.len());
            self.statuses
                .lock()
                .unwrap()
                .insert(id.clone(), SandboxStatus::Running);
            created.push(id.clone());
            Ok(SandboxHandle::new(SandboxId::new(id), self.name()))
        }

        async fn enable_observability(&self, id: &SandboxId) -> SandboxResult<()> {
            self.observability_enabled
                .lock()
                .unwrap()
                .push(id.as_str().to_owned());
            Ok(())
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

        async fn assign_iron_control_proxy_principal(
            &self,
            id: &SandboxId,
            principal_id: &str,
            requester_principal_id: Option<&str>,
            labels: &BTreeMap<String, String>,
        ) -> SandboxResult<()> {
            self.assigned.lock().unwrap().push((
                id.as_str().to_owned(),
                principal_id.to_owned(),
                requester_principal_id.map(ToOwned::to_owned),
                labels.clone(),
            ));
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
