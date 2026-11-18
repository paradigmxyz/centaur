//! Console scheduled tasks on Absurd (RFC 0006 Decision 3).
//!
//! The console owns the `scheduled_tasks` rows; api-rs owns when a run
//! happens. Each pending occurrence is one durable `scheduled_task.tick`
//! task that sleeps until `scheduled_for`, claims the occurrence from the
//! console, spawns the `console_workflow` run with Rails' idempotency key,
//! reports the result, and chains the next tick. The wire contract is
//! `services/api-rs/rfcs/0006a-scheduled-task-runner-contract.md`.

pub mod console;
pub mod keys;
pub mod spawner;
pub mod store;
mod tick;

use std::{sync::Arc, time::Duration};

use absurd::{
    Client, ClientOptions, CreateQueueOptions, RetryKind, RetryStrategy, RetryTaskOptions,
    SpawnOptions, TaskRegistrationOptions, Worker, WorkerOptions,
};
use chrono::{DateTime, SubsecRound, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub use console::{ConsoleClient, ConsoleError};
pub use spawner::{RunSpawner, SpawnError, SpawnedRun};
pub use tick::{TickOutcome, TickParams, TickResult, polynomially_longer};

/// The runner's Absurd queue.
pub const QUEUE: &str = "centaur_scheduled_tasks";
/// The tick task's name.
pub const TICK_TASK: &str = "scheduled_task.tick";
/// `ScheduledTask`'s oid prefix.
pub const OID_PREFIX: &str = "tsk";

/// Runner settings. [`RunnerConfig::default`] is the RFC 0006a policy.
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    pub queue_name: String,
    pub worker_concurrency: usize,
    pub worker_poll_interval: Duration,
    /// Periodic reconcile; `None` reconciles only at startup.
    pub reconcile_interval: Option<Duration>,
    pub tick_max_attempts: i32,
    pub tick_retry: RetryStrategy,
    /// `ScheduledTaskRunJob`'s `retry_on CentaurApiClient::Error, attempts: 5`.
    pub spawn_max_attempts: i32,
    /// The wait after the `n`th failed spawn ([`polynomially_longer`]).
    pub spawn_retry_delay: fn(i32) -> Duration,
    /// How long a tick waits before re-checking that every earlier
    /// occurrence of its task has checkpointed its claim.
    pub claim_wait: Duration,
    /// How many [`Self::claim_wait`]s that check waits at most before
    /// claiming anyway (the console decides).
    pub claim_wait_attempts: u32,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            queue_name: QUEUE.to_owned(),
            worker_concurrency: 2,
            worker_poll_interval: Duration::from_millis(250),
            reconcile_interval: Some(Duration::from_secs(60)),
            tick_max_attempts: 20,
            tick_retry: RetryStrategy {
                kind: RetryKind::Exponential,
                base_seconds: Some(5.0),
                factor: Some(2.0),
                max_seconds: Some(300.0),
            },
            spawn_max_attempts: 5,
            spawn_retry_delay: polynomially_longer,
            claim_wait: Duration::from_secs(5),
            claim_wait_attempts: 12,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("invalid scheduled task runner configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Absurd(#[from] absurd::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// What reconciling one task did with its tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TickAction {
    Created,
    Existing,
    Rearmed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EnsuredTick {
    pub task_id: String,
    pub action: TickAction,
}

/// `state` of a reconciled task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Scheduled,
    Unscheduled,
    Deleted,
}

/// The outcome of reconciling one task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskReconcile {
    pub state: TaskState,
    pub next_run_at: Option<DateTime<Utc>>,
    pub tick: Option<EnsuredTick>,
    pub cancelled: usize,
}

/// Counts from a full reconcile pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileSummary {
    pub tasks: usize,
    pub created: usize,
    pub rearmed: usize,
    pub cancelled: usize,
}

/// A queued manual run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualRun {
    pub scheduled_for: DateTime<Utc>,
    pub task_id: String,
    pub created: bool,
}

