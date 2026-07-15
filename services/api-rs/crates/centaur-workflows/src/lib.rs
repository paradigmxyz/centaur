use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration,
};

use absurd::{
    Client, ClientOptions, CreateQueueOptions, RetryKind, RetryStrategy, SpawnOptions, StepHandle,
    TaskContext, TaskRegistrationOptions, Worker, WorkerOptions,
};
use centaur_sandbox_core::SandboxSpec;
use centaur_session_core::{HarnessType, MessageRole, SessionMessageInput, ThreadKey};
use centaur_session_runtime::{
    ExecuteSessionInput, HarnessConflictPolicy, SESSION_OUTPUT_LINE_EVENT, SandboxRuntime,
    SessionRuntime,
};
use centaur_session_sqlx::PgSessionStore;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use futures_util::{TryStreamExt, pin_mut};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use thiserror::Error;
use time::OffsetDateTime;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
    task::JoinHandle,
};
use tracing::{info, warn};

pub const WORKFLOW_QUEUE: &str = "centaur_workflows";
pub const WORKFLOW_SLACK_LIVE_QUEUE: &str = "centaur_workflows_slack_live";
pub const WORKFLOW_ETL_QUEUE: &str = "centaur_workflows_etl";
pub const WORKFLOW_ETL_BACKFILL_QUEUE: &str = "centaur_workflows_etl_backfill";
pub const WORKFLOW_SCHEDULE_QUEUE: &str = "centaur_workflow_schedules";
pub const WORKFLOW_TASK: &str = "centaur.workflow";
pub const WORKFLOW_SCHEDULE_TASK: &str = "centaur.workflow.schedule_tick";
const PYTHON_HOST_ENV: &str = "PYTHON_WORKFLOW_HOST_PATH";
const PYTHON_HOST_INTERPRETER_ENV: &str = "PYTHON_WORKFLOW_HOST_PYTHON";
const WORKFLOW_TOOL_API_URL_ENV: &str = "WORKFLOW_TOOL_API_URL";
const DEFAULT_AGENT_IDLE_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_AGENT_MAX_DURATION_MS: u64 = 30 * 60 * 1_000;
const WORKFLOW_HOST_CLAIM_EXTENSION: Duration = Duration::from_secs(5 * 60);
const WORKFLOW_HOST_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
const WORKFLOW_RECONCILE_INTERVAL_SECS_ENV: &str = "WORKFLOW_RECONCILE_INTERVAL_SECS";
const DEFAULT_WORKFLOW_RECONCILE_INTERVAL_SECS: u64 = 60;
const WORKFLOW_ENABLE_MODE_ENV: &str = "WORKFLOW_ENABLE_MODE";
const WORKFLOW_ALLOWED_NAMES_ENV: &str = "WORKFLOW_ALLOWED_NAMES";
/// How many consecutive reconcile passes a workflow must be missing from
/// discovery before its active tasks are cancelled. 0 disables reaping.
const WORKFLOW_REAP_REMOVED_AFTER_TICKS_ENV: &str = "WORKFLOW_REAP_REMOVED_AFTER_TICKS";
const DEFAULT_WORKFLOW_REAP_REMOVED_AFTER_TICKS: u32 = 3;
const ABSURD_TERMINAL_TASK_STATES: &str = "('completed', 'failed', 'cancelled')";

/// Per-queue worker concurrency. The defaults preserve historical behavior; each
/// can be overridden via its env var to scale a queue independently (e.g. raise
/// the standard queue when webhook/agent workflows back up). A value that is
/// unset, empty, non-numeric, or zero falls back to the default (absurd also
/// clamps zero to one, since a queue at concurrency zero would never drain).
const WORKFLOW_WORKER_CONCURRENCY_ENV: &str = "WORKFLOW_WORKER_CONCURRENCY";
const DEFAULT_WORKFLOW_WORKER_CONCURRENCY: usize = 4;
const WORKFLOW_ETL_WORKER_CONCURRENCY_ENV: &str = "WORKFLOW_ETL_WORKER_CONCURRENCY";
const DEFAULT_WORKFLOW_ETL_WORKER_CONCURRENCY: usize = 1;
const WORKFLOW_ETL_BACKFILL_WORKER_CONCURRENCY_ENV: &str =
    "WORKFLOW_ETL_BACKFILL_WORKER_CONCURRENCY";
const DEFAULT_WORKFLOW_ETL_BACKFILL_WORKER_CONCURRENCY: usize = 1;
const WORKFLOW_SCHEDULE_WORKER_CONCURRENCY_ENV: &str = "WORKFLOW_SCHEDULE_WORKER_CONCURRENCY";
const DEFAULT_WORKFLOW_SCHEDULE_WORKER_CONCURRENCY: usize = 1;

struct WorkflowTaskHeartbeatGuard {
    task: JoinHandle<()>,
}

impl Drop for WorkflowTaskHeartbeatGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
pub struct WorkflowRuntime {
    inner: Arc<WorkflowRuntimeInner>,
}

