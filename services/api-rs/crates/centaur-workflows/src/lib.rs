use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    future::Future,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration,
};

use absurd::{
    AwaitEventOptions, Client, ClientOptions, CreateQueueOptions, RetryKind, RetryStrategy,
    SpawnOptions, StepHandle, TaskContext, TaskRegistrationOptions, Worker, WorkerOptions,
};
use centaur_iron_control::{IronControlClient, IronControlError, PrincipalInput, slugify};
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
use futures_util::{StreamExt, TryStreamExt, pin_mut, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
const WORKFLOW_TOOL_ALLOWLIST_ENV: &str = "WORKFLOW_TOOL_ALLOWLIST_JSON";
const MAX_WORKFLOW_TOOL_IDENTIFIER_BYTES: usize = 128;
const DEFAULT_AGENT_IDLE_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_AGENT_MAX_DURATION_MS: u64 = 30 * 60 * 1_000;
const DEFAULT_AGENT_BATCH_CONCURRENCY: usize = 4;
const MAX_AGENT_BATCH_CONCURRENCY: usize = 16;
const MAX_AGENT_BATCH_SIZE: usize = 32;
const MAX_AGENT_BATCH_NAME_BYTES: usize = 128;
const WORKFLOW_HOST_CLAIM_EXTENSION: Duration = Duration::from_secs(5 * 60);
const WORKFLOW_HOST_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
const WORKFLOW_RECONCILE_INTERVAL_SECS_ENV: &str = "WORKFLOW_RECONCILE_INTERVAL_SECS";
const DEFAULT_WORKFLOW_RECONCILE_INTERVAL_SECS: u64 = 60;
const SCHEDULE_FIRST_REGISTRATION_CATCH_UP_GRACE_SECS: i64 = 60;
const WORKFLOW_ENABLE_MODE_ENV: &str = "WORKFLOW_ENABLE_MODE";
const WORKFLOW_ALLOWED_NAMES_ENV: &str = "WORKFLOW_ALLOWED_NAMES";
const MAX_LIST_RUNS_LIMIT: i64 = 1_000;
/// How many consecutive reconcile passes a workflow must be missing from
/// discovery before its active tasks are cancelled. 0 disables reaping.
const WORKFLOW_REAP_REMOVED_AFTER_TICKS_ENV: &str = "WORKFLOW_REAP_REMOVED_AFTER_TICKS";
const DEFAULT_WORKFLOW_REAP_REMOVED_AFTER_TICKS: u32 = 3;
const ABSURD_TERMINAL_TASK_STATES: &str = "('completed', 'failed', 'cancelled')";
const SLACK_RECONCILIATION_PAGE_LIMIT: &str = "100";
const MAX_SLACK_RECONCILIATION_PAGES: usize = 3;

pub fn python_workflow_event_name(event_type: &str, correlation_id: &str) -> String {
    // JSON string encoding is unambiguous even when either component contains a delimiter.
    format!(
        "python:{}",
        serde_json::to_string(&(event_type, correlation_id))
            .expect("serializing two strings cannot fail")
    )
}

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
    event_trigger_registry: Arc<RwLock<Vec<RegisteredWorkflowEventTrigger>>>,
}

#[derive(Clone)]
struct WorkflowMetadataRegistries {
    webhooks: Arc<RwLock<BTreeMap<String, RegisteredWorkflowWebhook>>>,
    schedules: Arc<RwLock<BTreeMap<String, RegisteredWorkflowSchedule>>>,
    event_triggers: Arc<RwLock<Vec<RegisteredWorkflowEventTrigger>>>,
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
        metadata
            .event_triggers
            .retain(|trigger| self.is_enabled(&trigger.workflow_name));
        metadata.schedules.retain(|schedule| {
            schedule
                .get("workflow_name")
                .and_then(Value::as_str)
                .is_some_and(|workflow_name| self.is_enabled(workflow_name))
        });
        metadata
            .principals
            .retain(|workflow_name, _| self.is_enabled(workflow_name));
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
    workflow_principals: Arc<RwLock<WorkflowPrincipalAssignments>>,
}

#[derive(Clone, Default)]
struct WorkflowPrincipalAssignments {
    required: BTreeSet<String>,
    registered: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkflowPrincipalDeclaration {
    Managed,
    Existing(String),
}

impl WorkflowPrincipalAssignments {
    fn principal_for_workflow(
        &self,
        workflow_name: &str,
    ) -> Result<Option<String>, WorkflowRuntimeError> {
        if let Some(principal) = self.registered.get(workflow_name) {
            return Ok(Some(principal.clone()));
        }
        if self.required.contains(workflow_name) {
            return Err(WorkflowRuntimeError::Internal(format!(
                "workflow {workflow_name} declares WORKFLOW_PRINCIPAL but no scoped principal is registered"
            )));
        }
        Ok(None)
    }
}

impl WorkflowHostSandboxRuntime {
    pub fn new(runtime: SandboxRuntime, spec: SandboxSpec) -> Self {
        Self {
            runtime,
            spec,
            workflow_principals: Arc::new(RwLock::new(WorkflowPrincipalAssignments::default())),
        }
    }

    fn update_workflow_principals(
        &self,
        registered: BTreeMap<String, String>,
        required: BTreeSet<String>,
    ) {
        let mut current = self
            .workflow_principals
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *current = WorkflowPrincipalAssignments {
            required,
            registered,
        };
    }

    fn spec_for_workflow(&self, workflow_name: &str) -> Result<SandboxSpec, WorkflowRuntimeError> {
        let mut spec = self.spec.clone();
        let principal = {
            let assignments = self
                .workflow_principals
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assignments.principal_for_workflow(workflow_name)?
        };
        if let Some(principal) = principal {
            spec.iron_control_principal = Some(principal);
        }
        Ok(spec)
    }
}

#[derive(Clone)]
pub struct WorkflowPrincipalRegistrar {
    client: IronControlClient,
}

impl WorkflowPrincipalRegistrar {
    pub fn new(client: IronControlClient) -> Self {
        Self { client }
    }

    async fn register_workflow_principals(
        &self,
        principals: &BTreeMap<String, WorkflowPrincipalDeclaration>,
    ) -> Result<BTreeMap<String, String>, WorkflowRuntimeError> {
        let mut registered = BTreeMap::new();
        for (workflow_name, declaration) in principals {
            let record = match declaration {
                WorkflowPrincipalDeclaration::Managed => {
                    let foreign_id = canonical_workflow_principal_foreign_id(workflow_name);
                    self.client
                        .upsert_principal(&PrincipalInput {
                            foreign_id,
                            name: format!("Workflow {workflow_name}"),
                            labels: workflow_principal_labels(workflow_name),
                            kind: Some("workflow".to_owned()),
                            slack_user_id: None,
                            slack_channel_id: None,
                            slack_team_id: None,
                            slack_email: None,
                        })
                        .await?
                }
                WorkflowPrincipalDeclaration::Existing(reference) => {
                    self.client.get_principal(reference).await?
                }
            };
            registered.insert(workflow_name.clone(), record.id);
        }
        Ok(registered)
    }
}

fn canonical_workflow_principal_foreign_id(workflow_name: &str) -> String {
    format!("workflow-{}", slugify(workflow_name))
}

fn workflow_principal_labels(workflow_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("managed-by".to_owned(), "centaur".to_owned()),
        ("workflow_name".to_owned(), workflow_name.to_owned()),
    ])
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegisteredWorkflowEventTrigger {
    pub workflow_name: String,
    pub source_path: String,
    pub spec: WorkflowEventTriggerSpec,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowEventTriggerSpec {
    pub name: String,
    pub event_name_prefix: String,
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
    StandardWebhooks {
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

fn list_runs_limit(limit: i64) -> i64 {
    limit.clamp(1, MAX_LIST_RUNS_LIMIT)
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
        workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
        workflow_principal_registrar: WorkflowPrincipalRegistrar,
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

        let discovery = discover_python_workflow_metadata().await?;
        let enablement = WorkflowEnablement::from_env()?;
        let workflow_host_sandbox = prepare_workflow_host_sandbox(
            workflow_host_sandbox,
            workflow_principal_registrar.clone(),
            &discovery,
            &enablement,
        )
        .await?;
        let schedule_registry = Arc::new(RwLock::new(build_schedule_registry(
            &discovery,
            &enablement,
        )?));
        let webhook_registry = Arc::new(RwLock::new(build_webhook_registry(
            &discovery,
            &enablement,
        )?));
        let event_trigger_registry = Arc::new(RwLock::new(build_event_trigger_registry(
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
                WorkflowMetadataRegistries {
                    webhooks: webhook_registry.clone(),
                    schedules: schedule_registry.clone(),
                    event_triggers: event_trigger_registry.clone(),
                },
                workflow_host_sandbox.clone(),
                workflow_principal_registrar,
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
                event_trigger_registry,
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

    pub async fn list_runs(
        &self,
        limit: i64,
        workflow_name: Option<&str>,
    ) -> Result<Vec<WorkflowRun>, WorkflowRuntimeError> {
        let limit = list_runs_limit(limit);
        let mut runs = Vec::new();
        runs.extend(
            self.list_runs_for_queue(WORKFLOW_QUEUE, limit, workflow_name)
                .await?,
        );
        runs.extend(
            self.list_runs_for_queue(WORKFLOW_SLACK_LIVE_QUEUE, limit, workflow_name)
                .await?,
        );
        runs.extend(
            self.list_runs_for_queue(WORKFLOW_ETL_QUEUE, limit, workflow_name)
                .await?,
        );
        runs.extend(
            self.list_runs_for_queue(WORKFLOW_ETL_BACKFILL_QUEUE, limit, workflow_name)
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
        workflow_name: Option<&str>,
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
            where (
                $2::text is null
                or coalesce(t.params->>'workflow_name', '{WORKFLOW_TASK}') = $2
            )
            order by t.enqueue_at desc, t.task_id desc
            limit $1
            "#,
        ))
        .bind(limit)
        .bind(workflow_name)
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
        self.emit_event_with_idempotency(event_name, payload, None)
            .await?;
        Ok(())
    }

    pub async fn emit_event_with_idempotency(
        &self,
        event_name: &str,
        payload: Value,
        idempotency_key: Option<&str>,
    ) -> Result<Vec<CreateWorkflowRunResponse>, WorkflowRuntimeError> {
        let event_name = event_name.trim();
        if event_name.is_empty() {
            return Err(WorkflowRuntimeError::BadRequest(
                "event_name must not be empty".to_owned(),
            ));
        }
        let matching_triggers = self
            .inner
            .event_trigger_registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|trigger| event_name.starts_with(&trigger.spec.event_name_prefix))
            .cloned()
            .collect::<Vec<_>>();
        let idempotency_key = idempotency_key.map(str::trim).filter(|key| !key.is_empty());
        if !matching_triggers.is_empty() && idempotency_key.is_none() {
            return Err(WorkflowRuntimeError::BadRequest(
                "idempotency_key is required when an event starts workflows".to_owned(),
            ));
        }

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
            .emit_event(
                event_name,
                payload.clone(),
                Some(WORKFLOW_ETL_BACKFILL_QUEUE),
            )
            .await?;

        let mut runs = Vec::with_capacity(matching_triggers.len());
        for trigger in matching_triggers {
            let digest = Sha256::digest(
                format!("{}:{}", trigger.spec.name, idempotency_key.unwrap()).as_bytes(),
            );
            let response = self
                .create_run(CreateWorkflowRunRequest {
                    workflow_name: trigger.workflow_name,
                    input: json!({"event_name": event_name, "payload": payload.clone()}),
                    idempotency_key: Some(format!("event-trigger:{}", hex::encode(digest))),
                    harness_type: None,
                    max_attempts: Some(3),
                })
                .await?;
            runs.push(response);
        }
        Ok(runs)
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
        | "company_context_embeddings"
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

fn build_event_trigger_registry(
    discovery: &PythonWorkflowMetadata,
    enablement: &WorkflowEnablement,
) -> Result<Vec<RegisteredWorkflowEventTrigger>, WorkflowRuntimeError> {
    let mut names = BTreeSet::new();
    let mut registry = Vec::new();
    for mut trigger in discovery.event_triggers.clone() {
        if !enablement.is_enabled(&trigger.workflow_name) {
            continue;
        }
        trigger.workflow_name = trigger.workflow_name.trim().to_owned();
        trigger.spec.name = trigger.spec.name.trim().to_owned();
        trigger.spec.event_name_prefix = trigger.spec.event_name_prefix.trim().to_owned();
        if trigger.workflow_name.is_empty() {
            return Err(WorkflowRuntimeError::BadRequest(
                "workflow event trigger workflow_name must not be empty".to_owned(),
            ));
        }
        if trigger.spec.name.is_empty() || trigger.spec.name.len() > 128 {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "workflow event trigger name must contain 1..=128 bytes, got {:?}",
                trigger.spec.name
            )));
        }
        if trigger.spec.event_name_prefix.is_empty() || trigger.spec.event_name_prefix.len() > 256 {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "workflow event trigger {:?} event_name_prefix must contain 1..=256 bytes",
                trigger.spec.name
            )));
        }
        if !names.insert(trigger.spec.name.clone()) {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "duplicate workflow event trigger name {:?}",
                trigger.spec.name
            )));
        }
        registry.push(trigger);
    }
    registry.sort_by(|left, right| left.spec.name.cmp(&right.spec.name));
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
        WorkflowWebhookAuth::Github { secret_ref }
        | WorkflowWebhookAuth::StandardWebhooks { secret_ref }
        | WorkflowWebhookAuth::Bearer { secret_ref } => {
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
    event_triggers: Vec<RegisteredWorkflowEventTrigger>,
    #[serde(default)]
    schedule: Option<Value>,
    #[serde(default)]
    principal: Option<PythonWorkflowPrincipal>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PythonWorkflowPrincipal {
    Enabled(bool),
    Reference(String),
}

#[derive(Debug, Deserialize)]
struct PythonWorkflowDiscoveryPayload {
    workflows: Vec<PythonWorkflowDiscovery>,
}

#[derive(Debug, Default)]
struct PythonWorkflowMetadata {
    webhooks: Vec<RegisteredWorkflowWebhook>,
    event_triggers: Vec<RegisteredWorkflowEventTrigger>,
    schedules: Vec<Value>,
    workflow_names: BTreeSet<String>,
    principals: BTreeMap<String, WorkflowPrincipalDeclaration>,
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
        metadata.event_triggers.extend(workflow.event_triggers);
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
        match workflow.principal {
            Some(PythonWorkflowPrincipal::Enabled(true)) => {
                metadata.principals.insert(
                    workflow.workflow_name,
                    WorkflowPrincipalDeclaration::Managed,
                );
            }
            Some(PythonWorkflowPrincipal::Reference(reference)) if !reference.trim().is_empty() => {
                metadata.principals.insert(
                    workflow.workflow_name,
                    WorkflowPrincipalDeclaration::Existing(reference.trim().to_owned()),
                );
            }
            _ => {}
        }
    }
    metadata
}

