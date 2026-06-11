use std::{
    collections::HashMap,
    env,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    process,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use futures::{future::BoxFuture, FutureExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{postgres::PgPoolOptions, types::Json, PgPool, Row};
use tokio::{
    sync::{watch, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep, Instant},
};

pub type Result<T> = std::result::Result<T, Error>;
pub type JsonValue = Value;
pub type JsonObject = Map<String, Value>;

const MAX_QUEUE_NAME_LENGTH: usize = 57;
const DEFAULT_QUEUE_NAME: &str = "default";
const DEFAULT_MAX_ATTEMPTS: i32 = 5;
const DEFAULT_CLAIM_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(1);
const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);
const UNKNOWN_TASK_DEFER_BASE_SECONDS: u64 = 15;
const UNKNOWN_TASK_DEFER_JITTER_SECONDS: u64 = 15;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InvalidOptions(String),
    /// A task handler failed with an application-level error. The boxed
    /// error keeps the full `source()` chain intact instead of flattening
    /// it to a string.
    #[error(transparent)]
    TaskFailed(Box<dyn std::error::Error + Send + Sync>),
    #[error("queue name must be provided")]
    MissingQueueName,
    #[error("queue name {name:?} is too long (max {max} bytes)")]
    QueueNameTooLong { name: String, max: usize },
    #[error("task {task_name:?} is not registered. provide SpawnOptions.queue when spawning unregistered tasks")]
    UnregisteredTask { task_name: String },
    #[error("task {task_name:?} is registered for queue {registered_queue:?} but spawn requested queue {requested_queue:?}")]
    QueueMismatch {
        task_name: String,
        registered_queue: String,
        requested_queue: String,
    },
    #[error("task {0:?} not found")]
    TaskNotFound(String),
    #[error("task suspended")]
    Suspend,
    #[error("task cancelled")]
    Cancelled,
    #[error("task already failed")]
    FailedRun,
    #[error("{0}")]
    Timeout(String),
    #[error("invalid task headers: {0}")]
    InvalidTaskHeaders(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error("worker task join failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RetryKind {
    Fixed,
    Exponential,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryStrategy {
    pub kind: RetryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CancellationPolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_delay: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub max_attempts: Option<i32>,
    pub retry_strategy: Option<RetryStrategy>,
    pub headers: Option<JsonObject>,
    pub queue: Option<String>,
    pub cancellation: Option<CancellationPolicy>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum QueueStorageMode {
    #[default]
    Unpartitioned,
    Partitioned,
}

impl QueueStorageMode {
    fn as_str(self) -> &'static str {
        match self {
            QueueStorageMode::Unpartitioned => "unpartitioned",
            QueueStorageMode::Partitioned => "partitioned",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum QueueDetachMode {
    None,
    Empty,
}

impl QueueDetachMode {
    fn as_str(self) -> &'static str {
        match self {
            QueueDetachMode::None => "none",
            QueueDetachMode::Empty => "empty",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueuePolicyOptions {
    pub partition_lookahead: Option<String>,
    pub partition_lookback: Option<String>,
    pub cleanup_ttl: Option<String>,
    pub cleanup_limit: Option<i32>,
    pub detach_mode: Option<QueueDetachMode>,
    pub detach_min_age: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateQueueOptions {
    pub storage_mode: QueueStorageMode,
    pub policy: QueuePolicyOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePolicy {
    pub queue_name: String,
    pub storage_mode: QueueStorageMode,
    pub partition_lookahead: String,
    pub partition_lookback: String,
    pub cleanup_ttl: String,
    pub cleanup_limit: i32,
    pub detach_mode: QueueDetachMode,
    pub detach_min_age: String,
}

#[derive(Debug, Clone, Default)]
pub struct RetryTaskOptions {
    pub queue: Option<String>,
    pub max_attempts: Option<i32>,
    pub spawn_new_task: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpawnResult {
    pub task_id: String,
    pub run_id: String,
    pub attempt: i32,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskResultState {
    Pending,
    Running,
    Sleeping,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum TaskResultSnapshot {
    Pending,
    Running,
    Sleeping,
    Completed { result: Value },
    Failed { failure: Value },
    Cancelled,
}

impl TaskResultSnapshot {
    pub fn state(&self) -> TaskResultState {
        match self {
            TaskResultSnapshot::Pending => TaskResultState::Pending,
            TaskResultSnapshot::Running => TaskResultState::Running,
            TaskResultSnapshot::Sleeping => TaskResultState::Sleeping,
            TaskResultSnapshot::Completed { .. } => TaskResultState::Completed,
            TaskResultSnapshot::Failed { .. } => TaskResultState::Failed,
            TaskResultSnapshot::Cancelled => TaskResultState::Cancelled,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskResultSnapshot::Completed { .. }
                | TaskResultSnapshot::Failed { .. }
                | TaskResultSnapshot::Cancelled
        )
    }

    pub fn result<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        match self {
            TaskResultSnapshot::Completed { result } => {
                Ok(Some(serde_json::from_value(result.clone())?))
            }
            _ => Ok(None),
        }
    }

    pub fn failure<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        match self {
            TaskResultSnapshot::Failed { failure } => {
                Ok(Some(serde_json::from_value(failure.clone())?))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaimedTask {
    pub run_id: String,
    pub task_id: String,
    pub task_name: String,
    pub attempt: i32,
    pub params: Value,
    pub retry_strategy: Value,
    pub max_attempts: Option<i32>,
    pub headers: Option<JsonObject>,
    pub wake_event: Option<String>,
    pub event_payload: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct StepHandle<T> {
    pub name: String,
    pub checkpoint_name: String,
    pub done: bool,
    pub state: Option<T>,
}

#[derive(Debug, Clone)]
pub struct TaskRegistrationOptions {
    pub name: String,
    pub queue: Option<String>,
    pub default_max_attempts: Option<i32>,
    pub default_cancellation: Option<CancellationPolicy>,
}

impl TaskRegistrationOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            queue: None,
            default_max_attempts: None,
            default_cancellation: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct Hooks {
    pub before_spawn: Option<BeforeSpawnHook>,
    pub wrap_task_execution: Option<WrapTaskExecutionHook>,
}

pub type BeforeSpawnHook = Arc<
    dyn Fn(String, Value, SpawnOptions) -> BoxFuture<'static, Result<SpawnOptions>> + Send + Sync,
>;

pub type TaskExecute = Box<dyn FnOnce() -> BoxFuture<'static, Result<Value>> + Send + 'static>;

pub type WrapTaskExecutionHook =
    Arc<dyn Fn(TaskContext, TaskExecute) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

#[derive(Clone)]
pub struct ClientOptions {
    pub database_url: Option<String>,
    pub pool: Option<PgPool>,
    pub queue_name: String,
    pub default_max_attempts: i32,
    pub hooks: Hooks,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            database_url: None,
            pool: None,
            queue_name: DEFAULT_QUEUE_NAME.to_string(),
            default_max_attempts: DEFAULT_MAX_ATTEMPTS,
            hooks: Hooks::default(),
        }
    }
}

#[derive(Clone)]
pub struct Client {
    pool: PgPool,
    owned_pool: bool,
    queue_name: String,
    default_max_attempts: i32,
    hooks: Hooks,
    registry: Arc<RwLock<HashMap<String, RegisteredTask>>>,
}

type Handler =
    Arc<dyn Fn(TaskContext, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync + 'static>;

#[derive(Clone)]
struct RegisteredTask {
    queue: String,
    default_max_attempts: Option<i32>,
    default_cancellation: Option<CancellationPolicy>,
    handler: Handler,
}

#[derive(Debug, Clone)]
pub struct WorkBatchOptions {
    pub worker_id: String,
    pub claim_timeout: Duration,
    pub batch_size: usize,
}

impl Default for WorkBatchOptions {
    fn default() -> Self {
        Self {
            worker_id: "worker".to_string(),
            claim_timeout: DEFAULT_CLAIM_TIMEOUT,
            batch_size: 1,
        }
    }
}

#[derive(Clone)]
pub struct WorkerOptions {
    pub worker_id: Option<String>,
    pub claim_timeout: Duration,
    pub batch_size: Option<usize>,
    pub concurrency: usize,
    pub poll_interval: Duration,
    pub on_error: Option<Arc<dyn Fn(Error) + Send + Sync>>,
    pub fatal_on_lease_timeout: bool,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            worker_id: None,
            claim_timeout: DEFAULT_CLAIM_TIMEOUT,
            batch_size: None,
            concurrency: 1,
            poll_interval: DEFAULT_POLL_INTERVAL,
            on_error: None,
            fatal_on_lease_timeout: true,
        }
    }
}

pub struct Worker {
    shutdown: watch::Sender<bool>,
    join: JoinHandle<Result<()>>,
}

impl Worker {
    pub async fn close(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        self.join.await?
    }
}

impl Client {
    pub async fn connect(options: ClientOptions) -> Result<Self> {
        let (pool, owned_pool) = match options.pool.clone() {
            Some(pool) => (pool, false),
            None => {
                let database_url = resolve_database_url(options.database_url.as_deref());
                let pool = PgPoolOptions::new()
                    .max_connections(10)
                    .connect(&database_url)
                    .await?;
                (pool, true)
            }
        };
        Self::from_pool_with_options_and_ownership(pool, options, owned_pool)
    }

    pub fn from_pool(pool: PgPool) -> Result<Self> {
        Self::from_pool_with_options(pool, ClientOptions::default())
    }

    pub fn from_pool_with_options(pool: PgPool, options: ClientOptions) -> Result<Self> {
        Self::from_pool_with_options_and_ownership(pool, options, false)
    }

    fn from_pool_with_options_and_ownership(
        pool: PgPool,
        options: ClientOptions,
        owned_pool: bool,
    ) -> Result<Self> {
        let queue_name = validate_queue_name(&options.queue_name)?;
        let default_max_attempts = if options.default_max_attempts > 0 {
            options.default_max_attempts
        } else {
            DEFAULT_MAX_ATTEMPTS
        };
        Ok(Self {
            pool,
            owned_pool,
            queue_name,
            default_max_attempts,
            hooks: options.hooks,
            registry: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn queue_name(&self) -> &str {
        &self.queue_name
    }

    pub async fn close(&self) {
        if self.owned_pool {
            self.pool.close().await;
        }
    }

    pub fn register_task<P, R, F, Fut>(&self, name: impl Into<String>, handler: F) -> Result<()>
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + Send + 'static,
        F: Fn(P, TaskContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        self.register_task_with(TaskRegistrationOptions::new(name), handler)
    }

    pub fn register_task_with<P, R, F, Fut>(
        &self,
        options: TaskRegistrationOptions,
        handler: F,
    ) -> Result<()>
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + Send + 'static,
        F: Fn(P, TaskContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        if options.name.trim().is_empty() {
            return Err(Error::InvalidOptions(
                "task registration requires a name".to_string(),
            ));
        }
        if matches!(options.default_max_attempts, Some(value) if value < 1) {
            return Err(Error::InvalidOptions(
                "default_max_attempts must be at least 1".to_string(),
            ));
        }
        let queue = validate_queue_name(options.queue.as_deref().unwrap_or(&self.queue_name))?;
        let name = options.name.clone();
        let handler = Arc::new(move |ctx: TaskContext, raw: Value| {
            let params = serde_json::from_value::<P>(raw);
            let fut = match params {
                Ok(params) => EitherFuture::Handler(Box::pin(handler(params, ctx))),
                Err(err) => EitherFuture::Immediate(Some(Err(Error::Json(err)))),
            };
            async move {
                let rv = fut.await?;
                Ok(serde_json::to_value(rv)?)
            }
            .boxed()
        });

        let mut registry = self
            .registry
            .write()
            .map_err(|_| Error::InvalidOptions("task registry lock poisoned".to_string()))?;
        registry.insert(
            name,
            RegisteredTask {
                queue,
                default_max_attempts: options.default_max_attempts,
                default_cancellation: options.default_cancellation,
                handler,
            },
        );
        Ok(())
    }

    pub async fn create_queue(
        &self,
        queue_name: Option<&str>,
        options: CreateQueueOptions,
    ) -> Result<()> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        match options.storage_mode {
            QueueStorageMode::Unpartitioned => {
                sqlx::query("SELECT absurd.create_queue($1)")
                    .bind(&queue)
                    .execute(&self.pool)
                    .await?;
            }
            QueueStorageMode::Partitioned => {
                sqlx::query("SELECT absurd.create_queue($1, $2)")
                    .bind(&queue)
                    .bind(options.storage_mode.as_str())
                    .execute(&self.pool)
                    .await?;
            }
        }
        self.set_queue_policy(Some(&queue), options.policy).await
    }

    pub async fn set_queue_policy(
        &self,
        queue_name: Option<&str>,
        options: QueuePolicyOptions,
    ) -> Result<()> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        let payload = queue_policy_payload(options);
        if payload.is_empty() {
            return Ok(());
        }
        sqlx::query("SELECT absurd.set_queue_policy($1, $2::jsonb)")
            .bind(&queue)
            .bind(Json(Value::Object(payload)))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_queue_policy(&self, queue_name: Option<&str>) -> Result<Option<QueuePolicy>> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        let row = sqlx::query(
            r#"
            SELECT
              queue_name,
              storage_mode,
              partition_lookahead::text,
              partition_lookback::text,
              cleanup_ttl::text AS cleanup_ttl,
              cleanup_limit,
              detach_mode,
              detach_min_age::text
            FROM absurd.get_queue_policy($1)
            "#,
        )
        .bind(&queue)
        .fetch_optional(&self.pool)
        .await?;

        row.map(queue_policy_from_row).transpose()
    }

    pub async fn drop_queue(&self, queue_name: Option<&str>) -> Result<()> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        sqlx::query("SELECT absurd.drop_queue($1)")
            .bind(&queue)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_queues(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT queue_name FROM absurd.list_queues()")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| row.try_get::<String, _>("queue_name").map_err(Error::from))
            .collect()
    }

    pub async fn spawn<P: Serialize>(
        &self,
        task_name: &str,
        params: P,
        options: SpawnOptions,
    ) -> Result<SpawnResult> {
        let params_value = serde_json::to_value(params)?;
        let (queue, effective_options) = self.resolve_spawn(task_name, options)?;
        let effective_options = if let Some(hook) = &self.hooks.before_spawn {
            hook(
                task_name.to_string(),
                params_value.clone(),
                effective_options,
            )
            .await?
        } else {
            effective_options
        };
        let payload = normalize_spawn_options(effective_options);

        let row = sqlx::query(
            r#"
            SELECT task_id::text AS task_id, run_id::text AS run_id, attempt, created
            FROM absurd.spawn_task($1, $2, $3::jsonb, $4::jsonb)
            "#,
        )
        .bind(&queue)
        .bind(task_name)
        .bind(Json(params_value))
        .bind(Json(Value::Object(payload)))
        .fetch_one(&self.pool)
        .await?;

        spawn_result_from_row(row)
    }

    pub async fn emit_event<P: Serialize>(
        &self,
        event_name: &str,
        payload: P,
        queue_name: Option<&str>,
    ) -> Result<()> {
        if event_name.is_empty() {
            return Err(Error::InvalidOptions(
                "event_name must be a non-empty string".to_string(),
            ));
        }
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        sqlx::query("SELECT absurd.emit_event($1, $2, $3::jsonb)")
            .bind(&queue)
            .bind(event_name)
            .bind(Json(serde_json::to_value(payload)?))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn fetch_task_result(
        &self,
        task_id: &str,
        queue_name: Option<&str>,
    ) -> Result<Option<TaskResultSnapshot>> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        fetch_task_result_snapshot(&self.pool, &queue, task_id).await
    }

    pub async fn await_task_result(
        &self,
        task_id: &str,
        queue_name: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<TaskResultSnapshot> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        let pool = self.pool.clone();
        let queue_for_fetch = queue.clone();
        let task_id_for_fetch = task_id.to_string();
        await_task_result_with_backoff(
            move || {
                let pool = pool.clone();
                let queue = queue_for_fetch.clone();
                let task_id = task_id_for_fetch.clone();
                async move { fetch_task_result_snapshot(&pool, &queue, &task_id).await }.boxed()
            },
            task_id,
            timeout,
            None::<fn() -> BoxFuture<'static, Result<()>>>,
        )
        .await
    }

    pub async fn retry_task(
        &self,
        task_id: &str,
        options: RetryTaskOptions,
    ) -> Result<SpawnResult> {
        let queue = validate_queue_name(options.queue.as_deref().unwrap_or(&self.queue_name))?;
        let mut payload = Map::new();
        if let Some(max_attempts) = options.max_attempts {
            payload.insert("max_attempts".to_string(), json!(max_attempts));
        }
        if options.spawn_new_task {
            payload.insert("spawn_new".to_string(), json!(true));
        }

        let row = sqlx::query(
            r#"
            SELECT task_id::text AS task_id, run_id::text AS run_id, attempt, created
            FROM absurd.retry_task($1, $2::uuid, $3::jsonb)
            "#,
        )
        .bind(&queue)
        .bind(task_id)
        .bind(Json(Value::Object(payload)))
        .fetch_one(&self.pool)
        .await?;

        spawn_result_from_row(row)
    }

    pub async fn cancel_task(&self, task_id: &str, queue_name: Option<&str>) -> Result<()> {
        let queue = validate_queue_name(queue_name.unwrap_or(&self.queue_name))?;
        sqlx::query("SELECT absurd.cancel_task($1, $2::uuid)")
            .bind(&queue)
            .bind(task_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn claim_tasks(&self, options: WorkBatchOptions) -> Result<Vec<ClaimedTask>> {
        let claim_timeout = duration_seconds(options.claim_timeout);
        let batch_size = i32::try_from(options.batch_size.max(1)).unwrap_or(i32::MAX);
        let rows = sqlx::query(
            r#"
            SELECT run_id::text AS run_id,
                   task_id::text AS task_id,
                   attempt,
                   task_name,
                   params,
                   retry_strategy,
                   max_attempts,
                   headers,
                   wake_event,
                   event_payload
            FROM absurd.claim_task($1, $2, $3, $4)
            "#,
        )
        .bind(&self.queue_name)
        .bind(if options.worker_id.is_empty() {
            "worker"
        } else {
            options.worker_id.as_str()
        })
        .bind(claim_timeout)
        .bind(batch_size)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(claimed_task_from_row).collect()
    }

    pub async fn work_batch(&self, options: WorkBatchOptions) -> Result<()> {
        let claim_timeout = options.claim_timeout;
        let tasks = self.claim_tasks(options).await?;
        for task in tasks {
            self.execute_task(task, claim_timeout, false).await?;
        }
        Ok(())
    }

    pub fn start_worker(&self, options: WorkerOptions) -> Worker {
        let (shutdown, shutdown_rx) = watch::channel(false);
        let client = self.clone();
        let join = tokio::spawn(async move { client.worker_loop(options, shutdown_rx).await });
        Worker { shutdown, join }
    }

    pub async fn run_worker(&self, options: WorkerOptions) -> Result<()> {
        let (_shutdown, shutdown_rx) = watch::channel(false);
        self.clone().worker_loop(options, shutdown_rx).await
    }

    async fn worker_loop(
        self,
        mut options: WorkerOptions,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        if options.concurrency == 0 {
            options.concurrency = 1;
        }
        if options.poll_interval.is_zero() {
            options.poll_interval = DEFAULT_POLL_INTERVAL;
        }
        if options.claim_timeout.is_zero() {
            options.claim_timeout = DEFAULT_CLAIM_TIMEOUT;
        }
        let worker_id = options.worker_id.clone().unwrap_or_else(default_worker_id);
        let on_error = options.on_error.clone().unwrap_or_else(|| {
            Arc::new(|err| {
                eprintln!("[absurd] worker error: {err}");
            })
        });
        let mut executing = JoinSet::new();
        let semaphore = Arc::new(Semaphore::new(options.concurrency));

        loop {
            while let Some(result) = executing.try_join_next() {
                if let Err(err) = result {
                    on_error(Error::Join(err));
                }
            }

            if *shutdown.borrow() {
                break;
            }

            let available = semaphore.available_permits();
            if available == 0 {
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_ok() && *shutdown.borrow() {
                            break;
                        }
                    }
                    joined = executing.join_next() => {
                        if let Some(Err(err)) = joined {
                            on_error(Error::Join(err));
                        }
                    }
                    _ = sleep(options.poll_interval) => {}
                }
                continue;
            }

            let batch_size = options
                .batch_size
                .unwrap_or(options.concurrency)
                .min(available)
                .max(1);
            let tasks = match self
                .claim_tasks(WorkBatchOptions {
                    worker_id: worker_id.clone(),
                    claim_timeout: options.claim_timeout,
                    batch_size,
                })
                .await
            {
                Ok(tasks) => tasks,
                Err(err) => {
                    on_error(err);
                    tokio::select! {
                        changed = shutdown.changed() => {
                            if changed.is_ok() && *shutdown.borrow() {
                                break;
                            }
                        }
                        _ = sleep(options.poll_interval) => {}
                    }
                    continue;
                }
            };

            if tasks.is_empty() {
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_ok() && *shutdown.borrow() {
                            break;
                        }
                    }
                    _ = sleep(options.poll_interval) => {}
                }
                continue;
            }

            for task in tasks {
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semaphore closed");
                let client = self.clone();
                let on_error = on_error.clone();
                let claim_timeout = options.claim_timeout;
                let fatal_on_lease_timeout = options.fatal_on_lease_timeout;
                executing.spawn(async move {
                    let _permit = permit;
                    if let Err(err) = client
                        .execute_task(task, claim_timeout, fatal_on_lease_timeout)
                        .await
                    {
                        on_error(err);
                    }
                });
            }
        }

        while let Some(result) = executing.join_next().await {
            if let Err(err) = result {
                on_error(Error::Join(err));
            }
        }
        Ok(())
    }

    async fn execute_task(
        &self,
        task: ClaimedTask,
        claim_timeout: Duration,
        fatal_on_lease_timeout: bool,
    ) -> Result<()> {
        let registration = self.registration(&task.task_name)?;
        let Some(registration) = registration else {
            let delay = unknown_task_defer_delay(&task.run_id);
            match self.defer_claimed_run(&task.run_id, delay).await {
                Ok(()) => {
                    eprintln!(
                        "[absurd] claimed unknown task {:?} ({}); deferred run {} by {:?}",
                        task.task_name, task.task_id, task.run_id, delay
                    );
                    return Ok(());
                }
                Err(err) => {
                    eprintln!(
                        "[absurd] failed to defer unknown task {:?} ({}): {err}",
                        task.task_name, task.task_id
                    );
                    let failure = json!({
                        "name": "UnknownTaskDeferError",
                        "message": err.to_string(),
                    });
                    fail_task_run(&self.pool, &self.queue_name, &task.run_id, failure).await?;
                    return Ok(());
                }
            }
        };

        if registration.queue != self.queue_name {
            let failure = json!({
                "name": "QueueMismatch",
                "message": format!("misconfigured task {:?} (queue mismatch)", task.task_name),
            });
            fail_task_run(&self.pool, &self.queue_name, &task.run_id, failure).await?;
            return Ok(());
        }

        let effective_lease = normalize_lease_duration(claim_timeout, DEFAULT_CLAIM_TIMEOUT);
        let watchdog = LeaseWatchdog::new(
            task.task_name.clone(),
            task.task_id.clone(),
            fatal_on_lease_timeout,
        );
        watchdog.schedule(effective_lease);

        let ctx = match TaskContext::create(
            self.clone(),
            registration.queue.clone(),
            task.clone(),
            effective_lease,
            {
                let watchdog = watchdog.clone();
                Arc::new(move |lease| watchdog.schedule(lease))
            },
        )
        .await
        {
            Ok(ctx) => ctx,
            Err(err @ Error::InvalidTaskHeaders(_)) => {
                let failure = serialize_error(&err);
                fail_task_run(&self.pool, &self.queue_name, &task.run_id, failure).await?;
                watchdog.stop();
                return Ok(());
            }
            Err(err) => {
                watchdog.stop();
                return Err(err);
            }
        };

        let execute: TaskExecute = {
            let handler = registration.handler.clone();
            let ctx = ctx.clone();
            let params = task.params.clone();
            Box::new(move || {
                async move {
                    let result = handler(ctx, params).await?;
                    Ok(result)
                }
                .boxed()
            })
        };

        let run_result = if let Some(hook) = &self.hooks.wrap_task_execution {
            AssertUnwindSafe(hook(ctx.clone(), execute))
                .catch_unwind()
                .await
        } else {
            AssertUnwindSafe(execute()).catch_unwind().await
        };

        watchdog.stop();

        match run_result {
            Ok(Ok(result)) => {
                complete_task_run(&self.pool, &self.queue_name, &task.run_id, result).await
            }
            Ok(Err(Error::Suspend | Error::Cancelled | Error::FailedRun)) => Ok(()),
            Ok(Err(err)) => {
                eprintln!("[absurd] task execution failed: {err}");
                let failure = serialize_error(&err);
                match fail_task_run(&self.pool, &self.queue_name, &task.run_id, failure).await {
                    Err(Error::Cancelled | Error::FailedRun) => Ok(()),
                    other => other,
                }
            }
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                eprintln!("[absurd] task execution panicked: {message}");
                let failure = json!({
                    "name": "Panic",
                    "message": format!("panic: {message}"),
                });
                match fail_task_run(&self.pool, &self.queue_name, &task.run_id, failure).await {
                    Err(Error::Cancelled | Error::FailedRun) => Ok(()),
                    other => other,
                }
            }
        }
    }

    fn registration(&self, task_name: &str) -> Result<Option<RegisteredTask>> {
        let registry = self
            .registry
            .read()
            .map_err(|_| Error::InvalidOptions("task registry lock poisoned".to_string()))?;
        Ok(registry.get(task_name).cloned())
    }

    fn resolve_spawn(
        &self,
        task_name: &str,
        options: SpawnOptions,
    ) -> Result<(String, SpawnOptions)> {
        let registration = self.registration(task_name)?;
        let mut effective = options.clone();

        let queue = match registration {
            Some(registration) => {
                if let Some(requested_queue) = options.queue.as_deref() {
                    let requested_queue = validate_queue_name(requested_queue)?;
                    if requested_queue != registration.queue {
                        return Err(Error::QueueMismatch {
                            task_name: task_name.to_string(),
                            registered_queue: registration.queue,
                            requested_queue,
                        });
                    }
                }
                if effective.max_attempts.is_none() {
                    effective.max_attempts = registration
                        .default_max_attempts
                        .or(Some(self.default_max_attempts));
                }
                if effective.cancellation.is_none() {
                    effective.cancellation = registration.default_cancellation;
                }
                registration.queue
            }
            None => {
                let Some(queue) = options.queue.as_deref() else {
                    return Err(Error::UnregisteredTask {
                        task_name: task_name.to_string(),
                    });
                };
                if effective.max_attempts.is_none() {
                    effective.max_attempts = Some(self.default_max_attempts);
                }
                validate_queue_name(queue)?
            }
        };

        Ok((queue, effective))
    }

    async fn defer_claimed_run(&self, run_id: &str, delay: Duration) -> Result<()> {
        sqlx::query(
            "SELECT absurd.schedule_run($1, $2::uuid, absurd.current_time() + make_interval(secs => $3))",
        )
        .bind(&self.queue_name)
        .bind(run_id)
        .bind(duration_seconds(delay))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct TaskContext {
    inner: Arc<TaskContextInner>,
}

struct TaskContextInner {
    client: Client,
    queue_name: String,
    task_id: String,
    run_id: String,
    task_name: String,
    attempt: i32,
    claim_timeout: Duration,
    headers: JsonObject,
    wake_event: Mutex<Option<String>>,
    event_payload: Mutex<Option<Value>>,
    checkpoint_cache: Mutex<HashMap<String, Value>>,
    step_name_counter: Mutex<HashMap<String, usize>>,
    on_lease_extended: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl TaskContext {
    async fn create(
        client: Client,
        queue_name: String,
        task: ClaimedTask,
        claim_timeout: Duration,
        on_lease_extended: Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Result<Self> {
        let headers = match task.headers.clone() {
            Some(headers) => headers,
            None => Map::new(),
        };
        let rows = sqlx::query(
            r#"
            SELECT checkpoint_name, state
            FROM absurd.get_task_checkpoint_states($1, $2::uuid, $3::uuid)
            "#,
        )
        .bind(&queue_name)
        .bind(&task.task_id)
        .bind(&task.run_id)
        .fetch_all(&client.pool)
        .await?;

        let mut checkpoint_cache = HashMap::new();
        for row in rows {
            let name: String = row.try_get("checkpoint_name")?;
            let state: Value = row.try_get("state")?;
            checkpoint_cache.insert(name, state);
        }

        Ok(Self {
            inner: Arc::new(TaskContextInner {
                client,
                queue_name,
                task_id: task.task_id,
                run_id: task.run_id,
                task_name: task.task_name,
                attempt: task.attempt,
                claim_timeout,
                headers,
                wake_event: Mutex::new(task.wake_event),
                event_payload: Mutex::new(task.event_payload),
                checkpoint_cache: Mutex::new(checkpoint_cache),
                step_name_counter: Mutex::new(HashMap::new()),
                on_lease_extended,
            }),
        })
    }

    pub fn task_id(&self) -> &str {
        &self.inner.task_id
    }

    pub fn run_id(&self) -> &str {
        &self.inner.run_id
    }

    pub fn task_name(&self) -> &str {
        &self.inner.task_name
    }

    pub fn queue_name(&self) -> &str {
        &self.inner.queue_name
    }

    pub fn attempt(&self) -> i32 {
        self.inner.attempt
    }

    pub fn headers(&self) -> JsonObject {
        self.inner.headers.clone()
    }

    pub async fn step<T, F, Fut>(&self, name: &str, f: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned + Send + 'static,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>> + Send,
    {
        let handle = self.begin_step(name).await?;
        if handle.done {
            return handle
                .state
                .ok_or_else(|| Error::InvalidOptions("completed step missing state".to_string()));
        }
        let value = f().await?;
        self.complete_step(handle, value).await
    }

    pub async fn begin_step<T>(&self, name: &str) -> Result<StepHandle<T>>
    where
        T: DeserializeOwned,
    {
        let checkpoint_name = self.next_checkpoint_name(name)?;
        if let Some(state) = self.lookup_checkpoint(&checkpoint_name).await? {
            return Ok(StepHandle {
                name: name.to_string(),
                checkpoint_name,
                done: true,
                state: Some(serde_json::from_value(state)?),
            });
        }
        Ok(StepHandle {
            name: name.to_string(),
            checkpoint_name,
            done: false,
            state: None,
        })
    }

    pub async fn complete_step<T>(&self, handle: StepHandle<T>, value: T) -> Result<T>
    where
        T: Serialize,
    {
        if handle.done {
            return handle
                .state
                .ok_or_else(|| Error::InvalidOptions("completed step missing state".to_string()));
        }
        self.persist_checkpoint(&handle.checkpoint_name, &value)
            .await?;
        Ok(value)
    }

    pub async fn sleep_for(&self, step_name: &str, duration: Duration) -> Result<()> {
        let duration = chrono::Duration::from_std(duration)
            .map_err(|err| Error::InvalidOptions(err.to_string()))?;
        self.sleep_until(step_name, Utc::now() + duration).await
    }

    pub async fn sleep_until(&self, step_name: &str, wake_at: DateTime<Utc>) -> Result<()> {
        let checkpoint_name = self.next_checkpoint_name(step_name)?;
        let actual_wake_at = if let Some(state) = self.lookup_checkpoint(&checkpoint_name).await? {
            let stored: String = serde_json::from_value(state)?;
            DateTime::parse_from_rfc3339(&stored)
                .map_err(|err| Error::InvalidOptions(err.to_string()))?
                .with_timezone(&Utc)
        } else {
            self.persist_checkpoint(&checkpoint_name, &wake_at.to_rfc3339())
                .await?;
            wake_at
        };

        if Utc::now() < actual_wake_at {
            self.schedule_run(actual_wake_at).await?;
            return Err(Error::Suspend);
        }
        Ok(())
    }

    pub async fn await_event<T>(&self, event_name: &str, options: AwaitEventOptions) -> Result<T>
    where
        T: DeserializeOwned,
    {
        if event_name.is_empty() {
            return Err(Error::InvalidOptions(
                "event_name must be a non-empty string".to_string(),
            ));
        }
        let step_name = options
            .step_name
            .unwrap_or_else(|| format!("$awaitEvent:{event_name}"));
        let checkpoint_name = self.next_checkpoint_name(&step_name)?;
        if let Some(state) = self.lookup_checkpoint(&checkpoint_name).await? {
            return Ok(serde_json::from_value(state)?);
        }

        let timeout_seconds = options.timeout.map(duration_seconds);
        let row = sqlx::query(
            r#"
            SELECT should_suspend, payload
            FROM absurd.await_event($1, $2::uuid, $3::uuid, $4, $5, $6)
            "#,
        )
        .bind(&self.inner.queue_name)
        .bind(&self.inner.task_id)
        .bind(&self.inner.run_id)
        .bind(&checkpoint_name)
        .bind(event_name)
        .bind(timeout_seconds)
        .fetch_one(&self.inner.client.pool)
        .await
        .map_err(map_task_state_error)?;

        let should_suspend: bool = row.try_get("should_suspend")?;
        let payload: Option<Value> = row.try_get("payload")?;

        if should_suspend {
            return Err(Error::Suspend);
        }
        let Some(payload) = payload else {
            *self
                .inner
                .wake_event
                .lock()
                .map_err(|_| Error::InvalidOptions("wake event lock poisoned".to_string()))? = None;
            *self
                .inner
                .event_payload
                .lock()
                .map_err(|_| Error::InvalidOptions("event payload lock poisoned".to_string()))? =
                None;
            return Err(Error::Timeout(format!(
                "timed out waiting for event {event_name:?}"
            )));
        };

        self.cache_checkpoint(&checkpoint_name, payload.clone())?;
        *self
            .inner
            .wake_event
            .lock()
            .map_err(|_| Error::InvalidOptions("wake event lock poisoned".to_string()))? = None;
        *self
            .inner
            .event_payload
            .lock()
            .map_err(|_| Error::InvalidOptions("event payload lock poisoned".to_string()))? = None;
        Ok(serde_json::from_value(payload)?)
    }

    pub async fn await_task_result(
        &self,
        task_id: &str,
        options: AwaitTaskResultOptions,
    ) -> Result<TaskResultSnapshot> {
        let queue =
            validate_queue_name(options.queue.as_deref().unwrap_or(&self.inner.queue_name))?;
        if queue == self.inner.queue_name {
            return Err(Error::InvalidOptions(
                "TaskContext::await_task_result cannot wait on tasks in the same queue because this can deadlock workers. Spawn the child in a different queue and pass options.queue.".to_string(),
            ));
        }
        let step_name = options
            .step_name
            .unwrap_or_else(|| format!("$awaitTaskResult:{task_id}"));
        let pool = self.inner.client.pool.clone();
        let queue_for_fetch = queue.clone();
        let task_id_for_fetch = task_id.to_string();
        self.step(&step_name, || async {
            let heartbeat_interval = (self.inner.claim_timeout / 2).max(MIN_HEARTBEAT_INTERVAL);
            let next_heartbeat_at = Arc::new(Mutex::new(Instant::now() + heartbeat_interval));
            await_task_result_with_backoff(
                {
                    let pool = pool.clone();
                    let queue_for_fetch = queue_for_fetch.clone();
                    let task_id_for_fetch = task_id_for_fetch.clone();
                    move || {
                        let pool = pool.clone();
                        let queue = queue_for_fetch.clone();
                        let task_id = task_id_for_fetch.clone();
                        async move { fetch_task_result_snapshot(&pool, &queue, &task_id).await }
                            .boxed()
                    }
                },
                task_id,
                options.timeout,
                Some({
                    let ctx = self.clone();
                    let next_heartbeat_at = next_heartbeat_at.clone();
                    move || {
                        let ctx = ctx.clone();
                        let next_heartbeat_at = next_heartbeat_at.clone();
                        async move {
                            let should_heartbeat = {
                                let mut next = next_heartbeat_at.lock().map_err(|_| {
                                    Error::InvalidOptions("heartbeat lock poisoned".to_string())
                                })?;
                                if Instant::now() >= *next {
                                    *next = Instant::now() + heartbeat_interval;
                                    true
                                } else {
                                    false
                                }
                            };
                            if should_heartbeat {
                                ctx.heartbeat(None).await?;
                            }
                            Ok(())
                        }
                        .boxed()
                    }
                }),
            )
            .await
        })
        .await
    }

    pub async fn heartbeat(&self, seconds: Option<Duration>) -> Result<()> {
        let lease = normalize_lease_duration(
            seconds.unwrap_or(self.inner.claim_timeout),
            self.inner.claim_timeout,
        );
        sqlx::query("SELECT absurd.extend_claim($1, $2::uuid, $3)")
            .bind(&self.inner.queue_name)
            .bind(&self.inner.run_id)
            .bind(duration_seconds(lease))
            .execute(&self.inner.client.pool)
            .await
            .map_err(map_task_state_error)?;
        (self.inner.on_lease_extended)(lease);
        Ok(())
    }

    pub async fn emit_event<P: Serialize>(&self, event_name: &str, payload: P) -> Result<()> {
        if event_name.is_empty() {
            return Err(Error::InvalidOptions(
                "event_name must be a non-empty string".to_string(),
            ));
        }
        sqlx::query("SELECT absurd.emit_event($1, $2, $3::jsonb)")
            .bind(&self.inner.queue_name)
            .bind(event_name)
            .bind(Json(serde_json::to_value(payload)?))
            .execute(&self.inner.client.pool)
            .await?;
        Ok(())
    }

    fn next_checkpoint_name(&self, name: &str) -> Result<String> {
        let mut counter = self
            .inner
            .step_name_counter
            .lock()
            .map_err(|_| Error::InvalidOptions("step counter lock poisoned".to_string()))?;
        let count = counter.entry(name.to_string()).or_insert(0);
        *count += 1;
        if *count == 1 {
            Ok(name.to_string())
        } else {
            Ok(format!("{name}#{count}"))
        }
    }

    async fn lookup_checkpoint(&self, checkpoint_name: &str) -> Result<Option<Value>> {
        {
            let cache =
                self.inner.checkpoint_cache.lock().map_err(|_| {
                    Error::InvalidOptions("checkpoint cache lock poisoned".to_string())
                })?;
            if let Some(value) = cache.get(checkpoint_name) {
                return Ok(Some(value.clone()));
            }
        }

        let row = sqlx::query(
            r#"
            SELECT state
            FROM absurd.get_task_checkpoint_state($1, $2::uuid, $3)
            "#,
        )
        .bind(&self.inner.queue_name)
        .bind(&self.inner.task_id)
        .bind(checkpoint_name)
        .fetch_optional(&self.inner.client.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let state: Value = row.try_get("state")?;
        self.cache_checkpoint(checkpoint_name, state.clone())?;
        Ok(Some(state))
    }

    async fn persist_checkpoint<T: Serialize>(
        &self,
        checkpoint_name: &str,
        value: &T,
    ) -> Result<()> {
        let state = serde_json::to_value(value)?;
        sqlx::query(
            r#"
            SELECT absurd.set_task_checkpoint_state($1, $2::uuid, $3, $4::jsonb, $5::uuid, $6)
            "#,
        )
        .bind(&self.inner.queue_name)
        .bind(&self.inner.task_id)
        .bind(checkpoint_name)
        .bind(Json(state.clone()))
        .bind(&self.inner.run_id)
        .bind(duration_seconds(self.inner.claim_timeout))
        .execute(&self.inner.client.pool)
        .await
        .map_err(map_task_state_error)?;
        self.cache_checkpoint(checkpoint_name, state)?;
        (self.inner.on_lease_extended)(self.inner.claim_timeout);
        Ok(())
    }

    async fn schedule_run(&self, wake_at: DateTime<Utc>) -> Result<()> {
        sqlx::query("SELECT absurd.schedule_run($1, $2::uuid, $3)")
            .bind(&self.inner.queue_name)
            .bind(&self.inner.run_id)
            .bind(wake_at)
            .execute(&self.inner.client.pool)
            .await?;
        Ok(())
    }

    fn cache_checkpoint(&self, checkpoint_name: &str, value: Value) -> Result<()> {
        self.inner
            .checkpoint_cache
            .lock()
            .map_err(|_| Error::InvalidOptions("checkpoint cache lock poisoned".to_string()))?
            .insert(checkpoint_name.to_string(), value);
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct AwaitEventOptions {
    pub step_name: Option<String>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Default)]
pub struct AwaitTaskResultOptions {
    pub queue: Option<String>,
    pub step_name: Option<String>,
    pub timeout: Option<Duration>,
}

enum EitherFuture<F> {
    Handler(Pin<Box<F>>),
    Immediate(Option<Result<Value>>),
}

impl<F, T> Future for EitherFuture<F>
where
    F: Future<Output = Result<T>>,
    T: Serialize,
{
    type Output = Result<T>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match &mut *self {
            EitherFuture::Handler(fut) => fut.as_mut().poll(cx),
            EitherFuture::Immediate(result) => {
                let err = result.take().expect("future polled after completion");
                match err {
                    Ok(_) => unreachable!("immediate success is not used"),
                    Err(err) => std::task::Poll::Ready(Err(err)),
                }
            }
        }
    }
}

#[derive(Clone)]
struct LeaseWatchdog {
    task_label: Arc<String>,
    fatal_on_lease_timeout: bool,
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl LeaseWatchdog {
    fn new(task_name: String, task_id: String, fatal_on_lease_timeout: bool) -> Self {
        Self {
            task_label: Arc::new(format!("{task_name} ({task_id})")),
            fatal_on_lease_timeout,
            handles: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn schedule(&self, lease: Duration) {
        if lease.is_zero() {
            return;
        }
        self.stop();
        let task_label = self.task_label.clone();
        let warn = tokio::spawn(async move {
            sleep(lease).await;
            eprintln!("[absurd] task {task_label} exceeded claim timeout of {lease:?}");
        });

        let mut handles = match self.handles.lock() {
            Ok(handles) => handles,
            Err(_) => return,
        };
        handles.push(warn);
        if self.fatal_on_lease_timeout {
            let task_label = self.task_label.clone();
            handles.push(tokio::spawn(async move {
                sleep(lease.saturating_mul(2)).await;
                eprintln!(
                    "[absurd] task {task_label} exceeded claim timeout of {lease:?} by more than 100%; terminating process"
                );
                process::exit(1);
            }));
        }
    }

    fn stop(&self) {
        let Ok(mut handles) = self.handles.lock() else {
            return;
        };
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}

async fn fetch_task_result_snapshot(
    pool: &PgPool,
    queue_name: &str,
    task_id: &str,
) -> Result<Option<TaskResultSnapshot>> {
    let row = sqlx::query(
        r#"
        SELECT state, result, failure_reason
        FROM absurd.get_task_result($1, $2::uuid)
        "#,
    )
    .bind(queue_name)
    .bind(task_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let state: String = row.try_get("state")?;
    let result: Option<Value> = row.try_get("result")?;
    let failure: Option<Value> = row.try_get("failure_reason")?;
    Ok(Some(match state.as_str() {
        "pending" => TaskResultSnapshot::Pending,
        "running" => TaskResultSnapshot::Running,
        "sleeping" => TaskResultSnapshot::Sleeping,
        "completed" => TaskResultSnapshot::Completed {
            result: result.unwrap_or(Value::Null),
        },
        "failed" => TaskResultSnapshot::Failed {
            failure: failure.unwrap_or(Value::Null),
        },
        "cancelled" => TaskResultSnapshot::Cancelled,
        other => {
            return Err(Error::InvalidOptions(format!(
                "unknown task result state {other:?}"
            )))
        }
    }))
}

async fn await_task_result_with_backoff<F, B>(
    mut fetch_snapshot: F,
    task_id: &str,
    timeout: Option<Duration>,
    mut before_sleep: Option<B>,
) -> Result<TaskResultSnapshot>
where
    F: FnMut() -> BoxFuture<'static, Result<Option<TaskResultSnapshot>>>,
    B: FnMut() -> BoxFuture<'static, Result<()>>,
{
    let started = Instant::now();
    let mut delay = INITIAL_BACKOFF;

    loop {
        let snapshot = fetch_snapshot().await?;
        let Some(snapshot) = snapshot else {
            return Err(Error::TaskNotFound(task_id.to_string()));
        };
        if snapshot.is_terminal() {
            return Ok(snapshot);
        }

        if let Some(timeout) = timeout {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(Error::Timeout(format!(
                    "timed out waiting for task {task_id:?}"
                )));
            }
            delay = delay.min(timeout - elapsed);
        }

        if let Some(before_sleep) = before_sleep.as_mut() {
            before_sleep().await?;
        }
        sleep(delay).await;
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}

async fn complete_task_run(
    pool: &PgPool,
    queue_name: &str,
    run_id: &str,
    result: Value,
) -> Result<()> {
    sqlx::query("SELECT absurd.complete_run($1, $2::uuid, $3::jsonb)")
        .bind(queue_name)
        .bind(run_id)
        .bind(Json(result))
        .execute(pool)
        .await
        .map_err(map_task_state_error)?;
    Ok(())
}

async fn fail_task_run(
    pool: &PgPool,
    queue_name: &str,
    run_id: &str,
    failure: Value,
) -> Result<()> {
    sqlx::query("SELECT absurd.fail_run($1, $2::uuid, $3::jsonb, NULL)")
        .bind(queue_name)
        .bind(run_id)
        .bind(Json(failure))
        .execute(pool)
        .await
        .map_err(map_task_state_error)?;
    Ok(())
}

fn validate_queue_name(queue_name: &str) -> Result<String> {
    if queue_name.is_empty() {
        return Err(Error::MissingQueueName);
    }
    if queue_name.len() > MAX_QUEUE_NAME_LENGTH {
        return Err(Error::QueueNameTooLong {
            name: queue_name.to_string(),
            max: MAX_QUEUE_NAME_LENGTH,
        });
    }
    Ok(queue_name.to_string())
}

fn resolve_database_url(explicit: Option<&str>) -> String {
    if let Some(value) = explicit.filter(|value| !value.trim().is_empty()) {
        return value.to_string();
    }
    if let Ok(value) = env::var("ABSURD_DATABASE_URL") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    if let Ok(value) = env::var("PGDATABASE") {
        let value = value.trim();
        if !value.is_empty() {
            if value.contains("://") {
                return value.to_string();
            }
            return format!("postgresql://localhost/{value}");
        }
    }
    "postgresql://localhost/absurd".to_string()
}

fn queue_policy_payload(options: QueuePolicyOptions) -> Map<String, Value> {
    let mut payload = Map::new();
    if let Some(value) = options.partition_lookahead {
        payload.insert("partition_lookahead".to_string(), Value::String(value));
    }
    if let Some(value) = options.partition_lookback {
        payload.insert("partition_lookback".to_string(), Value::String(value));
    }
    if let Some(value) = options.cleanup_ttl {
        payload.insert("cleanup_ttl".to_string(), Value::String(value));
    }
    if let Some(value) = options.cleanup_limit {
        payload.insert("cleanup_limit".to_string(), json!(value));
    }
    if let Some(value) = options.detach_mode {
        payload.insert(
            "detach_mode".to_string(),
            Value::String(value.as_str().to_string()),
        );
    }
    if let Some(value) = options.detach_min_age {
        payload.insert("detach_min_age".to_string(), Value::String(value));
    }
    payload
}

fn normalize_spawn_options(options: SpawnOptions) -> Map<String, Value> {
    let mut payload = Map::new();
    if let Some(headers) = options.headers {
        payload.insert("headers".to_string(), Value::Object(headers));
    }
    if let Some(max_attempts) = options.max_attempts {
        payload.insert("max_attempts".to_string(), json!(max_attempts));
    }
    if let Some(retry_strategy) = options.retry_strategy {
        payload.insert(
            "retry_strategy".to_string(),
            serde_json::to_value(retry_strategy).expect("retry strategy is serializable"),
        );
    }
    if let Some(cancellation) = options.cancellation {
        let value =
            serde_json::to_value(cancellation).expect("cancellation policy is serializable");
        if value
            .as_object()
            .map(|obj| !obj.is_empty())
            .unwrap_or(false)
        {
            payload.insert("cancellation".to_string(), value);
        }
    }
    if let Some(idempotency_key) = options.idempotency_key {
        payload.insert(
            "idempotency_key".to_string(),
            Value::String(idempotency_key),
        );
    }
    payload
}

fn queue_policy_from_row(row: sqlx::postgres::PgRow) -> Result<QueuePolicy> {
    let storage_mode: String = row.try_get("storage_mode")?;
    let detach_mode: String = row.try_get("detach_mode")?;
    Ok(QueuePolicy {
        queue_name: row.try_get("queue_name")?,
        storage_mode: match storage_mode.as_str() {
            "unpartitioned" => QueueStorageMode::Unpartitioned,
            "partitioned" => QueueStorageMode::Partitioned,
            other => {
                return Err(Error::InvalidOptions(format!(
                    "unknown queue storage mode {other:?}"
                )))
            }
        },
        partition_lookahead: row.try_get("partition_lookahead")?,
        partition_lookback: row.try_get("partition_lookback")?,
        cleanup_ttl: row.try_get("cleanup_ttl")?,
        cleanup_limit: row.try_get("cleanup_limit")?,
        detach_mode: match detach_mode.as_str() {
            "none" => QueueDetachMode::None,
            "empty" => QueueDetachMode::Empty,
            other => {
                return Err(Error::InvalidOptions(format!(
                    "unknown queue detach mode {other:?}"
                )))
            }
        },
        detach_min_age: row.try_get("detach_min_age")?,
    })
}

fn spawn_result_from_row(row: sqlx::postgres::PgRow) -> Result<SpawnResult> {
    Ok(SpawnResult {
        task_id: row.try_get("task_id")?,
        run_id: row.try_get("run_id")?,
        attempt: row.try_get("attempt")?,
        created: row.try_get("created")?,
    })
}

fn claimed_task_from_row(row: sqlx::postgres::PgRow) -> Result<ClaimedTask> {
    let headers: Option<Value> = row.try_get("headers")?;
    let headers = match headers {
        Some(Value::Object(map)) => Some(map),
        Some(Value::Null) | None => None,
        Some(other) => {
            return Err(Error::InvalidTaskHeaders(format!(
                "headers payload must be a JSON object, got {other}"
            )))
        }
    };
    Ok(ClaimedTask {
        run_id: row.try_get("run_id")?,
        task_id: row.try_get("task_id")?,
        task_name: row.try_get("task_name")?,
        attempt: row.try_get("attempt")?,
        params: row.try_get("params")?,
        retry_strategy: row
            .try_get::<Option<Value>, _>("retry_strategy")?
            .unwrap_or(Value::Null),
        max_attempts: row.try_get("max_attempts")?,
        headers,
        wake_event: row.try_get("wake_event")?,
        event_payload: row.try_get("event_payload")?,
    })
}

fn duration_seconds(duration: Duration) -> i32 {
    let seconds = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
    i32::try_from(seconds).unwrap_or(i32::MAX)
}

fn normalize_lease_duration(duration: Duration, fallback: Duration) -> Duration {
    if duration.is_zero() {
        fallback
    } else {
        Duration::from_secs(duration_seconds(duration) as u64)
    }
}

fn map_task_state_error(err: sqlx::Error) -> Error {
    if let sqlx::Error::Database(db_err) = &err {
        if let Some(code) = db_err.code() {
            match code.as_ref() {
                "AB001" => return Error::Cancelled,
                "AB002" => return Error::FailedRun,
                _ => {}
            }
        }
    }
    Error::Sqlx(err)
}

fn serialize_error(err: &Error) -> Value {
    // Persist the full source chain: the failure payload is often the only
    // forensic artifact of a failed run.
    let mut message = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        let rendered = cause.to_string();
        if !message.contains(&rendered) {
            message.push_str(": ");
            message.push_str(&rendered);
        }
        source = cause.source();
    }
    json!({
        "name": match err {
            Error::Timeout(_) => "TimeoutError",
            Error::Cancelled => "CancelledTask",
            Error::FailedRun => "FailedTask",
            Error::Suspend => "SuspendTask",
            _ => "Error",
        },
        "message": message,
    })
}

fn default_worker_id() -> String {
    let host = env::var("HOSTNAME").unwrap_or_else(|_| "host".to_string());
    format!("{host}:{}", process::id())
}

fn unknown_task_defer_delay(seed: &str) -> Duration {
    Duration::from_secs(UNKNOWN_TASK_DEFER_BASE_SECONDS + deterministic_jitter(seed))
}

fn deterministic_jitter(seed: &str) -> u64 {
    if UNKNOWN_TASK_DEFER_JITTER_SECONDS == 0 {
        return 0;
    }
    let mut hash: u32 = 2_166_136_261;
    for byte in seed.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    u64::from(hash) % (UNKNOWN_TASK_DEFER_JITTER_SECONDS + 1)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(value) = payload.downcast_ref::<&str>() {
        (*value).to_string()
    } else if let Some(value) = payload.downcast_ref::<String>() {
        value.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        sync::OnceLock,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn schema_setup_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn optional_test_pool() -> Result<Option<PgPool>> {
        let Ok(database_url) = env::var("ABSURD_TEST_DATABASE_URL") else {
            return Ok(None);
        };

        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;

        let _guard = schema_setup_lock().lock().await;
        let has_schema: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM pg_namespace WHERE nspname = 'absurd'")
                .fetch_optional(&pool)
                .await?;
        if has_schema.is_none() {
            sqlx::raw_sql(include_str!(
                "../../centaur-session-sqlx/migrations/0007_absurd_workflows.sql"
            ))
            .execute(&pool)
            .await?;
        }

        Ok(Some(pool))
    }

    fn unique_queue(prefix: &str) -> String {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        format!("{prefix}_{suffix}")
    }

    #[test]
    fn queue_name_validation_counts_bytes() {
        assert!(validate_queue_name("default").is_ok());
        assert!(matches!(
            validate_queue_name(""),
            Err(Error::MissingQueueName)
        ));
        assert!(matches!(
            validate_queue_name(&"a".repeat(MAX_QUEUE_NAME_LENGTH + 1)),
            Err(Error::QueueNameTooLong { .. })
        ));
    }

    #[test]
    fn spawn_options_are_serialized_with_database_keys() {
        let mut headers = Map::new();
        headers.insert("trace_id".to_string(), json!("abc"));
        let payload = normalize_spawn_options(SpawnOptions {
            max_attempts: Some(3),
            retry_strategy: Some(RetryStrategy {
                kind: RetryKind::Fixed,
                base_seconds: Some(30.0),
                factor: None,
                max_seconds: None,
            }),
            headers: Some(headers),
            queue: Some("ignored".to_string()),
            cancellation: Some(CancellationPolicy {
                max_duration: Some(120),
                max_delay: Some(60),
            }),
            idempotency_key: Some("idem-1".to_string()),
        });

        assert_eq!(payload["max_attempts"], json!(3));
        assert_eq!(payload["headers"]["trace_id"], json!("abc"));
        assert_eq!(payload["retry_strategy"]["kind"], json!("fixed"));
        assert_eq!(payload["retry_strategy"]["base_seconds"], json!(30.0));
        assert_eq!(payload["cancellation"]["max_duration"], json!(120));
        assert_eq!(payload["cancellation"]["max_delay"], json!(60));
        assert_eq!(payload["idempotency_key"], json!("idem-1"));
        assert!(!payload.contains_key("queue"));
    }

    #[test]
    fn task_result_snapshot_uses_parity_json_shape() {
        let snapshot = TaskResultSnapshot::Completed {
            result: json!({"ok": true}),
        };
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            json!({"state": "completed", "result": {"ok": true}})
        );
    }

    #[test]
    fn deterministic_jitter_is_stable() {
        assert_eq!(deterministic_jitter("run-a"), deterministic_jitter("run-a"));
        assert!(deterministic_jitter("run-a") <= UNKNOWN_TASK_DEFER_JITTER_SECONDS);
    }

    #[tokio::test]
    async fn integration_processes_task_when_database_url_is_set() -> Result<()> {
        let Some(pool) = optional_test_pool().await? else {
            return Ok(());
        };

        #[derive(Debug, Serialize, Deserialize)]
        struct Params {
            value: i32,
        }

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Output {
            value: i32,
        }

        let queue = unique_queue("rust_sdk");
        let app = Client::from_pool_with_options(
            pool,
            ClientOptions {
                queue_name: queue.clone(),
                ..ClientOptions::default()
            },
        )?;
        app.create_queue(None, Default::default()).await?;

        let step_calls = Arc::new(AtomicUsize::new(0));
        app.register_task("double", {
            let step_calls = step_calls.clone();
            move |params: Params, ctx| {
                let step_calls = step_calls.clone();
                async move {
                    let value = ctx
                        .step("double", || async move {
                            step_calls.fetch_add(1, Ordering::SeqCst);
                            Ok(params.value * 2)
                        })
                        .await?;
                    Ok(Output { value })
                }
            }
        })?;

        let spawned = app
            .spawn("double", Params { value: 21 }, Default::default())
            .await?;
        app.work_batch(WorkBatchOptions {
            worker_id: "rust-test-worker".to_string(),
            ..WorkBatchOptions::default()
        })
        .await?;

        let snapshot = app
            .await_task_result(&spawned.task_id, None, Some(Duration::from_secs(5)))
            .await?;
        let TaskResultSnapshot::Completed { result } = snapshot else {
            return Err(Error::InvalidOptions(format!(
                "expected completed snapshot, got {snapshot:?}"
            )));
        };
        assert_eq!(
            serde_json::from_value::<Output>(result)?,
            Output { value: 42 }
        );
        assert_eq!(step_calls.load(Ordering::SeqCst), 1);

        Ok(())
    }

    #[tokio::test]
    async fn integration_events_retries_policy_and_idempotency_when_database_url_is_set(
    ) -> Result<()> {
        let Some(pool) = optional_test_pool().await? else {
            return Ok(());
        };

        let queue = unique_queue("rust_parity");
        let app = Client::from_pool_with_options(
            pool,
            ClientOptions {
                queue_name: queue.clone(),
                ..ClientOptions::default()
            },
        )?;
        app.create_queue(None, Default::default()).await?;

        let policy_queue = unique_queue("rust_policy");
        app.create_queue(
            Some(&policy_queue),
            CreateQueueOptions {
                storage_mode: QueueStorageMode::Partitioned,
                policy: QueuePolicyOptions {
                    cleanup_ttl: Some("4321 seconds".to_string()),
                    cleanup_limit: Some(12),
                    detach_mode: Some(QueueDetachMode::Empty),
                    detach_min_age: Some("30 days".to_string()),
                    ..QueuePolicyOptions::default()
                },
            },
        )
        .await?;
        let policy = app
            .get_queue_policy(Some(&policy_queue))
            .await?
            .ok_or_else(|| Error::InvalidOptions("expected queue policy".to_string()))?;
        assert_eq!(policy.storage_mode, QueueStorageMode::Partitioned);
        assert_eq!(policy.cleanup_limit, 12);
        assert_eq!(policy.detach_mode, QueueDetachMode::Empty);

        app.register_task("idem", |_params: Value, _ctx| async move {
            Ok(json!({"ok": true}))
        })?;
        let first = app
            .spawn(
                "idem",
                json!({"value": 1}),
                SpawnOptions {
                    idempotency_key: Some("idem-key".to_string()),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        let second = app
            .spawn(
                "idem",
                json!({"value": 2}),
                SpawnOptions {
                    idempotency_key: Some("idem-key".to_string()),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.task_id, second.task_id);
        assert_eq!(first.run_id, second.run_id);
        app.work_batch(WorkBatchOptions {
            worker_id: "rust-idem-worker".to_string(),
            ..WorkBatchOptions::default()
        })
        .await?;

        #[derive(Debug, Serialize, Deserialize)]
        struct WaitParams {
            event_name: String,
        }

        #[derive(Debug, Serialize, Deserialize)]
        struct EventPayload {
            message: String,
        }

        app.register_task("wait-event", |params: WaitParams, ctx| async move {
            let payload: EventPayload = ctx
                .await_event(&params.event_name, AwaitEventOptions::default())
                .await?;
            Ok(json!({ "message": payload.message }))
        })?;

        let event_name = format!("event.{}", unique_queue("rust"));
        let waiting = app
            .spawn(
                "wait-event",
                WaitParams {
                    event_name: event_name.clone(),
                },
                Default::default(),
            )
            .await?;
        app.work_batch(WorkBatchOptions {
            worker_id: "rust-event-worker".to_string(),
            ..WorkBatchOptions::default()
        })
        .await?;
        assert!(matches!(
            app.fetch_task_result(&waiting.task_id, None).await?,
            Some(TaskResultSnapshot::Sleeping)
        ));
        app.emit_event(
            &event_name,
            EventPayload {
                message: "ready".to_string(),
            },
            None,
        )
        .await?;
        app.work_batch(WorkBatchOptions {
            worker_id: "rust-event-worker".to_string(),
            ..WorkBatchOptions::default()
        })
        .await?;
        let event_snapshot = app
            .await_task_result(&waiting.task_id, None, Some(Duration::from_secs(5)))
            .await?;
        assert_eq!(
            event_snapshot.result::<Value>()?,
            Some(json!({"message": "ready"}))
        );

        let retry_calls = Arc::new(AtomicUsize::new(0));
        app.register_task("flaky", {
            let retry_calls = retry_calls.clone();
            move |_params: Value, _ctx| {
                let retry_calls = retry_calls.clone();
                async move {
                    if retry_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(Error::InvalidOptions("boom".to_string()));
                    }
                    Ok(json!({"ok": true}))
                }
            }
        })?;
        let flaky = app
            .spawn(
                "flaky",
                json!({}),
                SpawnOptions {
                    max_attempts: Some(2),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        app.work_batch(WorkBatchOptions {
            worker_id: "rust-retry-worker".to_string(),
            ..WorkBatchOptions::default()
        })
        .await?;
        app.work_batch(WorkBatchOptions {
            worker_id: "rust-retry-worker".to_string(),
            ..WorkBatchOptions::default()
        })
        .await?;
        let retry_snapshot = app
            .await_task_result(&flaky.task_id, None, Some(Duration::from_secs(5)))
            .await?;
        assert_eq!(retry_snapshot.result::<Value>()?, Some(json!({"ok": true})));
        assert_eq!(retry_calls.load(Ordering::SeqCst), 2);

        Ok(())
    }
}