struct WorkflowRuntimeInner {
    client: Client,
    slack_live_client: Client,
    etl_client: Client,
    etl_backfill_client: Client,
    _worker: Worker,
    _slack_live_worker: Worker,
    _etl_worker: Worker,
    _etl_backfill_worker: Worker,
    _schedule_worker: Worker,
    webhook_registry: Arc<RwLock<BTreeMap<String, RegisteredWorkflowWebhook>>>,
    schedule_registry: Arc<RwLock<BTreeMap<String, RegisteredWorkflowSchedule>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkflowEnablement {
    mode: WorkflowEnableMode,
    allowed_names: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkflowEnableMode {
    All,
    Allowlist,
}

impl WorkflowEnablement {
    fn all() -> Self {
        Self {
            mode: WorkflowEnableMode::All,
            allowed_names: BTreeSet::new(),
        }
    }

    fn allowlist(raw_allowed_names: &str) -> Self {
        Self {
            mode: WorkflowEnableMode::Allowlist,
            allowed_names: parse_workflow_allowed_names(raw_allowed_names),
        }
    }

    fn from_env() -> Result<Self, WorkflowRuntimeError> {
        let raw_mode = env::var(WORKFLOW_ENABLE_MODE_ENV).unwrap_or_default();
        let mode = raw_mode.trim();
        if mode.is_empty() || mode.eq_ignore_ascii_case("all") {
            return Ok(Self::all());
        }
        if mode.eq_ignore_ascii_case("allowlist") {
            return Ok(Self::allowlist(
                &env::var(WORKFLOW_ALLOWED_NAMES_ENV).unwrap_or_default(),
            ));
        }
        Err(WorkflowRuntimeError::BadRequest(format!(
            "{WORKFLOW_ENABLE_MODE_ENV} must be \"all\" or \"allowlist\", got {mode:?}"
        )))
    }

    fn is_enabled(&self, workflow_name: &str) -> bool {
        match self.mode {
            WorkflowEnableMode::All => true,
            WorkflowEnableMode::Allowlist => self.allowed_names.contains(workflow_name.trim()),
        }
    }

    fn ensure_enabled(&self, workflow_name: &str) -> Result<(), WorkflowRuntimeError> {
        if self.is_enabled(workflow_name) {
            return Ok(());
        }
        Err(WorkflowRuntimeError::Disabled(format!(
            "workflow {workflow_name:?} is disabled by {WORKFLOW_ENABLE_MODE_ENV}"
        )))
    }

    fn filter_metadata(&self, metadata: &mut PythonWorkflowMetadata) {
        if self.mode == WorkflowEnableMode::All {
            return;
        }
        metadata
            .workflow_names
            .retain(|workflow_name| self.is_enabled(workflow_name));
        metadata
            .webhooks
            .retain(|webhook| self.is_enabled(&webhook.workflow_name));
        metadata.schedules.retain(|schedule| {
            schedule
                .get("workflow_name")
                .and_then(Value::as_str)
                .is_some_and(|workflow_name| self.is_enabled(workflow_name))
        });
    }
}

fn parse_workflow_allowed_names(raw: &str) -> BTreeSet<String> {
    raw.split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter_map(|name| {
            let name = name.trim();
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

#[derive(Clone)]
struct WorkflowQueueClients {
    standard: Client,
    slack_live: Client,
    etl: Client,
    etl_backfill: Client,
}

#[derive(Clone)]
pub struct WorkflowHostSandboxRuntime {
    runtime: SandboxRuntime,
    spec: SandboxSpec,
}

impl WorkflowHostSandboxRuntime {
    pub fn new(runtime: SandboxRuntime, spec: SandboxSpec) -> Self {
        Self { runtime, spec }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateWorkflowRunRequest {
    pub workflow_name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub harness_type: Option<HarnessType>,
    #[serde(default)]
    pub max_attempts: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateWorkflowRunResponse {
    pub ok: bool,
    pub run_id: String,
    pub task_id: String,
    pub status: String,
    pub created: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub task_id: String,
    pub workflow_name: String,
    pub status: String,
    pub input: Value,
    pub result: Option<Value>,
    pub failure: Option<Value>,
    pub attempts: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegisteredWorkflowWebhook {
    pub workflow_name: String,
    pub source_path: String,
    pub spec: WorkflowWebhookSpec,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowWebhookSpec {
    pub slug: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub auth: WorkflowWebhookAuth,
    #[serde(default)]
    pub trigger_key: Option<WorkflowWebhookTriggerKey>,
    #[serde(default = "default_webhook_methods")]
    pub allowed_methods: Vec<String>,
    #[serde(default = "default_webhook_content_types")]
    pub allowed_content_types: Vec<String>,
    /// Optional edge pre-filter. When set, the API evaluates it against the
    /// parsed event (headers + JSON body) and only creates a workflow run when
    /// it matches. This keeps org-wide webhooks from spawning a sandbox per event.
    #[serde(default)]
    pub filter: Option<WebhookFilter>,
}

/// A declarative webhook pre-filter, evaluated in-process before a run is
/// created. A node is either a boolean combinator (`any`/`all`) or a leaf that
/// reads a `header` or a dot-path into the JSON `body` and applies `op`
/// (`equals` | `in` | `contains` | `prefix`).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WebhookFilter {
    #[serde(default)]
    pub any: Option<Vec<WebhookFilter>>,
    #[serde(default)]
    pub all: Option<Vec<WebhookFilter>>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum WorkflowWebhookAuth {
    #[default]
    None,
    Hmac {
        secret_ref: String,
        #[serde(default = "default_signature_header")]
        signature_header: String,
        #[serde(default = "default_hmac_algorithm")]
        algorithm: String,
        #[serde(default = "default_signature_prefix")]
        signature_prefix: String,
        #[serde(default = "default_hmac_encoding")]
        encoding: String,
    },
    Github {
        secret_ref: String,
    },
    Bearer {
        secret_ref: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowWebhookTriggerKey {
    Header { header: String },
    Static { value: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowTaskInput {
    workflow_name: String,
    input: Value,
    harness_type: HarnessType,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScheduleTickInput {
    schedule_id: String,
    scheduled_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegisteredWorkflowSchedule {
    pub schedule_id: String,
    pub workflow_name: String,
    pub source_path: String,
    pub kind: WorkflowScheduleKind,
    #[serde(default)]
    pub timezone: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub no_delivery: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowScheduleKind {
    Interval { interval_seconds: u64 },
    Cron { cron: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkflowResult {
    workflow_name: String,
    run_id: String,
    task_id: String,
    steps: Vec<String>,
    output: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AgentTurnResult {
    thread_key: String,
    execution_id: String,
    status: String,
    output_lines: Vec<String>,
    result_text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ToolResult {
    tool: String,
    method: String,
    output: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SlackPostResult {
    channel: String,
    ts: String,
}

impl WorkflowRuntime {
    pub async fn new(
        store: PgSessionStore,
        session_runtime: SessionRuntime,
    ) -> Result<Self, WorkflowRuntimeError> {
        Self::new_with_workflow_host_sandbox(store, session_runtime, None).await
    }

    pub async fn new_with_workflow_host_sandbox(
        store: PgSessionStore,
        session_runtime: SessionRuntime,
        workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
    ) -> Result<Self, WorkflowRuntimeError> {
        let client = Client::from_pool_with_options(
            store.pool().clone(),
            ClientOptions {
                queue_name: WORKFLOW_QUEUE.to_owned(),
                ..ClientOptions::default()
            },
        )?;
        client
            .create_queue(Some(WORKFLOW_QUEUE), CreateQueueOptions::default())
            .await?;
        let slack_live_client = Client::from_pool_with_options(
            store.pool().clone(),
            ClientOptions {
                queue_name: WORKFLOW_SLACK_LIVE_QUEUE.to_owned(),
                ..ClientOptions::default()
            },
        )?;
        slack_live_client
            .create_queue(
                Some(WORKFLOW_SLACK_LIVE_QUEUE),
                CreateQueueOptions::default(),
            )
            .await?;
        let etl_client = Client::from_pool_with_options(
            store.pool().clone(),
            ClientOptions {
                queue_name: WORKFLOW_ETL_QUEUE.to_owned(),
                ..ClientOptions::default()
            },
        )?;
        etl_client
            .create_queue(Some(WORKFLOW_ETL_QUEUE), CreateQueueOptions::default())
            .await?;
        let etl_backfill_client = Client::from_pool_with_options(
            store.pool().clone(),
            ClientOptions {
                queue_name: WORKFLOW_ETL_BACKFILL_QUEUE.to_owned(),
                ..ClientOptions::default()
            },
        )?;
        etl_backfill_client
            .create_queue(
                Some(WORKFLOW_ETL_BACKFILL_QUEUE),
                CreateQueueOptions::default(),
            )
            .await?;
        let schedule_client = Client::from_pool_with_options(
            store.pool().clone(),
            ClientOptions {
                queue_name: WORKFLOW_SCHEDULE_QUEUE.to_owned(),
                ..ClientOptions::default()
            },
        )?;
        schedule_client
            .create_queue(Some(WORKFLOW_SCHEDULE_QUEUE), CreateQueueOptions::default())
            .await?;
        let workflow_clients = WorkflowQueueClients {
            standard: client.clone(),
            slack_live: slack_live_client.clone(),
            etl: etl_client.clone(),
            etl_backfill: etl_backfill_client.clone(),
        };

        let discovery = discover_python_workflow_metadata()
            .await
            .unwrap_or_else(|error| {
                warn!(%error, "python workflow discovery failed");
                PythonWorkflowMetadata::default()
            });
        let enablement = WorkflowEnablement::from_env()?;
        let schedule_registry = Arc::new(RwLock::new(build_schedule_registry(
            &discovery,
            &enablement,
        )?));
        let webhook_registry = Arc::new(RwLock::new(build_webhook_registry(
            &discovery,
            &enablement,
        )?));

        let task_session_runtime = session_runtime.clone();
        let task_workflow_host_sandbox = workflow_host_sandbox.clone();
        let task_workflow_clients = workflow_clients.clone();
        client.register_task(WORKFLOW_TASK, move |input: WorkflowTaskInput, ctx| {
            let session_runtime = task_session_runtime.clone();
            let workflow_host_sandbox = task_workflow_host_sandbox.clone();
            let workflow_clients = task_workflow_clients.clone();
            async move {
                run_centaur_workflow(
                    input,
                    ctx,
                    session_runtime,
                    workflow_host_sandbox,
                    workflow_clients,
                )
                .await
            }
        })?;
        let slack_live_session_runtime = session_runtime.clone();
        let slack_live_workflow_host_sandbox = workflow_host_sandbox.clone();
        let slack_live_workflow_clients = workflow_clients.clone();
        slack_live_client.register_task(WORKFLOW_TASK, move |input: WorkflowTaskInput, ctx| {
            let session_runtime = slack_live_session_runtime.clone();
            let workflow_host_sandbox = slack_live_workflow_host_sandbox.clone();
            let workflow_clients = slack_live_workflow_clients.clone();
            async move {
                run_centaur_workflow(
                    input,
                    ctx,
                    session_runtime,
                    workflow_host_sandbox,
                    workflow_clients,
                )
                .await
            }
        })?;
        let etl_session_runtime = session_runtime.clone();
        let etl_workflow_host_sandbox = workflow_host_sandbox.clone();
        let etl_workflow_clients = workflow_clients.clone();
        etl_client.register_task(WORKFLOW_TASK, move |input: WorkflowTaskInput, ctx| {
            let session_runtime = etl_session_runtime.clone();
            let workflow_host_sandbox = etl_workflow_host_sandbox.clone();
            let workflow_clients = etl_workflow_clients.clone();
            async move {
                run_centaur_workflow(
                    input,
                    ctx,
                    session_runtime,
                    workflow_host_sandbox,
                    workflow_clients,
                )
                .await
            }
        })?;
        let etl_backfill_session_runtime = session_runtime.clone();
        let etl_backfill_workflow_host_sandbox = workflow_host_sandbox.clone();
        let etl_backfill_workflow_clients = workflow_clients.clone();
        etl_backfill_client.register_task(
            WORKFLOW_TASK,
            move |input: WorkflowTaskInput, ctx| {
                let session_runtime = etl_backfill_session_runtime.clone();
                let workflow_host_sandbox = etl_backfill_workflow_host_sandbox.clone();
                let workflow_clients = etl_backfill_workflow_clients.clone();
                async move {
                    run_centaur_workflow(
                        input,
                        ctx,
                        session_runtime,
                        workflow_host_sandbox,
                        workflow_clients,
                    )
                    .await
                }
            },
        )?;
        let schedule_tick_client = schedule_client.clone();
        let workflow_clients_for_schedule = workflow_clients.clone();
        let schedule_registry_for_task = schedule_registry.clone();
        schedule_client.register_task_with(
            TaskRegistrationOptions::new(WORKFLOW_SCHEDULE_TASK),
            move |input: ScheduleTickInput, ctx| {
                let schedule_client = schedule_tick_client.clone();
                let workflow_clients = workflow_clients_for_schedule.clone();
                let schedules = schedule_registry_for_task.clone();
                async move {
                    run_schedule_tick(input, ctx, schedule_client, workflow_clients, schedules)
                        .await
                }
            },
        )?;
        let startup_schedules = schedule_registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        reconcile_schedules(&schedule_client, &startup_schedules).await?;

        let worker = client.start_worker(WorkerOptions {
            worker_id: Some("centaur-api-rs-workflow-worker".to_owned()),
            concurrency: worker_concurrency(
                WORKFLOW_WORKER_CONCURRENCY_ENV,
                DEFAULT_WORKFLOW_WORKER_CONCURRENCY,
            ),
            on_error: Some(Arc::new(|error| {
                warn!(%error, "absurd workflow worker error");
            })),
            ..WorkerOptions::default()
        });
        let slack_live_worker = slack_live_client.start_worker(WorkerOptions {
            worker_id: Some("centaur-api-rs-workflow-slack-live-worker".to_owned()),
            concurrency: 1,
            on_error: Some(Arc::new(|error| {
                warn!(%error, "absurd workflow slack live worker error");
            })),
            ..WorkerOptions::default()
        });
        let etl_worker = etl_client.start_worker(WorkerOptions {
            worker_id: Some("centaur-api-rs-workflow-etl-worker".to_owned()),
            concurrency: worker_concurrency(
                WORKFLOW_ETL_WORKER_CONCURRENCY_ENV,
                DEFAULT_WORKFLOW_ETL_WORKER_CONCURRENCY,
            ),
            on_error: Some(Arc::new(|error| {
                warn!(%error, "absurd workflow etl worker error");
            })),
            ..WorkerOptions::default()
        });
        let etl_backfill_worker = etl_backfill_client.start_worker(WorkerOptions {
            worker_id: Some("centaur-api-rs-workflow-etl-backfill-worker".to_owned()),
            concurrency: worker_concurrency(
                WORKFLOW_ETL_BACKFILL_WORKER_CONCURRENCY_ENV,
                DEFAULT_WORKFLOW_ETL_BACKFILL_WORKER_CONCURRENCY,
            ),
            on_error: Some(Arc::new(|error| {
                warn!(%error, "absurd workflow etl backfill worker error");
            })),
            ..WorkerOptions::default()
        });
        let schedule_worker = schedule_client.start_worker(WorkerOptions {
            worker_id: Some("centaur-api-rs-workflow-schedule-worker".to_owned()),
            concurrency: worker_concurrency(
                WORKFLOW_SCHEDULE_WORKER_CONCURRENCY_ENV,
                DEFAULT_WORKFLOW_SCHEDULE_WORKER_CONCURRENCY,
            ),
            on_error: Some(Arc::new(|error| {
                warn!(%error, "absurd workflow schedule worker error");
            })),
            ..WorkerOptions::default()
        });
        info!(
            queue = WORKFLOW_QUEUE,
            task = WORKFLOW_TASK,
            "started absurd workflow worker"
        );
        info!(
            queue = WORKFLOW_SLACK_LIVE_QUEUE,
            task = WORKFLOW_TASK,
            "started absurd workflow slack live worker"
        );
        info!(
            queue = WORKFLOW_ETL_QUEUE,
            task = WORKFLOW_TASK,
            "started absurd workflow etl worker"
        );
        info!(
            queue = WORKFLOW_ETL_BACKFILL_QUEUE,
            task = WORKFLOW_TASK,
            "started absurd workflow etl backfill worker"
        );
        info!(
            queue = WORKFLOW_SCHEDULE_QUEUE,
            task = WORKFLOW_SCHEDULE_TASK,
            "started absurd workflow schedule worker"
        );

        if let Some(interval) = workflow_reconcile_interval() {
            spawn_workflow_metadata_reconciler(
                schedule_client.clone(),
                workflow_clients,
                webhook_registry.clone(),
                schedule_registry.clone(),
                interval,
            );
        }

        Ok(Self {
            inner: Arc::new(WorkflowRuntimeInner {
                client,
                slack_live_client,
                etl_client,
                etl_backfill_client,
                _worker: worker,
                _slack_live_worker: slack_live_worker,
                _etl_worker: etl_worker,
                _etl_backfill_worker: etl_backfill_worker,
                _schedule_worker: schedule_worker,
                webhook_registry,
                schedule_registry,
            }),
        })
    }

    pub async fn create_run(
        &self,
        request: CreateWorkflowRunRequest,
    ) -> Result<CreateWorkflowRunResponse, WorkflowRuntimeError> {
        let workflow_name = request.workflow_name.trim();
        if workflow_name.is_empty() {
            return Err(WorkflowRuntimeError::BadRequest(
                "workflow_name must not be empty".to_owned(),
            ));
        }
        WorkflowEnablement::from_env()?.ensure_enabled(workflow_name)?;
        let client = self.client_for_workflow(workflow_name);
        let spawn = client
            .spawn(
                WORKFLOW_TASK,
                WorkflowTaskInput {
                    workflow_name: workflow_name.to_owned(),
                    input: request.input,
                    harness_type: request.harness_type.unwrap_or(HarnessType::Codex),
                },
                SpawnOptions {
                    max_attempts: request.max_attempts,
                    idempotency_key: request.idempotency_key,
                    ..SpawnOptions::default()
                },
            )
            .await?;
        Ok(CreateWorkflowRunResponse {
            ok: true,
            run_id: spawn.run_id,
            task_id: spawn.task_id,
            status: "queued".to_owned(),
            created: spawn.created,
        })
    }

    pub async fn list_runs(&self, limit: i64) -> Result<Vec<WorkflowRun>, WorkflowRuntimeError> {
        let limit = limit.clamp(1, 200);
        let mut runs = Vec::new();
        runs.extend(self.list_runs_for_queue(WORKFLOW_QUEUE, limit).await?);
        runs.extend(
            self.list_runs_for_queue(WORKFLOW_SLACK_LIVE_QUEUE, limit)
                .await?,
        );
        runs.extend(self.list_runs_for_queue(WORKFLOW_ETL_QUEUE, limit).await?);
        runs.extend(
            self.list_runs_for_queue(WORKFLOW_ETL_BACKFILL_QUEUE, limit)
                .await?,
        );
        runs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(b.task_id.cmp(&a.task_id))
        });
        runs.truncate(limit as usize);
        Ok(runs)
    }

    async fn list_runs_for_queue(
        &self,
        queue_name: &str,
        limit: i64,
    ) -> Result<Vec<WorkflowRun>, WorkflowRuntimeError> {
        let (task_table, run_table) = absurd_queue_tables(queue_name)?;
        let rows = sqlx::query(&format!(
            r#"
            select
                r.run_id::text as run_id,
                t.task_id::text as task_id,
                t.task_name,
                t.params,
                t.state,
                t.attempts,
                t.completed_payload,
                r.failure_reason,
                t.enqueue_at as created_at,
                greatest(t.enqueue_at, coalesce(r.available_at, t.enqueue_at)) as updated_at
            from {task_table} t
            join {run_table} r on r.run_id = t.last_attempt_run
            order by t.enqueue_at desc, t.task_id desc
            limit $1
            "#,
        ))
        .bind(limit)
        .fetch_all(self.inner.client.pool())
        .await?;

        rows.into_iter().map(workflow_run_from_row).collect()
    }

    pub async fn get_run(&self, run_id: &str) -> Result<WorkflowRun, WorkflowRuntimeError> {
        for queue_name in [
            WORKFLOW_QUEUE,
            WORKFLOW_SLACK_LIVE_QUEUE,
            WORKFLOW_ETL_QUEUE,
            WORKFLOW_ETL_BACKFILL_QUEUE,
        ] {
            if let Some(run) = self.get_run_for_queue(queue_name, run_id).await? {
                return Ok(run);
            }
        }
        Err(WorkflowRuntimeError::NotFound(run_id.to_owned()))
    }

    async fn get_run_for_queue(
        &self,
        queue_name: &str,
        run_id: &str,
    ) -> Result<Option<WorkflowRun>, WorkflowRuntimeError> {
        let (task_table, run_table) = absurd_queue_tables(queue_name)?;
        let row = sqlx::query(&format!(
            r#"
            select
                r.run_id::text as run_id,
                t.task_id::text as task_id,
                t.task_name,
                t.params,
                t.state,
                t.attempts,
                t.completed_payload,
                r.failure_reason,
                t.enqueue_at as created_at,
                greatest(t.enqueue_at, coalesce(r.available_at, t.enqueue_at)) as updated_at
            from {run_table} r
            join {task_table} t on t.task_id = r.task_id
            where r.run_id = $1::uuid
            "#,
        ))
        .bind(run_id)
        .fetch_optional(self.inner.client.pool())
        .await?;
        row.map(workflow_run_from_row).transpose()
    }

    pub async fn cancel_run(&self, run_id: &str) -> Result<(), WorkflowRuntimeError> {
        for (queue_name, client) in [
            (WORKFLOW_QUEUE, &self.inner.client),
            (WORKFLOW_SLACK_LIVE_QUEUE, &self.inner.slack_live_client),
            (WORKFLOW_ETL_QUEUE, &self.inner.etl_client),
            (WORKFLOW_ETL_BACKFILL_QUEUE, &self.inner.etl_backfill_client),
        ] {
            if let Some(run) = self.get_run_for_queue(queue_name, run_id).await? {
                client.cancel_task(&run.task_id, Some(queue_name)).await?;
                return Ok(());
            }
        }
        Err(WorkflowRuntimeError::NotFound(run_id.to_owned()))
    }

    pub async fn emit_event(
        &self,
        event_name: &str,
        payload: Value,
    ) -> Result<(), WorkflowRuntimeError> {
        self.inner
            .client
            .emit_event(event_name, payload.clone(), Some(WORKFLOW_QUEUE))
            .await?;
        self.inner
            .slack_live_client
            .emit_event(event_name, payload.clone(), Some(WORKFLOW_SLACK_LIVE_QUEUE))
            .await?;
        self.inner
            .etl_client
            .emit_event(event_name, payload.clone(), Some(WORKFLOW_ETL_QUEUE))
            .await?;
        self.inner
            .etl_backfill_client
            .emit_event(event_name, payload, Some(WORKFLOW_ETL_BACKFILL_QUEUE))
            .await?;
        Ok(())
    }

    pub fn get_webhook(&self, slug: &str) -> Option<RegisteredWorkflowWebhook> {
        self.inner
            .webhook_registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(slug)
            .cloned()
    }

    pub fn list_webhooks(&self) -> Vec<RegisteredWorkflowWebhook> {
        self.inner
            .webhook_registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn list_schedules(&self) -> Vec<RegisteredWorkflowSchedule> {
        self.inner
            .schedule_registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn client_for_workflow(&self, workflow_name: &str) -> &Client {
        match workflow_queue_class(workflow_name) {
            WorkflowQueueClass::Standard => &self.inner.client,
            WorkflowQueueClass::SlackLive => &self.inner.slack_live_client,
            WorkflowQueueClass::Etl => &self.inner.etl_client,
            WorkflowQueueClass::EtlBackfill => &self.inner.etl_backfill_client,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkflowQueueClass {
    Standard,
    SlackLive,
    Etl,
    EtlBackfill,
}

fn workflow_queue_class(workflow_name: &str) -> WorkflowQueueClass {
    match workflow_name {
        "slack_sync" => WorkflowQueueClass::SlackLive,
        "slack_backfill" | "slack_archive_import" => WorkflowQueueClass::EtlBackfill,
        "google_calendar_sync"
        | "google_drive_sync"
        | "linear_sync"
        | "company_context_documents"
        | "slack_retention"
        | "chief_of_staff_daily" => WorkflowQueueClass::Etl,
        _ => WorkflowQueueClass::Standard,
    }
}

fn queue_name_for_class(class: WorkflowQueueClass) -> &'static str {
    match class {
        WorkflowQueueClass::Standard => WORKFLOW_QUEUE,
        WorkflowQueueClass::SlackLive => WORKFLOW_SLACK_LIVE_QUEUE,
        WorkflowQueueClass::Etl => WORKFLOW_ETL_QUEUE,
        WorkflowQueueClass::EtlBackfill => WORKFLOW_ETL_BACKFILL_QUEUE,
    }
}

fn absurd_queue_tables(
    queue_name: &str,
) -> Result<(&'static str, &'static str), WorkflowRuntimeError> {
    match queue_name {
        WORKFLOW_QUEUE => Ok(("absurd.t_centaur_workflows", "absurd.r_centaur_workflows")),
        WORKFLOW_SLACK_LIVE_QUEUE => Ok((
            "absurd.t_centaur_workflows_slack_live",
            "absurd.r_centaur_workflows_slack_live",
        )),
        WORKFLOW_ETL_QUEUE => Ok((
            "absurd.t_centaur_workflows_etl",
            "absurd.r_centaur_workflows_etl",
        )),
        WORKFLOW_ETL_BACKFILL_QUEUE => Ok((
            "absurd.t_centaur_workflows_etl_backfill",
            "absurd.r_centaur_workflows_etl_backfill",
        )),
        WORKFLOW_SCHEDULE_QUEUE => Ok((
            "absurd.t_centaur_workflow_schedules",
            "absurd.r_centaur_workflow_schedules",
        )),
        other => Err(WorkflowRuntimeError::Internal(format!(
            "unknown workflow queue {other:?}"
        ))),
    }
}

fn build_webhook_registry(
    discovery: &PythonWorkflowMetadata,
    enablement: &WorkflowEnablement,
) -> Result<BTreeMap<String, RegisteredWorkflowWebhook>, WorkflowRuntimeError> {
    let mut registry = BTreeMap::new();
    for webhook in discovery.webhooks.clone() {
        if !enablement.is_enabled(&webhook.workflow_name) {
            continue;
        }
        insert_webhook(&mut registry, webhook)?;
    }
    for webhook in default_workflow_webhooks() {
        if !enablement.is_enabled(&webhook.workflow_name) {
            continue;
        }
        insert_webhook_if_absent(&mut registry, webhook)?;
    }
    if let Ok(raw) = env::var("WORKFLOW_WEBHOOKS_JSON") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let webhooks: Vec<RegisteredWorkflowWebhook> = serde_json::from_str(trimmed)?;
            for webhook in webhooks {
                if !enablement.is_enabled(&webhook.workflow_name) {
                    continue;
                }
                insert_webhook_replace(&mut registry, webhook)?;
            }
        }
    }
    Ok(registry)
}

fn build_schedule_registry(
    discovery: &PythonWorkflowMetadata,
    enablement: &WorkflowEnablement,
) -> Result<BTreeMap<String, RegisteredWorkflowSchedule>, WorkflowRuntimeError> {
    let mut registry = BTreeMap::new();
    for schedule in &discovery.schedules {
        let schedule = normalize_schedule(schedule.clone())?;
        if !enablement.is_enabled(&schedule.workflow_name) {
            continue;
        }
        if registry
            .insert(schedule.schedule_id.clone(), schedule.clone())
            .is_some()
        {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "duplicate workflow schedule_id {:?}",
                schedule.schedule_id
            )));
        }
    }
    Ok(registry)
}

fn insert_webhook(
    registry: &mut BTreeMap<String, RegisteredWorkflowWebhook>,
    mut webhook: RegisteredWorkflowWebhook,
) -> Result<(), WorkflowRuntimeError> {
    normalize_webhook(&mut webhook)?;
    let slug = webhook.spec.slug.clone();
    if registry.insert(slug.clone(), webhook).is_some() {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "duplicate workflow webhook slug {slug:?}"
        )));
    }
    Ok(())
}

fn insert_webhook_if_absent(
    registry: &mut BTreeMap<String, RegisteredWorkflowWebhook>,
    mut webhook: RegisteredWorkflowWebhook,
) -> Result<(), WorkflowRuntimeError> {
    normalize_webhook(&mut webhook)?;
    registry.entry(webhook.spec.slug.clone()).or_insert(webhook);
    Ok(())
}

fn insert_webhook_replace(
    registry: &mut BTreeMap<String, RegisteredWorkflowWebhook>,
    mut webhook: RegisteredWorkflowWebhook,
) -> Result<(), WorkflowRuntimeError> {
    normalize_webhook(&mut webhook)?;
    registry.insert(webhook.spec.slug.clone(), webhook);
    Ok(())
}

fn normalize_webhook(webhook: &mut RegisteredWorkflowWebhook) -> Result<(), WorkflowRuntimeError> {
    if webhook.workflow_name.trim().is_empty() {
        return Err(WorkflowRuntimeError::BadRequest(
            "workflow webhook workflow_name must not be empty".to_owned(),
        ));
    }
    if !valid_webhook_slug(&webhook.spec.slug) {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "invalid workflow webhook slug {:?}",
            webhook.spec.slug
        )));
    }
    webhook.spec.allowed_methods = webhook
        .spec
        .allowed_methods
        .iter()
        .map(|method| method.trim().to_ascii_uppercase())
        .collect();
    if webhook.spec.allowed_methods.is_empty()
        || webhook
            .spec
            .allowed_methods
            .iter()
            .any(|method| method.is_empty() || !method.chars().all(|ch| ch.is_ascii_alphabetic()))
    {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "workflow webhook {:?} has invalid allowed_methods",
            webhook.spec.slug
        )));
    }
    webhook.spec.allowed_content_types = webhook
        .spec
        .allowed_content_types
        .iter()
        .map(|content_type| content_type.trim().to_ascii_lowercase())
        .collect();
    if webhook.spec.allowed_content_types.is_empty() {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "workflow webhook {:?} must allow at least one content type",
            webhook.spec.slug
        )));
    }
    match &webhook.spec.auth {
        WorkflowWebhookAuth::None => {}
        WorkflowWebhookAuth::Hmac {
            secret_ref,
            signature_header,
            algorithm,
            encoding,
            ..
        } => {
            if secret_ref.trim().is_empty() || signature_header.trim().is_empty() {
                return Err(WorkflowRuntimeError::BadRequest(format!(
                    "workflow webhook {:?} hmac auth requires secret_ref and signature_header",
                    webhook.spec.slug
                )));
            }
            if algorithm != "sha256" {
                return Err(WorkflowRuntimeError::BadRequest(
                    "only sha256 webhook HMAC auth is supported".to_owned(),
                ));
            }
            if !matches!(encoding.as_str(), "hex" | "base64") {
                return Err(WorkflowRuntimeError::BadRequest(
                    "webhook HMAC encoding must be hex or base64".to_owned(),
                ));
            }
        }
        WorkflowWebhookAuth::Github { secret_ref } | WorkflowWebhookAuth::Bearer { secret_ref } => {
            if secret_ref.trim().is_empty() {
                return Err(WorkflowRuntimeError::BadRequest(format!(
                    "workflow webhook {:?} auth requires secret_ref",
                    webhook.spec.slug
                )));
            }
        }
    }
    if let Some(filter) = &mut webhook.spec.filter {
        normalize_webhook_filter(&webhook.spec.slug, filter)?;
    }
    Ok(())
}

fn normalize_webhook_filter(
    slug: &str,
    filter: &mut WebhookFilter,
) -> Result<(), WorkflowRuntimeError> {
    normalize_webhook_filter_node(slug, filter, "filter")
}

fn normalize_webhook_filter_node(
    slug: &str,
    filter: &mut WebhookFilter,
    path: &str,
) -> Result<(), WorkflowRuntimeError> {
    let has_any = filter.any.is_some();
    let has_all = filter.all.is_some();
    let has_leaf = filter.source.is_some()
        || filter.key.is_some()
        || filter.op.is_some()
        || filter.value.is_some()
        || filter.values.is_some();
    if usize::from(has_any) + usize::from(has_all) + usize::from(has_leaf) != 1 {
        return Err(invalid_webhook_filter(
            slug,
            path,
            "node must be exactly one of any, all, or a leaf predicate",
        ));
    }

    if let Some(any) = &mut filter.any {
        if any.is_empty() {
            return Err(invalid_webhook_filter(slug, path, "any must not be empty"));
        }
        for (index, child) in any.iter_mut().enumerate() {
            normalize_webhook_filter_node(slug, child, &format!("{path}.any[{index}]"))?;
        }
        return Ok(());
    }
    if let Some(all) = &mut filter.all {
        if all.is_empty() {
            return Err(invalid_webhook_filter(slug, path, "all must not be empty"));
        }
        for (index, child) in all.iter_mut().enumerate() {
            normalize_webhook_filter_node(slug, child, &format!("{path}.all[{index}]"))?;
        }
        return Ok(());
    }

    let source = normalize_required_filter_string(&mut filter.source)
        .ok_or_else(|| invalid_webhook_filter(slug, path, "leaf requires source"))?;
    let key = normalize_required_filter_string(&mut filter.key)
        .ok_or_else(|| invalid_webhook_filter(slug, path, "leaf requires key"))?;
    let op = normalize_required_filter_string(&mut filter.op)
        .ok_or_else(|| invalid_webhook_filter(slug, path, "leaf requires op"))?;
    filter.source = Some(source.to_ascii_lowercase());
    filter.op = Some(op.to_ascii_lowercase());
    let source = filter.source.as_deref().unwrap_or_default();
    let op = filter.op.as_deref().unwrap_or_default();
    if !matches!(source, "header" | "body") {
        return Err(invalid_webhook_filter(
            slug,
            path,
            "source must be header or body",
        ));
    }
    if source == "body" && key.split('.').any(|part| part.trim().is_empty()) {
        return Err(invalid_webhook_filter(
            slug,
            path,
            "body key must be a non-empty dot path",
        ));
    }
    match op {
        "equals" | "contains" | "prefix" => {
            if filter.values.is_some() {
                return Err(invalid_webhook_filter(
                    slug,
                    path,
                    "values is only valid with op in",
                ));
            }
            normalize_required_filter_string(&mut filter.value).ok_or_else(|| {
                invalid_webhook_filter(slug, path, "op requires a non-empty value")
            })?;
        }
        "in" => {
            if filter.value.is_some() {
                return Err(invalid_webhook_filter(
                    slug,
                    path,
                    "value is not valid with op in",
                ));
            }
            let Some(values) = &mut filter.values else {
                return Err(invalid_webhook_filter(
                    slug,
                    path,
                    "op in requires non-empty values",
                ));
            };
            for value in values.iter_mut() {
                *value = value.trim().to_owned();
            }
            if values.is_empty() || values.iter().any(String::is_empty) {
                return Err(invalid_webhook_filter(
                    slug,
                    path,
                    "op in requires non-empty values",
                ));
            }
        }
        _ => {
            return Err(invalid_webhook_filter(
                slug,
                path,
                "op must be equals, in, contains, or prefix",
            ));
        }
    }
    Ok(())
}

fn normalize_required_filter_string(value: &mut Option<String>) -> Option<String> {
    let normalized = value.as_ref()?.trim().to_owned();
    if normalized.is_empty() {
        return None;
    }
    *value = Some(normalized.clone());
    Some(normalized)
}

fn invalid_webhook_filter(slug: &str, path: &str, reason: &str) -> WorkflowRuntimeError {
    WorkflowRuntimeError::BadRequest(format!(
        "workflow webhook {slug:?} has invalid filter at {path}: {reason}"
    ))
}

fn valid_webhook_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if slug.len() > 128 || !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    slug.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-'))
}

fn normalize_schedule(raw: Value) -> Result<RegisteredWorkflowSchedule, WorkflowRuntimeError> {
    let object = raw.as_object().ok_or_else(|| {
        WorkflowRuntimeError::BadRequest("workflow SCHEDULE must be an object".to_owned())
    })?;
    let workflow_name = object
        .get("workflow_name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest("workflow SCHEDULE missing workflow_name".to_owned())
        })?
        .trim()
        .to_owned();
    let schedule_id = object
        .get("schedule_id")
        .and_then(Value::as_str)
        .unwrap_or(&workflow_name)
        .trim()
        .to_owned();
    if !valid_webhook_slug(&schedule_id) {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "invalid workflow schedule_id {schedule_id:?}"
        )));
    }
    let enabled = schedule_bool(object.get("enabled"), true);
    let no_delivery = schedule_bool(object.get("no_delivery"), false);
    let timezone = object
        .get("timezone")
        .and_then(Value::as_str)
        .unwrap_or("America/Los_Angeles")
        .trim()
        .to_owned();
    let kind = if let Some(cron) = object.get("cron").and_then(Value::as_str) {
        let cron = cron.trim().to_owned();
        if cron.is_empty() {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "workflow schedule {schedule_id:?} has empty cron"
            )));
        }
        WorkflowScheduleKind::Cron { cron }
    } else if let Some(interval_seconds) = object.get("interval_seconds").and_then(Value::as_u64) {
        if interval_seconds == 0 {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "workflow schedule {schedule_id:?} interval_seconds must be > 0"
            )));
        }
        WorkflowScheduleKind::Interval { interval_seconds }
    } else {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "workflow schedule {schedule_id:?} must have cron or interval_seconds"
        )));
    };
    let input = workflow_schedule_input(&workflow_name, object, no_delivery);
    Ok(RegisteredWorkflowSchedule {
        schedule_id,
        workflow_name,
        source_path: object
            .get("source_path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        kind,
        timezone,
        input,
        enabled,
        no_delivery,
    })
}