async fn prepare_workflow_host_sandbox(
    workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
    workflow_principal_registrar: WorkflowPrincipalRegistrar,
    discovery: &PythonWorkflowMetadata,
    enablement: &WorkflowEnablement,
) -> Result<Option<WorkflowHostSandboxRuntime>, WorkflowRuntimeError> {
    let Some(sandbox) = workflow_host_sandbox else {
        if !discovery.principals.is_empty() {
            let workflow_names = discovery
                .principals
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "WORKFLOW_PRINCIPAL requires workflow-host sandboxing, but WORKFLOW_HOST_SANDBOX is disabled for workflows: {workflow_names}"
            )));
        }
        return Ok(None);
    };
    reconcile_workflow_principals(
        &sandbox,
        &workflow_principal_registrar,
        discovery,
        enablement,
    )
    .await?;
    Ok(Some(sandbox))
}

async fn reconcile_workflow_principals(
    sandbox: &WorkflowHostSandboxRuntime,
    registrar: &WorkflowPrincipalRegistrar,
    discovery: &PythonWorkflowMetadata,
    enablement: &WorkflowEnablement,
) -> Result<(), WorkflowRuntimeError> {
    let mut principals = discovery.principals.clone();
    principals.retain(|workflow_name, _| enablement.is_enabled(workflow_name));
    let required = principals.keys().cloned().collect();
    let registered = match registrar.register_workflow_principals(&principals).await {
        Ok(registered) => registered,
        Err(error) => {
            sandbox.update_workflow_principals(BTreeMap::new(), required);
            return Err(error);
        }
    };
    sandbox.update_workflow_principals(registered, required);
    Ok(())
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
    registries: WorkflowMetadataRegistries,
    workflow_host_sandbox: Option<WorkflowHostSandboxRuntime>,
    workflow_principal_registrar: WorkflowPrincipalRegistrar,
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
                &registries,
                workflow_host_sandbox.as_ref(),
                &workflow_principal_registrar,
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
    registries: &WorkflowMetadataRegistries,
    workflow_host_sandbox: Option<&WorkflowHostSandboxRuntime>,
    workflow_principal_registrar: &WorkflowPrincipalRegistrar,
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
    let next_event_triggers = build_event_trigger_registry(&discovery, &enablement)?;
    if let Some(sandbox) = workflow_host_sandbox {
        reconcile_workflow_principals(
            sandbox,
            workflow_principal_registrar,
            &discovery,
            &enablement,
        )
        .await?;
    }
    {
        let mut webhooks = registries
            .webhooks
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *webhooks = next_webhooks;
    }
    {
        let mut schedules = registries
            .schedules
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *schedules = next_schedules.clone();
    }
    {
        let mut event_triggers = registries
            .event_triggers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *event_triggers = next_event_triggers;
    }
    reconcile_schedules(schedule_client, &next_schedules).await?;
    info!(
        webhook_count = discovery.webhooks.len(),
        schedule_count = discovery.schedules.len(),
        event_trigger_count = discovery.event_triggers.len(),
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
                input: schedule_workflow_input(&schedule, input.scheduled_at),
                harness_type: HarnessType::Codex,
            },
            scheduled_workflow_spawn_options(fire_key.clone()),
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

fn schedule_workflow_input(
    schedule: &RegisteredWorkflowSchedule,
    scheduled_at: DateTime<Utc>,
) -> Value {
    let scheduled_for = scheduled_at.to_rfc3339();
    let mut input = schedule.input.as_object().cloned().unwrap_or_default();
    input.insert("scheduled_for".to_owned(), json!(&scheduled_for));

    let metadata = input.entry("metadata").or_insert_with(|| json!({}));
    if !metadata.is_object() {
        *metadata = json!({});
    }
    let metadata = metadata
        .as_object_mut()
        .expect("metadata was normalized to an object");
    metadata.insert("source".to_owned(), json!("workflow_schedule"));
    metadata.insert("workflow_name".to_owned(), json!(&schedule.workflow_name));
    metadata.insert("no_delivery".to_owned(), json!(schedule.no_delivery));
    metadata.insert("schedule_id".to_owned(), json!(&schedule.schedule_id));
    metadata.insert("scheduled_for".to_owned(), json!(&scheduled_for));

    Value::Object(input)
}

fn scheduled_workflow_spawn_options(idempotency_key: String) -> SpawnOptions {
    // Scheduled workflows may call Slack, tools, or source systems. Centaur
    // has checkpoints but no durable external-effect ledger, so a failed
    // attempt cannot prove that replay is side-effect free. Fail closed until
    // that ledger exists; manual/event workflows retain their normal policy.
    SpawnOptions {
        idempotency_key: Some(idempotency_key),
        max_attempts: Some(1),
        ..SpawnOptions::default()
    }
}

async fn ensure_schedule_tick(
    client: &Client,
    schedule: &RegisteredWorkflowSchedule,
    scheduled_at: DateTime<Utc>,
) -> Result<bool, WorkflowRuntimeError> {
    let now = Utc::now();
    let latest = latest_schedule_tick(client, &schedule.schedule_id).await?;
    if schedule_tick_is_active(latest.as_ref()) {
        return Ok(false);
    }
    let occurrence = schedule_reconcile_occurrence(schedule, scheduled_at, now, latest)?;
    spawn_schedule_tick(client, schedule, occurrence).await?;
    Ok(true)
}

#[derive(Debug, Clone)]
struct ScheduleTickRecord {
    state: String,
    scheduled_at: DateTime<Utc>,
}

fn is_terminal_task_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "cancelled")
}

fn schedule_tick_is_active(tick: Option<&ScheduleTickRecord>) -> bool {
    tick.is_some_and(|tick| !is_terminal_task_state(&tick.state))
}

async fn latest_schedule_tick(
    client: &Client,
    schedule_id: &str,
) -> Result<Option<ScheduleTickRecord>, WorkflowRuntimeError> {
    let (task_table, _) = absurd_queue_tables(WORKFLOW_SCHEDULE_QUEUE)?;
    let row = sqlx::query(&format!(
        r#"
        select state, (params->>'scheduled_at')::timestamptz as scheduled_at
        from {task_table} t
        where t.task_name = $1
          and t.params->>'schedule_id' = $2
          and t.params ? 'scheduled_at'
        order by (t.params->>'scheduled_at')::timestamptz desc
        limit 1
        "#,
    ))
    .bind(WORKFLOW_SCHEDULE_TASK)
    .bind(schedule_id)
    .fetch_optional(client.pool())
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let state: String = row.try_get("state").map_err(|error| {
        WorkflowRuntimeError::Internal(format!("failed to read schedule tick state: {error}"))
    })?;
    let scheduled_at: DateTime<Utc> = row.try_get("scheduled_at").map_err(|error| {
        WorkflowRuntimeError::Internal(format!("failed to read schedule tick time: {error}"))
    })?;
    Ok(Some(ScheduleTickRecord {
        state,
        scheduled_at,
    }))
}