pub(crate) struct Inner {
    pub(crate) client: Client,
    pub(crate) console_db: PgPool,
    pub(crate) console: ConsoleClient,
    pub(crate) spawner: Arc<dyn RunSpawner>,
    pub(crate) config: RunnerConfig,
}

/// The scheduled-task runner. Cheap to clone.
#[derive(Clone)]
pub struct ScheduledTaskRunner {
    inner: Arc<Inner>,
}

/// The running worker and reconcile loop; dropping it stops neither, call
/// [`RunnerHandle::shutdown`].
pub struct RunnerHandle {
    worker: Option<Worker>,
    reconciler: Option<JoinHandle<()>>,
}

impl RunnerHandle {
    pub async fn shutdown(self) {
        if let Some(reconciler) = self.reconciler {
            reconciler.abort();
        }
        if let Some(worker) = self.worker
            && let Err(error) = worker.close().await
        {
            warn!(%error, "scheduled task worker stopped with an error");
        }
    }
}

/// What kind of occurrence a tick is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickKind {
    Scheduled,
    Manual,
}

impl ScheduledTaskRunner {
    /// Creates the queue and registers the tick task. `pool` is the api-rs
    /// database (Absurd schema); `console_db` is the read-only console
    /// database.
    pub async fn new(
        config: RunnerConfig,
        pool: PgPool,
        console_db: PgPool,
        console: ConsoleClient,
        spawner: Arc<dyn RunSpawner>,
    ) -> Result<Self, RunnerError> {
        if config.queue_name.is_empty()
            || !config
                .queue_name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(RunnerError::Config(format!(
                "queue name {:?} must be lowercase letters, digits, and underscores",
                config.queue_name
            )));
        }
        if config.spawn_max_attempts < 1 || config.tick_max_attempts < 1 {
            return Err(RunnerError::Config(
                "attempt limits must be >= 1".to_owned(),
            ));
        }
        if config.claim_wait_attempts < 1 {
            return Err(RunnerError::Config(
                "claim_wait_attempts must be >= 1".to_owned(),
            ));
        }
        let client = Client::from_pool_with_options(
            pool,
            ClientOptions {
                queue_name: config.queue_name.clone(),
                ..ClientOptions::default()
            },
        )?;
        client
            .create_queue(None, CreateQueueOptions::default())
            .await?;
        let inner = Arc::new(Inner {
            client: client.clone(),
            console_db,
            console,
            spawner,
            config,
        });
        let runner = Self { inner };
        let task_runner = runner.clone();
        client.register_task_with(
            TaskRegistrationOptions::new(TICK_TASK),
            move |params: TickParams, ctx| {
                let runner = task_runner.clone();
                async move { tick::run(&runner.inner, params, ctx).await }
            },
        )?;
        Ok(runner)
    }

    /// The runner's Absurd client (tests drive it with `work_batch`).
    pub fn client(&self) -> &Client {
        &self.inner.client
    }

    pub fn config(&self) -> &RunnerConfig {
        &self.inner.config
    }

    /// Starts the tick worker, reconciles once, and starts the periodic reconcile loop.
    pub fn start(&self, worker_id: &str) -> RunnerHandle {
        let worker = self.inner.client.start_worker(WorkerOptions {
            worker_id: Some(worker_id.to_owned()),
            concurrency: self.inner.config.worker_concurrency.max(1),
            poll_interval: self.inner.config.worker_poll_interval,
            on_error: Some(Arc::new(|error| {
                warn!(%error, "scheduled task worker error");
            })),
            ..WorkerOptions::default()
        });
        let runner = self.clone();
        let interval = self.inner.config.reconcile_interval;
        let reconciler = Some(tokio::spawn(async move {
            loop {
                match runner.reconcile_all().await {
                    Ok(summary) => info!(
                        tasks = summary.tasks,
                        created = summary.created,
                        rearmed = summary.rearmed,
                        cancelled = summary.cancelled,
                        "reconciled scheduled tasks"
                    ),
                    Err(error) => warn!(%error, "scheduled task reconcile failed"),
                }
                let Some(interval) = interval else { break };
                tokio::time::sleep(interval).await;
            }
        }));
        info!(
            queue = %self.inner.config.queue_name,
            task = TICK_TASK,
            "started scheduled task runner"
        );
        RunnerHandle {
            worker: Some(worker),
            reconciler,
        }
    }

    /// Ensures one tick per enabled task's `next_run_at`, re-arms failed
    /// scheduled ticks, and cancels the ticks of deleted tasks.
    pub async fn reconcile_all(&self) -> Result<ReconcileSummary, RunnerError> {
        let inner = &self.inner;
        let queue = &inner.config.queue_name;
        let mut summary = ReconcileSummary::default();

        // Ticks of deleted tasks, and failed scheduled ticks.
        let open = store::open_ticks(inner.client.pool(), queue, TICK_TASK, None).await?;
        let mut referenced: Vec<i64> = open
            .iter()
            .filter_map(store::TickRow::scheduled_task_id)
            .collect();
        referenced.sort_unstable();
        referenced.dedup();
        let existing = store::existing_task_ids(&inner.console_db, &referenced).await?;
        for tick in &open {
            let Some(id) = tick.scheduled_task_id() else {
                continue;
            };
            if !existing.contains(&id) {
                if tick.state != "failed" && self.cancel_tick(tick).await {
                    summary.cancelled += 1;
                }
            } else if tick.state == "failed" && !tick.manual() && self.rearm_tick(tick).await {
                summary.rearmed += 1;
            }
        }

        // One tick per occurrence.
        let tasks = store::scheduled_tasks(&inner.console_db).await?;
        summary.tasks = tasks.len();
        let keys: Vec<String> = tasks
            .iter()
            .map(|(id, at)| keys::tick_key(*id, *at))
            .collect();
        let known = store::ticks_by_key(inner.client.pool(), queue, &keys).await?;
        for ((id, at), key) in tasks.iter().zip(&keys) {
            if known
                .iter()
                .any(|tick| tick.idempotency_key.as_deref() == Some(key))
            {
                continue;
            }
            let spawned = self.spawn_tick(*id, *at, TickKind::Scheduled).await?;
            if spawned.created {
                summary.created += 1;
            }
        }
        Ok(summary)
    }

    /// Reconciles one task (the console's notification after a write).
    pub async fn reconcile_task(&self, id: i64) -> Result<TaskReconcile, RunnerError> {
        let inner = &self.inner;
        let Some(row) = store::find_task(&inner.console_db, id).await? else {
            let open = store::open_ticks(
                inner.client.pool(),
                &inner.config.queue_name,
                TICK_TASK,
                Some(id),
            )
            .await?;
            let mut cancelled = 0;
            for tick in open.iter().filter(|tick| tick.state != "failed") {
                if self.cancel_tick(tick).await {
                    cancelled += 1;
                }
            }
            return Ok(TaskReconcile {
                state: TaskState::Deleted,
                next_run_at: None,
                tick: None,
                cancelled,
            });
        };
        let (true, Some(next_run_at)) = (row.enabled, row.next_run_at) else {
            return Ok(TaskReconcile {
                state: TaskState::Unscheduled,
                next_run_at: None,
                tick: None,
                cancelled: 0,
            });
        };
        let tick = self.ensure_tick(id, next_run_at).await?;
        Ok(TaskReconcile {
            state: TaskState::Scheduled,
            next_run_at: Some(next_run_at),
            tick: Some(tick),
            cancelled: 0,
        })
    }

    /// Ensures the tick for `(id, scheduled_for)`, re-arming it if it failed.
    pub async fn ensure_tick(
        &self,
        id: i64,
        scheduled_for: DateTime<Utc>,
    ) -> Result<EnsuredTick, RunnerError> {
        let key = keys::tick_key(id, scheduled_for);
        let known = store::ticks_by_key(
            self.inner.client.pool(),
            &self.inner.config.queue_name,
            std::slice::from_ref(&key),
        )
        .await?;
        if let Some(tick) = known.into_iter().next() {
            let action = if tick.state == "failed" && self.rearm_tick(&tick).await {
                TickAction::Rearmed
            } else {
                TickAction::Existing
            };
            return Ok(EnsuredTick {
                task_id: tick.task_id,
                action,
            });
        }
        let spawned = self
            .spawn_tick(id, scheduled_for, TickKind::Scheduled)
            .await?;
        Ok(EnsuredTick {
            task_id: spawned.task_id,
            action: if spawned.created {
                TickAction::Created
            } else {
                TickAction::Existing
            },
        })
    }

    /// Queues a manual run (`scheduled_for = now`, whole seconds, as Rails'
    /// `Time.current.iso8601`). The claim re-checks `enabled`.
    pub async fn run_now(&self, id: i64, now: DateTime<Utc>) -> Result<ManualRun, RunnerError> {
        let scheduled_for = now.trunc_subsecs(0);
        let spawned = self.spawn_tick(id, scheduled_for, TickKind::Manual).await?;
        Ok(ManualRun {
            scheduled_for,
            task_id: spawned.task_id,
            created: spawned.created,
        })
    }

    pub(crate) async fn spawn_tick(
        &self,
        id: i64,
        scheduled_for: DateTime<Utc>,
        kind: TickKind,
    ) -> Result<absurd::SpawnResult, RunnerError> {
        spawn_tick(&self.inner, id, scheduled_for, kind).await
    }

    async fn cancel_tick(&self, tick: &store::TickRow) -> bool {
        match self.inner.client.cancel_task(&tick.task_id, None).await {
            Ok(()) => {
                info!(tick_task_id = %tick.task_id, scheduled_task_id = ?tick.scheduled_task_id(), "cancelled tick of a deleted scheduled task");
                true
            }
            Err(error) => {
                warn!(%error, tick_task_id = %tick.task_id, "failed to cancel scheduled task tick");
                false
            }
        }
    }

    /// Gives a failed tick one more attempt, in place: its completed steps
    /// are not redone. A concurrent re-arm by another replica loses
    /// harmlessly.
    async fn rearm_tick(&self, tick: &store::TickRow) -> bool {
        match self
            .inner
            .client
            .retry_task(&tick.task_id, RetryTaskOptions::default())
            .await
        {
            Ok(_) => {
                info!(tick_task_id = %tick.task_id, scheduled_task_id = ?tick.scheduled_task_id(), "re-armed failed scheduled task tick");
                true
            }
            Err(error) => {
                warn!(%error, tick_task_id = %tick.task_id, "failed to re-arm scheduled task tick");
                false
            }
        }
    }
}