fn schedule_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Some(Value::Number(value)) => value.as_i64().unwrap_or(1) != 0,
        Some(Value::Null) | None => default,
        _ => default,
    }
}

fn workflow_schedule_input(
    workflow_name: &str,
    object: &serde_json::Map<String, Value>,
    no_delivery: bool,
) -> Value {
    let mut input = object
        .get("input")
        .cloned()
        .unwrap_or_else(|| json!({}))
        .as_object()
        .cloned()
        .unwrap_or_default();
    let metadata = input
        .entry("metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(metadata) = metadata {
        metadata.insert("source".to_owned(), json!("workflow_schedule"));
        metadata.insert("workflow_name".to_owned(), json!(workflow_name));
        metadata.insert("no_delivery".to_owned(), json!(no_delivery));
    }
    if let Some(thread_key) = object.get("thread_key").and_then(Value::as_str)
        && !thread_key.trim().is_empty()
    {
        input.insert("thread_key".to_owned(), json!(thread_key.trim()));
        if !input.contains_key("delivery")
            && let Some((channel, thread_ts)) = split_slack_thread_key(thread_key.trim())
        {
            input.insert(
                "delivery".to_owned(),
                json!({
                    "platform": "slack",
                    "channel": channel,
                    "thread_ts": thread_ts,
                }),
            );
        }
    }
    if let Some(slack_channel) = object.get("slack_channel").and_then(Value::as_str) {
        let slack_channel = slack_channel.trim().trim_start_matches('#');
        if !slack_channel.is_empty() && !input.contains_key("delivery") {
            input.insert(
                "delivery".to_owned(),
                json!({
                    "platform": "slack",
                    "channel": slack_channel,
                }),
            );
        }
    }
    Value::Object(input)
}

fn split_slack_thread_key(thread_key: &str) -> Option<(&str, &str)> {
    let parts = thread_key.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        [channel, thread_ts] if !channel.is_empty() && !thread_ts.is_empty() => {
            Some((channel, thread_ts))
        }
        ["slack", channel, thread_ts] if !channel.is_empty() && !thread_ts.is_empty() => {
            Some((channel, thread_ts))
        }
        _ => None,
    }
}

fn default_workflow_webhooks() -> Vec<RegisteredWorkflowWebhook> {
    vec![
        RegisteredWorkflowWebhook {
            workflow_name: "github_issue_triage".to_owned(),
            source_path: "workflows/github_issue_triage.py".to_owned(),
            spec: WorkflowWebhookSpec {
                slug: "github-issue-triage".to_owned(),
                provider: Some("github".to_owned()),
                auth: WorkflowWebhookAuth::Github {
                    secret_ref: "GITHUB_WEBHOOK_SECRET".to_owned(),
                },
                trigger_key: Some(WorkflowWebhookTriggerKey::Header {
                    header: "X-GitHub-Delivery".to_owned(),
                }),
                allowed_methods: vec!["POST".to_owned()],
                allowed_content_types: vec![
                    "application/json".to_owned(),
                    "application/x-www-form-urlencoded".to_owned(),
                ],
                filter: None,
            },
        },
        RegisteredWorkflowWebhook {
            workflow_name: "consensus_ci_triage".to_owned(),
            source_path: "centaur-tempo/workflows/consensus_ci_triage.py".to_owned(),
            spec: WorkflowWebhookSpec {
                slug: "github-consensus-ci-triage".to_owned(),
                provider: Some("github".to_owned()),
                auth: WorkflowWebhookAuth::Github {
                    secret_ref: "GITHUB_WEBHOOK_SECRET".to_owned(),
                },
                trigger_key: Some(WorkflowWebhookTriggerKey::Header {
                    header: "X-GitHub-Delivery".to_owned(),
                }),
                allowed_methods: vec!["POST".to_owned()],
                allowed_content_types: vec![
                    "application/json".to_owned(),
                    "application/x-www-form-urlencoded".to_owned(),
                ],
                filter: None,
            },
        },
        RegisteredWorkflowWebhook {
            workflow_name: "trivy_vulnerability_intake".to_owned(),
            source_path: "centaur-tempo/workflows/trivy_vulnerability_intake.py".to_owned(),
            spec: WorkflowWebhookSpec {
                slug: "trivy-vulnerability-intake".to_owned(),
                provider: Some("alertmanager".to_owned()),
                auth: WorkflowWebhookAuth::Bearer {
                    secret_ref: "TRIVY_INTAKE_WEBHOOK_TOKEN".to_owned(),
                },
                trigger_key: None,
                allowed_methods: vec!["POST".to_owned()],
                allowed_content_types: vec!["application/json".to_owned()],
                filter: None,
            },
        },
    ]
}

fn default_webhook_methods() -> Vec<String> {
    vec!["POST".to_owned()]
}

fn default_webhook_content_types() -> Vec<String> {
    vec!["application/json".to_owned()]
}

fn default_signature_header() -> String {
    "X-Webhook-Signature".to_owned()
}

fn default_signature_prefix() -> String {
    "sha256=".to_owned()
}

fn default_hmac_algorithm() -> String {
    "sha256".to_owned()
}

fn default_hmac_encoding() -> String {
    "hex".to_owned()
}

#[derive(Debug, Deserialize)]
struct PythonWorkflowDiscovery {
    workflow_name: String,
    source_path: String,
    #[serde(default)]
    webhooks: Vec<RegisteredWorkflowWebhook>,
    #[serde(default)]
    schedule: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct PythonWorkflowDiscoveryPayload {
    workflows: Vec<PythonWorkflowDiscovery>,
}

#[derive(Debug, Default)]
struct PythonWorkflowMetadata {
    webhooks: Vec<RegisteredWorkflowWebhook>,
    schedules: Vec<Value>,
    workflow_names: BTreeSet<String>,
}

fn metadata_from_discovery_payload(
    payload: PythonWorkflowDiscoveryPayload,
) -> PythonWorkflowMetadata {
    let mut metadata = PythonWorkflowMetadata::default();
    for workflow in payload.workflows {
        metadata
            .workflow_names
            .insert(workflow.workflow_name.clone());
        metadata.webhooks.extend(workflow.webhooks);
        if let Some(mut schedule) = workflow.schedule {
            if let Some(object) = schedule.as_object_mut() {
                object
                    .entry("workflow_name".to_owned())
                    .or_insert_with(|| json!(workflow.workflow_name));
                object
                    .entry("source_path".to_owned())
                    .or_insert_with(|| json!(workflow.source_path));
            }
            metadata.schedules.push(schedule);
        }
    }
    metadata
}

async fn discover_python_workflow_metadata() -> Result<PythonWorkflowMetadata, WorkflowRuntimeError>
{
    let host_path = python_workflow_host_path();
    let mut command = Command::new(
        env::var(PYTHON_HOST_INTERPRETER_ENV).unwrap_or_else(|_| "python3".to_owned()),
    );
    command
        .arg(&host_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if env::var_os("WORKFLOW_DIRS").is_none() {
        command.env("WORKFLOW_DIRS", default_workflow_dirs());
    }

    let mut child = command.spawn().map_err(|error| {
        WorkflowRuntimeError::Internal(format!(
            "failed to spawn Python workflow host {}: {error}",
            host_path.display()
        ))
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| WorkflowRuntimeError::Internal("workflow host stdin missing".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkflowRuntimeError::Internal("workflow host stdout missing".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkflowRuntimeError::Internal("workflow host stderr missing".to_owned()))?;
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut collected = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            collected.push(line);
        }
        collected.join("\n")
    });

    write_host_message(&mut stdin, &json!({"type": "workflow.discover"})).await?;
    drop(stdin);

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)?;
        match message.get("type").and_then(Value::as_str) {
            Some("workflow.discovery") => {
                let _ = child.wait().await;
                let payload: PythonWorkflowDiscoveryPayload = serde_json::from_value(message)?;
                let mut metadata = metadata_from_discovery_payload(payload);
                WorkflowEnablement::from_env()?.filter_metadata(&mut metadata);
                return Ok(metadata);
            }
            Some("host.error") | Some("workflow.error") => {
                let stderr = stderr_task.await.unwrap_or_default();
                return Err(WorkflowRuntimeError::Internal(format!(
                    "Python workflow discovery error: {}{}{}",
                    message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error"),
                    if stderr.is_empty() { "" } else { "\nstderr:\n" },
                    stderr,
                )));
            }
            other => {
                return Err(WorkflowRuntimeError::Internal(format!(
                    "unexpected Python workflow discovery message type {other:?}: {message}"
                )));
            }
        }
    }

    let status = child.wait().await?;
    let stderr = stderr_task.await.unwrap_or_default();
    Err(WorkflowRuntimeError::Internal(format!(
        "Python workflow host exited before workflow.discovery: status={status}, stderr={stderr}"
    )))
}