fn schedule_reconcile_occurrence(
    schedule: &RegisteredWorkflowSchedule,
    next_future: DateTime<Utc>,
    now: DateTime<Utc>,
    latest: Option<ScheduleTickRecord>,
) -> Result<DateTime<Utc>, WorkflowRuntimeError> {
    let WorkflowScheduleKind::Cron { .. } = schedule.kind else {
        return Ok(next_future);
    };
    let missed = latest_cron_occurrence_at_or_before(schedule, now)?;
    if latest
        .as_ref()
        .is_some_and(|tick| tick.scheduled_at < missed)
        || (latest.is_none()
            && now.signed_duration_since(missed).num_seconds()
                <= SCHEDULE_FIRST_REGISTRATION_CATCH_UP_GRACE_SECS)
    {
        // Coalesce all downtime into the newest missed occurrence. The tick
        // itself advances to the next future occurrence after it runs. A
        // schedule with no durable history gets this only during the short
        // first-registration grace window.
        return Ok(missed);
    }
    Ok(next_future)
}

fn latest_cron_occurrence_at_or_before(
    schedule: &RegisteredWorkflowSchedule,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, WorkflowRuntimeError> {
    let WorkflowScheduleKind::Cron { cron } = &schedule.kind else {
        return Err(WorkflowRuntimeError::Internal(
            "latest cron occurrence requested for an interval schedule".to_owned(),
        ));
    };
    let timezone = schedule.timezone.parse::<Tz>().map_err(|error| {
        WorkflowRuntimeError::BadRequest(format!(
            "invalid timezone {:?} for schedule {:?}: {error}",
            schedule.timezone, schedule.schedule_id
        ))
    })?;
    let parsed = Schedule::from_str(&normalize_cron_expression(cron)).map_err(|error| {
        WorkflowRuntimeError::BadRequest(format!(
            "invalid cron {:?} for schedule {:?}: {error}",
            cron, schedule.schedule_id
        ))
    })?;
    // `next_back` uses cron's DST-aware `prev_from` implementation. Starting
    // one second after `now` makes an occurrence exactly at `now` eligible,
    // while preserving the configured timezone and repeated fall-back hour.
    parsed
        .after(&(now.with_timezone(&timezone) + chrono::Duration::seconds(1)))
        .next_back()
        .map(|previous| previous.with_timezone(&Utc))
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest(format!(
                "cron {:?} for schedule {:?} produced no previous run",
                cron, schedule.schedule_id
            ))
        })
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
                // The tick only enqueues an idempotent occurrence. Keep the
                // durable scheduler retry policy for transient queue errors.
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
            let principal_foreign_id = parse_agent_principal(&input.input).map_err(absurd_error)?;
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
                                principal_foreign_id,
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
    let mut spec = sandbox.spec_for_workflow(&input.workflow_name)?;
    spec = spec
        .env("WORKFLOW_RUN_ID", ctx.run_id())
        .env("WORKFLOW_TASK_ID", ctx.task_id())
        .env("WORKFLOW_NAME", input.workflow_name.clone());
    if env::var_os("WORKFLOW_DIRS").is_none() && !sandbox_spec_has_env(&spec, "WORKFLOW_DIRS") {
        spec = spec.env("WORKFLOW_DIRS", default_workflow_dirs());
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
        Some("ctx.event.wait") => {
            let step = required_python_string(message, "step", "ctx.event.wait")?;
            let event_type = required_python_string(message, "event_type", "ctx.event.wait")?;
            let correlation_id =
                required_python_string(message, "correlation_id", "ctx.event.wait")?;
            let timeout = parse_optional_python_duration_seconds(message, "timeout_seconds")?;
            let event_name = python_workflow_event_name(event_type, correlation_id);
            match ctx
                .await_event::<Value>(
                    &event_name,
                    AwaitEventOptions {
                        step_name: Some(step.to_owned()),
                        timeout,
                    },
                )
                .await
            {
                Ok(value) => Ok(value),
                Err(absurd::Error::Suspend) => return Err(WorkflowRuntimeError::Suspend),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.agent_turn") => {
            let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
            match run_python_agent_turn(
                session_runtime.clone(),
                ctx,
                input,
                args,
                &request_id,
                None,
            )
            .await
            {
                Ok(value) => Ok(value),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.run_agents") => {
            match run_python_agent_batch(session_runtime.clone(), ctx, input, message, &request_id)
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
        Some("ctx.call_tool") => {
            // The task input is supplied by api-rs. Do not use a workflow-supplied
            // identity field from the RPC message when selecting authorization.
            match call_python_workflow_tool(&input.workflow_name, message).await {
                Ok(value) => Ok(value),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.post_to_slack") => {
            match post_python_slack_message(message, ctx, &request_id).await {
                Ok(value) => Ok(value),
                Err(error) => Err(error.to_string()),
            }
        }
        Some("ctx.update_slack") => match update_python_slack_message(message).await {
            Ok(value) => Ok(value),
            Err(error) => Err(error.to_string()),
        },
        Some("ctx.find_slack_message") => match find_python_slack_message(message).await {
            Ok(value) => Ok(value),
            Err(error) => Err(error.to_string()),
        },
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

fn required_python_string<'a>(
    message: &'a Value,
    field: &str,
    request_type: &str,
) -> Result<&'a str, WorkflowRuntimeError> {
    message
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest(format!("{request_type} requires a non-empty {field}"))
        })
}

fn parse_optional_python_duration_seconds(
    message: &Value,
    field: &str,
) -> Result<Option<Duration>, WorkflowRuntimeError> {
    let Some(value) = message.get(field) else {
        return Ok(None);
    };
    let seconds = value.as_f64().ok_or_else(|| {
        WorkflowRuntimeError::BadRequest(format!("ctx.event.wait {field} must be numeric"))
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "ctx.event.wait {field} must be a finite non-negative number"
        )));
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
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
    default_thread_key: Option<String>,
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
        default_thread_key.unwrap_or_else(|| {
            format!(
                "wf:{}:agent:{}",
                ctx.task_id().replace('-', ""),
                input.workflow_name
            )
        })
    });
    let harness_type = parse_agent_harness(&args)?.unwrap_or_else(|| input.harness_type.clone());
    let persona_id = args
        .get("persona_id")
        .or_else(|| args.get("persona"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let principal_foreign_id = parse_agent_principal(&args)?;
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
    if let Some(principal) = principal_foreign_id.as_deref() {
        object_insert(
            &mut execution_metadata,
            "principal_foreign_id",
            json!(principal),
        );
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
            principal_foreign_id,
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

#[derive(Debug, Clone, PartialEq)]
struct PythonAgentBatchItem {
    index: usize,
    name: String,
    args: Value,
}

fn parse_python_agent_batch(
    message: &Value,
    request_id: &str,
) -> Result<(Vec<PythonAgentBatchItem>, usize), WorkflowRuntimeError> {
    let raw_agents = message
        .get("agents")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            WorkflowRuntimeError::BadRequest("ctx.run_agents requires an agents array".to_owned())
        })?;
    if raw_agents.is_empty() {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.run_agents requires at least one agent".to_owned(),
        ));
    }
    if raw_agents.len() > MAX_AGENT_BATCH_SIZE {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "ctx.run_agents supports at most {MAX_AGENT_BATCH_SIZE} agents"
        )));
    }

    let max_concurrency = match message.get("max_concurrency") {
        Some(value) => value.as_u64().ok_or_else(|| {
            WorkflowRuntimeError::BadRequest(
                "ctx.run_agents max_concurrency must be an integer".to_owned(),
            )
        })? as usize,
        None => DEFAULT_AGENT_BATCH_CONCURRENCY,
    };
    if !(1..=MAX_AGENT_BATCH_CONCURRENCY).contains(&max_concurrency) {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "ctx.run_agents max_concurrency must be between 1 and {MAX_AGENT_BATCH_CONCURRENCY}"
        )));
    }

    let reserved_fields = [
        "thread_key",
        "message_id",
        "client_message_id",
        "idempotency_key",
        "execution_idempotency_key",
    ];
    let mut names = BTreeSet::new();
    let mut agents = Vec::with_capacity(raw_agents.len());
    for (index, raw_agent) in raw_agents.iter().enumerate() {
        let mut args = raw_agent.as_object().cloned().ok_or_else(|| {
            WorkflowRuntimeError::BadRequest(format!(
                "ctx.run_agents agent at index {index} must be an object"
            ))
        })?;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                WorkflowRuntimeError::BadRequest(format!(
                    "ctx.run_agents agent at index {index} requires a non-empty name"
                ))
            })?
            .to_owned();
        if name.len() > MAX_AGENT_BATCH_NAME_BYTES {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "ctx.run_agents agent name at index {index} must be at most {MAX_AGENT_BATCH_NAME_BYTES} bytes"
            )));
        }
        if !names.insert(name.clone()) {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "ctx.run_agents agent names must be unique; duplicate {name:?}"
            )));
        }
        if let Some(field) = reserved_fields
            .iter()
            .find(|field| args.contains_key(**field))
        {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "ctx.run_agents agent {name:?} cannot set reserved field {field:?}"
            )));
        }

        let metadata = args
            .entry("metadata".to_owned())
            .or_insert_with(|| json!({}));
        if !metadata.is_object() {
            *metadata = json!({});
        }
        object_insert(metadata, "workflow_agent_batch_name", json!(name));
        object_insert(metadata, "workflow_agent_batch_index", json!(index));
        object_insert(
            metadata,
            "workflow_agent_batch_request_id",
            json!(request_id),
        );

        agents.push(PythonAgentBatchItem {
            index,
            name,
            args: Value::Object(args),
        });
    }
    Ok((agents, max_concurrency))
}

async fn run_bounded_ordered<T, R, F, Fut>(
    items: Vec<T>,
    max_concurrency: usize,
    mut run: F,
) -> Vec<R>
where
    F: FnMut(T) -> Fut,
    Fut: Future<Output = R>,
{
    let item_count = items.len();
    let futures = items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let future = run(item);
            async move { (index, future.await) }
        })
        .collect::<Vec<_>>();
    let completed = stream::iter(futures)
        .buffer_unordered(max_concurrency)
        .collect::<Vec<_>>()
        .await;
    let mut ordered = (0..item_count).map(|_| None).collect::<Vec<_>>();
    for (index, result) in completed {
        ordered[index] = Some(result);
    }
    ordered
        .into_iter()
        .map(|result| result.expect("every bounded batch future must produce one result"))
        .collect()
}