pub(crate) async fn spawn_tick(
    inner: &Inner,
    id: i64,
    scheduled_for: DateTime<Utc>,
    kind: TickKind,
) -> Result<absurd::SpawnResult, RunnerError> {
    let scheduled_for = keys::to_micros(scheduled_for);
    let key = match kind {
        TickKind::Scheduled => keys::tick_key(id, scheduled_for),
        TickKind::Manual => keys::manual_tick_key(id, scheduled_for),
    };
    let manual = kind != TickKind::Scheduled;
    let params = TickParams {
        scheduled_task_id: id,
        scheduled_task_oid: centaur_console_db::oid(OID_PREFIX, id),
        scheduled_for: keys::wire_time(scheduled_for),
        manual,
    };
    let spawned = inner
        .client
        .spawn(
            TICK_TASK,
            params,
            SpawnOptions {
                idempotency_key: Some(key),
                max_attempts: Some(inner.config.tick_max_attempts),
                retry_strategy: Some(inner.config.tick_retry.clone()),
                ..SpawnOptions::default()
            },
        )
        .await?;
    if spawned.created {
        info!(
            scheduled_task_id = id,
            scheduled_for = %keys::wire_time(scheduled_for),
            ?kind,
            tick_task_id = %spawned.task_id,
            "spawned scheduled task tick"
        );
    }
    Ok(spawned)
}