async fn reconcile_schedules(
    client: &Client,
    schedules: &BTreeMap<String, RegisteredWorkflowSchedule>,
) -> Result<(), WorkflowRuntimeError> {
    for schedule in schedules.values().filter(|schedule| schedule.enabled) {
        let next_run_at = next_schedule_time(schedule, Utc::now())?;
        let spawned = ensure_schedule_tick(client, schedule, next_run_at).await?;
        info!(
            schedule_id = %schedule.schedule_id,
            workflow_name = %schedule.workflow_name,
            next_run_at = %next_run_at.to_rfc3339(),
            spawned,
            "reconciled absurd workflow schedule"
        );
    }
    Ok(())
}

fn workflow_reconcile_interval() -> Option<Duration> {
    let seconds = env::var(WORKFLOW_RECONCILE_INTERVAL_SECS_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_WORKFLOW_RECONCILE_INTERVAL_SECS);
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// Resolve a worker concurrency from `env_name`, falling back to `default` when
/// the value is unset, empty, non-numeric, or zero.
fn worker_concurrency(env_name: &str, default: usize) -> usize {
    parse_worker_concurrency(env::var(env_name).ok().as_deref(), default)
}

/// Pure parse for [`worker_concurrency`], split out so it is testable without
/// mutating process environment.
fn parse_worker_concurrency(raw: Option<&str>, default: usize) -> usize {
    raw.and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn spawn_workflow_metadata_reconciler(
    schedule_client: Client,
    workflow_clients: WorkflowQueueClients,
    webhook_registry: Arc<RwLock<BTreeMap<String, RegisteredWorkflowWebhook>>>,
    schedule_registry: Arc<RwLock<BTreeMap<String, RegisteredWorkflowSchedule>>>,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        let mut reaper = RemovedWorkflowReaper::from_env();
        let mut queue_metrics = WorkflowQueueMetricsRecorder::default();
        // Startup discovery already ran; wait one full period before refreshing.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            match reconcile_workflow_metadata_once(
                &schedule_client,
                &webhook_registry,
                &schedule_registry,
            )
            .await
            {
                Ok((metadata, schedules)) => {
                    if let Err(error) = record_workflow_queue_metrics(
                        &mut queue_metrics,
                        [
                            (WORKFLOW_QUEUE, &workflow_clients.standard),
                            (WORKFLOW_SLACK_LIVE_QUEUE, &workflow_clients.slack_live),
                            (WORKFLOW_ETL_QUEUE, &workflow_clients.etl),
                            (WORKFLOW_ETL_BACKFILL_QUEUE, &workflow_clients.etl_backfill),
                        ],
                        &metadata.workflow_names,
                    )
                    .await
                    {
                        warn!(%error, "failed to record workflow queue metrics");
                    }
                    if let Err(error) = reaper
                        .reap(&workflow_clients, &schedule_client, &metadata, &schedules)
                        .await
                    {
                        warn!(%error, "failed to reap removed workflow tasks");
                    }
                }
                Err(error) => warn!(%error, "failed to reconcile workflow metadata"),
            }
        }
    });
}

async fn reconcile_workflow_metadata_once(
    schedule_client: &Client,
    webhook_registry: &Arc<RwLock<BTreeMap<String, RegisteredWorkflowWebhook>>>,
    schedule_registry: &Arc<RwLock<BTreeMap<String, RegisteredWorkflowSchedule>>>,
) -> Result<
    (
        PythonWorkflowMetadata,
        BTreeMap<String, RegisteredWorkflowSchedule>,
    ),
    WorkflowRuntimeError,
> {
    let enablement = WorkflowEnablement::from_env()?;
    let discovery = discover_python_workflow_metadata().await?;
    let next_webhooks = build_webhook_registry(&discovery, &enablement)?;
    let next_schedules = build_schedule_registry(&discovery, &enablement)?;
    {
        let mut webhooks = webhook_registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *webhooks = next_webhooks;
    }
    {
        let mut schedules = schedule_registry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *schedules = next_schedules.clone();
    }
    reconcile_schedules(schedule_client, &next_schedules).await?;
    info!(
        webhook_count = discovery.webhooks.len(),
        schedule_count = discovery.schedules.len(),
        "reconciled workflow metadata"
    );
    Ok((discovery, next_schedules))
}

/// Cancels queued/running runs and pending schedule ticks that reference a
/// workflow which is no longer discoverable on disk. Without this, runs of a
/// deleted workflow keep retrying (each attempt spawning a sandbox that fails
/// with `unknown workflow_name`) and an interrupted run can sit in `running`
/// forever once its claim lapses.
struct RemovedWorkflowReaper {
    threshold: u32,
    workflow_miss_counts: BTreeMap<String, u32>,
    schedule_miss_counts: BTreeMap<String, u32>,
}

#[derive(Default)]
struct WorkflowQueueMetricsRecorder {
    seen_queue_states: BTreeSet<(String, String)>,
    seen_workflow_states: BTreeSet<(String, String, String)>,
}

struct WorkflowQueueMetricRow {
    queue_name: String,
    workflow_name: String,
    state: String,
    task_count: i64,
    oldest_age_seconds: f64,
}

const WORKFLOW_QUEUE_METRIC_STATES: &[&str] = &["pending", "running", "sleeping"];

async fn record_workflow_queue_metrics(
    recorder: &mut WorkflowQueueMetricsRecorder,
    queues: [(&str, &Client); 4],
    workflow_names: &BTreeSet<String>,
) -> Result<(), WorkflowRuntimeError> {
    let mut rows = Vec::new();
    for (queue_name, client) in queues {
        for state in WORKFLOW_QUEUE_METRIC_STATES {
            recorder
                .seen_queue_states
                .insert((queue_name.to_owned(), (*state).to_owned()));
        }
        rows.extend(fetch_workflow_queue_metric_rows(client, queue_name).await?);
    }

    for workflow_name in workflow_names {
        let queue_name = queue_name_for_class(workflow_queue_class(workflow_name));
        for state in WORKFLOW_QUEUE_METRIC_STATES {
            recorder.seen_workflow_states.insert((
                queue_name.to_owned(),
                (*state).to_owned(),
                workflow_name.clone(),
            ));
        }
    }

    for row in &rows {
        recorder
            .seen_queue_states
            .insert((row.queue_name.clone(), row.state.clone()));
        recorder.seen_workflow_states.insert((
            row.queue_name.clone(),
            row.state.clone(),
            row.workflow_name.clone(),
        ));
    }

    for (queue_name, state) in &recorder.seen_queue_states {
        centaur_telemetry::set_workflow_queue_tasks(queue_name, state, 0.0);
        centaur_telemetry::set_workflow_queue_oldest_task_age_seconds(queue_name, state, 0.0);
    }
    for (queue_name, state, workflow_name) in &recorder.seen_workflow_states {
        centaur_telemetry::set_workflow_queue_tasks_by_workflow(
            queue_name,
            state,
            workflow_name,
            0.0,
        );
        centaur_telemetry::set_workflow_queue_oldest_task_age_by_workflow_seconds(
            queue_name,
            state,
            workflow_name,
            0.0,
        );
    }

    let mut queue_totals: BTreeMap<(String, String), (i64, f64)> = BTreeMap::new();
    for row in rows {
        let total = queue_totals
            .entry((row.queue_name.clone(), row.state.clone()))
            .or_insert((0, 0.0));
        total.0 += row.task_count;
        total.1 = total.1.max(row.oldest_age_seconds);

        centaur_telemetry::set_workflow_queue_tasks_by_workflow(
            &row.queue_name,
            &row.state,
            &row.workflow_name,
            row.task_count as f64,
        );
        centaur_telemetry::set_workflow_queue_oldest_task_age_by_workflow_seconds(
            &row.queue_name,
            &row.state,
            &row.workflow_name,
            row.oldest_age_seconds,
        );
    }

    for ((queue_name, state), (task_count, oldest_age_seconds)) in queue_totals {
        centaur_telemetry::set_workflow_queue_tasks(&queue_name, &state, task_count as f64);
        centaur_telemetry::set_workflow_queue_oldest_task_age_seconds(
            &queue_name,
            &state,
            oldest_age_seconds,
        );
    }

    Ok(())
}

async fn fetch_workflow_queue_metric_rows(
    client: &Client,
    queue_name: &str,
) -> Result<Vec<WorkflowQueueMetricRow>, WorkflowRuntimeError> {
    let (task_table, _) = absurd_queue_tables(queue_name)?;
    let rows = sqlx::query(&format!(
        r#"
        select
            coalesce(nullif(t.params->>'workflow_name', ''), 'unknown') as workflow_name,
            t.state,
            count(*)::bigint as task_count,
            coalesce(
                extract(
                    epoch from now() - min(
                        case
                            when t.state = 'running'
                                then coalesce(t.first_started_at, t.enqueue_at)
                            else t.enqueue_at
                        end
                    )
                ),
                0
            )::float8 as oldest_age_seconds
        from {task_table} t
        where t.task_name = $1
          and t.state not in {ABSURD_TERMINAL_TASK_STATES}
        group by 1, 2
        "#,
    ))
    .bind(WORKFLOW_TASK)
    .fetch_all(client.pool())
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(WorkflowQueueMetricRow {
                queue_name: queue_name.to_owned(),
                workflow_name: row.try_get("workflow_name")?,
                state: row.try_get("state")?,
                task_count: row.try_get("task_count")?,
                oldest_age_seconds: row.try_get("oldest_age_seconds")?,
            })
        })
        .collect()
}

impl RemovedWorkflowReaper {
    fn from_env() -> Self {
        let threshold = env::var(WORKFLOW_REAP_REMOVED_AFTER_TICKS_ENV)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_WORKFLOW_REAP_REMOVED_AFTER_TICKS);
        Self {
            threshold,
            workflow_miss_counts: BTreeMap::new(),
            schedule_miss_counts: BTreeMap::new(),
        }
    }

    async fn reap(
        &mut self,
        workflow_clients: &WorkflowQueueClients,
        schedule_client: &Client,
        metadata: &PythonWorkflowMetadata,
        schedules: &BTreeMap<String, RegisteredWorkflowSchedule>,
    ) -> Result<(), WorkflowRuntimeError> {
        if self.threshold == 0 {
            return Ok(());
        }
        // An empty discovery result almost certainly means WORKFLOW_DIRS is
        // missing or broken; never treat that as "every workflow was deleted".
        if metadata.workflow_names.is_empty() {
            warn!("workflow discovery returned no workflows; skipping removed-workflow reaping");
            return Ok(());
        }

        let mut active_runs = Vec::new();
        for (queue_name, client) in [
            (WORKFLOW_QUEUE, &workflow_clients.standard),
            (WORKFLOW_SLACK_LIVE_QUEUE, &workflow_clients.slack_live),
            (WORKFLOW_ETL_QUEUE, &workflow_clients.etl),
            (WORKFLOW_ETL_BACKFILL_QUEUE, &workflow_clients.etl_backfill),
        ] {
            for (task_id, name) in
                fetch_active_named_tasks(client, queue_name, WORKFLOW_TASK, "workflow_name").await?
            {
                active_runs.push((queue_name, task_id, name));
            }
        }
        let run_keyed = active_runs
            .iter()
            .map(|(queue, task_id, name)| (format!("{queue}:{task_id}"), name.clone()))
            .collect::<Vec<_>>();
        let stale_runs = select_stale_cancellations(
            &run_keyed,
            &metadata.workflow_names,
            &mut self.workflow_miss_counts,
            self.threshold,
        );
        for key in &stale_runs {
            let Some((queue_name, task_id)) = key.split_once(':') else {
                continue;
            };
            let client = match queue_name {
                WORKFLOW_SLACK_LIVE_QUEUE => &workflow_clients.slack_live,
                WORKFLOW_ETL_QUEUE => &workflow_clients.etl,
                WORKFLOW_ETL_BACKFILL_QUEUE => &workflow_clients.etl_backfill,
                _ => &workflow_clients.standard,
            };
            if let Err(error) = client.cancel_task(task_id, Some(queue_name)).await {
                warn!(%error, queue_name, task_id, "failed to cancel run of removed workflow");
            } else {
                info!(queue_name, task_id, "cancelled run of removed workflow");
            }
        }

        let known_schedule_ids = schedules.keys().cloned().collect::<BTreeSet<_>>();
        let active_ticks = fetch_active_named_tasks(
            schedule_client,
            WORKFLOW_SCHEDULE_QUEUE,
            WORKFLOW_SCHEDULE_TASK,
            "schedule_id",
        )
        .await?;
        let stale_ticks = select_stale_cancellations(
            &active_ticks,
            &known_schedule_ids,
            &mut self.schedule_miss_counts,
            self.threshold,
        );
        for task_id in &stale_ticks {
            if let Err(error) = schedule_client
                .cancel_task(task_id, Some(WORKFLOW_SCHEDULE_QUEUE))
                .await
            {
                warn!(%error, task_id, "failed to cancel schedule tick of removed workflow");
            } else {
                info!(task_id, "cancelled schedule tick of removed workflow");
            }
        }

        if !stale_runs.is_empty() || !stale_ticks.is_empty() {
            info!(
                cancelled_runs = stale_runs.len(),
                cancelled_schedule_ticks = stale_ticks.len(),
                "reaped tasks referencing removed workflows"
            );
        }
        Ok(())
    }
}

/// Returns `(task_id, name)` for every non-terminal task in the queue, where
/// `name` is extracted from the task params (`workflow_name` for runs,
/// `schedule_id` for schedule ticks). Tasks without the field are skipped.
async fn fetch_active_named_tasks(
    client: &Client,
    queue_name: &str,
    task_name: &str,
    params_name_field: &str,
) -> Result<Vec<(String, String)>, WorkflowRuntimeError> {
    let (task_table, _) = absurd_queue_tables(queue_name)?;
    let rows = sqlx::query(&format!(
        r#"
        select t.task_id::text as task_id, t.params->>'{params_name_field}' as name
        from {task_table} t
        where t.task_name = $1
          and t.state not in {ABSURD_TERMINAL_TASK_STATES}
        "#,
    ))
    .bind(task_name)
    .fetch_all(client.pool())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let task_id: String = row.try_get("task_id").ok()?;
            let name: Option<String> = row.try_get("name").ok()?;
            Some((task_id, name?))
        })
        .collect())
}

/// Counts consecutive reconcile passes in which each referenced name was
/// absent from `known_names`, and returns the task ids whose name has been
/// missing for at least `threshold` passes. Counters for names that are known
/// again, or no longer referenced by any active task, are dropped.
fn select_stale_cancellations(
    active_tasks: &[(String, String)],
    known_names: &BTreeSet<String>,
    miss_counts: &mut BTreeMap<String, u32>,
    threshold: u32,
) -> Vec<String> {
    let mut active_by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (task_id, name) in active_tasks {
        active_by_name
            .entry(name.as_str())
            .or_default()
            .push(task_id.as_str());
    }
    let mut cancellations = Vec::new();
    let mut next_counts = BTreeMap::new();
    for (name, task_ids) in active_by_name {
        if known_names.contains(name) {
            continue;
        }
        let count = miss_counts
            .get(name)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if count >= threshold {
            cancellations.extend(task_ids.iter().map(|id| (*id).to_owned()));
        }
        next_counts.insert(name.to_owned(), count);
    }
    *miss_counts = next_counts;
    cancellations
}

async fn run_schedule_tick(
    input: ScheduleTickInput,
    ctx: TaskContext,
    schedule_client: Client,
    workflow_clients: WorkflowQueueClients,
    schedules: Arc<RwLock<BTreeMap<String, RegisteredWorkflowSchedule>>>,
) -> Result<Value, absurd::Error> {
    ctx.sleep_until("schedule_tick", input.scheduled_at).await?;
    let schedule = match schedules
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&input.schedule_id)
        .cloned()
    {
        Some(schedule) if schedule.enabled => schedule,
        Some(schedule) => {
            info!(
                schedule_id = %schedule.schedule_id,
                "skipping disabled workflow schedule tick"
            );
            return Ok(json!({
                "schedule_id": schedule.schedule_id,
                "scheduled_at": input.scheduled_at.to_rfc3339(),
                "skipped": true,
                "reason": "disabled",
            }));
        }
        None => {
            info!(
                schedule_id = %input.schedule_id,
                "skipping removed workflow schedule tick"
            );
            return Ok(json!({
                "schedule_id": input.schedule_id,
                "scheduled_at": input.scheduled_at.to_rfc3339(),
                "skipped": true,
                "reason": "removed",
            }));
        }
    };
    let fire_key = format!(
        "schedule:{}:{}",
        schedule.schedule_id,
        input.scheduled_at.to_rfc3339()
    );
    let target_client = match workflow_queue_class(&schedule.workflow_name) {
        WorkflowQueueClass::Standard => &workflow_clients.standard,
        WorkflowQueueClass::SlackLive => &workflow_clients.slack_live,
        WorkflowQueueClass::Etl => &workflow_clients.etl,
        WorkflowQueueClass::EtlBackfill => &workflow_clients.etl_backfill,
    };
    let workflow_spawn = target_client
        .spawn(
            WORKFLOW_TASK,
            WorkflowTaskInput {
                workflow_name: schedule.workflow_name.clone(),
                input: schedule.input.clone(),
                harness_type: HarnessType::Codex,
            },
            SpawnOptions {
                idempotency_key: Some(fire_key.clone()),
                ..SpawnOptions::default()
            },
        )
        .await?;
    let next_run_at = next_schedule_time_after_tick(&schedule, input.scheduled_at, Utc::now())
        .map_err(absurd_error)?;
    spawn_schedule_tick(&schedule_client, &schedule, next_run_at)
        .await
        .map_err(absurd_error)?;
    Ok(json!({
        "schedule_id": schedule.schedule_id,
        "workflow_name": schedule.workflow_name,
        "scheduled_at": input.scheduled_at.to_rfc3339(),
        "fire_key": fire_key,
        "workflow_task_id": workflow_spawn.task_id,
        "workflow_run_id": workflow_spawn.run_id,
        "workflow_created": workflow_spawn.created,
        "next_run_at": next_run_at.to_rfc3339(),
    }))
}