async fn run_python_agent_batch(
    session_runtime: SessionRuntime,
    ctx: &TaskContext,
    input: &WorkflowTaskInput,
    message: &Value,
    request_id: &str,
) -> Result<Value, WorkflowRuntimeError> {
    let (agents, max_concurrency) = parse_python_agent_batch(message, request_id)?;
    let task_id = ctx.task_id().replace('-', "");
    let batch_request_id = request_id.to_owned();
    let outcomes = run_bounded_ordered(agents, max_concurrency, |agent| {
        let session_runtime = session_runtime.clone();
        let agent_slug = slugify(&agent.name);
        let default_thread_key = format!(
            "wf:{task_id}:agent-batch:{batch_request_id}:{}:{agent_slug}",
            agent.index,
        );
        let agent_request_id = format!("{batch_request_id}:{}", agent.index);
        async move {
            let result = run_python_agent_turn(
                session_runtime,
                ctx,
                input,
                agent.args.clone(),
                &agent_request_id,
                Some(default_thread_key),
            )
            .await;
            (agent, result)
        }
    })
    .await;

    let mut succeeded = 0;
    let mut failed = 0;
    let results = outcomes
        .into_iter()
        .map(|(agent, result)| match result {
            Ok(result) => {
                succeeded += 1;
                json!({
                    "index": agent.index,
                    "name": agent.name,
                    "ok": true,
                    "result": result,
                })
            }
            Err(error) => {
                failed += 1;
                json!({
                    "index": agent.index,
                    "name": agent.name,
                    "ok": false,
                    "error": error.to_string(),
                })
            }
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "results": results,
        "succeeded": succeeded,
        "failed": failed,
    }))
}

/// Returns the first arg key that holds a non-empty (trimmed) string, owned.
fn first_str_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_agent_principal(args: &Value) -> Result<Option<String>, WorkflowRuntimeError> {
    let Some(value) = args
        .get("principal")
        .or_else(|| args.get("principal_foreign_id"))
    else {
        return Ok(None);
    };
    let foreign_id = value.as_str().map(str::trim).ok_or_else(|| {
        WorkflowRuntimeError::BadRequest(
            "ctx.agent_turn principal must be a non-empty foreign ID".to_owned(),
        )
    })?;
    if foreign_id.is_empty() {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.agent_turn principal must be a non-empty foreign ID".to_owned(),
        ));
    }
    Ok(Some(foreign_id.to_owned()))
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

type WorkflowToolAllowlist = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

fn validate_workflow_tool_identifier(kind: &str, value: &str) -> Result<(), WorkflowRuntimeError> {
    let invalid = || {
        WorkflowRuntimeError::BadRequest(format!(
            "{WORKFLOW_TOOL_ALLOWLIST_ENV} contains an invalid {kind} identifier"
        ))
    };
    if value.is_empty()
        || value != value.trim()
        || value.chars().any(char::is_control)
        || value.len() > MAX_WORKFLOW_TOOL_IDENTIFIER_BYTES
    {
        return Err(invalid());
    }
    Ok(())
}

fn parse_workflow_tool_allowlist(raw: &str) -> Result<WorkflowToolAllowlist, WorkflowRuntimeError> {
    let value: Value = serde_json::from_str(raw.trim()).map_err(|_| {
        WorkflowRuntimeError::BadRequest(format!(
            "{WORKFLOW_TOOL_ALLOWLIST_ENV} must be valid JSON"
        ))
    })?;
    let workflows = value.as_object().ok_or_else(|| {
        WorkflowRuntimeError::BadRequest(format!(
            "{WORKFLOW_TOOL_ALLOWLIST_ENV} must be a JSON object"
        ))
    })?;
    let mut allowlist = BTreeMap::new();
    for (workflow_name, raw_tools) in workflows {
        validate_workflow_tool_identifier("workflow", workflow_name)?;
        let tools = raw_tools.as_object().ok_or_else(|| {
            WorkflowRuntimeError::BadRequest(format!(
                "{WORKFLOW_TOOL_ALLOWLIST_ENV} workflow entries must be JSON objects"
            ))
        })?;
        let mut parsed_tools = BTreeMap::new();
        for (tool_name, raw_methods) in tools {
            validate_workflow_tool_identifier("tool", tool_name)?;
            let methods = raw_methods.as_array().ok_or_else(|| {
                WorkflowRuntimeError::BadRequest(format!(
                    "{WORKFLOW_TOOL_ALLOWLIST_ENV} tool entries must be arrays"
                ))
            })?;
            let mut parsed_methods = BTreeSet::new();
            for raw_method in methods {
                let method = raw_method.as_str().ok_or_else(|| {
                    WorkflowRuntimeError::BadRequest(format!(
                        "{WORKFLOW_TOOL_ALLOWLIST_ENV} methods must be strings"
                    ))
                })?;
                validate_workflow_tool_identifier("method", method)?;
                parsed_methods.insert(method.to_owned());
            }
            parsed_tools.insert(tool_name.to_owned(), parsed_methods);
        }
        allowlist.insert(workflow_name.to_owned(), parsed_tools);
    }
    Ok(allowlist)
}

fn workflow_tool_allowlist_from_env() -> Result<Option<WorkflowToolAllowlist>, WorkflowRuntimeError>
{
    let raw = match env::var(WORKFLOW_TOOL_ALLOWLIST_ENV) {
        Ok(raw) => raw,
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "{WORKFLOW_TOOL_ALLOWLIST_ENV} must be valid UTF-8"
            )));
        }
    };
    parse_workflow_tool_allowlist(&raw).map(Some)
}

fn workflow_tool_method_allowed(
    allowlist: Option<&WorkflowToolAllowlist>,
    workflow_name: &str,
    tool: &str,
    method: &str,
) -> bool {
    let Some(allowlist) = allowlist else {
        return true;
    };
    let Some(tools) = allowlist.get(workflow_name) else {
        return true;
    };
    tools
        .get(tool)
        .is_some_and(|methods| methods.contains(method))
}

fn authorize_workflow_tool_method(
    workflow_name: &str,
    tool: &str,
    method: &str,
) -> Result<(), WorkflowRuntimeError> {
    let allowlist = workflow_tool_allowlist_from_env()?;
    if workflow_tool_method_allowed(allowlist.as_ref(), workflow_name, tool, method) {
        return Ok(());
    }
    Err(WorkflowRuntimeError::Disabled(
        "ctx.call_tool tool/method is not allowed for this workflow".to_owned(),
    ))
}

fn tool_proxy_transport_error() -> WorkflowRuntimeError {
    WorkflowRuntimeError::Upstream("ctx.call_tool proxy_transport_error".to_owned())
}

fn tool_proxy_http_error(status: reqwest::StatusCode) -> WorkflowRuntimeError {
    WorkflowRuntimeError::Upstream(format!(
        "ctx.call_tool proxy_http_error status={}",
        status.as_u16()
    ))
}

fn tool_proxy_json_error(status: reqwest::StatusCode) -> WorkflowRuntimeError {
    WorkflowRuntimeError::Upstream(format!(
        "ctx.call_tool proxy_invalid_json status={}",
        status.as_u16()
    ))
}

async fn call_python_workflow_tool(
    workflow_name: &str,
    message: &Value,
) -> Result<Value, WorkflowRuntimeError> {
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
    authorize_workflow_tool_method(workflow_name, tool, method)?;

    let base_url = env::var(WORKFLOW_TOOL_API_URL_ENV).map_err(|_| {
        WorkflowRuntimeError::BadRequest(format!(
            "{WORKFLOW_TOOL_API_URL_ENV} must be set for ctx.call_tool"
        ))
    })?;
    let base_url = base_url.trim_end_matches('/');
    let url = format!("{base_url}/tools/{tool}/{method}");
    let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
    let request = reqwest::Client::new().post(&url).json(&args);
    let response = request
        .send()
        .await
        .map_err(|_| tool_proxy_transport_error())?;
    let status = response.status();
    if !status.is_success() {
        return Err(tool_proxy_http_error(status));
    }
    response
        .json()
        .await
        .map_err(|_| tool_proxy_json_error(status))
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

async fn update_python_slack_message(message: &Value) -> Result<Value, WorkflowRuntimeError> {
    let channel = required_python_string(message, "channel", "ctx.update_slack")?;
    let ts = required_python_string(message, "ts", "ctx.update_slack")?;
    let text = required_python_string(message, "text", "ctx.update_slack")?;
    let args = message.get("args").cloned().unwrap_or_else(|| json!({}));
    let payload = python_slack_update_payload(channel, ts, text, &args)?;

    let token = env::var("SLACK_BOT_TOKEN")
        .or_else(|_| env::var("SLACK_BOT_TOKEN_OVERRIDE"))
        .map_err(|_| {
            WorkflowRuntimeError::BadRequest(
                "SLACK_BOT_TOKEN or SLACK_BOT_TOKEN_OVERRIDE must be set".to_owned(),
            )
        })?;
    let response = send_slack_update(&token, payload).await?;
    serde_json::to_value(slack_update_result_from_response(channel, ts, response))
        .map_err(WorkflowRuntimeError::from)
}

fn validate_slack_reconciliation_field(
    message: &Value,
    field: &str,
    operation: &str,
    max_bytes: usize,
) -> Result<String, WorkflowRuntimeError> {
    let value = message
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| WorkflowRuntimeError::BadRequest(format!("{operation} requires {field}")))?;
    if value.trim().is_empty()
        || value.len() > max_bytes
        || value.chars().any(|character| character.is_control())
    {
        return Err(WorkflowRuntimeError::BadRequest(format!(
            "{operation} requires a valid {field}"
        )));
    }
    Ok(value.to_owned())
}

fn validate_slack_reconciliation_thread_ts(value: &str) -> Result<(), WorkflowRuntimeError> {
    let mut components = value.split('.');
    let seconds = components.next().unwrap_or_default();
    let fraction = components.next().unwrap_or_default();
    if components.next().is_some()
        || seconds.is_empty()
        || fraction.is_empty()
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.find_slack_message requires a valid thread_ts".to_owned(),
        ));
    }
    Ok(())
}

fn slack_reconciliation_match(
    channel: &str,
    client_msg_id: &str,
    thread_ts: Option<&str>,
    messages: &Value,
) -> Result<Option<Value>, WorkflowRuntimeError> {
    let messages = messages.as_array().ok_or_else(|| {
        WorkflowRuntimeError::Upstream(
            "Slack reconciliation returned malformed messages".to_owned(),
        )
    })?;

    for message in messages {
        if !message.is_object() {
            return Err(WorkflowRuntimeError::Upstream(
                "Slack reconciliation returned malformed messages".to_owned(),
            ));
        }
        if message.get("client_msg_id").and_then(Value::as_str) != Some(client_msg_id) {
            continue;
        }
        if message
            .get("channel")
            .and_then(Value::as_str)
            .is_some_and(|message_channel| message_channel != channel)
        {
            continue;
        }
        if let Some(thread_ts) = thread_ts {
            let is_requested_thread = message.get("ts").and_then(Value::as_str) == Some(thread_ts)
                || message.get("thread_ts").and_then(Value::as_str) == Some(thread_ts);
            if !is_requested_thread {
                continue;
            }
        }
        let ts = message
            .get("ts")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                WorkflowRuntimeError::Upstream(
                    "Slack reconciliation returned a matching message without ts".to_owned(),
                )
            })?;
        let mut result = json!({"found": true, "channel": channel, "ts": ts});
        if let Some(message_thread_ts) = message
            .get("thread_ts")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            result["thread_ts"] = json!(message_thread_ts);
        }
        return Ok(Some(result));
    }
    Ok(None)
}