async fn ensure_schedule_tick(
    client: &Client,
    schedule: &RegisteredWorkflowSchedule,
    scheduled_at: DateTime<Utc>,
) -> Result<bool, WorkflowRuntimeError> {
    if has_active_schedule_tick(client, &schedule.schedule_id).await? {
        return Ok(false);
    }
    spawn_schedule_tick(client, schedule, scheduled_at).await?;
    Ok(true)
}

async fn has_active_schedule_tick(
    client: &Client,
    schedule_id: &str,
) -> Result<bool, WorkflowRuntimeError> {
    let (task_table, _) = absurd_queue_tables(WORKFLOW_SCHEDULE_QUEUE)?;
    let row = sqlx::query(&format!(
        r#"
        select 1
        from {task_table} t
        where t.task_name = $1
          and t.params->>'schedule_id' = $2
          and t.state not in {ABSURD_TERMINAL_TASK_STATES}
        limit 1
        "#,
    ))
    .bind(WORKFLOW_SCHEDULE_TASK)
    .bind(schedule_id)
    .fetch_optional(client.pool())
    .await?;
    Ok(row.is_some())
}

async fn spawn_schedule_tick(
    client: &Client,
    schedule: &RegisteredWorkflowSchedule,
    scheduled_at: DateTime<Utc>,
) -> Result<(), WorkflowRuntimeError> {
    client
        .spawn(
            WORKFLOW_SCHEDULE_TASK,
            ScheduleTickInput {
                schedule_id: schedule.schedule_id.clone(),
                scheduled_at,
            },
            SpawnOptions {
                idempotency_key: Some(format!(
                    "schedule-tick:{}:{}",
                    schedule.schedule_id,
                    scheduled_at.to_rfc3339()
                )),
                max_attempts: Some(10),
                retry_strategy: Some(RetryStrategy {
                    kind: RetryKind::Fixed,
                    base_seconds: Some(30.0),
                    factor: None,
                    max_seconds: None,
                }),
                ..SpawnOptions::default()
            },
        )
        .await?;
    Ok(())
}

fn next_schedule_time_after_tick(
    schedule: &RegisteredWorkflowSchedule,
    scheduled_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, WorkflowRuntimeError> {
    match &schedule.kind {
        WorkflowScheduleKind::Interval { interval_seconds } => {
            if *interval_seconds == 0 {
                return Err(WorkflowRuntimeError::BadRequest(format!(
                    "invalid interval for schedule {:?}: interval_seconds must be > 0",
                    schedule.schedule_id
                )));
            }
            let interval = chrono::Duration::from_std(Duration::from_secs(*interval_seconds))
                .map_err(|error| {
                    WorkflowRuntimeError::BadRequest(format!(
                        "invalid interval for schedule {:?}: {error}",
                        schedule.schedule_id
                    ))
                })?;
            let mut next = scheduled_at + interval;
            if next <= now {
                let elapsed =
                    u64::try_from(now.signed_duration_since(scheduled_at).num_seconds().max(0))
                        .unwrap_or(0);
                let missed_intervals = (elapsed / *interval_seconds).saturating_add(1);
                let skipped = chrono::Duration::from_std(Duration::from_secs(
                    interval_seconds.saturating_mul(missed_intervals),
                ))
                .map_err(|error| {
                    WorkflowRuntimeError::BadRequest(format!(
                        "invalid interval for schedule {:?}: {error}",
                        schedule.schedule_id
                    ))
                })?;
                next = scheduled_at + skipped;
            }
            Ok(next)
        }
        WorkflowScheduleKind::Cron { .. } => next_schedule_time(schedule, now),
    }
}

fn next_schedule_time(
    schedule: &RegisteredWorkflowSchedule,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, WorkflowRuntimeError> {
    match &schedule.kind {
        WorkflowScheduleKind::Interval { interval_seconds } => Ok(after
            + chrono::Duration::from_std(Duration::from_secs(*interval_seconds)).map_err(
                |error| {
                    WorkflowRuntimeError::BadRequest(format!(
                        "invalid interval for schedule {:?}: {error}",
                        schedule.schedule_id
                    ))
                },
            )?),
        WorkflowScheduleKind::Cron { cron } => {
            let timezone = schedule.timezone.parse::<Tz>().map_err(|error| {
                WorkflowRuntimeError::BadRequest(format!(
                    "invalid timezone {:?} for schedule {:?}: {error}",
                    schedule.timezone, schedule.schedule_id
                ))
            })?;
            let normalized_cron = normalize_cron_expression(cron);
            let parsed = Schedule::from_str(&normalized_cron).map_err(|error| {
                WorkflowRuntimeError::BadRequest(format!(
                    "invalid cron {:?} for schedule {:?}: {error}",
                    cron, schedule.schedule_id
                ))
            })?;
            parsed
                .after(&after.with_timezone(&timezone))
                .next()
                .map(|next| next.with_timezone(&Utc))
                .ok_or_else(|| {
                    WorkflowRuntimeError::BadRequest(format!(
                        "cron {:?} for schedule {:?} produced no next run",
                        cron, schedule.schedule_id
                    ))
                })
        }
    }
}

/// Prepends a seconds field so five-field crontab-style expressions parse with the
/// `cron` crate. Note the crate's day-of-week numbering is Quartz-style (1 = Sunday,
/// 7 = Saturday; 0 rejected), NOT Unix crontab — schedules should use day names
/// (`MON-FRI`) to avoid firing on the wrong days.
fn normalize_cron_expression(expr: &str) -> String {
    let fields = expr.split_whitespace().collect::<Vec<_>>();
    if fields.len() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_owned()
    }
}

async fn run_centaur_workflow(
    input: WorkflowTaskInput,
    ctx: TaskContext,
    session_runtime: SessionRuntime,
    workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
    workflow_clients: WorkflowQueueClients,
) -> absurd::Result<WorkflowResult> {
    let mut cleanup_guard =
        WorkflowSandboxCleanupGuard::new(session_runtime.clone(), ctx.run_id().to_owned());
    let result = run_centaur_workflow_inner(
        input,
        ctx,
        session_runtime,
        workflow_host_sandbox,
        workflow_clients,
    )
    .await;
    if let Some(reason) = workflow_cleanup_reason(&result) {
        cleanup_guard.cleanup(reason).await;
    } else {
        cleanup_guard.disarm();
    }
    result
}

fn workflow_cleanup_reason(result: &absurd::Result<WorkflowResult>) -> Option<&'static str> {
    match result {
        Ok(_) => Some("workflow_completed"),
        Err(absurd::Error::Suspend) => None,
        Err(absurd::Error::Cancelled) => Some("workflow_cancelled"),
        Err(_) => Some("workflow_failed"),
    }
}

async fn run_centaur_workflow_inner(
    input: WorkflowTaskInput,
    ctx: TaskContext,
    session_runtime: SessionRuntime,
    workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
    workflow_clients: WorkflowQueueClients,
) -> absurd::Result<WorkflowResult> {
    let _heartbeat_guard = start_workflow_task_heartbeat(ctx.clone())
        .await
        .map_err(absurd_error)?;
    WorkflowEnablement::from_env()
        .and_then(|enablement| enablement.ensure_enabled(&input.workflow_name))
        .map_err(absurd_error)?;
    match input.workflow_name.as_str() {
        "echo" => {
            let output = ctx
                .step("echo", || async {
                    Ok(json!({
                        "echo": input.input,
                        "task_id": ctx.task_id(),
                        "run_id": ctx.run_id(),
                    }))
                })
                .await?;
            Ok(WorkflowResult {
                workflow_name: input.workflow_name,
                run_id: ctx.run_id().to_owned(),
                task_id: ctx.task_id().to_owned(),
                steps: vec!["echo".to_owned()],
                output,
            })
        }
        "sleep_echo" => {
            let sleep_ms = input
                .input
                .get("sleep_ms")
                .and_then(Value::as_u64)
                .unwrap_or(250);
            ctx.sleep_for("sleep", Duration::from_millis(sleep_ms))
                .await?;
            let output = ctx
                .step("echo_after_sleep", || async { Ok(input.input.clone()) })
                .await?;
            Ok(WorkflowResult {
                workflow_name: input.workflow_name,
                run_id: ctx.run_id().to_owned(),
                task_id: ctx.task_id().to_owned(),
                steps: vec!["sleep".to_owned(), "echo_after_sleep".to_owned()],
                output,
            })
        }
        "agent_turn" => {
            let prompt = input
                .input
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("Reply with exactly PONG and nothing else.")
                .to_owned();
            let idle_timeout_ms = input
                .input
                .get("idle_timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_AGENT_IDLE_TIMEOUT_MS);
            let max_duration_ms = input
                .input
                .get("max_duration_ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_AGENT_MAX_DURATION_MS);
            let agent = ctx
                .step("agent_turn", || {
                    let session_runtime = session_runtime.clone();
                    let harness_type = input.harness_type.clone();
                    let thread_key =
                        format!("wf:{}:agent:agent_turn", ctx.task_id().replace('-', ""));
                    let task_id = ctx.task_id().to_owned();
                    let run_id = ctx.run_id().to_owned();
                    async move {
                        let client_message_id = format!("absurd-workflow:{task_id}:native:user");
                        let metadata = json!({
                            "source": "absurd_workflow",
                            "workflow_name": "agent_turn",
                            "workflow_task_id": task_id,
                            "workflow_run_id": run_id,
                        });
                        run_agent_session_turn(
                            session_runtime,
                            AgentTurnRequest {
                                thread_key,
                                harness_type,
                                persona_id: None,
                                parts: vec![json!({"type": "text", "text": prompt})],
                                client_message_id: client_message_id.clone(),
                                session_metadata: metadata.clone(),
                                message_metadata: metadata.clone(),
                                execution_metadata: metadata,
                                execution_idempotency_key: format!(
                                    "absurd-workflow-agent-turn:{client_message_id}"
                                ),
                                workflow_owned_thread: true,
                                idle_timeout_ms,
                                max_duration_ms,
                                model: None,
                                provider: None,
                                reasoning: None,
                            },
                        )
                        .await
                        .map_err(absurd_error)
                    }
                })
                .await?;
            Ok(WorkflowResult {
                workflow_name: input.workflow_name,
                run_id: ctx.run_id().to_owned(),
                task_id: ctx.task_id().to_owned(),
                steps: vec!["agent_turn".to_owned()],
                output: serde_json::to_value(agent).map_err(absurd::Error::Json)?,
            })
        }
        "tool_and_slack" => {
            let slack_channel = input
                .input
                .get("slack_channel")
                .and_then(Value::as_str)
                .unwrap_or("#centaur-ai-zygis")
                .to_owned();
            let note = input
                .input
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or("Absurd workflow POC")
                .to_owned();
            let tool = ctx
                .step("tool:time.now", || async { Ok(run_time_now_tool()) })
                .await?;
            let slack = ctx
                .step("slack:post_result", || {
                    let slack_channel = slack_channel.clone();
                    let client_msg_id = ctx.task_id().to_owned();
                    let note = note.clone();
                    let tool = tool.clone();
                    async move {
                        post_tool_result_to_slack(&slack_channel, &client_msg_id, &note, &tool)
                            .await
                            .map_err(absurd_error)
                    }
                })
                .await?;
            Ok(WorkflowResult {
                workflow_name: input.workflow_name,
                run_id: ctx.run_id().to_owned(),
                task_id: ctx.task_id().to_owned(),
                steps: vec!["tool:time.now".to_owned(), "slack:post_result".to_owned()],
                output: json!({
                    "tool": tool,
                    "slack": slack,
                }),
            })
        }
        _ => {
            let workflow_name = input.workflow_name.clone();
            let output = run_python_workflow_host(
                input,
                ctx.clone(),
                session_runtime,
                workflow_host_sandbox,
                workflow_clients,
            )
            .await
            .map_err(absurd_error)?;
            Ok(WorkflowResult {
                workflow_name,
                run_id: ctx.run_id().to_owned(),
                task_id: ctx.task_id().to_owned(),
                steps: vec!["python_host".to_owned()],
                output,
            })
        }
    }
}

struct WorkflowSandboxCleanupGuard {
    session_runtime: Option<SessionRuntime>,
    workflow_run_id: String,
}

impl WorkflowSandboxCleanupGuard {
    fn new(session_runtime: SessionRuntime, workflow_run_id: String) -> Self {
        Self {
            session_runtime: Some(session_runtime),
            workflow_run_id,
        }
    }

    fn disarm(&mut self) {
        self.session_runtime = None;
    }

    async fn cleanup(&mut self, reason: &'static str) {
        let Some(session_runtime) = self.session_runtime.as_ref().cloned() else {
            return;
        };
        if let Err(error) = session_runtime
            .stop_workflow_owned_sandboxes(&self.workflow_run_id, reason)
            .await
        {
            warn!(
                workflow_run_id = %self.workflow_run_id,
                reason,
                %error,
                "failed to clean up workflow-owned sandboxes"
            );
            return;
        }
        self.session_runtime = None;
    }
}

impl Drop for WorkflowSandboxCleanupGuard {
    fn drop(&mut self) {
        let Some(session_runtime) = self.session_runtime.take() else {
            return;
        };
        let workflow_run_id = self.workflow_run_id.clone();
        tokio::spawn(async move {
            if let Err(error) = session_runtime
                .stop_workflow_owned_sandboxes(&workflow_run_id, "workflow_cancelled_or_dropped")
                .await
            {
                warn!(
                    workflow_run_id,
                    %error,
                    "failed to clean up dropped workflow-owned sandboxes"
                );
            }
        });
    }
}

async fn run_python_workflow_host(
    input: WorkflowTaskInput,
    ctx: TaskContext,
    session_runtime: SessionRuntime,
    workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
    workflow_clients: WorkflowQueueClients,
) -> Result<Value, WorkflowRuntimeError> {
    if let Some(sandbox) = workflow_host_sandbox {
        return run_python_workflow_host_in_sandbox(
            input,
            ctx,
            session_runtime,
            sandbox,
            workflow_clients,
        )
        .await;
    }
    run_python_workflow_host_local(input, ctx, session_runtime, workflow_clients).await
}

async fn start_workflow_task_heartbeat(
    ctx: TaskContext,
) -> Result<WorkflowTaskHeartbeatGuard, WorkflowRuntimeError> {
    ctx.heartbeat(Some(WORKFLOW_HOST_CLAIM_EXTENSION)).await?;
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(WORKFLOW_HOST_HEARTBEAT_INTERVAL).await;
            if let Err(error) = ctx.heartbeat(Some(WORKFLOW_HOST_CLAIM_EXTENSION)).await {
                warn!(%error, "failed to extend workflow task claim");
            }
        }
    });
    Ok(WorkflowTaskHeartbeatGuard { task })
}

async fn run_python_workflow_host_local(
    input: WorkflowTaskInput,
    ctx: TaskContext,
    session_runtime: SessionRuntime,
    workflow_clients: WorkflowQueueClients,
) -> Result<Value, WorkflowRuntimeError> {
    let host_path = python_workflow_host_path();
    let mut command = Command::new(
        env::var(PYTHON_HOST_INTERPRETER_ENV).unwrap_or_else(|_| "python3".to_owned()),
    );
    command
        .arg(&host_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if env::var_os("WORKFLOW_DIRS").is_none() {
        command.env("WORKFLOW_DIRS", default_workflow_dirs());
    }

    let mut child = command.spawn().map_err(|error| {
        WorkflowRuntimeError::Internal(format!(
            "failed to spawn Python workflow host {}: {error}",
            host_path.display()
        ))
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| WorkflowRuntimeError::Internal("workflow host stdin missing".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkflowRuntimeError::Internal("workflow host stdout missing".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkflowRuntimeError::Internal("workflow host stderr missing".to_owned()))?;
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut collected = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            collected.push(line);
        }
        collected.join("\n")
    });

    write_host_message(
        &mut stdin,
        &json!({
            "type": "workflow.start",
            "run_id": ctx.run_id(),
            "task_id": ctx.task_id(),
            "workflow_name": input.workflow_name,
            "input": input.input,
        }),
    )
    .await?;

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)?;
        match message.get("type").and_then(Value::as_str) {
            Some("workflow.result") => {
                drop(stdin);
                let _ = child.wait().await;
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            Some("workflow.error") | Some("host.error") => {
                let stderr = stderr_task.await.unwrap_or_default();
                return Err(WorkflowRuntimeError::Internal(format!(
                    "Python workflow host error: {}{}{}",
                    message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error"),
                    if stderr.is_empty() { "" } else { "\nstderr:\n" },
                    stderr,
                )));
            }
            Some("ctx.log") => {
                let workflow_log = message
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("workflow_log");
                info!(
                    workflow_log = %workflow_log,
                    fields = %message.get("fields").cloned().unwrap_or_else(|| json!({})),
                    task_id = ctx.task_id(),
                    run_id = ctx.run_id(),
                    "python workflow log"
                );
            }
            Some("ctx.metric") => {
                record_python_workflow_metric(&message);
            }
            Some(message_type) if message_type.starts_with("ctx.") => {
                let response = match handle_python_context_request(
                    &message,
                    &ctx,
                    &session_runtime,
                    &input,
                    &workflow_clients,
                )
                .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        drop(stdin);
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        return Err(error);
                    }
                };
                write_host_message(&mut stdin, &response).await?;
            }
            other => {
                return Err(WorkflowRuntimeError::Internal(format!(
                    "unexpected Python workflow host message type {other:?}: {message}"
                )));
            }
        }
    }

    let status = child.wait().await?;
    let stderr = stderr_task.await.unwrap_or_default();
    Err(WorkflowRuntimeError::Internal(format!(
        "Python workflow host exited before workflow.result: status={status}, stderr={stderr}"
    )))
}

async fn run_python_workflow_host_in_sandbox(
    input: WorkflowTaskInput,
    ctx: TaskContext,
    session_runtime: SessionRuntime,
    sandbox: WorkflowHostSandboxRuntime,
    workflow_clients: WorkflowQueueClients,
) -> Result<Value, WorkflowRuntimeError> {
    let mut spec = sandbox.spec.clone();
    spec = spec
        .env("WORKFLOW_RUN_ID", ctx.run_id())
        .env("WORKFLOW_TASK_ID", ctx.task_id())
        .env("WORKFLOW_NAME", input.workflow_name.clone());
    if env::var_os("WORKFLOW_DIRS").is_none() && !sandbox_spec_has_env(&spec, "WORKFLOW_DIRS") {
        spec = spec.env("WORKFLOW_DIRS", default_workflow_dirs());
    }
    if let Ok(database_url) = env::var("DATABASE_URL")
        && !sandbox_spec_has_env(&spec, "DATABASE_URL")
    {
        spec = spec.env("DATABASE_URL", database_url);
    }
    let (sandbox_id, io) = sandbox.runtime.create_running_io(spec).await?;
    let mut stdin = io.stdin;
    let stderr_task = tokio::spawn(async move {
        let _guard = io.guard;
        let mut lines = BufReader::new(io.stderr).lines();
        let mut collected = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            collected.push(line);
        }
        collected.join("\n")
    });
    let result = run_python_workflow_host_protocol(
        input,
        ctx,
        session_runtime,
        workflow_clients,
        &mut stdin,
        io.stdout,
        stderr_task,
    )
    .await;
    drop(stdin);
    if let Err(error) = sandbox.runtime.stop_sandbox(&sandbox_id).await {
        warn!(sandbox_id = %sandbox_id.as_str(), %error, "failed to stop workflow host sandbox");
    }
    result
}

fn sandbox_spec_has_env(spec: &SandboxSpec, name: &str) -> bool {
    spec.env.iter().any(|entry| entry.name == name)
}

async fn run_python_workflow_host_protocol<W, R>(
    input: WorkflowTaskInput,
    ctx: TaskContext,
    session_runtime: SessionRuntime,
    workflow_clients: WorkflowQueueClients,
    stdin: &mut W,
    stdout: R,
    stderr_task: JoinHandle<String>,
) -> Result<Value, WorkflowRuntimeError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    write_host_message(
        stdin,
        &json!({
            "type": "workflow.start",
            "run_id": ctx.run_id(),
            "task_id": ctx.task_id(),
            "workflow_name": input.workflow_name,
            "input": input.input,
        }),
    )
    .await?;

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)?;
        match message.get("type").and_then(Value::as_str) {
            Some("workflow.result") => {
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            Some("workflow.error") | Some("host.error") => {
                let stderr = stderr_task.await.unwrap_or_default();
                return Err(WorkflowRuntimeError::Internal(format!(
                    "Python workflow host error: {}{}{}",
                    message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error"),
                    if stderr.is_empty() { "" } else { "\nstderr:\n" },
                    stderr,
                )));
            }
            Some("ctx.log") => {
                let workflow_log = message
                    .get("message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("workflow_log");
                info!(
                    workflow_log = %workflow_log,
                    fields = %message.get("fields").cloned().unwrap_or_else(|| json!({})),
                    task_id = ctx.task_id(),
                    run_id = ctx.run_id(),
                    "python workflow log"
                );
            }
            Some("ctx.metric") => {
                record_python_workflow_metric(&message);
            }
            Some(message_type) if message_type.starts_with("ctx.") => {
                let response = handle_python_context_request(
                    &message,
                    &ctx,
                    &session_runtime,
                    &input,
                    &workflow_clients,
                )
                .await?;
                write_host_message(stdin, &response).await?;
            }
            other => {
                return Err(WorkflowRuntimeError::Internal(format!(
                    "unexpected Python workflow host message type {other:?}: {message}"
                )));
            }
        }
    }

    let stderr = stderr_task.await.unwrap_or_default();
    Err(WorkflowRuntimeError::Internal(format!(
        "Python workflow host exited before workflow.result: stderr={stderr}"
    )))
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PythonWorkflowMetricKind {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Debug, Clone, PartialEq)]
struct PythonWorkflowMetric {
    kind: PythonWorkflowMetricKind,
    name: String,
    value: f64,
    labels: Vec<(String, String)>,
}

fn record_python_workflow_metric(message: &Value) {
    let metric = match parse_python_workflow_metric(message) {
        Ok(metric) => metric,
        Err(error) => {
            warn!(%error, message = %message, "ignored invalid Python workflow metric");
            return;
        }
    };

    match metric.kind {
        PythonWorkflowMetricKind::Counter => {
            if metric.value > 0.0 && metric.value.fract() == 0.0 {
                centaur_telemetry::record_workflow_counter(
                    &metric.name,
                    &metric.labels,
                    metric.value as u64,
                );
            } else {
                warn!(
                    metric = %metric.name,
                    value = metric.value,
                    "ignored invalid Python workflow counter value"
                );
            }
        }
        PythonWorkflowMetricKind::Gauge => {
            centaur_telemetry::set_workflow_gauge(&metric.name, &metric.labels, metric.value);
        }
        PythonWorkflowMetricKind::Histogram => {
            centaur_telemetry::record_workflow_histogram(
                &metric.name,
                &metric.labels,
                metric.value,
            );
        }
    }
}

fn parse_python_workflow_metric(message: &Value) -> Result<PythonWorkflowMetric, String> {
    let kind = match message.get("kind").and_then(Value::as_str) {
        Some("counter") => PythonWorkflowMetricKind::Counter,
        Some("gauge") => PythonWorkflowMetricKind::Gauge,
        Some("histogram") => PythonWorkflowMetricKind::Histogram,
        other => return Err(format!("unsupported metric kind {other:?}")),
    };
    let name = message
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| is_valid_prometheus_name(name))
        .ok_or_else(|| "metric name missing or invalid".to_owned())?
        .to_owned();
    let value = message
        .get("value")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| "metric value missing or invalid".to_owned())?;
    let labels = message
        .get("labels")
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .filter(|(key, _)| is_valid_prometheus_name(key))
                .map(|(key, value)| {
                    (
                        key.to_owned(),
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PythonWorkflowMetric {
        kind,
        name,
        value,
        labels,
    })
}

fn is_valid_prometheus_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

async fn handle_python_context_request(
    message: &Value,
    ctx: &TaskContext,
    session_runtime: &SessionRuntime,
    input: &WorkflowTaskInput,
    workflow_clients: &WorkflowQueueClients,
) -> Result<Value, WorkflowRuntimeError> {
    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let result = match message.get("type").and_then(Value::as_str) {
        Some("ctx.step.get") => {
            let step = message
                .get("step")
                .and_then(Value::as_str)
                .unwrap_or("step");
            match ctx.begin_step::<Value>(step).await {
                Ok(handle) if handle.done => Ok(json!({
                    "done": true,
                    "checkpoint_name": handle.checkpoint_name,
                    "value": handle.state.unwrap_or(Value::Null),
                })),
                Ok(handle) => Ok(json!({
                    "done": false,
                    "checkpoint_name": handle.checkpoint_name,
                })),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.step.put") => {
            let checkpoint_name = message
                .get("checkpoint_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let value = message.get("value").cloned().unwrap_or(Value::Null);
            if checkpoint_name.is_empty() {
                Err("ctx.step.put missing checkpoint_name".to_owned())
            } else {
                let handle = StepHandle::<Value> {
                    name: checkpoint_name.clone(),
                    checkpoint_name,
                    done: false,
                    state: None,
                };
                match ctx.complete_step(handle, value).await {
                    Ok(value) => Ok(value),
                    Err(error) => Err(error.to_string()),
                }
            }
        }
        Some("ctx.sleep") => {
            let step = message
                .get("step")
                .and_then(Value::as_str)
                .unwrap_or("sleep");
            match parse_python_duration_seconds(message) {
                Ok(duration) => match ctx.sleep_for(step, duration).await {
                    Ok(()) => Ok(json!({"slept": true})),
                    Err(absurd::Error::Suspend) => return Err(WorkflowRuntimeError::Suspend),
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error),
            }
        }
        Some("ctx.sleep_until") => {
            let step = message
                .get("step")
                .and_then(Value::as_str)
                .unwrap_or("sleep_until");
            match parse_python_wake_at(message) {
                Ok(wake_at) => match ctx.sleep_until(step, wake_at).await {
                    Ok(()) => Ok(json!({"slept": true})),
                    Err(absurd::Error::Suspend) => return Err(WorkflowRuntimeError::Suspend),
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error),
            }
        }
        Some("ctx.agent_turn") => {
            let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
            match run_python_agent_turn(session_runtime.clone(), ctx, input, args, &request_id)
                .await
            {
                Ok(value) => Ok(value),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.workflow.start") => {
            match start_python_child_workflow(message, input, workflow_clients).await {
                Ok(value) => Ok(value),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.call_tool") => match call_python_workflow_tool(message).await {
            Ok(value) => Ok(value),
            Err(error) => Err(error.to_string()),
        },
        Some("ctx.post_to_slack") => {
            match post_python_slack_message(message, ctx, &request_id).await {
                Ok(value) => Ok(value),
                Err(error) => Err(error.to_string()),
            }
        }
        other => Err(format!("unsupported context request type {other:?}")),
    };
    Ok(match result {
        Ok(value) => json!({
            "type": "ctx.response",
            "request_id": request_id,
            "ok": true,
            "value": value,
        }),
        Err(error) => json!({
            "type": "ctx.response",
            "request_id": request_id,
            "ok": false,
            "error": error,
        }),
    })
}

async fn start_python_child_workflow(
    message: &Value,
    parent: &WorkflowTaskInput,
    workflow_clients: &WorkflowQueueClients,
) -> Result<Value, WorkflowRuntimeError> {
    let workflow_name = message
        .get("workflow_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest(
                "ctx.workflow.start requires a non-empty workflow_name".to_owned(),
            )
        })?;
    WorkflowEnablement::from_env()?.ensure_enabled(workflow_name)?;
    let child_input = message.get("input").cloned().unwrap_or_else(|| json!({}));
    if !child_input.is_object() {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.workflow.start input must be an object".to_owned(),
        ));
    }
    let idempotency_key = message
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(ToOwned::to_owned);
    let target_client = match workflow_queue_class(workflow_name) {
        WorkflowQueueClass::Standard => &workflow_clients.standard,
        WorkflowQueueClass::SlackLive => &workflow_clients.slack_live,
        WorkflowQueueClass::Etl => &workflow_clients.etl,
        WorkflowQueueClass::EtlBackfill => &workflow_clients.etl_backfill,
    };
    let spawn = target_client
        .spawn(
            WORKFLOW_TASK,
            WorkflowTaskInput {
                workflow_name: workflow_name.to_owned(),
                input: child_input,
                harness_type: parent.harness_type.clone(),
            },
            SpawnOptions {
                idempotency_key,
                ..SpawnOptions::default()
            },
        )
        .await?;
    Ok(json!({
        "workflow_name": workflow_name,
        "task_id": spawn.task_id,
        "run_id": spawn.run_id,
        "created": spawn.created,
    }))
}

fn parse_python_duration_seconds(message: &Value) -> Result<Duration, String> {
    let seconds = message
        .get("duration_seconds")
        .and_then(Value::as_f64)
        .ok_or_else(|| "ctx.sleep missing numeric duration_seconds".to_owned())?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("ctx.sleep duration_seconds must be a finite non-negative number".to_owned());
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn parse_python_wake_at(message: &Value) -> Result<DateTime<Utc>, String> {
    let raw = message
        .get("wake_at")
        .and_then(Value::as_str)
        .ok_or_else(|| "ctx.sleep_until missing wake_at".to_owned())?;
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| format!("ctx.sleep_until invalid wake_at: {error}"))
}

async fn run_python_agent_turn(
    session_runtime: SessionRuntime,
    ctx: &TaskContext,
    input: &WorkflowTaskInput,
    args: Value,
    request_id: &str,
) -> Result<Value, WorkflowRuntimeError> {
    let text = args
        .get("text")
        .or_else(|| args.get("prompt"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let parts = args
        .get("content")
        .or_else(|| args.get("parts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![json!({"type": "text", "text": text})]
            }
        });
    if parts.is_empty() {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.agent_turn requires text, prompt, content, or parts".to_owned(),
        ));
    }
    let explicit_thread_key = args
        .get("thread_key")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let workflow_owned_thread = explicit_thread_key.is_none();
    let thread_key = explicit_thread_key.unwrap_or_else(|| {
        format!(
            "wf:{}:agent:{}",
            ctx.task_id().replace('-', ""),
            input.workflow_name
        )
    });
    let harness_type = parse_agent_harness(&args)?.unwrap_or_else(|| input.harness_type.clone());
    let persona_id = args
        .get("persona_id")
        .or_else(|| args.get("persona"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let client_message_id = args
        .get("message_id")
        .or_else(|| args.get("client_message_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("absurd-workflow:{}:{request_id}:user", ctx.task_id()));
    let mut session_metadata = agent_metadata(&args, ctx, input, "session");
    if let Some(persona) = args.get("persona").and_then(Value::as_str) {
        object_insert(&mut session_metadata, "persona", json!(persona));
    }
    if let Some(engine) = args.get("engine").and_then(Value::as_str) {
        object_insert(&mut session_metadata, "engine", json!(engine));
    }
    let mut message_metadata = agent_metadata(&args, ctx, input, "message");
    object_insert(
        &mut message_metadata,
        "workflow_agent_request_id",
        json!(request_id),
    );
    let mut execution_metadata = agent_metadata(&args, ctx, input, "execution");
    if let Some(delivery) = args.get("delivery") {
        object_insert(&mut execution_metadata, "delivery", delivery.clone());
    }
    if let Some(persona) = args.get("persona").and_then(Value::as_str) {
        object_insert(&mut execution_metadata, "persona", json!(persona));
    }
    if let Some(engine) = args.get("engine").and_then(Value::as_str) {
        object_insert(&mut execution_metadata, "engine", json!(engine));
    }
    let idle_timeout_ms = args
        .get("idle_timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_AGENT_IDLE_TIMEOUT_MS);
    let max_duration_ms = args
        .get("max_duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_AGENT_MAX_DURATION_MS);
    let execution_idempotency_key = args
        .get("idempotency_key")
        .or_else(|| args.get("execution_idempotency_key"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("absurd-workflow-agent-turn:{client_message_id}"));
    // Optional per-turn harness knobs, mirroring the slackbot's `--model` /
    // `--bedrock` / `-rsn` flags. `reasoning` accepts `reasoning_effort` and
    // `effort` aliases so Python callers can use whichever reads best.
    let model = first_str_arg(&args, &["model"]);
    let provider = first_str_arg(&args, &["provider"]);
    let reasoning = first_str_arg(&args, &["reasoning", "reasoning_effort", "effort"]);
    // Record the model on the execution like the slackbot does, so Console
    // readers can show what a workflow-dispatched turn ran on.
    if let Some(model) = model.as_deref() {
        object_insert(&mut execution_metadata, "model", json!(model));
    }
    let result = run_agent_session_turn(
        session_runtime,
        AgentTurnRequest {
            thread_key,
            harness_type,
            persona_id,
            parts,
            client_message_id,
            session_metadata,
            message_metadata,
            execution_metadata,
            execution_idempotency_key,
            workflow_owned_thread,
            idle_timeout_ms,
            max_duration_ms,
            model,
            provider,
            reasoning,
        },
    )
    .await?;
    serde_json::to_value(result).map_err(WorkflowRuntimeError::from)
}

/// Returns the first arg key that holds a non-empty (trimmed) string, owned.
fn first_str_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_agent_harness(args: &Value) -> Result<Option<HarnessType>, WorkflowRuntimeError> {
    let Some(raw) = args
        .get("harness_type")
        .or_else(|| args.get("harness"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    HarnessType::from_str(raw).map(Some).map_err(|_| {
        WorkflowRuntimeError::BadRequest(format!("unsupported ctx.agent_turn harness {raw:?}"))
    })
}

fn agent_metadata(
    args: &Value,
    ctx: &TaskContext,
    input: &WorkflowTaskInput,
    phase: &str,
) -> Value {
    let mut metadata = args.get("metadata").cloned().unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }
    object_insert(&mut metadata, "source", json!("absurd_workflow"));
    object_insert(&mut metadata, "workflow_name", json!(input.workflow_name));
    object_insert(&mut metadata, "workflow_task_id", json!(ctx.task_id()));
    object_insert(&mut metadata, "workflow_run_id", json!(ctx.run_id()));
    object_insert(&mut metadata, "workflow_context_phase", json!(phase));
    metadata
}

fn object_insert(value: &mut Value, key: &str, item: Value) {
    if let Value::Object(object) = value {
        object.insert(key.to_owned(), item);
    }
}

async fn write_host_message<W>(stdin: &mut W, message: &Value) -> Result<(), WorkflowRuntimeError>
where
    W: AsyncWrite + Unpin,
{
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await?;
    Ok(())
}

fn python_workflow_host_path() -> PathBuf {
    if let Ok(path) = env::var(PYTHON_HOST_ENV) {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("workflow-python")
        .join("workflow_host.py")
}

fn default_workflow_dirs() -> String {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("..");
    repo_root.join("workflows").to_string_lossy().to_string()
}

fn run_time_now_tool() -> ToolResult {
    let now = chrono::Utc::now();
    ToolResult {
        tool: "time".to_owned(),
        method: "now".to_owned(),
        output: json!({
            "utc": now.to_rfc3339(),
            "unix_ms": now.timestamp_millis(),
            "source": "centaur-workflows-poc",
        }),
    }
}

async fn call_python_workflow_tool(message: &Value) -> Result<Value, WorkflowRuntimeError> {
    let tool = message
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkflowRuntimeError::BadRequest("ctx.call_tool requires tool".to_owned()))?
        .trim();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest("ctx.call_tool requires method".to_owned())
        })?
        .trim();
    if tool.is_empty() || method.is_empty() {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.call_tool requires non-empty tool and method".to_owned(),
        ));
    }
    if tool == "time" && matches!(method, "now" | "time_now") {
        return serde_json::to_value(run_time_now_tool()).map_err(WorkflowRuntimeError::from);
    }

    let base_url = env::var(WORKFLOW_TOOL_API_URL_ENV).map_err(|_| {
        WorkflowRuntimeError::BadRequest(format!(
            "{WORKFLOW_TOOL_API_URL_ENV} must be set for ctx.call_tool({tool}.{method})"
        ))
    })?;
    let base_url = base_url.trim_end_matches('/');
    let url = format!("{base_url}/tools/{tool}/{method}");
    let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
    let request = reqwest::Client::new().post(&url).json(&args);
    let response = request.send().await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "ctx.call_tool({tool}.{method}) failed with status {status}: {body}"
        )));
    }
    Ok(body)
}