fn slack_reconciliation_next_cursor(body: &Value) -> Result<Option<String>, WorkflowRuntimeError> {
    let has_more = match body.get("has_more") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            WorkflowRuntimeError::Upstream(
                "Slack reconciliation returned malformed pagination".to_owned(),
            )
        })?,
    };
    let next_cursor = match body.get("response_metadata") {
        None | Some(Value::Null) => None,
        Some(metadata) => {
            let metadata = metadata.as_object().ok_or_else(|| {
                WorkflowRuntimeError::Upstream(
                    "Slack reconciliation returned malformed pagination".to_owned(),
                )
            })?;
            match metadata.get("next_cursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(cursor)) if !cursor.trim().is_empty() => Some(cursor.to_owned()),
                Some(Value::String(_)) => None,
                Some(_) => {
                    return Err(WorkflowRuntimeError::Upstream(
                        "Slack reconciliation returned malformed pagination".to_owned(),
                    ));
                }
            }
        }
    };
    if has_more && next_cursor.is_none() {
        return Err(WorkflowRuntimeError::Upstream(
            "Slack reconciliation returned incomplete pagination".to_owned(),
        ));
    }
    Ok(next_cursor)
}

fn slack_reconciliation_endpoint(thread_ts: Option<&str>) -> &'static str {
    if thread_ts.is_some() {
        "https://slack.com/api/conversations.replies"
    } else {
        "https://slack.com/api/conversations.history"
    }
}

async fn find_python_slack_message(message: &Value) -> Result<Value, WorkflowRuntimeError> {
    let operation = "ctx.find_slack_message";
    let channel = validate_slack_reconciliation_field(message, "channel", operation, 255)?;
    let client_msg_id =
        validate_slack_reconciliation_field(message, "client_msg_id", operation, 512)?;
    let thread_ts = message
        .get("thread_ts")
        .map(|value| {
            value.as_str().ok_or_else(|| {
                WorkflowRuntimeError::BadRequest(format!("{operation} requires a valid thread_ts"))
            })
        })
        .transpose()?;
    if let Some(thread_ts) = thread_ts {
        if thread_ts.trim().is_empty() || thread_ts.len() > 64 {
            return Err(WorkflowRuntimeError::BadRequest(
                "ctx.find_slack_message requires a valid thread_ts".to_owned(),
            ));
        }
        validate_slack_reconciliation_thread_ts(thread_ts)?;
    }

    let token = env::var("SLACK_BOT_TOKEN")
        .or_else(|_| env::var("SLACK_BOT_TOKEN_OVERRIDE"))
        .map_err(|_| {
            WorkflowRuntimeError::BadRequest(
                "SLACK_BOT_TOKEN or SLACK_BOT_TOKEN_OVERRIDE must be set".to_owned(),
            )
        })?;
    let endpoint = slack_reconciliation_endpoint(thread_ts);
    let client = reqwest::Client::new();
    let mut cursor: Option<String> = None;

    for page in 0..MAX_SLACK_RECONCILIATION_PAGES {
        let mut url = reqwest::Url::parse(endpoint).map_err(|_| {
            WorkflowRuntimeError::Internal("invalid Slack reconciliation endpoint".to_owned())
        })?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("channel", &channel);
            query.append_pair("limit", SLACK_RECONCILIATION_PAGE_LIMIT);
            if let Some(thread_ts) = thread_ts {
                query.append_pair("ts", thread_ts);
            }
            if let Some(cursor) = cursor.as_deref() {
                query.append_pair("cursor", cursor);
            }
        }
        let request = client.get(url).bearer_auth(&token);

        let response = request.send().await.map_err(|_| {
            WorkflowRuntimeError::Upstream("Slack reconciliation lookup failed".to_owned())
        })?;
        if !response.status().is_success() {
            return Err(WorkflowRuntimeError::Upstream(
                "Slack reconciliation lookup failed".to_owned(),
            ));
        }
        let body = response.json::<Value>().await.map_err(|_| {
            WorkflowRuntimeError::Upstream(
                "Slack reconciliation returned malformed response".to_owned(),
            )
        })?;
        if body.get("ok") != Some(&Value::Bool(true)) {
            return Err(WorkflowRuntimeError::Upstream(
                "Slack reconciliation lookup failed".to_owned(),
            ));
        }
        let messages = body.get("messages").ok_or_else(|| {
            WorkflowRuntimeError::Upstream(
                "Slack reconciliation returned malformed messages".to_owned(),
            )
        })?;
        if let Some(result) =
            slack_reconciliation_match(&channel, &client_msg_id, thread_ts, messages)?
        {
            return Ok(result);
        }
        cursor = slack_reconciliation_next_cursor(&body)?;
        if cursor.is_none() {
            return Ok(json!({"found": false, "channel": channel}));
        }
        if page + 1 == MAX_SLACK_RECONCILIATION_PAGES {
            return Err(WorkflowRuntimeError::Upstream(
                "Slack reconciliation lookup exceeded bounded pagination".to_owned(),
            ));
        }
    }
    Err(WorkflowRuntimeError::Upstream(
        "Slack reconciliation lookup exceeded bounded pagination".to_owned(),
    ))
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
    if let Some(mrkdwn) = args.get("mrkdwn").and_then(Value::as_bool) {
        payload["mrkdwn"] = json!(mrkdwn);
    }
    if let Some(blocks) = args.get("blocks") {
        payload["blocks"] = blocks.clone();
    }
    if let Some(username) = args.get("username").and_then(Value::as_str) {
        payload["username"] = json!(username);
    }
    if let Some(icon_emoji) = args.get("icon_emoji").and_then(Value::as_str) {
        payload["icon_emoji"] = json!(icon_emoji);
    }
    if let Some(no_attribution) = args.get("no_attribution").and_then(Value::as_bool) {
        payload["no_attribution"] = json!(no_attribution);
    }
    payload
}