async fn post_tool_result_to_slack(
    channel: &str,
    client_msg_id: &str,
    note: &str,
    tool: &ToolResult,
) -> Result<SlackPostResult, WorkflowRuntimeError> {
    let token = env::var("SLACK_BOT_TOKEN")
        .or_else(|_| env::var("SLACK_BOT_TOKEN_OVERRIDE"))
        .map_err(|_| {
            WorkflowRuntimeError::BadRequest(
                "SLACK_BOT_TOKEN or SLACK_BOT_TOKEN_OVERRIDE must be set".to_owned(),
            )
        })?;
    let text = format!(
        "{note}\nworkflow=tool_and_slack\ntool={}.{}\nresult={}",
        tool.tool,
        tool.method,
        serde_json::to_string(&tool.output)?,
    );
    let response = send_slack_message(
        &token,
        json!({
            "channel": channel,
            "text": text,
            "client_msg_id": client_msg_id,
            "unfurl_links": false,
            "unfurl_media": false,
        }),
    )
    .await?;
    Ok(slack_post_result_from_response(channel, response))
}

async fn post_python_slack_message(
    message: &Value,
    ctx: &TaskContext,
    request_id: &str,
) -> Result<Value, WorkflowRuntimeError> {
    let channel = message
        .get("channel")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest("ctx.post_to_slack requires channel".to_owned())
        })?;
    let text = message.get("text").and_then(Value::as_str).ok_or_else(|| {
        WorkflowRuntimeError::BadRequest("ctx.post_to_slack requires text".to_owned())
    })?;
    let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
    let client_msg_id = args
        .get("client_msg_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}:slack:{request_id}", ctx.task_id()));

    let token = env::var("SLACK_BOT_TOKEN")
        .or_else(|_| env::var("SLACK_BOT_TOKEN_OVERRIDE"))
        .map_err(|_| {
            WorkflowRuntimeError::BadRequest(
                "SLACK_BOT_TOKEN or SLACK_BOT_TOKEN_OVERRIDE must be set".to_owned(),
            )
        })?;
    let payload = python_slack_message_payload(channel, text, &client_msg_id, &args);
    let response = send_slack_message(&token, payload).await?;
    serde_json::to_value(slack_post_result_from_response(channel, response))
        .map_err(WorkflowRuntimeError::from)
}

fn python_slack_message_payload(
    channel: &str,
    text: &str,
    client_msg_id: &str,
    args: &Value,
) -> Value {
    let mut payload = json!({
        "channel": channel,
        "text": text,
        "client_msg_id": client_msg_id,
        "unfurl_links": args
            .get("unfurl_links")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "unfurl_media": args
            .get("unfurl_media")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    });
    if let Some(thread_ts) = args.get("thread_ts").and_then(Value::as_str) {
        payload["thread_ts"] = json!(thread_ts);
    }
    if let Some(reply_broadcast) = args.get("reply_broadcast").and_then(Value::as_bool) {
        payload["reply_broadcast"] = json!(reply_broadcast);
    }
    if let Some(blocks) = args.get("blocks") {
        payload["blocks"] = blocks.clone();
    }
    if let Some(no_attribution) = args.get("no_attribution").and_then(Value::as_bool) {
        payload["no_attribution"] = json!(no_attribution);
    }
    payload
}

async fn send_slack_message(token: &str, payload: Value) -> Result<Value, WorkflowRuntimeError> {
    let response: Value = reqwest::Client::new()
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(WorkflowRuntimeError::Upstream(format!(
            "Slack chat.postMessage failed: {}",
            response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error")
        )));
    }
    Ok(response)
}

fn slack_post_result_from_response(channel: &str, response: Value) -> SlackPostResult {
    SlackPostResult {
        channel: response
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or(channel)
            .to_owned(),
        ts: response
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    }
}

struct AgentTurnRequest {
    thread_key: String,
    harness_type: HarnessType,
    persona_id: Option<String>,
    parts: Vec<Value>,
    client_message_id: String,
    session_metadata: Value,
    message_metadata: Value,
    execution_metadata: Value,
    execution_idempotency_key: String,
    workflow_owned_thread: bool,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
    // Optional per-turn model / provider / reasoning-effort overrides. When set
    // they ride the execute input line exactly like the slackbot's per-turn
    // `--model` / `--bedrock` / `-rsn` flags do (see slackbotv2's
    // `toCodexInputLineWithStaged`), so the harness applies them to this turn;
    // when `None` the deployment/baked harness default stands. `provider` and
    // `reasoning` only affect the codex harness (claude/amp ignore them).
    model: Option<String>,
    provider: Option<String>,
    reasoning: Option<String>,
}

/// Builds the single `type: "user"` execute input line for a workflow agent
/// turn, mirroring the blocks-protocol shape the harness parses
/// (`BlocksLine` in `harness-server`): optional top-level `model` / `provider`
/// / `reasoning` keys, then the `message.content` parts. api-rs enriches the
/// line with session/trace context before forwarding, so those keys are omitted
/// here.
fn agent_turn_input_line(
    parts: &[Value],
    model: Option<&str>,
    provider: Option<&str>,
    reasoning: Option<&str>,
) -> Result<String, serde_json::Error> {
    let mut line = serde_json::Map::new();
    line.insert("type".to_owned(), json!("user"));
    if let Some(model) = model {
        line.insert("model".to_owned(), json!(model));
    }
    if let Some(provider) = provider {
        line.insert("provider".to_owned(), json!(provider));
    }
    if let Some(reasoning) = reasoning {
        line.insert("reasoning".to_owned(), json!(reasoning));
    }
    line.insert("message".to_owned(), json!({ "content": parts }));
    serde_json::to_string(&Value::Object(line))
}

async fn run_agent_session_turn(
    session_runtime: SessionRuntime,
    turn: AgentTurnRequest,
) -> Result<AgentTurnResult, WorkflowRuntimeError> {
    let AgentTurnRequest {
        thread_key,
        harness_type,
        persona_id,
        parts,
        client_message_id,
        session_metadata,
        message_metadata,
        execution_metadata,
        execution_idempotency_key,
        workflow_owned_thread,
        idle_timeout_ms,
        max_duration_ms,
        model,
        provider,
        reasoning,
    } = turn;
    let thread_key = ThreadKey::parse(thread_key)?;
    let mut session_metadata = session_metadata;
    if workflow_owned_thread {
        object_insert(&mut session_metadata, "workflow_owned_thread", json!(true));
    }
    session_runtime
        .create_or_get_session(
            &thread_key,
            &harness_type,
            persona_id.as_deref(),
            Some(session_metadata),
            HarnessConflictPolicy::Reject,
        )
        .await?;
    session_runtime
        .append_messages(
            &thread_key,
            &[SessionMessageInput {
                client_message_id: Some(client_message_id),
                role: MessageRole::User,
                parts: parts.clone(),
                metadata: message_metadata,
            }],
        )
        .await?;
    let execution = session_runtime
        .execute_session(
            &thread_key,
            ExecuteSessionInput {
                idempotency_key: Some(execution_idempotency_key),
                metadata: Some(execution_metadata),
                input_lines: vec![agent_turn_input_line(
                    &parts,
                    model.as_deref(),
                    provider.as_deref(),
                    reasoning.as_deref(),
                )?],
                idle_timeout_ms: Some(idle_timeout_ms),
                max_duration_ms: Some(max_duration_ms),
            },
        )
        .await?;

    let events = session_runtime
        .stream_events(&thread_key, 0, Some(&execution.execution_id))
        .await?;
    pin_mut!(events);
    let mut output_lines = Vec::new();
    while let Some(event) = events.try_next().await? {
        if event.execution_id.as_deref() != Some(execution.execution_id.as_str()) {
            continue;
        }
        match event.event_type.as_str() {
            SESSION_OUTPUT_LINE_EVENT => {
                if let Some(line) = event.payload.as_str() {
                    output_lines.push(line.to_owned());
                }
            }
            "session.execution_completed" => {
                return Ok(AgentTurnResult {
                    thread_key: thread_key.into_string(),
                    execution_id: execution.execution_id,
                    status: "completed".to_owned(),
                    result_text: result_text_from_output_lines(&output_lines),
                    output_lines,
                });
            }
            "session.execution_failed" | "session.execution_cancelled" => {
                let result = AgentTurnResult {
                    thread_key: thread_key.into_string(),
                    execution_id: execution.execution_id,
                    status: event.event_type,
                    result_text: result_text_from_output_lines(&output_lines),
                    output_lines,
                };
                return Err(WorkflowRuntimeError::Upstream(format!(
                    "agent turn {} for thread {} ended with {}",
                    result.execution_id, result.thread_key, result.status
                )));
            }
            _ => {}
        }
    }

    Err(WorkflowRuntimeError::Upstream(
        "session event stream ended before terminal execution event".to_owned(),
    ))
}