fn python_slack_update_payload(
    channel: &str,
    ts: &str,
    text: &str,
    args: &Value,
) -> Result<Value, WorkflowRuntimeError> {
    for (field, value) in [("channel", channel), ("ts", ts), ("text", text)] {
        if value.trim().is_empty() {
            return Err(WorkflowRuntimeError::BadRequest(format!(
                "ctx.update_slack requires non-empty {field}"
            )));
        }
    }
    if !args.is_object() {
        return Err(WorkflowRuntimeError::BadRequest(
            "ctx.update_slack args must be an object".to_owned(),
        ));
    }

    let mut payload = json!({
        "channel": channel,
        "ts": ts,
        "text": text,
    });
    // Keep this allowlist deliberately narrower than chat.postMessage. In
    // particular, never forward caller-selected identity, token, or endpoint
    // fields. `mrkdwn` is retained for heartbeat's common message contract.
    if let Some(blocks) = args.get("blocks")
        && blocks.is_array()
    {
        payload["blocks"] = blocks.clone();
    }
    if let Some(mrkdwn) = args.get("mrkdwn").and_then(Value::as_bool) {
        payload["mrkdwn"] = json!(mrkdwn);
    }
    Ok(payload)
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

async fn send_slack_update(token: &str, payload: Value) -> Result<Value, WorkflowRuntimeError> {
    let response: Value = reqwest::Client::new()
        .post("https://slack.com/api/chat.update")
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(WorkflowRuntimeError::Upstream(format!(
            "Slack chat.update failed: {}",
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

fn slack_update_result_from_response(channel: &str, ts: &str, response: Value) -> SlackPostResult {
    SlackPostResult {
        channel: response
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or(channel)
            .to_owned(),
        ts: response
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or(ts)
            .to_owned(),
    }
}

struct AgentTurnRequest {
    thread_key: String,
    harness_type: HarnessType,
    persona_id: Option<String>,
    principal_foreign_id: Option<String>,
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
        principal_foreign_id,
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
        .create_or_get_session_with_principal(
            &thread_key,
            &harness_type,
            persona_id.as_deref(),
            Some(session_metadata),
            HarnessConflictPolicy::Reject,
            principal_foreign_id.as_deref(),
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
                    result_text: agent_turn_result_text(&event.payload, &output_lines),
                    output_lines,
                });
            }
            "session.execution_failed" | "session.execution_cancelled" => {
                let result = AgentTurnResult {
                    thread_key: thread_key.into_string(),
                    execution_id: execution.execution_id,
                    status: event.event_type,
                    result_text: agent_turn_result_text(&event.payload, &output_lines),
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

fn agent_turn_result_text(terminal_payload: &Value, output_lines: &[String]) -> String {
    if let Some(result_text) = terminal_payload.get("result_text").and_then(Value::as_str) {
        return result_text.to_owned();
    }

    output_lines
        .iter()
        .rev()
        .find_map(|line| completed_final_answer_text(line))
        .unwrap_or_default()
}

fn completed_final_answer_text(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if value.get("type").and_then(Value::as_str) == Some("assistant.message") {
        let payload = value.get("payload")?;
        if !matches!(
            payload.get("phase").and_then(Value::as_str),
            Some("final_answer" | "answer") | None
        ) {
            return None;
        }
        return non_empty_text(payload.get("text"));
    }

    if !matches!(
        value.get("method").and_then(Value::as_str),
        Some("item/completed")
    ) && !matches!(
        value.get("type").and_then(Value::as_str),
        Some("item.completed")
    ) {
        return None;
    }

    let item = value
        .get("item")
        .or_else(|| value.pointer("/params/item"))?;
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("agentMessage" | "agent_message")
    ) || !matches!(
        item.get("phase").and_then(Value::as_str),
        Some("final_answer" | "answer") | None
    ) {
        return None;
    }
    non_empty_text(item.get("text"))
}

fn non_empty_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
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
    IronControl(#[from] IronControlError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn python_event_names_are_collision_free() {
        assert_ne!(
            python_workflow_event_name("review:a", "b"),
            python_workflow_event_name("review", "a:b")
        );
        assert_eq!(
            python_workflow_event_name("review", "change:42"),
            "python:[\"review\",\"change:42\"]"
        );
    }

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
    fn agent_turn_uses_terminal_result_text_instead_of_stream_deltas() {
        let output_lines = vec![
            json!({
                "method": "item/reasoning/summaryTextDelta",
                "params": {"delta": "internal reasoning"}
            })
            .to_string(),
            json!({
                "method": "item/agentMessage/delta",
                "params": {"delta": "streamed draft"}
            })
            .to_string(),
        ];

        assert_eq!(
            agent_turn_result_text(
                &json!({"result_text": "Canonical final answer."}),
                &output_lines
            ),
            "Canonical final answer."
        );
    }

    #[test]
    fn agent_turn_result_fallback_uses_only_completed_final_answer() {
        let output_lines = vec![
            json!({
                "method": "item/reasoning/summaryTextDelta",
                "params": {"delta": "internal reasoning"}
            })
            .to_string(),
            json!({
                "method": "item/agentMessage/delta",
                "params": {"delta": "streamed commentary"}
            })
            .to_string(),
            json!({
                "method": "item/completed",
                "params": {
                    "item": {
                        "type": "agentMessage",
                        "phase": "commentary",
                        "text": "Commentary update."
                    }
                }
            })
            .to_string(),
            json!({
                "type": "item.completed",
                "item": {
                    "type": "agentMessage",
                    "phase": "final_answer",
                    "text": "Fallback final answer."
                }
            })
            .to_string(),
        ];

        assert_eq!(
            agent_turn_result_text(&json!({}), &output_lines),
            "Fallback final answer."
        );
    }

    #[test]
    fn agent_turn_result_fallback_does_not_return_untyped_deltas() {
        let output_lines = vec![
            json!({"type": "reasoning.delta", "delta": "internal reasoning"}).to_string(),
            json!({"type": "item.agentMessage.delta", "delta": "ambiguous draft"}).to_string(),
        ];

        assert_eq!(agent_turn_result_text(&json!({}), &output_lines), "");
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
    fn parse_agent_principal_accepts_foreign_id_and_rejects_invalid_values() {
        assert_eq!(
            parse_agent_principal(&json!({"principal": " finance-automation "})).unwrap(),
            Some("finance-automation".to_owned())
        );
        assert_eq!(
            parse_agent_principal(&json!({"principal_foreign_id": "support"})).unwrap(),
            Some("support".to_owned())
        );
        assert_eq!(parse_agent_principal(&json!({})).unwrap(), None);
        assert!(parse_agent_principal(&json!({"principal": "  "})).is_err());
        assert!(parse_agent_principal(&json!({"principal": true})).is_err());
    }

    #[test]
    fn parse_agent_batch_requires_unique_names_and_adds_metadata() {
        let message = json!({
            "type": "ctx.run_agents",
            "max_concurrency": 2,
            "agents": [
                {
                    "name": "correctness",
                    "text": "Review correctness",
                    "principal": "security-reviewers",
                    "metadata": {"pr": 42}
                },
                {"name": "security", "text": "Review security"}
            ]
        });

        let (agents, max_concurrency) = parse_python_agent_batch(&message, "7").unwrap();

        assert_eq!(max_concurrency, 2);
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.name.as_str())
                .collect::<Vec<_>>(),
            vec!["correctness", "security"]
        );
        assert_eq!(agents[0].args.pointer("/metadata/pr"), Some(&json!(42)));
        assert_eq!(
            agents[0].args.get("principal"),
            Some(&json!("security-reviewers"))
        );
        assert_eq!(
            agents[0]
                .args
                .pointer("/metadata/workflow_agent_batch_name"),
            Some(&json!("correctness"))
        );
        assert_eq!(
            agents[1]
                .args
                .pointer("/metadata/workflow_agent_batch_index"),
            Some(&json!(1))
        );
        assert_eq!(
            agents[1]
                .args
                .pointer("/metadata/workflow_agent_batch_request_id"),
            Some(&json!("7"))
        );
    }

    #[test]
    fn parse_agent_batch_rejects_duplicate_names_and_identity_overrides() {
        let duplicate = json!({
            "agents": [
                {"name": "security", "text": "first"},
                {"name": "security", "text": "second"}
            ]
        });
        let error = parse_python_agent_batch(&duplicate, "1").unwrap_err();
        assert!(error.to_string().contains("names must be unique"));

        let overridden_identity = json!({
            "agents": [{
                "name": "security",
                "text": "review",
                "thread_key": "workflow:shared"
            }]
        });
        let error = parse_python_agent_batch(&overridden_identity, "1").unwrap_err();
        assert!(error.to_string().contains("reserved field \"thread_key\""));
    }

    #[test]
    fn parse_agent_batch_enforces_size_and_concurrency_bounds() {
        let empty = json!({"agents": []});
        let error = parse_python_agent_batch(&empty, "1").unwrap_err();
        assert!(error.to_string().contains("at least one agent"));

        for max_concurrency in [0, MAX_AGENT_BATCH_CONCURRENCY + 1] {
            let message = json!({
                "agents": [{"name": "correctness", "text": "review"}],
                "max_concurrency": max_concurrency,
            });
            let error = parse_python_agent_batch(&message, "1").unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("max_concurrency must be between")
            );
        }

        let too_many = json!({
            "agents": (0..=MAX_AGENT_BATCH_SIZE)
                .map(|index| json!({"name": format!("reviewer-{index}"), "text": "review"}))
                .collect::<Vec<_>>(),
        });
        let error = parse_python_agent_batch(&too_many, "1").unwrap_err();
        assert!(error.to_string().contains("supports at most"));
    }

    #[tokio::test]
    async fn bounded_batch_limits_concurrency_and_restores_input_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let results = run_bounded_ordered(vec![0_u64, 1, 2, 3], 2, {
            let active = active.clone();
            let peak = peak.clone();
            move |item| {
                let active = active.clone();
                let peak = peak.clone();
                async move {
                    let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now_active, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(4 * (4 - item))).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    if item == 2 {
                        Err("review failed")
                    } else {
                        Ok(item)
                    }
                }
            }
        })
        .await;

        assert_eq!(results, vec![Ok(0), Ok(1), Err("review failed"), Ok(3)]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
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
    fn list_runs_limit_is_clamped_to_supported_range() {
        assert_eq!(list_runs_limit(-1), 1);
        assert_eq!(list_runs_limit(50), 50);
        assert_eq!(list_runs_limit(1_000), 1_000);
        assert_eq!(list_runs_limit(10_000), 1_000);
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
    fn scheduled_cron_input_carries_the_authoritative_occurrence() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "hourly_report",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
            "no_delivery": true,
            "input": {"report": "daily", "metadata": {"keep": "this"}},
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let child_input = schedule_workflow_input(&schedule, scheduled_at);
        let scheduled_for = scheduled_at.to_rfc3339();

        assert_eq!(child_input["report"], json!("daily"));
        assert_eq!(child_input["scheduled_for"], json!(scheduled_for));
        assert_eq!(
            child_input["metadata"],
            json!({
                "keep": "this",
                "source": "workflow_schedule",
                "workflow_name": "hourly_report",
                "no_delivery": true,
                "schedule_id": "hourly_report",
                "scheduled_for": scheduled_for,
            })
        );
    }

    #[test]
    fn scheduled_cron_input_overwrites_caller_provenance_spoof() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "authoritative_schedule",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
            "no_delivery": true,
            "input": {
                "scheduled_for": "1999-01-01T00:00:00Z",
                "metadata": {
                    "schedule_id": "caller_schedule",
                    "scheduled_for": "1999-01-01T00:00:00Z",
                    "no_delivery": true,
                },
            },
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let child_input = schedule_workflow_input(&schedule, scheduled_at);
        let scheduled_for = scheduled_at.to_rfc3339();

        assert_eq!(child_input["scheduled_for"], json!(scheduled_for));
        assert_eq!(
            child_input["metadata"]["schedule_id"],
            json!("authoritative_schedule")
        );
        assert_eq!(
            child_input["metadata"]["scheduled_for"],
            json!(scheduled_for)
        );
        assert_eq!(child_input["metadata"]["no_delivery"], json!(true));
    }

    #[test]
    fn scheduled_input_rebuilds_authoritative_metadata_for_non_object_spoof() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "authoritative_schedule",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
            "no_delivery": true,
            "input": {
                "metadata": "caller-controlled metadata",
                "scheduled_for": "1999-01-01T00:00:00Z"
            },
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let child_input = schedule_workflow_input(&schedule, scheduled_at);
        let scheduled_for = scheduled_at.to_rfc3339();

        assert_eq!(
            child_input["metadata"],
            json!({
                "source": "workflow_schedule",
                "workflow_name": "hourly_report",
                "no_delivery": true,
                "schedule_id": "authoritative_schedule",
                "scheduled_for": scheduled_for,
            })
        );
    }

    #[test]
    fn coalesced_cron_occurrence_is_the_child_provenance() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "hourly_report",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
            "input": {"metadata": {"no_delivery": false}},
        }))
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 25, 0).unwrap();
        let latest = Some(ScheduleTickRecord {
            state: "completed".to_owned(),
            scheduled_at: Utc.with_ymd_and_hms(2026, 6, 16, 10, 0, 0).unwrap(),
        });
        let next_future = Utc.with_ymd_and_hms(2026, 6, 16, 13, 0, 0).unwrap();
        let coalesced = schedule_reconcile_occurrence(&schedule, next_future, now, latest).unwrap();
        let child_input = schedule_workflow_input(&schedule, coalesced);
        let scheduled_for = coalesced.to_rfc3339();

        assert_eq!(
            coalesced,
            Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap()
        );
        assert_eq!(child_input["scheduled_for"], json!(scheduled_for));
        assert_eq!(
            child_input["metadata"]["scheduled_for"],
            json!(scheduled_for)
        );
        assert_eq!(
            child_input["metadata"]["schedule_id"],
            json!("hourly_report")
        );
    }

    #[test]
    fn scheduled_cron_input_formats_dst_occurrence_as_rfc3339_utc() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "daily_report",
            "schedule_id": "daily_report",
            "cron": "30 1 * * *",
            "timezone": "America/Los_Angeles",
            "enabled": true,
        }))
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 9, 35, 0).unwrap();
        let scheduled_at = latest_cron_occurrence_at_or_before(&schedule, now).unwrap();
        let child_input = schedule_workflow_input(&schedule, scheduled_at);
        let scheduled_for = scheduled_at.to_rfc3339();

        assert_eq!(
            scheduled_at,
            Utc.with_ymd_and_hms(2026, 11, 1, 9, 30, 0).unwrap()
        );
        assert_eq!(child_input["scheduled_for"], json!(scheduled_for));
        assert_eq!(
            child_input["metadata"]["scheduled_for"],
            json!(scheduled_for)
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(child_input["scheduled_for"].as_str().unwrap())
                .is_ok()
        );
    }

    #[test]
    fn scheduled_interval_input_carries_occurrence_and_delivery_metadata() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "slack_backfill",
            "schedule_id": "slack_backfill",
            "interval_seconds": 600,
            "enabled": true,
            "no_delivery": true,
            "input": {"batch": 4},
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 10, 0).unwrap();
        let child_input = schedule_workflow_input(&schedule, scheduled_at);
        let scheduled_for = scheduled_at.to_rfc3339();

        assert_eq!(child_input["batch"], json!(4));
        assert_eq!(child_input["scheduled_for"], json!(scheduled_for));
        assert_eq!(
            child_input["metadata"]["schedule_id"],
            json!("slack_backfill")
        );
        assert_eq!(
            child_input["metadata"]["scheduled_for"],
            json!(scheduled_for)
        );
        assert_eq!(child_input["metadata"]["no_delivery"], json!(true));
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
    fn cron_reconcile_coalesces_downtime_to_one_latest_missed_occurrence() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "weekday_report",
            "schedule_id": "weekday_report",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
        }))
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 25, 0).unwrap();
        let next_future = Utc.with_ymd_and_hms(2026, 6, 16, 13, 0, 0).unwrap();
        let latest = Some(ScheduleTickRecord {
            state: "completed".to_owned(),
            scheduled_at: Utc.with_ymd_and_hms(2026, 6, 16, 10, 0, 0).unwrap(),
        });
        assert_eq!(
            schedule_reconcile_occurrence(&schedule, next_future, now, latest).unwrap(),
            Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn cron_reconcile_does_not_repeat_already_recorded_missed_occurrence() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "hourly_report",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
        }))
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 25, 0).unwrap();
        let next_future = Utc.with_ymd_and_hms(2026, 6, 16, 13, 0, 0).unwrap();
        let latest = Some(ScheduleTickRecord {
            state: "completed".to_owned(),
            scheduled_at: Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap(),
        });

        assert_eq!(
            schedule_reconcile_occurrence(&schedule, next_future, now, latest).unwrap(),
            next_future
        );
    }

    #[test]
    fn active_overdue_tick_blocks_a_second_reconciliation() {
        let tick = ScheduleTickRecord {
            state: "running".to_owned(),
            scheduled_at: Utc.with_ymd_and_hms(2026, 6, 16, 10, 0, 0).unwrap(),
        };
        assert!(schedule_tick_is_active(Some(&tick)));
        assert!(!schedule_tick_is_active(None));
        assert!(!schedule_tick_is_active(Some(&ScheduleTickRecord {
            state: "completed".to_owned(),
            scheduled_at: tick.scheduled_at,
        })));
    }

    #[test]
    fn cron_reconcile_keeps_the_next_future_occurrence_on_time() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "hourly_report",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
        }))
        .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let next_future = Utc.with_ymd_and_hms(2026, 6, 16, 13, 0, 0).unwrap();
        let latest = Some(ScheduleTickRecord {
            state: "completed".to_owned(),
            scheduled_at: Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap(),
        });

        assert_eq!(
            schedule_reconcile_occurrence(&schedule, next_future, now, latest).unwrap(),
            next_future
        );
    }

    #[test]
    fn cron_first_registration_catches_up_only_within_grace() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "hourly_report",
            "cron": "0 * * * *",
            "timezone": "UTC",
            "enabled": true,
        }))
        .unwrap();
        let next_future = Utc.with_ymd_and_hms(2026, 6, 16, 13, 0, 0).unwrap();
        let within_grace = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 30).unwrap();
        assert_eq!(
            schedule_reconcile_occurrence(&schedule, next_future, within_grace, None).unwrap(),
            Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap()
        );

        let outside_grace = Utc.with_ymd_and_hms(2026, 6, 16, 12, 2, 0).unwrap();
        assert_eq!(
            schedule_reconcile_occurrence(&schedule, next_future, outside_grace, None).unwrap(),
            next_future
        );
    }

    #[test]
    fn cron_reconcile_preserves_dst_fallback_occurrence_timezone() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "daily_report",
            "schedule_id": "daily_report",
            "cron": "30 1 * * *",
            "timezone": "America/Los_Angeles",
            "enabled": true,
        }))
        .unwrap();
        // 09:35 UTC is 01:35 PST after the 2026 fall-back transition.
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 9, 35, 0).unwrap();
        assert_eq!(
            latest_cron_occurrence_at_or_before(&schedule, now).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 1, 9, 30, 0).unwrap()
        );
    }

    #[test]
    fn delayed_interval_tick_coalesces_multiple_intervals_once() {
        let schedule = normalize_schedule(json!({
            "workflow_name": "hourly_report",
            "schedule_id": "hourly_report",
            "interval_seconds": 600,
            "enabled": true,
        }))
        .unwrap();
        let scheduled_at = Utc.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 6, 16, 12, 35, 0).unwrap();
        let next = next_schedule_time_after_tick(&schedule, scheduled_at, now).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 6, 16, 12, 40, 0).unwrap());
        // Re-running the same tick uses the same scheduled occurrence and
        // therefore computes the same single next future occurrence.
        assert_eq!(
            next_schedule_time_after_tick(&schedule, scheduled_at, now).unwrap(),
            next
        );
    }

    #[test]
    fn scheduled_workflows_fail_closed_without_a_side_effect_ledger() {
        let options = scheduled_workflow_spawn_options("schedule:test:occurrence".to_owned());
        assert_eq!(options.max_attempts, Some(1));
        assert_eq!(
            options.idempotency_key.as_deref(),
            Some("schedule:test:occurrence")
        );
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
            "company_context_embeddings",
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
                "mrkdwn": true,
                "username": "The Date Goblin",
                "icon_emoji": ":female_mage:",
            }),
        );

        assert_eq!(payload["channel"], json!("C123"));
        assert_eq!(payload["text"], json!("hello"));
        assert_eq!(payload["client_msg_id"], json!("client-1"));
        assert_eq!(payload["thread_ts"], json!("1710000000.000100"));
        assert_eq!(payload["reply_broadcast"], json!(true));
        assert_eq!(payload["unfurl_links"], json!(true));
        assert_eq!(payload["unfurl_media"], json!(true));
        assert_eq!(payload["mrkdwn"], json!(true));
        assert_eq!(payload["username"], json!("The Date Goblin"));
        assert_eq!(payload["icon_emoji"], json!(":female_mage:"));
    }

    #[test]
    fn python_slack_payload_omits_custom_identity_by_default() {
        let payload = python_slack_message_payload("C123", "hello", "client-1", &json!({}));

        assert!(payload.get("username").is_none());
        assert!(payload.get("icon_emoji").is_none());
    }

    #[test]
    fn python_slack_update_payload_forwards_only_safe_fields() {
        let payload = python_slack_update_payload(
            "C123",
            "1710000000.000100",
            "Heartbeat is healthy.",
            &json!({
                "blocks": [{"type": "section"}],
                "mrkdwn": true,
                "reply_broadcast": false,
                "username": "attacker-controlled",
                "icon_emoji": ":skull:",
                "token": "xoxb-not-forwarded",
                "endpoint": "https://attacker.invalid",
                "unfurl_links": true,
                "unfurl_media": true,
            }),
        )
        .unwrap();

        assert_eq!(
            payload,
            json!({
                "channel": "C123",
                "ts": "1710000000.000100",
                "text": "Heartbeat is healthy.",
                "blocks": [{"type": "section"}],
                "mrkdwn": true,
            })
        );
    }

    #[test]
    fn python_slack_update_payload_requires_non_empty_fields() {
        for (channel, ts, text, field) in [
            ("", "1710000000.000100", "hello", "channel"),
            ("   ", "1710000000.000100", "hello", "channel"),
            ("C123", "", "hello", "ts"),
            ("C123", "   ", "hello", "ts"),
            ("C123", "1710000000.000100", "", "text"),
            ("C123", "1710000000.000100", "   ", "text"),
        ] {
            let error = python_slack_update_payload(channel, ts, text, &json!({})).unwrap_err();
            assert!(error.to_string().contains(&format!("non-empty {field}")));
        }
    }

    #[test]
    fn python_slack_update_result_is_stable_when_slack_omits_identifiers() {
        let result = slack_update_result_from_response("C123", "1710000000.000100", json!({}));

        assert_eq!(result.channel, "C123");
        assert_eq!(result.ts, "1710000000.000100");
    }

    #[test]
    fn slack_reconciliation_exact_match_returns_only_safe_identity_fields() {
        let result = slack_reconciliation_match(
            "C123",
            "client-1",
            None,
            &json!([
                {
                    "channel": "C123",
                    "client_msg_id": "wrong-id",
                    "ts": "1710000000.000001",
                    "text": "secret wrong message"
                },
                {
                    "channel": "C123",
                    "client_msg_id": "client-1",
                    "ts": "1710000000.000100",
                    "text": "secret message",
                    "blocks": [{"type": "section", "text": {"text": "secret"}}],
                    "metadata": {"event_type": "private"}
                }
            ]),
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result,
            json!({"found": true, "channel": "C123", "ts": "1710000000.000100"})
        );
        assert!(result.get("text").is_none());
        assert!(result.get("blocks").is_none());
        assert!(result.get("metadata").is_none());
    }

    #[test]
    fn slack_reconciliation_rejects_wrong_channel_and_client_id() {
        let result = slack_reconciliation_match(
            "C123",
            "client-1",
            None,
            &json!([
                {
                    "channel": "C999",
                    "client_msg_id": "client-1",
                    "ts": "1710000000.000100"
                },
                {
                    "channel": "C123",
                    "client_msg_id": "client-2",
                    "ts": "1710000000.000101"
                }
            ]),
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn slack_reconciliation_thread_lookup_isolated_from_other_threads() {
        let messages = json!([
            {
                "channel": "C123",
                "client_msg_id": "client-1",
                "ts": "1710000000.000200",
                "thread_ts": "1710000000.000999",
                "text": "other thread secret"
            },
            {
                "channel": "C123",
                "client_msg_id": "client-1",
                "ts": "1710000000.000201",
                "thread_ts": "1710000000.000100",
                "text": "requested thread secret"
            }
        ]);
        let result =
            slack_reconciliation_match("C123", "client-1", Some("1710000000.000100"), &messages)
                .unwrap()
                .unwrap();
        assert_eq!(
            result,
            json!({
                "found": true,
                "channel": "C123",
                "ts": "1710000000.000201",
                "thread_ts": "1710000000.000100"
            })
        );

        assert!(
            slack_reconciliation_match("C123", "client-1", Some("1710000000.000555"), &messages)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn slack_reconciliation_validates_malformed_inputs_and_pagination() {
        for (field, value) in [
            ("channel", json!("")),
            ("channel", json!("C\n123")),
            ("client_msg_id", json!("")),
            ("client_msg_id", json!("client\u{0000}id")),
        ] {
            let mut request = json!({});
            request[field] = value;
            let error =
                validate_slack_reconciliation_field(&request, field, "ctx.find_slack_message", 255)
                    .unwrap_err();
            assert!(error.to_string().contains(field));
        }

        assert!(validate_slack_reconciliation_thread_ts("not-a-slack-ts").is_err());
        assert!(validate_slack_reconciliation_thread_ts("1710000000.000100").is_ok());
        assert!(slack_reconciliation_match("C123", "client-1", None, &json!([null])).is_err());
        assert!(
            slack_reconciliation_next_cursor(&json!({
                "has_more": true,
                "response_metadata": {}
            }))
            .is_err()
        );
        assert_eq!(
            slack_reconciliation_next_cursor(&json!({
                "has_more": true,
                "response_metadata": {"next_cursor": "cursor-2"}
            }))
            .unwrap(),
            Some("cursor-2".to_owned())
        );
    }

    #[test]
    fn slack_reconciliation_selects_only_the_scoped_official_endpoint() {
        assert_eq!(
            slack_reconciliation_endpoint(None),
            "https://slack.com/api/conversations.history"
        );
        assert_eq!(
            slack_reconciliation_endpoint(Some("1710000000.000100")),
            "https://slack.com/api/conversations.replies"
        );
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
        let value = call_python_workflow_tool(
            "any-workflow",
            &json!({
                "type": "ctx.call_tool",
                "tool": "time",
                "method": "now",
                "args": {},
            }),
        )
        .await
        .unwrap();
        assert_eq!(value["tool"], json!("time"));
        assert_eq!(value["method"], json!("now"));
        assert!(value.pointer("/output/utc").is_some());
    }

    #[test]
    fn workflow_tool_allowlist_requires_exact_methods_and_scopes_by_workflow() {
        let allowlist = parse_workflow_tool_allowlist(
            r#"{
                "daily_digest": {"slack": ["send_message"], "time": ["now"]},
                "other_workflow": {"slack": ["read_message"]}
            }"#,
        )
        .unwrap();

        assert!(workflow_tool_method_allowed(
            Some(&allowlist),
            "daily_digest",
            "slack",
            "send_message"
        ));
        assert!(!workflow_tool_method_allowed(
            Some(&allowlist),
            "daily_digest",
            "slack",
            "delete_message"
        ));
        assert!(!workflow_tool_method_allowed(
            Some(&allowlist),
            "other_workflow",
            "slack",
            "send_message"
        ));
        assert!(!workflow_tool_method_allowed(
            Some(&allowlist),
            "daily_digest",
            "github",
            "list"
        ));
    }

    #[test]
    fn workflow_tool_allowlist_preserves_unconfigured_workflow_compatibility() {
        let allowlist = parse_workflow_tool_allowlist(r#"{"configured": {"slack": []}}"#).unwrap();

        assert!(workflow_tool_method_allowed(
            None,
            "configured",
            "slack",
            "send_message"
        ));
        assert!(workflow_tool_method_allowed(
            Some(&allowlist),
            "unconfigured",
            "slack",
            "send_message"
        ));
    }

    #[test]
    fn workflow_tool_allowlist_rejects_malformed_configuration() {
        for raw in [
            "not-json",
            "",
            " ",
            "[]",
            r#"{"workflow": {"slack": "send_message"}}"#,
            r#"{"workflow": {"slack": ["send_message", 1]}}"#,
            r#"{" workflow": {"slack": ["send_message"]}}"#,
            r#"{"workflow": {"slack ": ["send_message"]}}"#,
            r#"{"workflow": {"slack": ["send_message "]}}"#,
            r#"{"workflow\n": {"slack": ["send_message"]}}"#,
            r#"{"workflow": {"slack": ["send\u0000message"]}}"#,
        ] {
            assert!(parse_workflow_tool_allowlist(raw).is_err(), "config: {raw}");
        }
        let overlong_workflow = format!(
            r#"{{"{}": {{"slack": ["send_message"]}}}}"#,
            "w".repeat(MAX_WORKFLOW_TOOL_IDENTIFIER_BYTES + 1)
        );
        let overlong_tool = format!(
            r#"{{"workflow": {{"{}": ["send_message"]}}}}"#,
            "t".repeat(MAX_WORKFLOW_TOOL_IDENTIFIER_BYTES + 1)
        );
        let overlong_method = format!(
            r#"{{"workflow": {{"slack": ["{}"]}}}}"#,
            "m".repeat(MAX_WORKFLOW_TOOL_IDENTIFIER_BYTES + 1)
        );
        for raw in [overlong_workflow, overlong_tool, overlong_method] {
            assert!(
                parse_workflow_tool_allowlist(&raw).is_err(),
                "config: {raw}"
            );
        }
    }

    #[test]
    fn tool_proxy_errors_are_bounded_and_do_not_include_request_or_response_data() {
        let sensitive = [
            "provider response body",
            "bearer secret token",
            "query text",
            "request header",
            "request payload",
        ];
        let errors = [
            tool_proxy_transport_error(),
            tool_proxy_http_error(reqwest::StatusCode::UNAUTHORIZED),
            tool_proxy_json_error(reqwest::StatusCode::OK),
        ];
        for error in errors {
            let rendered = error.to_string();
            assert!(rendered.len() <= 128);
            for value in sensitive {
                assert!(
                    !rendered.contains(value),
                    "error leaked {value:?}: {rendered}"
                );
            }
        }
        assert_eq!(
            tool_proxy_http_error(reqwest::StatusCode::UNAUTHORIZED).to_string(),
            "ctx.call_tool proxy_http_error status=401"
        );
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
                    "principal": true,
                },
                {
                    "workflow_name": "manual_workflow",
                    "source_path": "workflows/manual_workflow.py",
                    "principal": "finance-automation",
                    "event_triggers": [{
                        "workflow_name": "manual_workflow",
                        "source_path": "workflows/manual_workflow.py",
                        "spec": {
                            "name": "manual-actions",
                            "event_name_prefix": "slack.block_action.manual."
                        }
                    }],
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
        assert_eq!(metadata.event_triggers.len(), 1);
        assert_eq!(metadata.event_triggers[0].spec.name, "manual-actions");
        assert_eq!(
            metadata.schedules[0].get("workflow_name"),
            Some(&json!("scheduled_workflow"))
        );
        assert_eq!(
            metadata.principals.get("scheduled_workflow"),
            Some(&WorkflowPrincipalDeclaration::Managed)
        );
        assert_eq!(
            metadata.principals.get("manual_workflow"),
            Some(&WorkflowPrincipalDeclaration::Existing(
                "finance-automation".to_owned()
            ))
        );
    }

    #[test]
    fn discovery_metadata_preserves_workflow_principal_oid() {
        let payload: PythonWorkflowDiscoveryPayload = serde_json::from_value(json!({
            "workflows": [{
                "workflow_name": "oid_workflow",
                "source_path": "workflows/oid_workflow.py",
                "principal": " prn_01k2m3n4p5 ",
            }],
        }))
        .unwrap();

        let metadata = metadata_from_discovery_payload(payload);

        assert_eq!(
            metadata.principals.get("oid_workflow"),
            Some(&WorkflowPrincipalDeclaration::Existing(
                "prn_01k2m3n4p5".to_owned()
            ))
        );
    }

    #[test]
    fn event_trigger_registry_validates_names_and_enablement() {
        let trigger = RegisteredWorkflowEventTrigger {
            workflow_name: "heartbeat_feedback".to_owned(),
            source_path: "workflows/heartbeat_feedback.py".to_owned(),
            spec: WorkflowEventTriggerSpec {
                name: "heartbeat-actions".to_owned(),
                event_name_prefix: "slack.block_action.phai.heartbeat.".to_owned(),
            },
        };
        let metadata = PythonWorkflowMetadata {
            event_triggers: vec![trigger.clone()],
            workflow_names: BTreeSet::from(["heartbeat_feedback".to_owned()]),
            ..PythonWorkflowMetadata::default()
        };

        assert_eq!(
            build_event_trigger_registry(&metadata, &WorkflowEnablement::all())
                .unwrap()
                .len(),
            1
        );
        assert!(
            build_event_trigger_registry(
                &metadata,
                &WorkflowEnablement::allowlist("other_workflow")
            )
            .unwrap()
            .is_empty()
        );

        let duplicate = PythonWorkflowMetadata {
            event_triggers: vec![trigger.clone(), trigger],
            ..PythonWorkflowMetadata::default()
        };
        let error = build_event_trigger_registry(&duplicate, &WorkflowEnablement::all())
            .expect_err("duplicate event trigger names must fail discovery");
        assert!(
            error
                .to_string()
                .contains("duplicate workflow event trigger")
        );
    }

    #[test]
    fn workflow_principal_foreign_id_is_derived_from_workflow_name() {
        assert_eq!(
            canonical_workflow_principal_foreign_id("nightly_report"),
            "workflow-nightly-report"
        );
        assert_eq!(
            canonical_workflow_principal_foreign_id("Managing Partner Daily Briefing"),
            "workflow-managing-partner-daily-briefing"
        );
    }

    #[test]
    fn workflow_principal_labels_keep_extensible_metadata_only() {
        let labels = workflow_principal_labels("nightly_report");

        assert!(!labels.contains_key("kind"));
        assert!(!labels.contains_key("purpose"));
        assert_eq!(
            labels.get("workflow_name").map(String::as_str),
            Some("nightly_report")
        );
    }

    #[test]
    fn required_workflow_principal_fails_closed_when_unregistered() {
        let assignments = WorkflowPrincipalAssignments {
            required: BTreeSet::from(["nightly_report".to_owned()]),
            registered: BTreeMap::new(),
        };

        let error = assignments
            .principal_for_workflow("nightly_report")
            .expect_err("required workflow principal should not fall back");

        assert!(matches!(error, WorkflowRuntimeError::Internal(_)));
        assert!(error.to_string().contains("nightly_report"));
        assert!(error.to_string().contains("WORKFLOW_PRINCIPAL"));
    }

    #[test]
    fn optional_workflow_principal_uses_shared_principal() {
        let assignments = WorkflowPrincipalAssignments::default();

        assert_eq!(
            assignments
                .principal_for_workflow("nightly_report")
                .expect("optional workflow should be allowed"),
            None
        );
    }

    #[tokio::test]
    async fn workflow_principal_requires_workflow_host_sandbox() {
        let discovery = PythonWorkflowMetadata {
            principals: BTreeMap::from([(
                "nightly_report".to_owned(),
                WorkflowPrincipalDeclaration::Managed,
            )]),
            workflow_names: BTreeSet::from(["nightly_report".to_owned()]),
            ..PythonWorkflowMetadata::default()
        };

        let registrar = WorkflowPrincipalRegistrar::new(IronControlClient::new(
            "http://127.0.0.1:1",
            "test-key",
        ));
        let error = match prepare_workflow_host_sandbox(
            None,
            registrar,
            &discovery,
            &WorkflowEnablement::all(),
        )
        .await
        {
            Ok(_) => panic!("workflow principal should require workflow-host sandboxing"),
            Err(error) => error,
        };

        assert!(matches!(error, WorkflowRuntimeError::BadRequest(_)));
        assert!(error.to_string().contains("WORKFLOW_HOST_SANDBOX"));
        assert!(error.to_string().contains("nightly_report"));
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

    #[test]
    fn discovery_metadata_preserves_standard_webhooks_auth() {
        let payload: PythonWorkflowDiscoveryPayload = serde_json::from_value(json!({
            "workflows": [
                {
                    "workflow_name": "feed_ingest",
                    "source_path": "workflows/feed_ingest.py",
                    "webhooks": [
                        {
                            "workflow_name": "feed_ingest",
                            "source_path": "workflows/feed_ingest.py",
                            "spec": {
                                "slug": "feed-ingest",
                                "auth": {
                                    "type": "standard_webhooks",
                                    "secret_ref": "FEED_WEBHOOK_SECRET"
                                },
                                "trigger_key": {
                                    "type": "header",
                                    "header": "webhook-id"
                                }
                            }
                        }
                    ]
                }
            ],
        }))
        .unwrap();

        let metadata = metadata_from_discovery_payload(payload);
        let registry =
            build_webhook_registry(&metadata, &WorkflowEnablement::allowlist("feed_ingest"))
                .unwrap();
        let webhook = registry.get("feed-ingest").unwrap();

        assert!(matches!(
            &webhook.spec.auth,
            WorkflowWebhookAuth::StandardWebhooks { secret_ref }
                if secret_ref == "FEED_WEBHOOK_SECRET"
        ));
        assert!(matches!(
            &webhook.spec.trigger_key,
            Some(WorkflowWebhookTriggerKey::Header { header }) if header == "webhook-id"
        ));
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
                    "principal": true,
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
                    "principal": true,
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
        assert_eq!(
            metadata.principals.keys().cloned().collect::<Vec<_>>(),
            vec!["allowed_workflow".to_owned()]
        );
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