fn result_text_from_output_lines(lines: &[String]) -> String {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| {
            value
                .get("delta")
                .or_else(|| value.pointer("/params/delta"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>()
        .join("")
}

fn workflow_run_from_row(row: sqlx::postgres::PgRow) -> Result<WorkflowRun, WorkflowRuntimeError> {
    let params: Value = row.try_get("params")?;
    let input = params.get("input").cloned().unwrap_or(Value::Null);
    let workflow_name = params
        .get("workflow_name")
        .and_then(Value::as_str)
        .unwrap_or(WORKFLOW_TASK)
        .to_owned();
    Ok(WorkflowRun {
        run_id: row.try_get("run_id")?,
        task_id: row.try_get("task_id")?,
        workflow_name,
        status: row.try_get("state")?,
        input,
        result: row.try_get("completed_payload")?,
        failure: row.try_get("failure_reason")?,
        attempts: row.try_get("attempts")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn absurd_error(error: WorkflowRuntimeError) -> absurd::Error {
    match error {
        WorkflowRuntimeError::Suspend => absurd::Error::Suspend,
        other => absurd::Error::TaskFailed(Box::new(other)),
    }
}

#[derive(Debug, Error)]
pub enum WorkflowRuntimeError {
    #[error("workflow suspended")]
    Suspend,
    /// The caller supplied an invalid request or workflow configuration.
    /// Maps to HTTP 400.
    #[error("{0}")]
    BadRequest(String),
    /// The workflow exists but is disabled by environment policy. Maps to
    /// HTTP 403.
    #[error("{0}")]
    Disabled(String),
    #[error("workflow run not found: {0}")]
    NotFound(String),
    /// Server-side failure (workflow host spawn/protocol, internal dispatch).
    /// Maps to HTTP 500.
    #[error("{0}")]
    Internal(String),
    /// An upstream dependency (Slack, agent session) failed. Maps to
    /// HTTP 502.
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Absurd(#[from] absurd::Error),
    #[error(transparent)]
    SessionRuntime(#[from] centaur_session_runtime::SessionRuntimeError),
    #[error(transparent)]
    SessionStore(#[from] centaur_session_sqlx::SessionStoreError),
    #[error(transparent)]
    ThreadKey(#[from] centaur_session_core::ThreadKeyError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn agent_turn_input_line_omits_unset_harness_knobs() {
        let parts = vec![json!({"type": "text", "text": "hi"})];
        let line = agent_turn_input_line(&parts, None, None, None).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value.get("type"), Some(&json!("user")));
        assert_eq!(value.pointer("/message/content"), Some(&json!(parts)));
        assert!(value.get("model").is_none());
        assert!(value.get("provider").is_none());
        assert!(value.get("reasoning").is_none());
    }

    #[test]
    fn agent_turn_input_line_forwards_model_provider_reasoning() {
        let parts = vec![json!({"type": "text", "text": "hi"})];
        let line = agent_turn_input_line(
            &parts,
            Some("claude-opus-4-8"),
            Some("amazon-bedrock"),
            Some("high"),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        // Keys match the blocks-protocol shape the harness parses (BlocksLine).
        assert_eq!(value.get("model"), Some(&json!("claude-opus-4-8")));
        assert_eq!(value.get("provider"), Some(&json!("amazon-bedrock")));
        assert_eq!(value.get("reasoning"), Some(&json!("high")));
        assert_eq!(value.pointer("/message/content"), Some(&json!(parts)));
    }

    #[test]
    fn first_str_arg_picks_first_non_empty_alias() {
        let args = json!({"reasoning": "  ", "reasoning_effort": " high ", "effort": "low"});
        assert_eq!(
            first_str_arg(&args, &["reasoning", "reasoning_effort", "effort"]),
            Some("high".to_owned())
        );
        assert_eq!(first_str_arg(&json!({}), &["model"]), None);
        assert_eq!(first_str_arg(&json!({"model": "   "}), &["model"]), None);
    }

    #[test]
    fn parse_worker_concurrency_uses_override_or_default() {
        // Override wins.
        assert_eq!(parse_worker_concurrency(Some("16"), 4), 16);
        assert_eq!(parse_worker_concurrency(Some("  8 "), 4), 8);
        // Unset / empty / non-numeric / zero / negative fall back to the default.
        assert_eq!(parse_worker_concurrency(None, 4), 4);
        assert_eq!(parse_worker_concurrency(Some(""), 4), 4);
        assert_eq!(parse_worker_concurrency(Some("lots"), 4), 4);
        assert_eq!(parse_worker_concurrency(Some("0"), 4), 4);
        assert_eq!(parse_worker_concurrency(Some("-2"), 1), 1);
    }

    #[test]
    fn normalizes_interval_schedule_with_delivery_metadata() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "slack_sync",
            "source_path": "workflows/slack_sync.py",
            "schedule_id": "slack_sync",
            "interval_seconds": 60,
            "enabled": true,
            "no_delivery": true,
        }))
        .unwrap();
        assert_eq!(schedule.schedule_id, "slack_sync");
        assert!(schedule.enabled);
        assert!(schedule.no_delivery);
        assert_eq!(
            schedule.input.pointer("/metadata/source"),
            Some(&json!("workflow_schedule"))
        );
        assert_eq!(
            schedule.input.pointer("/metadata/no_delivery"),
            Some(&json!(true))
        );
    }

    #[test]
    fn cron_schedule_uses_configured_timezone() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "chief_of_staff_daily",
            "schedule_id": "chief_of_staff_daily",
            "cron": "45 7 * * *",
            "timezone": "America/Los_Angeles",
            "enabled": true,
            "no_delivery": true,
        }))
        .unwrap();
        let after = Utc.with_ymd_and_hms(2026, 6, 8, 12, 0, 0).unwrap();
        let next = next_schedule_time(&schedule, after).unwrap();
        assert_eq!(
            next,
            chrono_tz::America::Los_Angeles
                .with_ymd_and_hms(2026, 6, 8, 7, 45, 0)
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn cron_schedule_day_names_avoid_quartz_numbering() {
        let named_days = normalize_schedule(json!({
            "workflow_name": "weekday_report",
            "schedule_id": "named_weekdays",
            "cron": "0 9 * * MON-FRI",
            "timezone": "UTC",
            "enabled": true,
        }))
        .unwrap();
        let numeric_days = normalize_schedule(json!({
            "workflow_name": "weekday_report",
            "schedule_id": "numeric_days",
            "cron": "0 9 * * 1-5",
            "timezone": "UTC",
            "enabled": true,
        }))
        .unwrap();
        let after_thursday = Utc.with_ymd_and_hms(2026, 7, 16, 10, 0, 0).unwrap();

        assert_eq!(
            next_schedule_time(&named_days, after_thursday).unwrap(),
            Utc.with_ymd_and_hms(2026, 7, 17, 9, 0, 0).unwrap()
        );
        assert_eq!(
            next_schedule_time(&numeric_days, after_thursday).unwrap(),
            Utc.with_ymd_and_hms(2026, 7, 19, 9, 0, 0).unwrap()
        );
    }

    #[test]
    fn interval_tick_reschedules_from_scheduled_time_without_drift() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "slack_backfill",
            "schedule_id": "slack_backfill",
            "interval_seconds": 600,
            "enabled": true,
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 5).unwrap();

        let next = next_schedule_time_after_tick(&schedule, scheduled_at, now).unwrap();

        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 16, 12, 10, 0).unwrap());
    }

    #[test]
    fn interval_tick_skips_missed_runs_when_delayed() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "slack_backfill",
            "schedule_id": "slack_backfill",
            "interval_seconds": 600,
            "enabled": true,
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 25, 0).unwrap();

        let next = next_schedule_time_after_tick(&schedule, scheduled_at, now).unwrap();

        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 16, 12, 30, 0).unwrap());
    }

    #[test]
    fn scheduled_etls_use_isolated_etl_queues() {
        assert_eq!(
            workflow_queue_class("slack_sync"),
            WorkflowQueueClass::SlackLive
        );
        for workflow_name in [
            "google_calendar_sync",
            "google_drive_sync",
            "linear_sync",
            "company_context_documents",
            "slack_retention",
            "chief_of_staff_daily",
        ] {
            assert_eq!(workflow_queue_class(workflow_name), WorkflowQueueClass::Etl);
        }
        assert_eq!(
            workflow_queue_class("slack_backfill"),
            WorkflowQueueClass::EtlBackfill
        );
        assert_eq!(
            workflow_queue_class("slack_archive_import"),
            WorkflowQueueClass::EtlBackfill
        );
        assert_eq!(
            workflow_queue_class("github_issue_triage"),
            WorkflowQueueClass::Standard
        );
    }

    #[test]
    fn python_slack_payload_passes_reply_broadcast() {
        let payload = python_slack_message_payload(
            "C123",
            "hello",
            "client-1",
            &json!({
                "thread_ts": "1710000000.000100",
                "reply_broadcast": true,
                "unfurl_links": true,
                "unfurl_media": true,
            }),
        );

        assert_eq!(payload["channel"], json!("C123"));
        assert_eq!(payload["text"], json!("hello"));
        assert_eq!(payload["client_msg_id"], json!("client-1"));
        assert_eq!(payload["thread_ts"], json!("1710000000.000100"));
        assert_eq!(payload["reply_broadcast"], json!(true));
        assert_eq!(payload["unfurl_links"], json!(true));
        assert_eq!(payload["unfurl_media"], json!(true));
    }

    #[test]
    fn parses_python_workflow_metric_notification() {
        let metric = parse_python_workflow_metric(&json!({
            "type": "ctx.metric",
            "kind": "counter",
            "name": "etl_items_seen_total",
            "value": 12,
            "labels": {
                "namespace": "centaur-system",
                "environment": "production",
                "source": "slack",
                "source_type": "channel",
                "item_type": "thread_refresh_reply",
            },
        }))
        .unwrap();

        assert_eq!(metric.kind, PythonWorkflowMetricKind::Counter);
        assert_eq!(metric.name, "etl_items_seen_total");
        assert_eq!(metric.value, 12.0);
        assert!(
            metric
                .labels
                .contains(&("namespace".to_owned(), "centaur-system".to_owned()))
        );
        assert!(
            metric
                .labels
                .contains(&("source".to_owned(), "slack".to_owned()))
        );
    }

    #[test]
    fn rejects_python_workflow_metric_with_invalid_name() {
        let error = parse_python_workflow_metric(&json!({
            "type": "ctx.metric",
            "kind": "counter",
            "name": "bad-name",
            "value": 1,
        }))
        .unwrap_err();

        assert_eq!(error, "metric name missing or invalid");
    }

    #[tokio::test]
    async fn ctx_call_tool_supports_builtin_time_now() {
        let value = call_python_workflow_tool(&json!({
            "type": "ctx.call_tool",
            "tool": "time",
            "method": "now",
            "args": {},
        }))
        .await
        .unwrap();
        assert_eq!(value["tool"], json!("time"));
        assert_eq!(value["method"], json!("now"));
        assert!(value.pointer("/output/utc").is_some());
    }

    #[test]
    fn workflow_run_timestamps_serialize_as_rfc3339() {
        let at = OffsetDateTime::from_unix_timestamp(1_781_012_105).unwrap()
            + time::Duration::nanoseconds(44_019_000);
        let run = WorkflowRun {
            run_id: "run".to_owned(),
            task_id: "task".to_owned(),
            workflow_name: "workflow".to_owned(),
            status: "completed".to_owned(),
            input: json!({}),
            result: None,
            failure: None,
            attempts: 1,
            created_at: at,
            updated_at: at,
        };
        let value = serde_json::to_value(run).unwrap();
        assert_eq!(value["created_at"], json!("2026-06-09T13:35:05.044019Z"));
        assert_eq!(value["updated_at"], json!("2026-06-09T13:35:05.044019Z"));
    }

    #[test]
    fn discovery_metadata_collects_all_workflow_names() {
        let payload: PythonWorkflowDiscoveryPayload = serde_json::from_value(json!({
            "workflows": [
                {
                    "workflow_name": "scheduled_workflow",
                    "source_path": "workflows/scheduled_workflow.py",
                    "schedule": {"schedule_id": "scheduled_workflow", "cron": "*/5 * * * *"},
                },
                {
                    "workflow_name": "manual_workflow",
                    "source_path": "workflows/manual_workflow.py",
                },
            ],
        }))
        .unwrap();
        let metadata = metadata_from_discovery_payload(payload);
        assert_eq!(
            metadata.workflow_names,
            BTreeSet::from([
                "scheduled_workflow".to_owned(),
                "manual_workflow".to_owned()
            ])
        );
        assert_eq!(metadata.schedules.len(), 1);
        assert_eq!(
            metadata.schedules[0].get("workflow_name"),
            Some(&json!("scheduled_workflow"))
        );
    }

    #[test]
    fn discovery_metadata_preserves_webhook_filter() {
        let payload: PythonWorkflowDiscoveryPayload = serde_json::from_value(json!({
            "workflows": [
                {
                    "workflow_name": "github_issue_triage",
                    "source_path": "workflows/github_issue_triage.py",
                    "webhooks": [
                        {
                            "workflow_name": "github_issue_triage",
                            "source_path": "workflows/github_issue_triage.py",
                            "spec": {
                                "slug": "github-issue-triage",
                                "auth": {
                                    "type": "github",
                                    "secret_ref": "GITHUB_WEBHOOK_SECRET"
                                },
                                "filter": {
                                    "all": [
                                        {
                                            "source": "header",
                                            "key": "x-github-event",
                                            "op": "equals",
                                            "value": "issue_comment"
                                        },
                                        {
                                            "source": "body",
                                            "key": "repository.full_name",
                                            "op": "in",
                                            "values": ["ethereum-optimism/optimism"]
                                        }
                                    ]
                                }
                            }
                        }
                    ]
                }
            ],
        }))
        .unwrap();

        let metadata = metadata_from_discovery_payload(payload);
        let filter = metadata.webhooks[0].spec.filter.as_ref().unwrap();
        let all = filter.all.as_ref().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].source.as_deref(), Some("header"));
        assert_eq!(all[1].key.as_deref(), Some("repository.full_name"));
    }

    fn webhook_with_filter(filter: Value) -> RegisteredWorkflowWebhook {
        RegisteredWorkflowWebhook {
            workflow_name: "github_issue_triage".to_owned(),
            source_path: "workflows/github_issue_triage.py".to_owned(),
            spec: WorkflowWebhookSpec {
                slug: "github-issue-triage".to_owned(),
                provider: Some("github".to_owned()),
                auth: WorkflowWebhookAuth::Github {
                    secret_ref: "GITHUB_WEBHOOK_SECRET".to_owned(),
                },
                trigger_key: Some(WorkflowWebhookTriggerKey::Header {
                    header: "X-GitHub-Delivery".to_owned(),
                }),
                allowed_methods: vec!["POST".to_owned()],
                allowed_content_types: vec!["application/json".to_owned()],
                filter: Some(serde_json::from_value(filter).unwrap()),
            },
        }
    }

    #[test]
    fn normalize_webhook_accepts_and_normalizes_filter() {
        let mut webhook = webhook_with_filter(json!({
            "all": [
                {
                    "source": " Header ",
                    "key": " x-github-event ",
                    "op": " EQUALS ",
                    "value": " issue_comment "
                },
                {
                    "source": "body",
                    "key": "repository.full_name",
                    "op": "in",
                    "values": [" ethereum-optimism/optimism "]
                }
            ]
        }));

        normalize_webhook(&mut webhook).unwrap();

        let all = webhook.spec.filter.unwrap().all.unwrap();
        assert_eq!(all[0].source.as_deref(), Some("header"));
        assert_eq!(all[0].key.as_deref(), Some("x-github-event"));
        assert_eq!(all[0].op.as_deref(), Some("equals"));
        assert_eq!(all[0].value.as_deref(), Some("issue_comment"));
        assert_eq!(
            all[1].values.as_ref().unwrap(),
            &vec!["ethereum-optimism/optimism".to_owned()]
        );
    }

    #[test]
    fn normalize_webhook_rejects_malformed_filters() {
        for filter in [
            json!({}),
            json!({"any": []}),
            json!({
                "any": [{"source": "header", "key": "x-github-event", "op": "equals", "value": "issues"}],
                "source": "header",
                "key": "x-github-event",
                "op": "equals",
                "value": "issues"
            }),
            json!({"source": "headers", "key": "x-github-event", "op": "equals", "value": "issues"}),
            json!({"source": "body", "key": "repository..full_name", "op": "equals", "value": "repo"}),
            json!({"source": "body", "key": "repository.full_name", "op": "regex", "value": "repo"}),
            json!({"source": "body", "key": "repository.full_name", "op": "equals", "values": ["repo"]}),
            json!({"source": "body", "key": "repository.full_name", "op": "in", "value": "repo"}),
            json!({"source": "body", "key": "repository.full_name", "op": "in", "values": []}),
            json!({"source": "body", "key": "repository.full_name", "op": "in", "values": [""]}),
        ] {
            let mut webhook = webhook_with_filter(filter);
            let error = normalize_webhook(&mut webhook).unwrap_err();
            assert!(matches!(error, WorkflowRuntimeError::BadRequest(_)));
        }
    }

    #[test]
    fn workflow_allowlist_parses_comma_and_whitespace_names() {
        let enablement = WorkflowEnablement::allowlist("agent_turn, slack_sync\ncompany_context");
        assert!(enablement.is_enabled("agent_turn"));
        assert!(enablement.is_enabled("slack_sync"));
        assert!(enablement.is_enabled("company_context"));
        assert!(!enablement.is_enabled("google_drive_sync"));
    }

    #[test]
    fn workflow_allowlist_filters_discovered_metadata() {
        let payload: PythonWorkflowDiscoveryPayload = serde_json::from_value(json!({
            "workflows": [
                {
                    "workflow_name": "allowed_workflow",
                    "source_path": "workflows/allowed_workflow.py",
                    "schedule": {"schedule_id": "allowed", "cron": "*/5 * * * *"},
                    "webhooks": [{
                        "workflow_name": "allowed_workflow",
                        "source_path": "workflows/allowed_workflow.py",
                        "spec": {
                            "slug": "allowed",
                            "auth": {"type": "none"}
                        }
                    }]
                },
                {
                    "workflow_name": "blocked_workflow",
                    "source_path": "workflows/blocked_workflow.py",
                    "schedule": {"schedule_id": "blocked", "cron": "*/10 * * * *"},
                    "webhooks": [{
                        "workflow_name": "blocked_workflow",
                        "source_path": "workflows/blocked_workflow.py",
                        "spec": {
                            "slug": "blocked",
                            "auth": {"type": "none"}
                        }
                    }]
                },
            ],
        }))
        .unwrap();
        let mut metadata = metadata_from_discovery_payload(payload);
        WorkflowEnablement::allowlist("allowed_workflow").filter_metadata(&mut metadata);

        assert_eq!(
            metadata.workflow_names,
            BTreeSet::from(["allowed_workflow".to_owned()])
        );
        assert_eq!(metadata.schedules.len(), 1);
        assert_eq!(
            metadata.schedules[0].get("schedule_id"),
            Some(&json!("allowed"))
        );
        assert_eq!(metadata.webhooks.len(), 1);
        assert_eq!(metadata.webhooks[0].workflow_name, "allowed_workflow");
    }

    #[test]
    fn workflow_allowlist_filters_default_webhooks() {
        let metadata = PythonWorkflowMetadata::default();
        let registry = build_webhook_registry(
            &metadata,
            &WorkflowEnablement::allowlist("github_issue_triage"),
        )
        .unwrap();
        assert!(registry.contains_key("github-issue-triage"));
        assert!(!registry.contains_key("github-consensus-ci-triage"));
        assert!(!registry.contains_key("trivy-vulnerability-intake"));
    }

    #[test]
    fn disabled_workflow_returns_policy_error() {
        let error = WorkflowEnablement::allowlist("agent_turn")
            .ensure_enabled("slack_sync")
            .unwrap_err();
        assert!(matches!(error, WorkflowRuntimeError::Disabled(_)));
    }

    #[test]
    fn workflow_cleanup_reason_skips_suspended_runs() {
        let completed: absurd::Result<WorkflowResult> = Ok(WorkflowResult {
            workflow_name: "test".to_owned(),
            run_id: "run-1".to_owned(),
            task_id: "task-1".to_owned(),
            steps: Vec::new(),
            output: json!({}),
        });
        assert_eq!(
            workflow_cleanup_reason(&completed),
            Some("workflow_completed")
        );

        let suspended: absurd::Result<WorkflowResult> = Err(absurd::Error::Suspend);
        assert_eq!(workflow_cleanup_reason(&suspended), None);

        let cancelled: absurd::Result<WorkflowResult> = Err(absurd::Error::Cancelled);
        assert_eq!(
            workflow_cleanup_reason(&cancelled),
            Some("workflow_cancelled")
        );

        let failed: absurd::Result<WorkflowResult> = Err(absurd::Error::Timeout("boom".to_owned()));
        assert_eq!(workflow_cleanup_reason(&failed), Some("workflow_failed"));
    }

    #[test]
    fn stale_cancellations_wait_for_threshold_consecutive_misses() {
        let known = BTreeSet::from(["alive".to_owned()]);
        let active = vec![
            ("task-1".to_owned(), "removed".to_owned()),
            ("task-2".to_owned(), "removed".to_owned()),
            ("task-3".to_owned(), "alive".to_owned()),
        ];
        let mut counts = BTreeMap::new();

        assert!(select_stale_cancellations(&active, &known, &mut counts, 3).is_empty());
        assert!(select_stale_cancellations(&active, &known, &mut counts, 3).is_empty());
        assert_eq!(
            select_stale_cancellations(&active, &known, &mut counts, 3),
            vec!["task-1".to_owned(), "task-2".to_owned()]
        );
        assert!(!counts.contains_key("alive"));
    }

    #[test]
    fn stale_cancellation_counter_resets_when_workflow_reappears() {
        let active = vec![("task-1".to_owned(), "flaky".to_owned())];
        let mut counts = BTreeMap::new();

        assert!(select_stale_cancellations(&active, &BTreeSet::new(), &mut counts, 2).is_empty());
        // Workflow discovered again: counter must drop so a later removal
        // starts counting from scratch.
        let known = BTreeSet::from(["flaky".to_owned()]);
        assert!(select_stale_cancellations(&active, &known, &mut counts, 2).is_empty());
        assert!(counts.is_empty());
        assert!(select_stale_cancellations(&active, &BTreeSet::new(), &mut counts, 2).is_empty());
        assert_eq!(
            select_stale_cancellations(&active, &BTreeSet::new(), &mut counts, 2),
            vec!["task-1".to_owned()]
        );
    }

    #[test]
    fn stale_cancellation_counter_drops_idle_names() {
        let active = vec![("task-1".to_owned(), "removed".to_owned())];
        let mut counts = BTreeMap::new();
        assert!(select_stale_cancellations(&active, &BTreeSet::new(), &mut counts, 2).is_empty());
        // No active tasks reference the name anymore (e.g. all cancelled).
        assert!(select_stale_cancellations(&[], &BTreeSet::new(), &mut counts, 2).is_empty());
        assert!(counts.is_empty());
    }

    #[test]
    fn zero_threshold_disables_reaping_selection() {
        // threshold 0 is handled by RemovedWorkflowReaper::reap returning
        // early; the selection helper itself treats it as "cancel instantly",
        // so guard the contract here to catch accidental misuse.
        let active = vec![("task-1".to_owned(), "removed".to_owned())];
        let mut counts = BTreeMap::new();
        assert_eq!(
            select_stale_cancellations(&active, &BTreeSet::new(), &mut counts, 1),
            vec!["task-1".to_owned()]
        );
    }
}
