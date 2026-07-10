mod cleanup;
mod title_generator;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use centaur_iron_control::SessionRegistrar;
use centaur_sandbox_core::{
    Mount, RepoCacheAccess, SandboxBackend, SandboxCapabilities as BackendSandboxCapabilities,
    SandboxError, SandboxId, SandboxIoGuard, SandboxRead, SandboxSpec, SandboxStatus, SandboxWrite,
};
use centaur_sandbox_manager::{
    SandboxManager, SandboxReaper, SandboxReaperConfig, WarmPoolConfig, WarmPoolError,
    WarmPoolManager, WarmSandboxSpecFactory,
};
use centaur_session_core::{
    ExecutionStatus, HarnessType, MessageRole, SandboxCapabilities as SessionSandboxCapabilities,
    SandboxRepoCacheAccess as SessionRepoCacheAccess, Session, SessionEvent, SessionExecution,
    SessionMessageInput, ThreadKey,
};
use centaur_session_sqlx::{
    PgSessionStore, SandboxCapacityCandidate, SessionEventListener, SessionStoreError,
    default_metadata,
};
use centaur_telemetry::{
    export_thread_trace_root_span, record_sandbox_warm_pool_claim,
    record_session_execution_finished, record_session_execution_started, record_session_failure,
    record_session_first_token_latency, set_span_parent_trace,
};
use dashmap::{DashMap, DashSet};
use futures_util::{FutureExt, SinkExt, Stream, StreamExt, future::BoxFuture, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io,
    sync::Mutex,
    time::{Instant, Interval, MissedTickBehavior, interval_at, sleep, timeout},
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};
use tracing::{Instrument, Span, debug, error, info, info_span, warn};
use uuid::Uuid;

pub use cleanup::SessionSandboxCleanupConfig;
pub use title_generator::SessionTitleGenerationError;
use title_generator::{
    OpenAiSessionTitleGenerator, sanitize_session_title, session_title_source_from_parts,
};

pub const SESSION_OUTPUT_LINE_EVENT: &str = "session.output.line";
pub const SESSION_FIRST_TOKEN_EVENT: &str = "session.first_token";

const EVENT_STREAM_SAFETY_POLL_INTERVAL: Duration = Duration::from_secs(30);
const STEERING_STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const STEERING_STARTUP_RETRY_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_PIPE_MAX_REATTACH_ATTEMPTS: u32 = 3;
const SESSION_PIPE_REATTACH_DELAY: Duration = Duration::from_millis(500);
const STDOUT_OWNER_LEASE: Duration = Duration::from_secs(45);
const STDOUT_OWNER_RENEW_INTERVAL: Duration = Duration::from_secs(10);
const EXECUTION_HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(500);
const EXECUTION_HANDOFF_DB_TIMEOUT: Duration = Duration::from_secs(5);
/// A live execution can briefly have no sandbox while it moves from queued
/// through warm-sandbox assignment. A periodic adoption scan must not fail a
/// young row it observes in that window.
const PRE_SANDBOX_ORPHAN_GRACE: Duration = Duration::from_secs(120);
const COMPONENT_SESSION_RUNTIME: &str = "session_runtime";
const SANDBOX_REPOS_MOUNT_PATH: &str = "/home/agent/github";
const PUBLIC_REPO_CACHE_SUBPATH: &str = "public";
const CENTAUR_SKILL_DIRS_ENV: &str = "CENTAUR_SKILL_DIRS";
const CENTAUR_PUBLIC_SKILL_DIRS_ENV: &str = "CENTAUR_PUBLIC_SKILL_DIRS";
const SANDBOX_REPO_CACHE_LABEL: &str = "centaur.sandbox_repo_cache";
const OBSERVABILITY_TOOL_BLOCKLIST: &str =
    "vlogs,vmetrics,grafana,centaur_investigator,centaur-investigator";

type SandboxSpecFactory = Arc<
    dyn Fn(&ThreadKey, &str, &HarnessType, Option<&PersonaContext>) -> SandboxSpec + Send + Sync,
>;
type SessionInputSink = FramedWrite<SandboxWrite, LinesCodec>;
type ExecutionSpanRegistry = Arc<Mutex<HashMap<String, Span>>>;
type SessionPipeMap = Arc<DashMap<String, SessionPipe>>;
type SessionPipeOpenLocks = Arc<DashMap<String, Arc<Mutex<()>>>>;
type ToolHostCallLocks = Arc<DashMap<String, Arc<Mutex<()>>>>;
type SessionTitleThreadSet = Arc<DashSet<ThreadKey>>;
type SessionTitleGenerator = Arc<
    dyn Fn(String) -> BoxFuture<'static, Result<String, SessionTitleGenerationError>> + Send + Sync,
>;

#[derive(Clone)]
pub struct SessionRuntime {
    store: PgSessionStore,
    sandbox_runtime: SandboxRuntime,
    sandbox_pipes: SessionPipeMap,
    sandbox_pipe_open_locks: SessionPipeOpenLocks,
    tool_host_call_locks: ToolHostCallLocks,
    execution_spans: ExecutionSpanRegistry,
    iron_control: Option<SessionRegistrar>,
    warm_pool: Option<Arc<WarmPoolManager>>,
    personas: Option<Arc<PersonaRegistry>>,
    session_title_generator: Option<SessionTitleGenerator>,
    session_title_in_flight: SessionTitleThreadSet,
    session_title_rerun_requested: SessionTitleThreadSet,
    capacity: Option<Arc<SandboxCapacityController>>,
    stdout_owner_id: String,
    /// Set once a shutdown handoff begins; fences new stdout-owner claims
    /// so an execution cannot start on a control plane that is about to
    /// exit and release its leases.
    shutting_down: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug)]
pub struct SandboxCapacityConfig {
    pub max_running: usize,
    pub hot_idle_grace: Duration,
}

impl SandboxCapacityConfig {
    pub fn is_enabled(&self) -> bool {
        self.max_running > 0
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PersonaRegistry {
    personas: BTreeMap<String, PersonaDefinition>,
    default_persona_id: Option<String>,
    overlay_chain: Vec<String>,
    public_source_roots: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaDefinition {
    pub id: String,
    pub source_root: String,
    pub source_path: String,
    pub source_ref: Option<String>,
    pub prompt_hash: String,
    #[serde(skip_serializing)]
    pub prompt: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaSummary {
    pub id: String,
    pub source_root: String,
    pub source_path: String,
    pub source_ref: Option<String>,
    pub prompt_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaContext {
    pub persona_id: String,
    pub source_root: String,
    pub source_path: String,
    pub source_ref: Option<String>,
    pub prompt_hash: String,
    pub defaulted: bool,
    pub overlay_chain: Vec<String>,
}

impl PersonaRegistry {
    pub fn new(
        personas: impl IntoIterator<Item = PersonaDefinition>,
        default_persona_id: Option<String>,
        overlay_chain: Vec<String>,
    ) -> Result<Self, String> {
        let personas = personas
            .into_iter()
            .map(|persona| (persona.id.clone(), persona))
            .collect::<BTreeMap<_, _>>();
        if let Some(default_persona_id) = default_persona_id.as_deref()
            && !personas.contains_key(default_persona_id)
        {
            return Err(format!(
                "CENTAUR_DEFAULT_PERSONA {default_persona_id:?} is not in the deployed persona registry"
            ));
        }
        Ok(Self {
            personas,
            default_persona_id,
            overlay_chain,
            public_source_roots: BTreeSet::new(),
        })
    }

    pub fn with_public_source_roots(
        mut self,
        public_source_roots: impl IntoIterator<Item = String>,
    ) -> Self {
        self.public_source_roots = public_source_roots.into_iter().collect();
        self
    }

    pub fn summaries(&self) -> Vec<PersonaSummary> {
        self.personas
            .values()
            .map(|persona| PersonaSummary {
                id: persona.id.clone(),
                source_root: persona.source_root.clone(),
                source_path: persona.source_path.clone(),
                source_ref: persona.source_ref.clone(),
                prompt_hash: persona.prompt_hash.clone(),
            })
            .collect()
    }

    fn default_persona_id(&self) -> Option<&str> {
        self.default_persona_id.as_deref()
    }

    fn default_persona_id_for_access(&self, access: &SessionRepoCacheAccess) -> Option<&str> {
        let default_persona_id = self.default_persona_id()?;
        let persona = self.get(default_persona_id)?;
        if self.persona_allowed_for_access(persona, access) {
            Some(default_persona_id)
        } else {
            None
        }
    }

    fn get(&self, persona_id: &str) -> Option<&PersonaDefinition> {
        self.personas.get(persona_id)
    }

    fn persona_allowed_for_access(
        &self,
        persona: &PersonaDefinition,
        access: &SessionRepoCacheAccess,
    ) -> bool {
        !matches!(access, SessionRepoCacheAccess::Public)
            || self.public_source_roots.contains(&persona.source_root)
    }

    fn context_for_access(
        &self,
        persona_id: &str,
        defaulted: bool,
        access: &SessionRepoCacheAccess,
    ) -> Result<PersonaContext, String> {
        let Some(persona) = self.get(persona_id) else {
            return Err(format!(
                "persona {persona_id:?} is not available in this deployment"
            ));
        };
        if !self.persona_allowed_for_access(persona, access) {
            return Err(format!(
                "persona {persona_id:?} is not available for public sandbox repo-cache access"
            ));
        }
        Ok(PersonaContext {
            persona_id: persona.id.clone(),
            source_root: persona.source_root.clone(),
            source_path: persona.source_path.clone(),
            source_ref: persona.source_ref.clone(),
            prompt_hash: persona.prompt_hash.clone(),
            defaulted,
            overlay_chain: self.overlay_chain.clone(),
        })
    }
}

#[derive(Clone)]
pub struct SandboxRuntime {
    manager: Arc<SandboxManager>,
    spec_factory: SandboxSpecFactory,
    warm_spec_factory: Option<WarmSandboxSpecFactory>,
    workload_key: Option<String>,
    /// The harness warm sandboxes boot with. A warm claim is only valid for a
    /// session on the same harness; other sessions get a cold sandbox.
    warm_harness: Option<HarnessType>,
}

#[derive(Clone, Debug)]
pub enum SandboxWorkloadMode {
    MockAppServer {
        image: String,
    },
    CodexAppServer {
        image: String,
        env: Vec<(String, String)>,
        mounts: Vec<Mount>,
        /// The harness used for warm sandboxes and as the workload default.
        /// Per-session sandboxes run the session's own harness.
        harness: HarnessType,
    },
}

/// What to do when a session already exists with a different harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessConflictPolicy {
    /// Fail with [`SessionStoreError::HarnessConflict`] (the default).
    Reject,
    /// Restart the thread on the requested harness: stop the old sandbox,
    /// clear the harness thread state, and switch the session row over. The
    /// new harness starts with no conversational memory.
    Restart,
}

/// Result of [`SessionRuntime::create_or_get_session`].
#[derive(Clone, Debug)]
pub struct CreateOrGetSessionOutcome {
    pub session: Session,
    /// True when the session was restarted onto a different harness because
    /// the request asked for [`HarnessConflictPolicy::Restart`].
    pub harness_switched: bool,
}

/// Outcome of [`SessionRuntime::drain`]: the sandboxes that were stopped and
/// any that failed to stop (with the backend error text).
#[derive(Debug, Default)]
pub struct DrainReport {
    pub stopped: Vec<String>,
    pub failed: Vec<DrainFailure>,
}

#[derive(Debug)]
pub struct DrainFailure {
    pub sandbox_id: String,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct WorkflowSandboxCleanupReport {
    pub stopped: Vec<String>,
    pub missing: Vec<String>,
    pub failed: Vec<DrainFailure>,
}

#[derive(Debug)]
pub struct ExecuteSessionInput {
    pub idempotency_key: Option<String>,
    pub metadata: Option<Value>,
    pub input_lines: Vec<String>,
    pub idle_timeout_ms: Option<u64>,
    pub max_duration_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct InterruptExecutionOutcome {
    pub interrupted: bool,
    pub execution_id: Option<String>,
}

#[derive(Debug)]
pub struct ToolHostCallInput {
    pub principal_id: String,
    pub token_id: Option<String>,
    pub tool_name: String,
    pub method: String,
    pub arguments: Value,
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct ToolHostCallOutput {
    pub sandbox_id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_status: Option<i32>,
    pub timed_out: bool,
}

#[derive(Clone)]
struct SessionPipe {
    stdin: Arc<Mutex<SessionInputSink>>,
}

#[derive(Serialize)]
struct ToolHostRequest {
    id: String,
    tool: String,
    method: String,
    arguments: Value,
    principal_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_id: Option<String>,
    timeout_seconds: u64,
}

#[derive(Deserialize)]
struct ToolHostResponse {
    status: Option<i32>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    timed_out: bool,
}

/// Shared handles threaded through background session tasks (stdout pump,
/// terminal-output recording, max-duration failure, idle pause).
#[derive(Clone)]
struct RuntimeContext {
    store: PgSessionStore,
    manager: Arc<SandboxManager>,
    sandbox_pipes: SessionPipeMap,
    execution_spans: ExecutionSpanRegistry,
    stdout_owner_id: String,
}

struct SandboxCapacityController {
    store: PgSessionStore,
    manager: Arc<SandboxManager>,
    sandbox_pipes: SessionPipeMap,
    lock: Mutex<()>,
    config: SandboxCapacityConfig,
}

impl SandboxCapacityController {
    fn new(
        store: PgSessionStore,
        manager: Arc<SandboxManager>,
        sandbox_pipes: SessionPipeMap,
        config: SandboxCapacityConfig,
    ) -> Self {
        Self {
            store,
            manager,
            sandbox_pipes,
            lock: Mutex::new(()),
            config,
        }
    }

    async fn run_with_capacity<T, F, Fut>(
        &self,
        protected_thread_key: &ThreadKey,
        trigger_execution_id: &str,
        operation: &'static str,
        action: F,
    ) -> Result<T, SessionRuntimeError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, SessionRuntimeError>>,
    {
        let _guard = self.lock.lock().await;
        self.ensure_running_slot(protected_thread_key, trigger_execution_id, operation)
            .await?;
        action().await
    }

    async fn ensure_running_slot(
        &self,
        protected_thread_key: &ThreadKey,
        trigger_execution_id: &str,
        operation: &'static str,
    ) -> Result<(), SessionRuntimeError> {
        let running = self.running_slot_count().await?;
        if running < self.config.max_running {
            return Ok(());
        }

        let mut slots_needed = running.saturating_sub(self.config.max_running) + 1;
        let mut stopped_warm = 0usize;
        let mut paused_idle = 0usize;
        let mut stale_candidates_reconciled = 0usize;

        for sandbox_id in self
            .store
            .reserve_ready_warm_sandboxes_for_eviction(candidate_fetch_limit(slots_needed))
            .await?
        {
            if slots_needed == 0 {
                break;
            }
            let id = SandboxId::new(sandbox_id.as_str());
            match self.manager.status(&id).await {
                Ok(status) if status_consumes_running_slot(&status) => {}
                Ok(_) | Err(SandboxError::NotFound(_)) => {
                    let _ = self
                        .store
                        .mark_warm_sandbox_failed(
                            sandbox_id.as_str(),
                            "not running during sandbox capacity admission",
                        )
                        .await;
                    continue;
                }
                Err(error) => {
                    let failure =
                        format!("status failed during sandbox capacity admission: {error}");
                    let _ = self
                        .store
                        .mark_warm_sandbox_failed(sandbox_id.as_str(), &failure)
                        .await;
                    return Err(SessionRuntimeError::Sandbox(error));
                }
            }

            match self.manager.stop(&id).await {
                Ok(()) | Err(SandboxError::NotFound(_)) => {
                    stopped_warm += 1;
                    slots_needed -= 1;
                    let _ = self
                        .store
                        .mark_warm_sandbox_failed(
                            sandbox_id.as_str(),
                            "stopped for sandbox capacity pressure",
                        )
                        .await;
                    info!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "sandbox_capacity_warm_stopped",
                        sandbox_id,
                        trigger_thread_key = %protected_thread_key,
                        trigger_execution_id,
                        operation,
                        max_running = self.config.max_running,
                        "stopped warm sandbox for capacity"
                    );
                }
                Err(error) => {
                    let failure = format!("stop failed during sandbox capacity admission: {error}");
                    let _ = self
                        .store
                        .mark_warm_sandbox_failed(sandbox_id.as_str(), &failure)
                        .await;
                    return Err(SessionRuntimeError::Sandbox(error));
                }
            }
        }

        if slots_needed > 0 {
            loop {
                let candidates = self
                    .store
                    .list_sandbox_capacity_candidates(
                        Some(protected_thread_key),
                        self.config.hot_idle_grace,
                        candidate_fetch_limit(slots_needed),
                    )
                    .await?;
                if candidates.is_empty() {
                    break;
                }

                let mut made_progress = false;
                for candidate in candidates {
                    if slots_needed == 0 {
                        break;
                    }
                    match self
                        .pause_capacity_candidate(
                            &candidate,
                            protected_thread_key,
                            trigger_execution_id,
                            operation,
                        )
                        .await?
                    {
                        CapacityCandidateAction::Paused => {
                            paused_idle += 1;
                            slots_needed -= 1;
                            made_progress = true;
                        }
                        CapacityCandidateAction::ReconciledStale => {
                            stale_candidates_reconciled += 1;
                            made_progress = true;
                        }
                        CapacityCandidateAction::Skipped => {}
                    }
                }

                if slots_needed == 0 || !made_progress {
                    break;
                }
            }
        }

        if slots_needed == 0 {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "sandbox_capacity_admitted",
                trigger_thread_key = %protected_thread_key,
                trigger_execution_id,
                operation,
                running_before = running,
                max_running = self.config.max_running,
                stopped_warm,
                paused_idle,
                stale_candidates_reconciled,
                "admitted sandbox operation under capacity pressure"
            );
            return Ok(());
        }

        Err(SessionRuntimeError::CapacityExceeded {
            max_running: self.config.max_running,
            running,
            operation,
        })
    }

    async fn pause_capacity_candidate(
        &self,
        candidate: &SandboxCapacityCandidate,
        protected_thread_key: &ThreadKey,
        trigger_execution_id: &str,
        operation: &'static str,
    ) -> Result<CapacityCandidateAction, SessionRuntimeError> {
        let id = SandboxId::new(candidate.sandbox_id.as_str());
        match self.manager.status(&id).await {
            Ok(SandboxStatus::Running | SandboxStatus::Created | SandboxStatus::Unknown(_)) => {}
            Ok(SandboxStatus::Suspended) => {
                return Ok(CapacityCandidateAction::Skipped);
            }
            Ok(SandboxStatus::Stopped | SandboxStatus::Gone) => {
                return self.reconcile_stale_capacity_candidate(candidate).await;
            }
            Err(SandboxError::NotFound(_)) => {
                return self.reconcile_stale_capacity_candidate(candidate).await;
            }
            Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
        }

        self.sandbox_pipes.remove(candidate.sandbox_id.as_str());
        match self.manager.pause(&id).await {
            Ok(()) => {
                self.store
                    .append_event(
                        &candidate.thread_key,
                        candidate.latest_execution_id.as_deref(),
                        "session.sandbox_paused",
                        json!({
                            "thread_key": candidate.thread_key.as_str(),
                            "sandbox_id": candidate.sandbox_id.as_str(),
                            "reason": "capacity_pressure",
                            "trigger_thread_key": protected_thread_key.as_str(),
                            "trigger_execution_id": trigger_execution_id,
                            "operation": operation,
                            "last_active_at": candidate.last_active_at,
                            "hot_idle_grace_ms": duration_millis_u64(self.config.hot_idle_grace),
                            "max_running": self.config.max_running,
                        }),
                    )
                    .await?;
                info!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "sandbox_capacity_idle_paused",
                    thread_key = %candidate.thread_key,
                    sandbox_id = %candidate.sandbox_id,
                    trigger_thread_key = %protected_thread_key,
                    trigger_execution_id,
                    operation,
                    last_active_at = %candidate.last_active_at,
                    max_running = self.config.max_running,
                    "paused idle sandbox for capacity"
                );
                Ok(CapacityCandidateAction::Paused)
            }
            Err(error) => {
                self.store
                    .append_event(
                        &candidate.thread_key,
                        candidate.latest_execution_id.as_deref(),
                        "session.sandbox_pause_failed",
                        json!({
                            "thread_key": candidate.thread_key.as_str(),
                            "sandbox_id": candidate.sandbox_id.as_str(),
                            "reason": "capacity_pressure",
                            "trigger_thread_key": protected_thread_key.as_str(),
                            "trigger_execution_id": trigger_execution_id,
                            "operation": operation,
                            "error": error.to_string(),
                        }),
                    )
                    .await?;
                Err(SessionRuntimeError::Sandbox(error))
            }
        }
    }

    async fn reconcile_stale_capacity_candidate(
        &self,
        candidate: &SandboxCapacityCandidate,
    ) -> Result<CapacityCandidateAction, SessionRuntimeError> {
        let cleared = self
            .store
            .clear_sandbox_id_if_matches(&candidate.thread_key, candidate.sandbox_id.as_str())
            .await?;
        if cleared {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "sandbox_capacity_stale_reconciled",
                thread_key = %candidate.thread_key,
                sandbox_id = %candidate.sandbox_id,
                "cleared stale sandbox assignment during capacity admission"
            );
            Ok(CapacityCandidateAction::ReconciledStale)
        } else {
            Ok(CapacityCandidateAction::Skipped)
        }
    }

    async fn running_slot_count(&self) -> Result<usize, SessionRuntimeError> {
        Ok(self
            .manager
            .list_observed()
            .await?
            .into_iter()
            .filter(|observed| status_consumes_running_slot(&observed.status))
            .count())
    }
}

enum CapacityCandidateAction {
    Paused,
    ReconciledStale,
    Skipped,
}

fn candidate_fetch_limit(slots_needed: usize) -> i64 {
    slots_needed.saturating_mul(4).clamp(16, 1000) as i64
}

fn status_consumes_running_slot(status: &SandboxStatus) -> bool {
    matches!(
        status,
        SandboxStatus::Created | SandboxStatus::Running | SandboxStatus::Unknown(_)
    )
}

struct EventStreamState {
    store: PgSessionStore,
    thread_key: ThreadKey,
    after_event_id: i64,
    execution_id: Option<String>,
    pending: VecDeque<SessionEvent>,
    listener: SessionEventListener,
    safety_tick: Interval,
    done: bool,
    emitted_count: u64,
    span: Span,
}

struct SandboxReadyObservation<'a> {
    thread_key: &'a ThreadKey,
    execution_id: &'a str,
    sandbox_id: &'a str,
    harness_type: &'a HarnessType,
    source: &'static str,
    ready_duration: Duration,
    startup_duration: Option<Duration>,
}

struct EnsureSessionSandboxRequest<'a> {
    thread_key: &'a ThreadKey,
    harness_type: &'a HarnessType,
    persona_id: Option<&'a str>,
    existing_sandbox_id: Option<&'a str>,
    existing_sandbox_capabilities: Option<&'a SessionSandboxCapabilities>,
    iron_control_principal: Option<&'a str>,
    desired_capabilities: &'a SessionSandboxCapabilities,
    execution_id: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SandboxBootMode {
    Harness,
    ToolHost { principal_id: String },
}

impl SandboxBootMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::ToolHost { .. } => "tool_host",
        }
    }

    fn uses_warm_pool(&self) -> bool {
        matches!(self, Self::Harness)
    }
}

struct PersonaResolution {
    persona_id: Option<String>,
    context: Option<PersonaContext>,
    defaulted: bool,
}

impl SessionRuntime {
    pub fn new(store: PgSessionStore, sandbox_runtime: SandboxRuntime) -> Self {
        Self {
            store,
            sandbox_runtime,
            sandbox_pipes: Arc::new(DashMap::new()),
            sandbox_pipe_open_locks: Arc::new(DashMap::new()),
            tool_host_call_locks: Arc::new(DashMap::new()),
            execution_spans: Arc::new(Mutex::new(HashMap::new())),
            iron_control: None,
            warm_pool: None,
            personas: None,
            session_title_generator: None,
            session_title_in_flight: Arc::new(DashSet::new()),
            session_title_rerun_requested: Arc::new(DashSet::new()),
            capacity: None,
            stdout_owner_id: format!("api-rs-{}", uuid::Uuid::new_v4().simple()),
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_session_title_generator<F, Fut>(mut self, generator: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, SessionTitleGenerationError>> + Send + 'static,
    {
        self.session_title_generator = Some(Arc::new(move |source| generator(source).boxed()));
        self
    }

    pub fn with_openai_session_title_generator_from_env(mut self) -> Self {
        let Some(generator) = OpenAiSessionTitleGenerator::from_env() else {
            return self;
        };
        self.session_title_generator = Some(Arc::new(move |source| {
            let generator = generator.clone();
            async move { generator.generate(source).await }.boxed()
        }));
        self
    }

    pub fn with_personas(mut self, personas: PersonaRegistry) -> Self {
        self.personas = Some(Arc::new(personas));
        self
    }

    pub fn personas(&self) -> Vec<PersonaSummary> {
        self.personas
            .as_ref()
            .map(|personas| personas.summaries())
            .unwrap_or_default()
    }

    pub async fn session_title(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<String>, SessionRuntimeError> {
        Ok(self.store.get_session_title(thread_key).await?)
    }

    fn resolve_persona_for_create(
        &self,
        requested_persona_id: Option<&str>,
        capabilities: &SessionSandboxCapabilities,
    ) -> Result<PersonaResolution, SessionRuntimeError> {
        let requested = requested_persona_id.and_then(clean_persona_id);
        let selected = requested.or_else(|| self.default_persona_id_for_access(capabilities));
        let defaulted = requested.is_none() && selected.is_some();
        let context = self.resolve_persona_context(selected, defaulted, capabilities)?;
        Ok(PersonaResolution {
            persona_id: selected.map(str::to_owned),
            context,
            defaulted,
        })
    }

    fn resolve_stored_persona(
        &self,
        persona_id: Option<&str>,
        _harness_type: &HarnessType,
        capabilities: &SessionSandboxCapabilities,
    ) -> Result<Option<PersonaContext>, SessionRuntimeError> {
        self.resolve_persona_context(persona_id.and_then(clean_persona_id), false, capabilities)
    }

    fn resolve_persona_context(
        &self,
        persona_id: Option<&str>,
        defaulted: bool,
        capabilities: &SessionSandboxCapabilities,
    ) -> Result<Option<PersonaContext>, SessionRuntimeError> {
        let Some(persona_id) = persona_id else {
            return Ok(None);
        };
        let Some(registry) = self.personas.as_ref() else {
            return Err(SessionRuntimeError::BadRequest(format!(
                "persona {persona_id:?} was requested but no persona registry is configured"
            )));
        };
        registry
            .context_for_access(persona_id, defaulted, &capabilities.repo_cache)
            .map(Some)
            .map_err(SessionRuntimeError::BadRequest)
    }

    fn default_persona_id(&self) -> Option<&str> {
        self.personas
            .as_ref()
            .and_then(|personas| personas.default_persona_id())
    }

    fn default_persona_id_for_access(
        &self,
        capabilities: &SessionSandboxCapabilities,
    ) -> Option<&str> {
        self.personas
            .as_ref()
            .and_then(|personas| personas.default_persona_id_for_access(&capabilities.repo_cache))
    }

    fn context(&self) -> RuntimeContext {
        RuntimeContext {
            store: self.store.clone(),
            manager: self.sandbox_runtime.manager.clone(),
            sandbox_pipes: self.sandbox_pipes.clone(),
            execution_spans: self.execution_spans.clone(),
            stdout_owner_id: self.stdout_owner_id.clone(),
        }
    }

    pub async fn run_tool_host_call(
        &self,
        input: ToolHostCallInput,
    ) -> Result<ToolHostCallOutput, SessionRuntimeError> {
        let principal_id = input.principal_id.trim().to_owned();
        let tool_name = input.tool_name.trim().to_owned();
        let method = input.method.trim().to_owned();
        if principal_id.is_empty() {
            return Err(SessionRuntimeError::BadRequest(
                "tool host principal_id is required".to_owned(),
            ));
        }
        if tool_name.is_empty() {
            return Err(SessionRuntimeError::BadRequest(
                "tool host tool_name is required".to_owned(),
            ));
        }
        if method.is_empty() {
            return Err(SessionRuntimeError::BadRequest(
                "tool host method is required".to_owned(),
            ));
        }
        if input.timeout.is_zero() {
            return Err(SessionRuntimeError::BadRequest(
                "tool host timeout must be non-zero".to_owned(),
            ));
        }

        let thread_key = tool_host_thread_key(&principal_id)?;
        let input = ToolHostCallInput {
            principal_id,
            tool_name,
            method,
            ..input
        };
        let call_lock = self.tool_host_call_lock(&thread_key);
        let result = {
            let _call_guard = call_lock.lock().await;
            self.locked_tool_host_call(&thread_key, input).await
        };
        // Drop our clone so an idle entry is only referenced by the map, then
        // evict it; remove_if holds the shard lock, so no concurrent caller
        // can clone the entry between the count check and the removal.
        drop(call_lock);
        self.tool_host_call_locks
            .remove_if(thread_key.as_str(), |_, lock| Arc::strong_count(lock) == 1);
        result
    }

    fn tool_host_call_lock(&self, thread_key: &ThreadKey) -> Arc<Mutex<()>> {
        self.tool_host_call_locks
            .entry(thread_key.as_str().to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn locked_tool_host_call(
        &self,
        thread_key: &ThreadKey,
        input: ToolHostCallInput,
    ) -> Result<ToolHostCallOutput, SessionRuntimeError> {
        let ToolHostCallInput {
            principal_id,
            token_id,
            tool_name,
            method,
            arguments,
            timeout,
        } = input;
        self.create_or_get_tool_host_session(thread_key, &principal_id)
            .await?;

        let request_id = format!("mcp-call-{}", Uuid::new_v4().simple());
        let request = ToolHostRequest {
            id: request_id.clone(),
            tool: tool_name.clone(),
            method: method.clone(),
            arguments,
            principal_id,
            token_id,
            timeout_seconds: timeout.as_secs().max(1),
        };
        let input_line = serde_json::to_string(&request).map_err(|error| {
            SessionRuntimeError::Sandbox(SandboxError::io_source("encode tool host request", error))
        })?;
        let response_timeout = timeout.saturating_add(Duration::from_secs(5));
        let execution = self
            .execute_session(
                thread_key,
                ExecuteSessionInput {
                    idempotency_key: Some(request_id.clone()),
                    metadata: Some(json!({
                        "mcp_tool_host_call": true,
                        "request_id": request_id,
                        "tool": tool_name,
                        "method": method,
                        "timeout_ms": duration_millis_u64(timeout),
                    })),
                    input_lines: vec![input_line],
                    idle_timeout_ms: None,
                    max_duration_ms: Some(duration_millis_u64(response_timeout)),
                },
            )
            .await?;
        self.wait_for_tool_host_call(thread_key, &execution.execution_id, response_timeout)
            .await
    }

    async fn create_or_get_tool_host_session(
        &self,
        thread_key: &ThreadKey,
        principal_id: &str,
    ) -> Result<(), SessionRuntimeError> {
        let harness = self
            .sandbox_runtime
            .warm_harness
            .clone()
            .unwrap_or(HarnessType::Codex);
        let metadata = tool_host_session_metadata(principal_id);
        let session = self
            .store
            .create_or_get_session(thread_key, &harness, None, metadata)
            .await?;
        if self.iron_control.is_some()
            && session.iron_control_principal.as_deref() != Some(principal_id)
        {
            self.store
                .set_iron_control_principal(thread_key, Some(principal_id))
                .await?;
        }
        Ok(())
    }

    async fn wait_for_tool_host_call(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        response_timeout: Duration,
    ) -> Result<ToolHostCallOutput, SessionRuntimeError> {
        let events = self
            .stream_events(thread_key, 0, Some(execution_id))
            .await?;
        futures_util::pin_mut!(events);
        match timeout(response_timeout, async {
            while let Some(event) = events.next().await {
                let event = event?;
                match event.event_type.as_str() {
                    "session.execution_completed" => {
                        return self.tool_host_completed_output(thread_key, &event).await;
                    }
                    "session.execution_failed" => {
                        return self.tool_host_failed_output(thread_key, &event).await;
                    }
                    _ => {}
                }
            }
            Err(SessionRuntimeError::Sandbox(SandboxError::io(
                "session event stream ended before tool host call completed",
            )))
        })
        .await
        {
            Ok(output) => output,
            // Best-effort sandbox id: a store error must not replace the
            // timeout result with an internal error.
            Err(_) => Ok(ToolHostCallOutput {
                sandbox_id: self
                    .current_sandbox_id(thread_key)
                    .await
                    .unwrap_or_default(),
                stdout: String::new(),
                stderr: format!(
                    "tool host call timed out after {} ms",
                    response_timeout.as_millis()
                ),
                exit_status: None,
                timed_out: true,
            }),
        }
    }

    async fn tool_host_completed_output(
        &self,
        thread_key: &ThreadKey,
        event: &SessionEvent,
    ) -> Result<ToolHostCallOutput, SessionRuntimeError> {
        let sandbox_id = self.current_sandbox_id(thread_key).await?;
        let Some(result_text) = event.payload.get("result_text").and_then(Value::as_str) else {
            return Ok(ToolHostCallOutput {
                sandbox_id,
                stdout: String::new(),
                stderr: String::new(),
                exit_status: Some(0),
                timed_out: false,
            });
        };
        let response = serde_json::from_str::<ToolHostResponse>(result_text).map_err(|error| {
            SessionRuntimeError::Sandbox(SandboxError::io_source(
                "decode tool host response",
                error,
            ))
        })?;
        Ok(ToolHostCallOutput {
            sandbox_id,
            stdout: response.stdout,
            stderr: response.stderr,
            exit_status: response.status,
            timed_out: response.timed_out,
        })
    }

    async fn tool_host_failed_output(
        &self,
        thread_key: &ThreadKey,
        event: &SessionEvent,
    ) -> Result<ToolHostCallOutput, SessionRuntimeError> {
        let error = event
            .payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("tool host execution failed")
            .to_owned();
        let timed_out = event
            .payload
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| reason == "max_duration_exceeded");
        Ok(ToolHostCallOutput {
            sandbox_id: self.current_sandbox_id(thread_key).await?,
            stdout: String::new(),
            stderr: error,
            exit_status: None,
            timed_out,
        })
    }

    async fn current_sandbox_id(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<String, SessionRuntimeError> {
        Ok(self
            .store
            .get_session(thread_key)
            .await?
            .sandbox_id
            .unwrap_or_default())
    }

    async fn claim_stdout_owner(&self, execution_id: &str) -> Result<(), SessionRuntimeError> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(SessionRuntimeError::ShuttingDown);
        }
        let claimed = self
            .store
            .claim_stdout_owner(execution_id, &self.stdout_owner_id, STDOUT_OWNER_LEASE)
            .await?;
        if !claimed {
            return Err(SessionRuntimeError::BadRequest(format!(
                "execution {execution_id} stdout is owned by another control plane process"
            )));
        }
        spawn_stdout_owner_renewer(self.context(), execution_id.to_owned());
        Ok(())
    }

    async fn claim_expired_stdout_owner(
        &self,
        execution_id: &str,
    ) -> Result<bool, SessionRuntimeError> {
        let claimed = self
            .store
            .claim_expired_stdout_owner(execution_id, &self.stdout_owner_id, STDOUT_OWNER_LEASE)
            .await?;
        if claimed {
            spawn_stdout_owner_renewer(self.context(), execution_id.to_owned());
        }
        Ok(claimed)
    }

    /// Attach an iron-control registrar so each new session upserts its
    /// principal and assigns it the configured roles.
    pub fn with_iron_control(mut self, registrar: SessionRegistrar) -> Self {
        self.iron_control = Some(registrar);
        self
    }

    /// Register the shared unauthenticated MCP tool-host principal when
    /// iron-control is enabled, so proxy-backed tool calls can resolve an
    /// effective config without minting per-user credentials in this layer.
    pub async fn register_mcp_tool_host_principal(
        &self,
        principal_id: &str,
    ) -> Result<String, SessionRuntimeError> {
        let principal_id = principal_id.trim();
        if principal_id.is_empty() {
            return Err(SessionRuntimeError::BadRequest(
                "mcp tool host principal_id is required".to_owned(),
            ));
        }
        if principal_id.contains(':') {
            return Err(SessionRuntimeError::BadRequest(
                "mcp tool host principal_id must not contain ':'".to_owned(),
            ));
        }
        let thread_key = tool_host_thread_key(principal_id)?;
        if let Some(registrar) = &self.iron_control {
            // Serialize with run_tool_host_call so concurrent registrations
            // for the same principal cannot interleave with session setup.
            let call_lock = self.tool_host_call_lock(&thread_key);
            let _call_guard = call_lock.lock().await;
            let metadata = tool_host_session_metadata(principal_id);
            let principal = registrar
                .register_session(thread_key.as_str(), Some(&metadata))
                .await?;
            return Ok(principal.id);
        }
        Ok(principal_id.to_owned())
    }

    pub fn with_warm_pool(mut self, config: WarmPoolConfig) -> Self {
        if config.target_size == 0 {
            return self;
        }

        let (Some(spec_factory), Some(workload_key)) = (
            self.sandbox_runtime.warm_spec_factory.clone(),
            self.sandbox_runtime.workload_key.clone(),
        ) else {
            warn!(
                target_size = config.target_size,
                "session sandbox warm pool requested for runtime without a warm sandbox spec"
            );
            return self;
        };

        let pool = Arc::new(WarmPoolManager::new(
            self.sandbox_runtime.manager.clone(),
            self.store.clone(),
            spec_factory,
            workload_key,
            config,
        ));
        pool.clone().spawn_replenisher();
        self.warm_pool = Some(pool);
        self
    }

    pub fn with_sandbox_capacity(mut self, config: SandboxCapacityConfig) -> Self {
        if !config.is_enabled() {
            return self;
        }
        self.capacity = Some(Arc::new(SandboxCapacityController::new(
            self.store.clone(),
            self.sandbox_runtime.manager.clone(),
            self.sandbox_pipes.clone(),
            config,
        )));
        self
    }

    async fn run_with_running_capacity<T, F, Fut>(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        operation: &'static str,
        action: F,
    ) -> Result<T, SessionRuntimeError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, SessionRuntimeError>>,
    {
        if let Some(capacity) = self.capacity.as_ref() {
            capacity
                .run_with_capacity(thread_key, execution_id, operation, action)
                .await
        } else {
            action().await
        }
    }

    /// Spawn the background reaper that stops sandboxes whose total lifetime
    /// expired. No-op when max-lifetime reaping is disabled.
    pub fn with_sandbox_reaper(self, config: SandboxReaperConfig) -> Self {
        if !config.is_enabled() {
            return self;
        }
        SandboxReaper::new(self.sandbox_runtime.manager.clone(), config).spawn();
        self
    }

    /// Spawn the DB-aware cleanup worker that reaps backend sandboxes no durable
    /// session/warm-pool row references and restores idle pauses lost across
    /// control-plane restarts.
    pub fn with_sandbox_cleanup(self, config: SessionSandboxCleanupConfig) -> Self {
        if !config.is_enabled() {
            return self;
        }
        cleanup::SessionSandboxCleanupWorker::new(self.context(), config).spawn();
        self
    }

    pub async fn create_or_get_session(
        &self,
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
        persona_id: Option<&str>,
        metadata: Option<Value>,
        on_harness_conflict: HarnessConflictPolicy,
    ) -> Result<CreateOrGetSessionOutcome, SessionRuntimeError> {
        let span = info_span!(
            "centaur.api_rs.session.create_or_get",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_create_or_get",
            "centaur.thread_key" = thread_key.as_str(),
            "centaur.harness_type" = %harness_type,
            thread_key = %thread_key,
            harness_type = %harness_type,
            iron_control_enabled = self.iron_control.is_some(),
        );
        set_span_parent_trace(
            &span,
            &thread_trace_id(thread_key),
            &thread_trace_parent_span_id(thread_key),
        );
        let result = async {
            ensure_thread_trace_root_span(thread_key);
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_create_or_get_started",
                thread_key = %thread_key,
                harness_type = %harness_type,
                iron_control_enabled = self.iron_control.is_some(),
                "creating or loading session"
            );
            let mut harness_switched = false;
            let mut session_metadata = default_metadata(metadata);
            let (registered_principal, desired_capabilities) =
                if let Some(registrar) = &self.iron_control {
                    let principal = registrar
                        .register_session(thread_key.as_str(), Some(&session_metadata))
                        .await?;
                    let desired_capabilities = sandbox_capabilities_from_principal(&principal);
                    (Some(principal), desired_capabilities)
                } else {
                    (None, SessionSandboxCapabilities::default_enabled())
                };
            let persona_resolution =
                self.resolve_persona_for_create(persona_id, &desired_capabilities)?;
            if let Some(context) = persona_resolution.context.as_ref() {
                add_persona_metadata(&mut session_metadata, context);
            }
            let session = match self
                .store
                .create_or_get_session(
                    thread_key,
                    harness_type,
                    persona_resolution.persona_id.as_deref(),
                    session_metadata.clone(),
                )
                .await
            {
                Ok(session) => session,
                Err(SessionStoreError::PersonaConflict { existing, .. })
                    if persona_id.is_none() && persona_resolution.defaulted =>
                {
                    self.store
                        .create_or_get_session(
                            thread_key,
                            harness_type,
                            existing.as_deref(),
                            default_metadata(None),
                        )
                        .await?
                }
                Err(SessionStoreError::HarnessConflict { existing, .. })
                    if on_harness_conflict == HarnessConflictPolicy::Restart =>
                {
                    let session = self
                        .restart_session_on_harness(thread_key, harness_type, &existing)
                        .await?;
                    harness_switched = true;
                    session
                }
                Err(error) => return Err(error.into()),
            };
            if let Some(context) = self.resolve_stored_persona(
                session.persona_id.as_deref(),
                harness_type,
                &desired_capabilities,
            )? {
                self.store
                    .append_event(
                        thread_key,
                        None,
                        "session.persona_resolved",
                        json!({
                            "persona": context,
                            "requested_persona_id": persona_id,
                            "deployment_default_persona_id": self.default_persona_id(),
                        }),
                    )
                    .await?;
            }
            if let Some(principal) = registered_principal {
                // Persist the principal OID on the session row so a resumed session
                // can recreate its sandbox after a restart without re-deriving it.
                let session = self
                    .store
                    .set_iron_control_principal(thread_key, Some(&principal.id))
                    .await?;
                info!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_create_or_get_completed",
                    thread_key = %thread_key,
                    harness_type = %harness_type,
                    status = %session.status,
                    iron_control_principal_persisted = true,
                    harness_switched,
                    "session ready"
                );
                return Ok(CreateOrGetSessionOutcome {
                    session,
                    harness_switched,
                });
            }
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_create_or_get_completed",
                thread_key = %thread_key,
                harness_type = %harness_type,
                status = %session.status,
                iron_control_principal_persisted = session.iron_control_principal.is_some(),
                harness_switched,
                "session ready"
            );
            Ok(CreateOrGetSessionOutcome {
                session,
                harness_switched,
            })
        }
        .instrument(span)
        .await;

        if let Err(error) = &result {
            error!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_create_or_get_failed",
                thread_key = %thread_key,
                harness_type = %harness_type,
                %error,
                "failed to create or load session"
            );
        }
        result
    }

    /// Restart an existing session on a different harness: stop its sandbox
    /// (killing any in-flight execution), clear the harness thread state, and
    /// flip the session row to the requested harness. Stored messages and
    /// events are preserved for the record, but the new harness boots with no
    /// conversational memory — callers that want continuity must re-send
    /// context with the next turn.
    async fn restart_session_on_harness(
        &self,
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
        previous_harness: &str,
    ) -> Result<Session, SessionRuntimeError> {
        let previous = self.store.get_session(thread_key).await?;
        if let Some(sandbox_id) = previous.sandbox_id.as_deref() {
            self.sandbox_pipes.remove(sandbox_id);
            match self
                .sandbox_runtime
                .manager
                .stop(&SandboxId::new(sandbox_id))
                .await
            {
                Ok(()) | Err(SandboxError::NotFound(_)) => {}
                Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
            }
        }
        let session = self
            .store
            .switch_session_harness(thread_key, harness_type)
            .await?;
        self.store
            .append_event(
                thread_key,
                None,
                "session.harness_switched",
                json!({
                    "thread_key": thread_key.as_str(),
                    "from_harness": previous_harness,
                    "to_harness": harness_type.as_ref(),
                    "stopped_sandbox_id": previous.sandbox_id,
                }),
            )
            .await?;
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_harness_switched",
            thread_key = %thread_key,
            from_harness = previous_harness,
            to_harness = %harness_type,
            stopped_sandbox_id = previous.sandbox_id.as_deref().unwrap_or(""),
            "restarted session on a new harness"
        );
        Ok(session)
    }

    pub async fn append_messages(
        &self,
        thread_key: &ThreadKey,
        messages: &[SessionMessageInput],
    ) -> Result<Vec<String>, SessionRuntimeError> {
        let span = info_span!(
            "centaur.api_rs.session.messages.append",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_messages_append",
            "centaur.thread_key" = thread_key.as_str(),
            thread_key = %thread_key,
            message_count = messages.len(),
        );
        set_span_parent_trace(
            &span,
            &thread_trace_id(thread_key),
            &thread_trace_parent_span_id(thread_key),
        );
        let result = async {
            ensure_thread_trace_root_span(thread_key);
            if messages.is_empty() {
                return Err(SessionRuntimeError::BadRequest(
                    "messages must not be empty".to_owned(),
                ));
            }
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_messages_append_started",
                thread_key = %thread_key,
                message_count = messages.len(),
                "appending session messages"
            );
            let message_ids = self.store.append_messages(thread_key, messages).await?;
            if let Err(error) = self.store.touch_session_sandbox_activity(thread_key).await {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_sandbox_activity_touch_failed",
                    thread_key = %thread_key,
                    %error,
                    "failed to touch sandbox activity after message append"
                );
            }
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_messages_append_completed",
                thread_key = %thread_key,
                message_count = messages.len(),
                message_id_count = message_ids.len(),
                "session messages appended"
            );
            Ok(message_ids)
        }
        .instrument(span)
        .await;

        let message_ids = match result {
            Ok(message_ids) => message_ids,
            Err(error) => {
                error!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_messages_append_failed",
                    thread_key = %thread_key,
                    message_count = messages.len(),
                    %error,
                    "failed to append session messages"
                );
                return Err(error);
            }
        };
        self.forward_messages_to_active_execution(thread_key, messages, &message_ids)
            .await;
        self.spawn_session_title_generation(thread_key);
        Ok(message_ids)
    }

    fn spawn_session_title_generation(&self, thread_key: &ThreadKey) {
        let Some(generator) = self.session_title_generator.clone() else {
            return;
        };
        if !self.session_title_in_flight.insert(thread_key.clone()) {
            self.session_title_rerun_requested
                .insert(thread_key.clone());
            return;
        }
        let store = self.store.clone();
        let in_flight = self.session_title_in_flight.clone();
        let rerun_requested = self.session_title_rerun_requested.clone();
        let thread_key = thread_key.clone();
        tokio::spawn(async move {
            // Appends skipped while generation is in flight request one more pass,
            // which lets low-signal wakeups defer to a later substantive message.
            loop {
                rerun_requested.remove(&thread_key);
                maybe_generate_session_title(store.clone(), generator.clone(), thread_key.clone())
                    .await;
                if rerun_requested.remove(&thread_key).is_some() {
                    continue;
                }

                in_flight.remove(&thread_key);
                if rerun_requested.remove(&thread_key).is_some()
                    && in_flight.insert(thread_key.clone())
                {
                    continue;
                }
                break;
            }
        });
    }

    /// Stop every non-terminal sandbox the backend currently owns.
    ///
    /// Intended for a clean control-plane shutdown (e.g. before a deploy):
    /// each sandbox is stopped independently so one failure does not abort the
    /// rest, and the [`DrainReport`] records which were stopped and which
    /// failed so the caller can surface partial failure.
    pub async fn drain(&self) -> Result<DrainReport, SessionRuntimeError> {
        let observed = self.sandbox_runtime.manager.list_observed().await?;
        let mut report = DrainReport::default();
        for sandbox in observed {
            if sandbox.status.is_terminal() {
                continue;
            }
            let id = sandbox.id.as_str().to_owned();
            match self.sandbox_runtime.manager.stop(&sandbox.id).await {
                Ok(()) => {
                    self.sandbox_pipes.remove(&id);
                    if let Err(error) = self
                        .store
                        .mark_warm_sandbox_failed(&id, "sandbox drained")
                        .await
                    {
                        warn!(sandbox_id = %id, %error, "drain failed to clear warm sandbox row");
                        report.failed.push(DrainFailure {
                            sandbox_id: id.clone(),
                            error: error.to_string(),
                        });
                    }
                    report.stopped.push(id);
                }
                Err(error) => {
                    warn!(sandbox_id = %id, %error, "drain failed to stop sandbox");
                    report.failed.push(DrainFailure {
                        sandbox_id: id,
                        error: error.to_string(),
                    });
                }
            }
        }
        Ok(report)
    }

    pub async fn stop_workflow_owned_sandboxes(
        &self,
        workflow_run_id: &str,
        reason: &str,
    ) -> Result<WorkflowSandboxCleanupReport, SessionRuntimeError> {
        let sandboxes = self
            .store
            .list_workflow_owned_sandboxes(workflow_run_id)
            .await?;
        let mut report = WorkflowSandboxCleanupReport::default();

        for sandbox in sandboxes {
            let sandbox_id = sandbox.sandbox_id;
            let thread_key = sandbox.thread_key;
            self.sandbox_pipes.remove(&sandbox_id);
            let id = SandboxId::new(sandbox_id.clone());
            let mut missing = false;
            match self.sandbox_runtime.manager.stop(&id).await {
                Ok(()) => report.stopped.push(sandbox_id.clone()),
                Err(SandboxError::NotFound(_)) => {
                    missing = true;
                    report.missing.push(sandbox_id.clone());
                }
                Err(error) => {
                    let error = error.to_string();
                    warn!(
                        thread_key = %thread_key,
                        sandbox_id,
                        workflow_run_id,
                        reason,
                        %error,
                        "failed to stop workflow-owned sandbox"
                    );
                    report.failed.push(DrainFailure {
                        sandbox_id: sandbox_id.clone(),
                        error: error.clone(),
                    });
                    if let Err(event_error) = self
                        .store
                        .append_event(
                            &thread_key,
                            None,
                            "session.workflow_sandbox_stop_failed",
                            json!({
                                "thread_key": thread_key.as_str(),
                                "sandbox_id": sandbox_id,
                                "workflow_run_id": workflow_run_id,
                                "reason": reason,
                                "error": error,
                            }),
                        )
                        .await
                    {
                        warn!(
                            thread_key = %thread_key,
                            sandbox_id,
                            workflow_run_id,
                            %event_error,
                            "failed to append workflow sandbox stop failure event"
                        );
                    }
                    continue;
                }
            }

            if let Err(error) = self
                .store
                .mark_warm_sandbox_failed(&sandbox_id, "workflow-owned sandbox stopped")
                .await
            {
                warn!(
                    thread_key = %thread_key,
                    sandbox_id,
                    workflow_run_id,
                    %error,
                    "failed to mark workflow-owned warm sandbox failed"
                );
            }

            let cleared = self
                .store
                .clear_sandbox_id_if_matches(&thread_key, &sandbox_id)
                .await?;
            if let Err(error) = self
                .store
                .append_event(
                    &thread_key,
                    None,
                    "session.workflow_sandbox_stopped",
                    json!({
                        "thread_key": thread_key.as_str(),
                        "sandbox_id": sandbox_id,
                        "workflow_run_id": workflow_run_id,
                        "reason": reason,
                        "missing": missing,
                        "cleared": cleared,
                    }),
                )
                .await
            {
                warn!(
                    thread_key = %thread_key,
                    sandbox_id,
                    workflow_run_id,
                    %error,
                    "failed to append workflow sandbox cleanup event"
                );
            }
        }

        Ok(report)
    }

    pub async fn execute_session(
        &self,
        thread_key: &ThreadKey,
        input: ExecuteSessionInput,
    ) -> Result<SessionExecution, SessionRuntimeError> {
        let ExecuteSessionInput {
            idempotency_key,
            metadata,
            input_lines,
            idle_timeout_ms,
            max_duration_ms,
        } = input;
        let input_line_count = input_lines.len();
        let idempotency_key_present = idempotency_key.is_some();
        let span = info_span!(
            "centaur.api_rs.session.execute",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_execute",
            "centaur.thread_key" = thread_key.as_str(),
            "centaur.execution_id" = tracing::field::Empty,
            "centaur.sandbox_id" = tracing::field::Empty,
            thread_key = %thread_key,
            execution_id = tracing::field::Empty,
            sandbox_id = tracing::field::Empty,
            input_line_count,
            idempotency_key_present,
        );
        set_span_parent_trace(
            &span,
            &thread_trace_id(thread_key),
            &thread_trace_parent_span_id(thread_key),
        );
        let result = async {
            ensure_thread_trace_root_span(thread_key);
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_execute_started",
                thread_key = %thread_key,
                input_line_count,
                idempotency_key_present,
                "starting session execution"
            );
            let session = self.store.get_session(thread_key).await?;
            let harness_label = session.harness_type.to_string();
            validate_input_lines(&input_lines)?;
            let (idle_timeout, max_duration) = duration_options(idle_timeout_ms, max_duration_ms)?;

            let execution = self
                .store
                .create_execution(
                    thread_key,
                    idempotency_key.as_deref(),
                    execution_metadata(metadata, idle_timeout_ms, max_duration_ms),
                )
                .await?;
            span.record(
                "centaur.execution_id",
                execution.execution.execution_id.as_str(),
            );
            span.record("execution_id", execution.execution.execution_id.as_str());
            if !execution.created && execution.execution.status != ExecutionStatus::Queued {
                info!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_execute_idempotent_replay",
                    thread_key = %thread_key,
                    execution_id = %execution.execution.execution_id,
                    status = %execution.execution.status,
                    "returning existing execution"
                );
                return Ok(execution.execution);
            }
            let claim = self
                .store
                .mark_execution_running(&execution.execution.execution_id)
                .await?;
            let execution = claim.execution;
            span.record("centaur.execution_id", execution.execution_id.as_str());
            span.record("execution_id", execution.execution_id.as_str());
            if !claim.claimed {
                // A concurrent request with the same idempotency key claimed
                // the execution first (or it already reached a terminal
                // state). Do not drive it again — return the current row so
                // the caller can attach to the event stream.
                info!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_execute_not_claimed",
                    thread_key = %thread_key,
                    execution_id = %execution.execution_id,
                    status = %execution.status,
                    "execution was already claimed or terminal"
                );
                return Ok(execution);
            }
            if let Err(error) = self.claim_stdout_owner(&execution.execution_id).await {
                self.record_execution_failure(thread_key, &execution.execution_id, &error)
                    .await;
                return Err(error);
            }
            let execution_trace_span = info_span!(
                "centaur.api_rs.session.execution",
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_execution",
                "centaur.thread_key" = thread_key.as_str(),
                "centaur.execution_id" = execution.execution_id.as_str(),
                "centaur.sandbox_id" = tracing::field::Empty,
                thread_key = %thread_key,
                execution_id = %execution.execution_id,
                sandbox_id = tracing::field::Empty,
            );
            set_span_parent_trace(
                &execution_trace_span,
                &thread_trace_id(thread_key),
                &thread_trace_parent_span_id(thread_key),
            );
            self.execution_spans
                .lock()
                .await
                .insert(execution.execution_id.clone(), execution_trace_span.clone());
            record_session_execution_started(&harness_label);
            self.store
                .append_event(
                    thread_key,
                    Some(&execution.execution_id),
                    "session.execution_started",
                    json!({
                        "execution_id": execution.execution_id,
                        "thread_key": thread_key.as_str(),
                        "input_line_count": input_line_count,
                        "idle_timeout_ms": idle_timeout_ms,
                        "max_duration_ms": max_duration_ms,
                    }),
                )
                .await?;
            let desired_capabilities = self
                .resolve_sandbox_capabilities(session.iron_control_principal.as_deref())
                .await?;

            let sandbox_id = match self
                .ensure_session_sandbox(EnsureSessionSandboxRequest {
                    thread_key,
                    harness_type: &session.harness_type,
                    persona_id: session.persona_id.as_deref(),
                    existing_sandbox_id: session.sandbox_id.as_deref(),
                    existing_sandbox_capabilities: session.sandbox_capabilities.as_ref(),
                    iron_control_principal: session.iron_control_principal.as_deref(),
                    desired_capabilities: &desired_capabilities,
                    execution_id: &execution.execution_id,
                })
                .instrument(execution_trace_span.clone())
                .await
            {
                Ok(sandbox_id) => sandbox_id,
                Err(error) => {
                    self.record_execution_failure(thread_key, &execution.execution_id, &error)
                        .await;
                    return Err(error);
                }
            };
            span.record("centaur.sandbox_id", sandbox_id.as_str());
            span.record("sandbox_id", sandbox_id.as_str());
            execution_trace_span.record("centaur.sandbox_id", sandbox_id.as_str());
            execution_trace_span.record("sandbox_id", sandbox_id.as_str());

            let pipe = match self
                .ensure_session_pipe(thread_key, &sandbox_id)
                .instrument(execution_trace_span.clone())
                .await
            {
                Ok(pipe) => pipe,
                Err(error) => {
                    self.record_execution_failure(thread_key, &execution.execution_id, &error)
                        .await;
                    return Err(error);
                }
            };

            let trace = SessionTraceContext::new(thread_key, Some(&execution_trace_span));
            let input_lines = input_lines_with_session_context(thread_key, &trace, &input_lines);
            if let Err(error) = write_input_lines(
                &pipe,
                &input_lines,
                thread_key,
                &execution.execution_id,
                Some(&sandbox_id),
            )
            .instrument(execution_trace_span.clone())
            .await
            {
                self.record_execution_failure(thread_key, &execution.execution_id, &error)
                    .await;
                return Err(error);
            }

            if let Some(max_duration) = max_duration {
                spawn_max_duration_failure(
                    self.context(),
                    thread_key.clone(),
                    execution.execution_id.clone(),
                    max_duration,
                    idle_timeout,
                );
            }

            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_execute_completed",
                thread_key = %thread_key,
                execution_id = %execution.execution_id,
                sandbox_id = %sandbox_id,
                status = %execution.status,
                completion_reason = "input_accepted",
                "session execution accepted input"
            );
            Ok(execution)
        }
        .instrument(span.clone())
        .await;

        if let Err(error) = &result {
            error!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_execute_failed",
                thread_key = %thread_key,
                input_line_count,
                %error,
                "session execution failed"
            );
        }
        result
    }

    async fn record_execution_failure(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        error: &SessionRuntimeError,
    ) {
        self.execution_spans.lock().await.remove(execution_id);
        let error_message = error.to_string();
        let execution = match self
            .store
            .fail_execution_if_active_and_stdout_owner(
                execution_id,
                &self.stdout_owner_id,
                &error_message,
            )
            .await
        {
            Ok(Some(execution)) => execution,
            Ok(None) => return,
            Err(_) => return,
        };
        let _ = self
            .store
            .append_event(
                thread_key,
                Some(execution_id),
                "session.execution_failed",
                json!({
                    "execution_id": execution_id,
                    "thread_key": thread_key.as_str(),
                    "error": error_message,
                }),
            )
            .await;
        record_finished_execution_metric(
            &self.store,
            thread_key,
            &execution,
            "failed",
            Some(runtime_error_failure_class(error)),
        )
        .await;
    }

    async fn forward_messages_to_active_execution(
        &self,
        thread_key: &ThreadKey,
        messages: &[SessionMessageInput],
        message_ids: &[String],
    ) {
        let input_lines = steering_input_lines(thread_key, messages, message_ids);
        if input_lines.is_empty() {
            return;
        }

        let Some(execution) = (match self.store.active_execution_for_thread(thread_key).await {
            Ok(execution) => execution,
            Err(error) => {
                warn!(%thread_key, %error, "active execution lookup failed during message append");
                return;
            }
        }) else {
            return;
        };

        // Steering joins the active execution's trace so harness spans for the
        // steered turn stay in the same tree.
        let execution_span = self
            .execution_spans
            .lock()
            .await
            .get(&execution.execution_id)
            .cloned();
        let trace = SessionTraceContext::new(thread_key, execution_span.as_ref());
        let input_lines = input_lines_with_session_context(thread_key, &trace, &input_lines);

        let pipe = match self
            .wait_for_active_steering_pipe(thread_key, &execution.execution_id)
            .await
        {
            Ok(pipe) => pipe,
            Err(error) => {
                self.record_steering_failure(thread_key, &execution.execution_id, error)
                    .await;
                return;
            }
        };

        if let Err(error) = write_input_lines(
            &pipe,
            &input_lines,
            thread_key,
            &execution.execution_id,
            None,
        )
        .await
        {
            self.record_steering_failure(thread_key, &execution.execution_id, error.to_string())
                .await;
            return;
        }

        if let Err(error) = self
            .store
            .append_event(
                thread_key,
                Some(&execution.execution_id),
                "session.steering_delivered",
                json!({
                    "execution_id": execution.execution_id,
                    "thread_key": thread_key.as_str(),
                    "message_ids": message_ids,
                    "input_line_count": input_lines.len(),
                }),
            )
            .await
        {
            warn!(%thread_key, %error, "failed to record steering delivery");
        }
    }

    pub async fn interrupt_active_execution(
        &self,
        thread_key: &ThreadKey,
        reason: &str,
    ) -> Result<InterruptExecutionOutcome, SessionRuntimeError> {
        let Some(execution) = self.store.active_execution_for_thread(thread_key).await? else {
            return Ok(InterruptExecutionOutcome {
                interrupted: false,
                execution_id: None,
            });
        };

        let execution_span = self
            .execution_spans
            .lock()
            .await
            .get(&execution.execution_id)
            .cloned();
        let trace = SessionTraceContext::new(thread_key, execution_span.as_ref());
        let input_lines = input_lines_with_session_context(
            thread_key,
            &trace,
            &[interrupt_input_line(thread_key, reason)],
        );

        let pipe = self
            .wait_for_active_steering_pipe(thread_key, &execution.execution_id)
            .await
            .map_err(SessionRuntimeError::BadRequest)?;
        write_input_lines(
            &pipe,
            &input_lines,
            thread_key,
            &execution.execution_id,
            None,
        )
        .await?;

        self.store
            .append_event(
                thread_key,
                Some(&execution.execution_id),
                "session.interrupt_delivered",
                json!({
                    "execution_id": execution.execution_id,
                    "thread_key": thread_key.as_str(),
                    "reason": reason,
                }),
            )
            .await?;

        Ok(InterruptExecutionOutcome {
            interrupted: true,
            execution_id: Some(execution.execution_id),
        })
    }

    async fn wait_for_active_steering_pipe(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
    ) -> Result<SessionPipe, String> {
        let deadline = Instant::now() + STEERING_STARTUP_RETRY_TIMEOUT;
        loop {
            let session = self
                .store
                .get_session(thread_key)
                .await
                .map_err(|error| format!("get session: {error}"))?;

            if let Some(sandbox_id) = session.sandbox_id.as_deref() {
                match self.ensure_session_pipe(thread_key, sandbox_id).await {
                    Ok(pipe) => return Ok(pipe),
                    Err(error)
                        if is_transient_steering_startup_error(&error)
                            && Instant::now() < deadline => {}
                    Err(error) => return Err(error.to_string()),
                }
            } else if Instant::now() >= deadline {
                return Err("session has no sandbox assigned".to_owned());
            }

            if !execution_still_active(&self.store, thread_key, execution_id).await {
                return Err("execution is no longer active".to_owned());
            }
            sleep(STEERING_STARTUP_RETRY_INTERVAL).await;
        }
    }

    async fn record_steering_failure(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        error: String,
    ) {
        warn!(%thread_key, %execution_id, %error, "active steering delivery failed");
        let _ = self
            .store
            .append_event(
                thread_key,
                Some(execution_id),
                "session.steering_failed",
                json!({
                    "execution_id": execution_id,
                    "thread_key": thread_key.as_str(),
                    "error": error,
                }),
            )
            .await;
    }

    pub async fn stream_events(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
        execution_id: Option<&str>,
    ) -> Result<
        impl Stream<Item = Result<SessionEvent, SessionRuntimeError>> + use<>,
        SessionRuntimeError,
    > {
        let span = info_span!(
            "centaur.api_rs.session.events.stream",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_events_stream",
            "centaur.thread_key" = thread_key.as_str(),
            thread_key = %thread_key,
            after_event_id,
            execution_id = execution_id.unwrap_or(""),
        );
        let result = async {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_events_stream_started",
                thread_key = %thread_key,
                after_event_id,
                execution_id = execution_id.unwrap_or(""),
                "opening session event stream"
            );
            let session = self.store.get_session(thread_key).await?;
            if let Some(sandbox_id) = session.sandbox_id.as_deref() {
                self.ensure_session_pipe_if_live(thread_key, sandbox_id)
                    .await?;
            }

            let listener = self.store.listen_session_events().await?;

            Ok(session_event_stream(
                self.store.clone(),
                thread_key.clone(),
                after_event_id,
                execution_id.map(ToOwned::to_owned),
                listener,
                span.clone(),
            ))
        }
        .instrument(span.clone())
        .await;

        if let Err(error) = &result {
            error!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_events_stream_failed",
                thread_key = %thread_key,
                after_event_id,
                %error,
                "failed to open session event stream"
            );
        }
        result
    }

    async fn ensure_session_sandbox(
        &self,
        request: EnsureSessionSandboxRequest<'_>,
    ) -> Result<String, SessionRuntimeError> {
        let EnsureSessionSandboxRequest {
            thread_key,
            harness_type,
            persona_id,
            existing_sandbox_id,
            existing_sandbox_capabilities,
            iron_control_principal,
            desired_capabilities,
            execution_id,
        } = request;
        let boot_mode = sandbox_boot_mode_for_thread(thread_key, iron_control_principal);
        let span = info_span!(
            "centaur.api_rs.sandbox.ensure",
            component = COMPONENT_SESSION_RUNTIME,
            event = "sandbox_ensure",
            "centaur.thread_key" = thread_key.as_str(),
            "centaur.execution_id" = execution_id,
            "centaur.sandbox_id" = tracing::field::Empty,
            thread_key = %thread_key,
            execution_id,
            sandbox_id = tracing::field::Empty,
            existing_sandbox_id = existing_sandbox_id.unwrap_or(""),
            iron_control_principal_present = iron_control_principal.is_some(),
            persona_id = persona_id.unwrap_or(""),
            sandbox_boot_mode = boot_mode.as_str(),
            sandbox_repo_cache_access = desired_capabilities.repo_cache.as_str(),
            sandbox_repo_cache_enabled = desired_capabilities.repo_cache_enabled(),
            sandbox_observability_enabled = desired_capabilities.observability_enabled,
            sandbox_api_server_enabled = desired_capabilities.api_server_enabled,
        );
        let ensure_started = Instant::now();
        let result = async {
            let persona_context =
                self.resolve_stored_persona(persona_id, harness_type, desired_capabilities)?;
            if let Some(sandbox_id) = existing_sandbox_id {
                let id = SandboxId::new(sandbox_id);
                if !sandbox_capabilities_match(existing_sandbox_capabilities, desired_capabilities)
                {
                    self.sandbox_pipes.remove(sandbox_id);
                    match self.sandbox_runtime.manager.stop(&id).await {
                        Ok(()) | Err(SandboxError::NotFound(_)) => {}
                        Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
                    }
                    self.store.update_sandbox_id(thread_key, None).await?;
                    self.store
                        .append_event(
                            thread_key,
                            Some(execution_id),
                            "session.sandbox_capabilities_replaced",
                            json!({
                                "execution_id": execution_id,
                                "thread_key": thread_key.as_str(),
                                "sandbox_id": sandbox_id,
                                "previous_capabilities": existing_sandbox_capabilities,
                                "desired_capabilities": desired_capabilities,
                            }),
                        )
                        .await?;
                    info!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "sandbox_ensure_capabilities_replaced",
                        thread_key = %thread_key,
                        execution_id,
                        sandbox_id,
                        sandbox_repo_cache_access = desired_capabilities.repo_cache.as_str(),
                        sandbox_repo_cache_enabled = desired_capabilities.repo_cache_enabled(),
                        sandbox_observability_enabled = desired_capabilities.observability_enabled,
                        sandbox_api_server_enabled = desired_capabilities.api_server_enabled,
                        "replacing existing sandbox whose capabilities do not match"
                    );
                } else {
                match self.sandbox_runtime.manager.status(&id).await {
                    Ok(status) => match existing_sandbox_action(&status) {
                        ExistingSandboxAction::Reuse => {
                            if let Some(principal_id) = iron_control_principal {
                                self.sandbox_runtime
                                    .manager
                                    .ensure_iron_control_proxy_resources(&id, principal_id)
                                    .await?;
                            }
                            span.record("centaur.sandbox_id", sandbox_id);
                            span.record("sandbox_id", sandbox_id);
                            let ready_duration = ensure_started.elapsed();
                            self.record_sandbox_ready(SandboxReadyObservation {
                                thread_key,
                                execution_id,
                                sandbox_id,
                                harness_type,
                                source: "reused",
                                ready_duration,
                                startup_duration: None,
                            })
                            .await;
                            info!(
                                component = COMPONENT_SESSION_RUNTIME,
                                event = "sandbox_ensure_reused",
                                thread_key = %thread_key,
                                execution_id,
                                sandbox_id,
                                harness_type = %harness_type,
                                sandbox_ready_source = "reused",
                                sandbox_ready_duration_ms = duration_millis_u64(ready_duration),
                                "reusing existing session sandbox"
                            );
                            return Ok(sandbox_id.to_owned());
                        }
                        ExistingSandboxAction::ResumeOrReplace => {
                            self.sandbox_pipes.remove(sandbox_id);
                            let resume_id = id.clone();
                            match self
                                .run_with_running_capacity(
                                    thread_key,
                                    execution_id,
                                    "resume",
                                    || async {
                                        self.sandbox_runtime
                                            .manager
                                            .resume(&resume_id)
                                            .await
                                            .map_err(SessionRuntimeError::Sandbox)
                                    },
                                )
                                .await
                            {
                                Ok(()) => {
                                    span.record("centaur.sandbox_id", sandbox_id);
                                    span.record("sandbox_id", sandbox_id);
                                    let ready_duration = ensure_started.elapsed();
                                    self.store
                                        .append_event(
                                            thread_key,
                                            Some(execution_id),
                                            "session.sandbox_resumed",
                                            json!({
                                                "execution_id": execution_id,
                                                "thread_key": thread_key.as_str(),
                                                "sandbox_id": sandbox_id,
                                            }),
                                        )
                                        .await?;
                                    self.record_sandbox_ready(SandboxReadyObservation {
                                        thread_key,
                                        execution_id,
                                        sandbox_id,
                                        harness_type,
                                        source: "resumed",
                                        ready_duration,
                                        startup_duration: None,
                                    })
                                    .await;
                                    info!(
                                        component = COMPONENT_SESSION_RUNTIME,
                                        event = "sandbox_ensure_resumed",
                                        thread_key = %thread_key,
                                        execution_id,
                                        sandbox_id,
                                        harness_type = %harness_type,
                                        sandbox_ready_source = "resumed",
                                        sandbox_ready_duration_ms = duration_millis_u64(ready_duration),
                                        "resumed existing session sandbox"
                                    );
                                    return Ok(sandbox_id.to_owned());
                                }
                                Err(SessionRuntimeError::Sandbox(error)) => {
                                    warn!(
                                        component = COMPONENT_SESSION_RUNTIME,
                                        event = "sandbox_ensure_resume_failed",
                                        %thread_key,
                                        %execution_id,
                                        %sandbox_id,
                                        %error,
                                        "replacing sandbox after resume failed"
                                    );
                                    self.store
                                        .append_event(
                                            thread_key,
                                            Some(execution_id),
                                            "session.sandbox_resume_failed",
                                            json!({
                                                "execution_id": execution_id,
                                                "thread_key": thread_key.as_str(),
                                                "sandbox_id": sandbox_id,
                                                "error": error.to_string(),
                                            }),
                                        )
                                        .await?;
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        ExistingSandboxAction::Replace => {
                            info!(
                                component = COMPONENT_SESSION_RUNTIME,
                                event = "sandbox_ensure_replacing",
                                thread_key = %thread_key,
                                execution_id,
                                sandbox_id,
                                status = ?status,
                                "existing sandbox is not reusable"
                            );
                        }
                    },
                    Err(SandboxError::NotFound(_)) => {
                        info!(
                            component = COMPONENT_SESSION_RUNTIME,
                            event = "sandbox_ensure_missing",
                            thread_key = %thread_key,
                            execution_id,
                            sandbox_id,
                            "existing sandbox is missing"
                        );
                    }
                    Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
                }
                }
            }

            // Warm sandboxes are pre-booted with the workload's default
            // harness; a session on any other harness needs a cold sandbox.
            let warm_harness_matches = self
                .sandbox_runtime
                .warm_harness
                .as_ref()
                .is_none_or(|warm| warm == harness_type);
            let warm_persona_matches = persona_context.is_none();
            if !warm_harness_matches && self.warm_pool.is_some() {
                record_sandbox_warm_pool_claim("harness_mismatch");
            }
            if !warm_persona_matches && self.warm_pool.is_some() {
                record_sandbox_warm_pool_claim("persona_specific");
            }
            if !desired_capabilities.is_default_enabled() && self.warm_pool.is_some() {
                record_sandbox_warm_pool_claim("capabilities_non_default");
            }
            if let Some(warm_pool) = self
                .warm_pool
                .as_ref()
                .filter(|_| {
                    boot_mode.uses_warm_pool()
                        && warm_harness_matches
                        && warm_persona_matches
                        && desired_capabilities.is_default_enabled()
                })
            {
                match warm_pool
                    .claim(thread_key.as_str(), iron_control_principal)
                    .await
                {
                    Ok(Some(sandbox_id)) => {
                        record_sandbox_warm_pool_claim("hit");
                        span.record("centaur.sandbox_id", sandbox_id.as_str());
                        span.record("sandbox_id", sandbox_id.as_str());
                        let ready_duration = ensure_started.elapsed();
                        self.store
                            .update_sandbox_assignment(
                                thread_key,
                                sandbox_id.as_str(),
                                desired_capabilities,
                            )
                            .await?;
                        self.store
                            .append_event(
                                thread_key,
                                None,
                                "session.warm_sandbox_claimed",
                                json!({
                                    "sandbox_id": sandbox_id.as_str(),
                                    "workload_key": warm_pool.workload_key(),
                                    "iron_control_principal": iron_control_principal,
                                    "sandbox_capabilities": desired_capabilities,
                                }),
                            )
                            .await?;
                        self.record_sandbox_ready(SandboxReadyObservation {
                            thread_key,
                            execution_id,
                            sandbox_id: sandbox_id.as_str(),
                            harness_type,
                            source: "warm_pool",
                            ready_duration,
                            startup_duration: None,
                        })
                        .await;
                        info!(
                            component = COMPONENT_SESSION_RUNTIME,
                            event = "sandbox_ensure_warm_claimed",
                            thread_key = %thread_key,
                            execution_id,
                            sandbox_id = %sandbox_id,
                            harness_type = %harness_type,
                            sandbox_ready_source = "warm_pool",
                            sandbox_ready_duration_ms = duration_millis_u64(ready_duration),
                            workload_key = warm_pool.workload_key(),
                            "claimed warm session sandbox"
                        );
                        return Ok(sandbox_id);
                    }
                    Ok(None) => record_sandbox_warm_pool_claim("miss"),
                    Err(error) => {
                        record_sandbox_warm_pool_claim("error");
                        return Err(SessionRuntimeError::WarmPool(error));
                    }
                }
            }

            let mut spec = (self.sandbox_runtime.spec_factory)(
                thread_key,
                execution_id,
                harness_type,
                persona_context.as_ref(),
            );
            if let Some(principal) = iron_control_principal {
                spec.iron_control_principal = Some(principal.to_owned());
            }
            apply_sandbox_boot_mode(&mut spec, &boot_mode);
            apply_sandbox_capabilities(&mut spec, desired_capabilities);
            let create_started = Instant::now();
            let handle = self
                .run_with_running_capacity(thread_key, execution_id, "cold_create", || async {
                    self.sandbox_runtime
                        .manager
                        .create_running(spec)
                        .await
                        .map_err(SessionRuntimeError::Sandbox)
                })
                .await?;
            let startup_duration = create_started.elapsed();
            let ready_duration = ensure_started.elapsed();
            span.record("centaur.sandbox_id", handle.id.as_str());
            span.record("sandbox_id", handle.id.as_str());
            self.store
                .update_sandbox_assignment(thread_key, handle.id.as_str(), desired_capabilities)
                .await?;
            self.record_sandbox_ready(SandboxReadyObservation {
                thread_key,
                execution_id,
                sandbox_id: handle.id.as_str(),
                harness_type,
                source: "cold_create",
                ready_duration,
                startup_duration: Some(startup_duration),
            })
            .await;
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "sandbox_ensure_created",
                thread_key = %thread_key,
                execution_id,
                sandbox_id = %handle.id.as_str(),
                harness_type = %harness_type,
                sandbox_ready_source = "cold_create",
                sandbox_ready_duration_ms = duration_millis_u64(ready_duration),
                sandbox_startup_duration_ms = duration_millis_u64(startup_duration),
                sandbox_startup_duration_seconds = startup_duration.as_secs_f64(),
                "created new session sandbox"
            );
            Ok(handle.id.into_string())
        }
        .instrument(span.clone())
        .await;

        if let Err(error) = &result {
            error!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "sandbox_ensure_failed",
                thread_key = %thread_key,
                execution_id,
                %error,
                "failed to ensure session sandbox"
            );
        }
        result
    }

    async fn resolve_sandbox_capabilities(
        &self,
        iron_control_principal: Option<&str>,
    ) -> Result<SessionSandboxCapabilities, SessionRuntimeError> {
        let Some(principal_id) = iron_control_principal else {
            return Ok(SessionSandboxCapabilities::default_enabled());
        };
        let Some(registrar) = &self.iron_control else {
            return Ok(SessionSandboxCapabilities::default_enabled());
        };
        let principal = registrar.get_principal(principal_id).await?;
        Ok(sandbox_capabilities_from_principal(&principal))
    }

    async fn record_sandbox_ready(&self, observation: SandboxReadyObservation<'_>) {
        let SandboxReadyObservation {
            thread_key,
            execution_id,
            sandbox_id,
            harness_type,
            source,
            ready_duration,
            startup_duration,
        } = observation;
        let ready_duration_ms = duration_millis_u64(ready_duration);
        let startup_duration_ms = startup_duration.map(duration_millis_u64).unwrap_or(0);
        let sandbox_started_for_request = startup_duration.is_some();

        if let Err(error) = self
            .store
            .touch_sandbox_activity(thread_key, sandbox_id)
            .await
        {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_sandbox_activity_touch_failed",
                thread_key = %thread_key,
                execution_id,
                sandbox_id,
                %error,
                "failed to touch sandbox activity after sandbox ready"
            );
        }

        if let Err(error) = self
            .store
            .append_event(
                thread_key,
                Some(execution_id),
                "session.sandbox_ready",
                json!({
                    "execution_id": execution_id,
                    "thread_key": thread_key.as_str(),
                    "sandbox_id": sandbox_id,
                    "harness_type": harness_type.to_string(),
                    "sandbox_ready_source": source,
                    "sandbox_ready_duration_ms": ready_duration_ms,
                    "sandbox_startup_duration_ms": startup_duration_ms,
                    "sandbox_started_for_request": sandbox_started_for_request,
                }),
            )
            .await
        {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "sandbox_ready_event_append_failed",
                thread_key = %thread_key,
                execution_id,
                sandbox_id,
                %error,
                "failed to append sandbox ready event"
            );
        }

        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "sandbox_ready",
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
            harness_type = %harness_type,
            sandbox_ready_source = source,
            sandbox_ready_duration_ms = ready_duration_ms,
            sandbox_startup_duration_ms = startup_duration_ms,
            sandbox_started_for_request,
            "session sandbox ready"
        );
    }

    async fn ensure_session_pipe_if_live(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: &str,
    ) -> Result<(), SessionRuntimeError> {
        let id = SandboxId::new(sandbox_id);
        match self.sandbox_runtime.manager.status(&id).await {
            Ok(status) if should_attach_session_pipe(&status) => {
                if let Err(error) = self.ensure_session_pipe(thread_key, sandbox_id).await
                    && !is_event_stream_attach_race(&error)
                {
                    return Err(error);
                }
            }
            Ok(_) => {}
            Err(SandboxError::NotFound(_)) => {}
            Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
        }
        Ok(())
    }

    async fn ensure_session_pipe(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: &str,
    ) -> Result<SessionPipe, SessionRuntimeError> {
        let span = info_span!(
            "centaur.api_rs.session.pipe.ensure",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_pipe_ensure",
            "centaur.thread_key" = thread_key.as_str(),
            "centaur.sandbox_id" = sandbox_id,
            thread_key = %thread_key,
            sandbox_id,
        );
        let result = async {
            if let Some(pipe) = self
                .sandbox_pipes
                .get(sandbox_id)
                .map(|entry| entry.clone())
            {
                info!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_pipe_reused",
                    thread_key = %thread_key,
                    sandbox_id,
                    "reusing session pipe"
                );
                return Ok(pipe);
            }

            let open_lock = {
                let entry = self
                    .sandbox_pipe_open_locks
                    .entry(sandbox_id.to_owned())
                    .or_insert_with(|| Arc::new(Mutex::new(())));
                entry.clone()
            };
            let _open_guard = open_lock.lock().await;

            if let Some(pipe) = self
                .sandbox_pipes
                .get(sandbox_id)
                .map(|entry| entry.clone())
            {
                info!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_pipe_reused",
                    thread_key = %thread_key,
                    sandbox_id,
                    "reusing session pipe"
                );
                return Ok(pipe);
            }

            let io = self
                .sandbox_runtime
                .manager
                .open_io(&SandboxId::new(sandbox_id))
                .await?
                .into_parts();
            let pipe = session_pipe_from_stdin(io.stdin);

            self.sandbox_pipes
                .insert(sandbox_id.to_owned(), pipe.clone());
            drop(_open_guard);
            let ctx = self.context();
            let thread_key = thread_key.clone();
            let pump_thread_key = thread_key.clone();
            let pump_key = sandbox_id.to_owned();
            let pump_pipe = pipe.clone();
            let stdout = io.stdout;
            let stderr = io.stderr;
            let guard = io.guard;
            let stderr_key = pump_key.clone();

            spawn_stdout_pump_loop(StdoutPumpLoop {
                ctx,
                open_lock,
                thread_key: pump_thread_key,
                sandbox_id: pump_key,
                pipe: pump_pipe,
                stdout,
                guard,
            });

            spawn_stderr_drain(stderr_key, stderr);

            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_pipe_opened",
                thread_key = %thread_key,
                sandbox_id,
                "session pipe opened"
            );
            Ok(pipe)
        }
        .instrument(span.clone())
        .await;

        if let Err(error) = &result {
            error!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_pipe_ensure_failed",
                thread_key = %thread_key,
                sandbox_id,
                %error,
                "failed to ensure session pipe"
            );
        }
        result
    }

    /// Reconciles executions left `queued`/`running` by a previous control
    /// plane process. Execution rows never time out on their own: the only
    /// writer of a terminal status is the process that was watching the
    /// sandbox, so a kill mid-turn leaves the row active forever, wedging the
    /// thread (the one-active-execution index blocks new executes) and any
    /// event-stream consumer waiting for a terminal event.
    ///
    /// Adoption order of preference:
    /// 1. The sandbox already finished the turn while nobody was attached:
    ///    recover the terminal outcome from the backend's recorded output.
    /// 2. The sandbox is still running the turn: re-attach the stdout pump
    ///    and re-arm the remaining max-duration deadline.
    /// 3. The sandbox is gone: record the failure honestly.
    pub async fn adopt_orphaned_executions(&self) {
        // A one-shot scan has no later tick to revisit skipped rows, so
        // queued orphans are failed immediately regardless of age — the
        // pre-rescan startup behavior.
        self.run_orphan_adoption_scan(&mut OrphanAdoptionState::default(), None)
            .await;
    }

    /// Re-run the orphan adoption scan every `interval` for the lifetime of
    /// the process (the first scan runs immediately). A startup-only scan
    /// misses executions orphaned after it ran — most commonly the previous
    /// pod of a rolling deploy reaching its termination grace period
    /// mid-turn after the new pod already scanned — and those stay wedged
    /// until the next deploy.
    pub fn spawn_orphan_adoption(&self, interval: Duration) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut state = OrphanAdoptionState::default();
            let mut ticker = interval_at(Instant::now(), interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                runtime
                    .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
                    .await;
            }
        });
    }

    /// One pass over all active executions. `pre_sandbox_grace` is the
    /// minimum age before a row awaiting sandbox assignment is treated as
    /// orphaned; `None` is only correct when no re-scan will follow.
    async fn run_orphan_adoption_scan(
        &self,
        state: &mut OrphanAdoptionState,
        pre_sandbox_grace: Option<Duration>,
    ) {
        let executions = match self.store.list_active_executions_with_ownership().await {
            Ok(executions) => executions,
            Err(error) => {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_adoption_scan_failed",
                    %error,
                    "failed to list orphaned executions"
                );
                return;
            }
        };
        if executions.is_empty() {
            state.deferred.clear();
            return;
        }
        let mut adopted = 0_usize;
        let mut failed = 0_usize;
        let mut skipped = 0_usize;
        let mut own = 0_usize;
        let mut deferred = HashSet::new();
        for candidate in executions {
            let execution_id = candidate.execution.execution_id.clone();
            // Advisory fast path: a live lease means the execution has an
            // active pump somewhere. Skip our own executions silently and
            // defer peers' without touching the session row or the sandbox
            // backend — the conditional claim below stays the sole authority
            // on ownership.
            if candidate.stdout_owner_lease_active {
                if candidate.stdout_owner_id.as_deref() == Some(self.stdout_owner_id.as_str()) {
                    own += 1;
                    continue;
                }
                if !state.deferred.contains(&execution_id) {
                    self.record_adoption_deferral(&candidate.execution).await;
                }
                deferred.insert(execution_id);
                continue;
            }
            let record_deferral = !state.deferred.contains(&execution_id);
            match self
                .adopt_orphaned_execution(&candidate.execution, record_deferral, pre_sandbox_grace)
                .await
            {
                Ok(OrphanAdoption::Adopted) => adopted += 1,
                Ok(OrphanAdoption::Failed) => failed += 1,
                Ok(OrphanAdoption::Skipped) => skipped += 1,
                Ok(OrphanAdoption::Deferred) => {
                    deferred.insert(execution_id);
                }
                Err(error) => {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_adoption_failed",
                        thread_key = %candidate.execution.thread_key,
                        execution_id = %candidate.execution.execution_id,
                        %error,
                        "failed to adopt orphaned execution; will retry on the next scan"
                    );
                    // Keep the dedup entry across transient errors so a
                    // recovered deferral is not re-recorded.
                    if state.deferred.contains(&execution_id) {
                        deferred.insert(execution_id);
                    }
                }
            }
        }
        state.deferred = deferred;
        if adopted > 0 || failed > 0 {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "execution_adoption_scan",
                adopted,
                failed,
                deferred = state.deferred.len(),
                skipped,
                own,
                "adopted executions orphaned by a previous control plane process"
            );
        } else {
            debug!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "execution_adoption_scan",
                adopted,
                failed,
                deferred = state.deferred.len(),
                skipped,
                own,
                "orphan adoption scan found nothing adoptable"
            );
        }
    }

    async fn record_adoption_deferral(&self, execution: &SessionExecution) {
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "execution_adoption_deferred",
            thread_key = %execution.thread_key,
            execution_id = %execution.execution_id,
            "active stdout owner lease still exists; deferring adoption"
        );
        let _ = self
            .store
            .append_event(
                &execution.thread_key,
                Some(&execution.execution_id),
                "session.execution_adoption_deferred",
                json!({ "reason": "stdout_owner_lease_active" }),
            )
            .await;
    }

    async fn adopt_orphaned_execution(
        &self,
        execution: &SessionExecution,
        record_deferral: bool,
        pre_sandbox_grace: Option<Duration>,
    ) -> Result<OrphanAdoption, SessionRuntimeError> {
        let thread_key = &execution.thread_key;
        let execution_id = execution.execution_id.as_str();
        if execution.status == ExecutionStatus::Queued {
            // Input is only written after an execution is marked running, so
            // a queued orphan never reached the harness: nothing can come.
            // On a periodic scan, young queued rows are skipped instead of
            // failed: they are most likely a live execute_session observed
            // mid-transition, and a later tick revisits them.
            if let Some(grace) = pre_sandbox_grace {
                let age = SystemTime::now()
                    .duration_since(SystemTime::from(execution.created_at))
                    .unwrap_or_default();
                if age < grace {
                    debug!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_adoption_skipped",
                        thread_key = %thread_key,
                        execution_id,
                        age_ms = duration_millis_u64(age),
                        "skipping young queued execution; a live execute may still claim it"
                    );
                    return Ok(OrphanAdoption::Skipped);
                }
            }
            self.fail_orphaned_execution(
                thread_key,
                execution_id,
                "",
                "orphaned before input was sent",
            )
            .await;
            return Ok(OrphanAdoption::Failed);
        }
        let session = self.store.get_session(thread_key).await?;
        let Some(sandbox_id) = session.sandbox_id.as_deref() else {
            let running_since = execution.started_at.unwrap_or(execution.created_at);
            let running_age = SystemTime::now()
                .duration_since(SystemTime::from(running_since))
                .unwrap_or_default();
            if pre_sandbox_grace.is_some_and(|grace| running_age < grace) {
                debug!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_adoption_skipped",
                    thread_key = %thread_key,
                    execution_id,
                    age_ms = duration_millis_u64(running_age),
                    "skipping young running execution awaiting sandbox assignment"
                );
                return Ok(OrphanAdoption::Skipped);
            }
            self.fail_orphaned_execution(
                thread_key,
                execution_id,
                "",
                "orphaned with no sandbox assigned",
            )
            .await;
            return Ok(OrphanAdoption::Failed);
        };
        let id = SandboxId::new(sandbox_id);
        let status = match self.sandbox_runtime.manager.status(&id).await {
            Ok(status) => status,
            Err(SandboxError::NotFound(_)) => SandboxStatus::Gone,
            // Transient status failures must not fail a possibly live
            // execution; surface the error and retry on the next startup.
            Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
        };
        if !status.can_open_io() {
            self.fail_orphaned_execution(
                thread_key,
                execution_id,
                sandbox_id,
                &format!("sandbox no longer accepts io (status {status:?})"),
            )
            .await;
            return Ok(OrphanAdoption::Failed);
        }
        if !self.claim_expired_stdout_owner(execution_id).await? {
            // Deferrals repeat on every periodic scan while another control
            // plane pumps the execution; only the first observation is worth
            // an info log and a durable event.
            if record_deferral {
                self.record_adoption_deferral(execution).await;
            } else {
                debug!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_adoption_deferred",
                    thread_key = %thread_key,
                    execution_id,
                    sandbox_id,
                    "active stdout owner lease still exists; deferring adoption"
                );
            }
            return Ok(OrphanAdoption::Deferred);
        }

        // The turn may have finished while no control plane was attached. An
        // attach stream cannot replay that output, but the backend's recorded
        // history (pod logs) can.
        let since = execution.started_at.unwrap_or(execution.created_at);
        let lines = match self
            .sandbox_runtime
            .manager
            .read_output_since(&id, Some(SystemTime::from(since)))
            .await
        {
            Ok(lines) => lines,
            Err(SandboxError::Unsupported { .. }) => Vec::new(),
            Err(error) => {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_adoption_log_read_failed",
                    thread_key = %thread_key,
                    execution_id,
                    sandbox_id,
                    %error,
                    "failed to read recorded sandbox output; adopting live"
                );
                Vec::new()
            }
        };
        if let Some(terminal) = terminal_output_from_lines(&lines) {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "execution_adopted",
                thread_key = %thread_key,
                execution_id,
                sandbox_id,
                mode = "recorded_output",
                "adopted orphaned execution from recorded sandbox output"
            );
            let _ = self
                .store
                .append_event(
                    thread_key,
                    Some(execution_id),
                    "session.execution_adopted",
                    json!({ "sandbox_id": sandbox_id, "mode": "recorded_output" }),
                )
                .await;
            record_terminal_output(
                &self.context(),
                thread_key,
                sandbox_id,
                execution_id,
                terminal,
            )
            .await?;
            return Ok(OrphanAdoption::Adopted);
        }

        // No terminal in the recorded output: treat the turn as still in
        // flight. Re-attach the stdout pump and re-arm the remaining
        // max-duration budget so an adopted-but-silent turn stays bounded.
        if let Err(error) = self.ensure_session_pipe(thread_key, sandbox_id).await {
            let _ = self
                .store
                .release_stdout_owner(execution_id, &self.stdout_owner_id)
                .await;
            return Err(error);
        }
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "execution_adopted",
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
            mode = "live_attach",
            "adopted orphaned execution with a live sandbox attach"
        );
        let _ = self
            .store
            .append_event(
                thread_key,
                Some(execution_id),
                "session.execution_adopted",
                json!({ "sandbox_id": sandbox_id, "mode": "live_attach" }),
            )
            .await;
        if let Some(max_duration) = max_duration_from_execution(execution) {
            let elapsed = SystemTime::now()
                .duration_since(SystemTime::from(since))
                .unwrap_or_default();
            spawn_max_duration_failure(
                self.context(),
                thread_key.clone(),
                execution.execution_id.clone(),
                max_duration.saturating_sub(elapsed),
                idle_timeout_from_execution(execution),
            );
        }
        Ok(OrphanAdoption::Adopted)
    }

    async fn fail_orphaned_execution(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        sandbox_id: &str,
        detail: &str,
    ) {
        let _ = self
            .store
            .claim_stdout_owner(execution_id, &self.stdout_owner_id, STDOUT_OWNER_LEASE)
            .await;
        let error = format!("execution orphaned by control plane restart; {detail}");
        if let Err(record_error) = record_terminal_output(
            &self.context(),
            thread_key,
            sandbox_id,
            execution_id,
            TerminalOutput::Failed { error },
        )
        .await
        {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "execution_adoption_fail_record_failed",
                thread_key = %thread_key,
                execution_id,
                error = %record_error,
                "failed to record orphaned execution failure"
            );
        }
    }

    /// Hands off this control plane's in-flight executions before process
    /// exit. Waits up to `timeout` for owned executions to finish naturally
    /// (their stdout pumps keep running until the process exits), then
    /// releases the remaining stdout-owner leases so another control
    /// plane's adoption scan can claim the executions right away instead of
    /// waiting out the lease TTL. Turn output produced after the release is
    /// not lost: adoption replays it from the sandbox backend's recorded
    /// output.
    pub async fn handoff_owned_executions(&self, timeout: Duration) {
        // Fence new stdout-owner claims first: an execution accepted after
        // this point would otherwise claim a lease that outlives the
        // process, stranding it until the lease TTL expires.
        self.shutting_down.store(true, Ordering::SeqCst);
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        loop {
            let count = tokio::time::timeout(
                EXECUTION_HANDOFF_DB_TIMEOUT,
                self.store
                    .count_executions_with_stdout_owner(&self.stdout_owner_id),
            )
            .await;
            let Ok(count) = count else {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_handoff_count_timeout",
                    "timed out counting in-flight executions; releasing leases now"
                );
                break;
            };
            match count {
                Ok(0) => {
                    info!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_handoff_idle",
                        "no in-flight executions to hand off at shutdown"
                    );
                    return;
                }
                Ok(in_flight) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    info!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_handoff_waiting",
                        in_flight,
                        "waiting for in-flight executions to finish before shutdown"
                    );
                }
                Err(error) => {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_handoff_count_failed",
                        %error,
                        "failed to count in-flight executions; releasing leases now"
                    );
                    break;
                }
            }
            sleep(EXECUTION_HANDOFF_POLL_INTERVAL).await;
        }
        let released = tokio::time::timeout(
            EXECUTION_HANDOFF_DB_TIMEOUT,
            self.store
                .release_stdout_owned_executions(&self.stdout_owner_id),
        )
        .await;
        let Ok(released) = released else {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "execution_handoff_release_timeout",
                "timed out releasing stdout-owner leases; peers must wait for lease expiry"
            );
            return;
        };
        match released {
            Ok(released) => {
                for execution in &released {
                    info!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_handoff_released",
                        thread_key = %execution.thread_key,
                        execution_id = %execution.execution_id,
                        "released stdout-owner lease at shutdown for adoption by a peer"
                    );
                    let _ = self
                        .store
                        .append_event(
                            &execution.thread_key,
                            Some(&execution.execution_id),
                            "session.stdout_owner_released",
                            json!({
                                "execution_id": execution.execution_id,
                                "reason": "control_plane_shutdown",
                            }),
                        )
                        .await;
                }
                if released.is_empty() {
                    info!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "execution_handoff_idle",
                        "in-flight executions finished during the shutdown drain"
                    );
                }
            }
            Err(error) => {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_handoff_release_failed",
                    %error,
                    "failed to release stdout-owner leases at shutdown"
                );
            }
        }
    }
}

/// Outcome of one orphan-adoption attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrphanAdoption {
    /// Terminal output was recovered or a live pump was re-attached.
    Adopted,
    /// Another control plane still holds the stdout-owner lease.
    Deferred,
    /// The execution was failed as unrecoverable.
    Failed,
    /// Too young to judge (freshly queued); revisit on a later scan.
    Skipped,
}

/// Scan state carried across periodic orphan-adoption ticks.
#[derive(Debug, Default)]
struct OrphanAdoptionState {
    /// Executions whose deferral was already recorded, so long-lived leases
    /// do not produce a `session.execution_adoption_deferred` event on every
    /// tick.
    deferred: HashSet<String>,
}

async fn maybe_generate_session_title(
    store: PgSessionStore,
    generator: SessionTitleGenerator,
    thread_key: ThreadKey,
) {
    let parts = match store.title_generation_candidate(&thread_key).await {
        Ok(Some(parts)) => parts,
        Ok(None) => return,
        Err(error) => {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_title_candidate_failed",
                thread_key = %thread_key,
                %error,
                "failed to load session title candidate"
            );
            return;
        }
    };
    let Some(source) = session_title_source_from_parts(&parts) else {
        return;
    };
    let raw_title = match generator(source).await {
        Ok(title) => title,
        Err(error) => {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_title_generation_failed",
                thread_key = %thread_key,
                %error,
                "failed to generate session title"
            );
            return;
        }
    };
    let Some(title) = sanitize_session_title(&raw_title) else {
        warn!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_title_generation_empty",
            thread_key = %thread_key,
            "session title generation returned an empty title"
        );
        return;
    };
    match store.set_session_title_if_empty(&thread_key, &title).await {
        Ok(true) => {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_title_set",
                thread_key = %thread_key,
                title,
                "session title set"
            );
        }
        Ok(false) => {}
        Err(error) => {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_title_set_failed",
                thread_key = %thread_key,
                %error,
                "failed to set session title"
            );
        }
    }
}

impl SandboxRuntime {
    pub async fn create_running_io(
        &self,
        spec: SandboxSpec,
    ) -> Result<(SandboxId, centaur_sandbox_core::SandboxIoParts), SessionRuntimeError> {
        let handle = self.manager.create_running(spec).await?;
        let io = self.manager.open_io(&handle.id).await?.into_parts();
        Ok((handle.id, io))
    }

    pub async fn stop_sandbox(&self, sandbox_id: &SandboxId) -> Result<(), SessionRuntimeError> {
        self.manager.stop(sandbox_id).await?;
        Ok(())
    }

    pub fn backend(backend: Arc<dyn SandboxBackend>, spec: SandboxSpec) -> Self {
        let warm_spec = spec.clone();
        let spec_factory =
            move |_thread_key: &ThreadKey,
                  _execution_id: &str,
                  _harness: &HarnessType,
                  _persona: Option<&PersonaContext>| { spec.clone() };
        let warm_spec_factory = move || warm_spec.clone();
        Self::backend_with_warm_spec_factory(backend, spec_factory, warm_spec_factory)
    }

    pub fn backend_with_workload(
        backend: Arc<dyn SandboxBackend>,
        workload: SandboxWorkloadMode,
    ) -> Self {
        let warm_harness = workload.default_harness();
        let warm_workload = workload.clone();
        let mut runtime = Self::backend_with_warm_spec_factory(
            backend,
            move |thread_key, _execution_id, harness, persona| {
                workload.spec(thread_key, harness, persona)
            },
            move || warm_workload.warm_spec(),
        );
        runtime.warm_harness = warm_harness;
        runtime
    }

    pub fn backend_with_spec_factory<F>(backend: Arc<dyn SandboxBackend>, spec_factory: F) -> Self
    where
        F: Fn(&ThreadKey, &str, &HarnessType, Option<&PersonaContext>) -> SandboxSpec
            + Send
            + Sync
            + 'static,
    {
        Self {
            manager: Arc::new(SandboxManager::new(backend)),
            spec_factory: Arc::new(spec_factory),
            warm_spec_factory: None,
            workload_key: None,
            warm_harness: None,
        }
    }

    pub fn backend_with_warm_spec_factory<F, W>(
        backend: Arc<dyn SandboxBackend>,
        spec_factory: F,
        warm_spec_factory: W,
    ) -> Self
    where
        F: Fn(&ThreadKey, &str, &HarnessType, Option<&PersonaContext>) -> SandboxSpec
            + Send
            + Sync
            + 'static,
        W: Fn() -> SandboxSpec + Send + Sync + 'static,
    {
        let warm_spec_factory: WarmSandboxSpecFactory = Arc::new(warm_spec_factory);
        let workload_key = sandbox_spec_key(&warm_spec_factory());
        Self {
            manager: Arc::new(SandboxManager::new(backend)),
            spec_factory: Arc::new(spec_factory),
            warm_spec_factory: Some(warm_spec_factory),
            workload_key: Some(workload_key),
            warm_harness: None,
        }
    }
}

impl SandboxWorkloadMode {
    pub fn mock_app_server(image: impl Into<String>) -> Self {
        Self::MockAppServer {
            image: image.into(),
        }
    }

    pub fn codex_app_server(
        image: impl Into<String>,
        env: impl IntoIterator<Item = (String, String)>,
        harness: HarnessType,
    ) -> Self {
        Self::CodexAppServer {
            image: image.into(),
            env: env.into_iter().collect(),
            mounts: Vec::new(),
            harness,
        }
    }

    pub fn mount(mut self, mount: Mount) -> Self {
        match &mut self {
            Self::MockAppServer { .. } => {}
            Self::CodexAppServer { mounts, .. } => mounts.push(mount),
        }
        self
    }

    fn default_harness(&self) -> Option<HarnessType> {
        match self {
            Self::MockAppServer { .. } => None,
            Self::CodexAppServer { harness, .. } => Some(harness.clone()),
        }
    }

    fn spec(
        &self,
        thread_key: &ThreadKey,
        harness: &HarnessType,
        persona: Option<&PersonaContext>,
    ) -> SandboxSpec {
        self.spec_for(Some(thread_key), harness, persona)
    }

    fn warm_spec(&self) -> SandboxSpec {
        match self {
            Self::MockAppServer { .. } => self.spec_for(None, &HarnessType::Codex, None),
            Self::CodexAppServer { harness, .. } => self.spec_for(None, harness, None),
        }
    }

    fn spec_for(
        &self,
        thread_key: Option<&ThreadKey>,
        harness: &HarnessType,
        persona: Option<&PersonaContext>,
    ) -> SandboxSpec {
        match self {
            Self::MockAppServer { image } => apply_persona_spec_env(
                SandboxSpec::new(image)
                    .command(["/bin/sh", "-lc"])
                    .args([mock_app_server_script()])
                    .env("CENTAUR_HARNESS_TYPE", harness.as_ref()),
                persona,
            ),
            Self::CodexAppServer {
                image, env, mounts, ..
            } => {
                // Pin the harness via container args (the image entrypoint is
                // kept) so the sandbox runs the session's harness rather than
                // whatever the image CMD defaults to.
                let mut spec = SandboxSpec::new(image)
                    .label("centaur.ai/component", "session-sandbox")
                    .label("centaur.ai/harness", harness.to_string())
                    .args(["harness-server", harness_server_subcommand(harness)]);
                if let Some(thread_key) = thread_key {
                    spec = spec.env("CENTAUR_THREAD_KEY", thread_key.as_str());
                }
                for mount in mounts {
                    spec = spec.mount(mount.clone());
                }
                for (name, value) in env {
                    spec = spec.env(name.clone(), value.clone());
                }
                apply_persona_spec_env(spec, persona)
            }
        }
    }
}

/// The harness-server CLI subcommand for a harness type
/// (see crates/harness-server/src/main.rs).
fn harness_server_subcommand(harness: &HarnessType) -> &'static str {
    match harness {
        HarnessType::Codex => "codex",
        HarnessType::ClaudeCode => "claude-code",
        HarnessType::Amp => "amp",
    }
}

fn sandbox_spec_key(spec: &SandboxSpec) -> String {
    let encoded = serde_json::to_vec(spec).expect("sandbox specs should serialize");
    let digest = Sha256::digest(encoded);
    format!("sandbox-spec-sha256:{digest:x}")
}

fn mock_app_server_script() -> &'static str {
    r#"while IFS= read -r line; do
model="$(printf '%s\n' "$line" | sed -n 's/.*"model":"\([^"]*\)".*/\1/p')"
[ -n "$model" ] || model="unknown"
harness="${CENTAUR_HARNESS_TYPE:-unknown}"
printf '%s\n' '{"type":"system","subtype":"wrapper_heartbeat","phase":"startup"}'
sleep 0.2
printf '%s\n' '{"type":"system","subtype":"wrapper_heartbeat","phase":"app_server_started"}'
sleep 0.2
printf '%s\n' '{"type":"thread.started","thread_id":"mock-codex-thread"}'
sleep 0.2
turn_index=1
while [ "$turn_index" -le 3 ]; do
  turn_id="mock-turn-$turn_index"
  printf '{"type":"turn.started","turn_id":"%s"}\n' "$turn_id"
  sleep 0.2
  printf '{"type":"item.agentMessage.delta","turnId":"%s","session_id":"mock-codex-thread","delta":"PONG model=%s harness=%s"}\n' "$turn_id" "$model" "$harness"
  sleep 0.2
  printf '{"type":"turn.completed","turn":{"id":"%s"},"usage":{"input_tokens":0,"output_tokens":1}}\n' "$turn_id"
  sleep 0.2
  turn_index=$((turn_index + 1))
done
done"#
}

fn session_event_stream(
    store: PgSessionStore,
    thread_key: ThreadKey,
    after_event_id: i64,
    execution_id: Option<String>,
    listener: SessionEventListener,
    span: Span,
) -> impl Stream<Item = Result<SessionEvent, SessionRuntimeError>> {
    stream::unfold(
        EventStreamState {
            store,
            thread_key,
            after_event_id,
            execution_id,
            pending: VecDeque::new(),
            listener,
            safety_tick: {
                let mut tick = interval_at(
                    Instant::now() + EVENT_STREAM_SAFETY_POLL_INTERVAL,
                    EVENT_STREAM_SAFETY_POLL_INTERVAL,
                );
                tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
                tick
            },
            done: false,
            emitted_count: 0,
            span,
        },
        |mut state| {
            let span = state.span.clone();
            async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        state.after_event_id = event.event_id;
                        state.emitted_count += 1;
                        // Execution-scoped streams are per-turn: after the
                        // execution's terminal event nothing else will ever
                        // arrive, so complete the response instead of parking
                        // forever. Abandoned client connections otherwise pin
                        // this stream's dedicated LISTEN connection until the
                        // TCP peer is proven dead (the 2026-07-06 incident
                        // exhausted both the Slackbot fetch pool and staging
                        // Postgres this way). The 30s safety tick makes this
                        // robust even when the notify is missed.
                        if state.execution_id.is_some()
                            && is_terminal_execution_event(&event.event_type)
                        {
                            state.done = true;
                        }
                        return Some((Ok(event), state));
                    }
                    if state.done {
                        info!(
                            component = COMPONENT_SESSION_RUNTIME,
                            event = "session_events_stream_completed",
                            thread_key = %state.thread_key,
                            emitted_count = state.emitted_count,
                            "session event stream completed"
                        );
                        return None;
                    }
                    match state
                        .store
                        .list_events_after(
                            &state.thread_key,
                            state.after_event_id,
                            state.execution_id.as_deref(),
                            100,
                        )
                        .await
                    {
                        Ok(events) if events.is_empty() => loop {
                            tokio::select! {
                                notification = state.listener.recv() => {
                                    match notification {
                                        Ok(notification)
                                            if notification.thread_key == state.thread_key.as_str()
                                                && notification.event_id > state.after_event_id =>
                                        {
                                            break;
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            state.done = true;
                                            return Some((Err(SessionRuntimeError::Store(error)), state));
                                        }
                                    }
                                }
                                _ = state.safety_tick.tick() => break,
                            }
                        }
                        Ok(events) => state.pending = events.into(),
                        Err(error) => {
                            state.done = true;
                            return Some((Err(SessionRuntimeError::Store(error)), state));
                        }
                    }
                }
            }
            .instrument(span)
        },
    )
}

/// Terminal event types for a single execution: once one of these is emitted
/// on an execution-scoped stream, the stream has nothing left to deliver.
fn is_terminal_execution_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "session.execution_completed" | "session.execution_failed" | "session.execution_cancelled"
    )
}

/// How a stdout pump pass ended once the attach stream closed.
enum StdoutPumpEnd {
    /// The stream closed with no execution in flight, or the execution was
    /// already terminalized by a read/codec failure.
    Idle,
    /// The stream closed while an execution was still active. Treat this as a
    /// transport detach; the pump loop decides whether to recover or fail.
    EofActiveExecution {
        execution: Box<SessionExecution>,
        lines_pumped: u64,
    },
}

struct StdoutPumpLoop {
    ctx: RuntimeContext,
    open_lock: Arc<Mutex<()>>,
    thread_key: ThreadKey,
    sandbox_id: String,
    pipe: SessionPipe,
    stdout: SandboxRead,
    guard: SandboxIoGuard,
}

enum ReattachOutcome {
    Reattached {
        pipe: SessionPipe,
        stdout: SandboxRead,
        guard: SandboxIoGuard,
    },
    /// Another pipe replaced ours; that pump now owns the sandbox stream.
    Superseded,
    /// A retryable attach/status failure. The caller bounds attempts.
    Retryable(String),
    /// The sandbox cannot serve IO anymore.
    Dead(String),
}

fn session_pipe_from_stdin(stdin: SandboxWrite) -> SessionPipe {
    SessionPipe {
        stdin: Arc::new(Mutex::new(FramedWrite::new(stdin, LinesCodec::new()))),
    }
}

fn spawn_stderr_drain(sandbox_id: String, stderr: SandboxRead) {
    tokio::spawn(async move {
        if let Err(error) = drain_stderr(stderr).await {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_stderr_drain_failed",
                sandbox_id = %sandbox_id,
                %error,
                "session stderr drain failed"
            );
        }
    });
}

fn remove_pipe_if_current(sandbox_pipes: &SessionPipeMap, sandbox_id: &str, pipe: &SessionPipe) {
    sandbox_pipes.remove_if(sandbox_id, |_sandbox_id, current| {
        Arc::ptr_eq(&current.stdin, &pipe.stdin)
    });
}

/// Runs the stdout pump and reattaches when Kubernetes closes the attach
/// stream before the active execution emits terminal output.
fn spawn_stdout_pump_loop(state: StdoutPumpLoop) {
    tokio::spawn(async move {
        let StdoutPumpLoop {
            ctx,
            open_lock,
            thread_key,
            sandbox_id,
            mut pipe,
            mut stdout,
            mut guard,
        } = state;
        let mut reattach_attempts = 0_u32;
        let mut last_reattach_detail = "stdout reattach attempts exhausted".to_owned();

        'pump: loop {
            let result =
                run_stdout_pump(ctx.clone(), thread_key.clone(), &sandbox_id, stdout, guard).await;
            let (execution, lines_pumped) = match result {
                Ok(StdoutPumpEnd::Idle) => break,
                Ok(StdoutPumpEnd::EofActiveExecution {
                    execution,
                    lines_pumped,
                }) => (execution, lines_pumped),
                Err(error) => {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "session_stdout_pump_failed",
                        thread_key = %thread_key,
                        sandbox_id = %sandbox_id,
                        %error,
                        "session stdout pump failed"
                    );
                    let _ = ctx
                        .store
                        .append_event(
                            &thread_key,
                            None,
                            "session.stdout_pump_failed",
                            json!({
                                "sandbox_id": sandbox_id.as_str(),
                                "error": error.to_string(),
                            }),
                        )
                        .await;
                    break;
                }
            };

            if recover_detached_terminal_output(&ctx, &thread_key, &sandbox_id, &execution)
                .await
                .unwrap_or_else(|error| {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "session_stdout_recovery_failed",
                        thread_key = %thread_key,
                        sandbox_id = %sandbox_id,
                        execution_id = %execution.execution_id,
                        %error,
                        "failed to recover detached stdout from recorded output"
                    );
                    false
                })
            {
                break;
            }

            if lines_pumped > 0 {
                reattach_attempts = 0;
            }

            loop {
                if reattach_attempts >= SESSION_PIPE_MAX_REATTACH_ATTEMPTS {
                    fail_detached_execution(
                        &ctx,
                        &thread_key,
                        &sandbox_id,
                        &execution.execution_id,
                        &last_reattach_detail,
                    )
                    .await;
                    break 'pump;
                }
                reattach_attempts += 1;
                if reattach_attempts > 1 {
                    sleep(SESSION_PIPE_REATTACH_DELAY).await;
                }

                match reattach_session_pipe(&ctx, &open_lock, &sandbox_id, &pipe).await {
                    ReattachOutcome::Reattached {
                        pipe: new_pipe,
                        stdout: new_stdout,
                        guard: new_guard,
                    } => {
                        info!(
                            component = COMPONENT_SESSION_RUNTIME,
                            event = "session_stdout_pump_reattached",
                            thread_key = %thread_key,
                            sandbox_id = %sandbox_id,
                            execution_id = %execution.execution_id,
                            attempt = reattach_attempts,
                            "reattached session stdout pump after eof"
                        );
                        let _ = ctx
                            .store
                            .append_event(
                                &thread_key,
                                Some(&execution.execution_id),
                                "session.stdout_pump_reattached",
                                json!({
                                    "sandbox_id": sandbox_id.as_str(),
                                    "attempt": reattach_attempts,
                                }),
                            )
                            .await;
                        pipe = new_pipe;
                        stdout = new_stdout;
                        guard = new_guard;
                        continue 'pump;
                    }
                    ReattachOutcome::Superseded => return,
                    ReattachOutcome::Retryable(detail) => {
                        warn!(
                            component = COMPONENT_SESSION_RUNTIME,
                            event = "session_stdout_pump_reattach_failed",
                            thread_key = %thread_key,
                            sandbox_id = %sandbox_id,
                            execution_id = %execution.execution_id,
                            attempt = reattach_attempts,
                            detail = %detail,
                            "session stdout pump reattach attempt failed"
                        );
                        last_reattach_detail = detail;
                    }
                    ReattachOutcome::Dead(detail) => {
                        fail_detached_execution(
                            &ctx,
                            &thread_key,
                            &sandbox_id,
                            &execution.execution_id,
                            &detail,
                        )
                        .await;
                        break 'pump;
                    }
                }
            }
        }

        remove_pipe_if_current(&ctx.sandbox_pipes, &sandbox_id, &pipe);
    });
}

async fn reattach_session_pipe(
    ctx: &RuntimeContext,
    open_lock: &Arc<Mutex<()>>,
    sandbox_id: &str,
    pipe: &SessionPipe,
) -> ReattachOutcome {
    let _open_guard = open_lock.lock().await;
    if ctx
        .sandbox_pipes
        .get(sandbox_id)
        .is_none_or(|current| !Arc::ptr_eq(&current.stdin, &pipe.stdin))
    {
        return ReattachOutcome::Superseded;
    }

    let id = SandboxId::new(sandbox_id);
    match ctx.manager.status(&id).await {
        Ok(status) if status.can_open_io() => match ctx.manager.open_io(&id).await {
            Ok(io) => {
                let parts = io.into_parts();
                let new_pipe = session_pipe_from_stdin(parts.stdin);
                ctx.sandbox_pipes
                    .insert(sandbox_id.to_owned(), new_pipe.clone());
                spawn_stderr_drain(sandbox_id.to_owned(), parts.stderr);
                ReattachOutcome::Reattached {
                    pipe: new_pipe,
                    stdout: parts.stdout,
                    guard: parts.guard,
                }
            }
            Err(error) => {
                ReattachOutcome::Retryable(format!("sandbox stdout reattach failed: {error}"))
            }
        },
        Ok(status) => {
            ReattachOutcome::Dead(format!("sandbox no longer accepts io (status {status:?})"))
        }
        Err(SandboxError::NotFound(_)) => {
            ReattachOutcome::Dead("sandbox no longer exists".to_owned())
        }
        Err(error) => ReattachOutcome::Retryable(format!("sandbox status check failed: {error}")),
    }
}

async fn recover_detached_terminal_output(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    execution: &SessionExecution,
) -> Result<bool, SessionRuntimeError> {
    let since = execution.started_at.unwrap_or(execution.created_at);
    let id = SandboxId::new(sandbox_id);
    let lines = match ctx
        .manager
        .read_output_since(&id, Some(SystemTime::from(since)))
        .await
    {
        Ok(lines) => lines,
        Err(SandboxError::Unsupported { .. }) => return Ok(false),
        Err(error) => {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_stdout_recorded_output_read_failed",
                thread_key = %thread_key,
                execution_id = %execution.execution_id,
                sandbox_id,
                %error,
                "failed to read recorded sandbox output; reattaching live"
            );
            return Ok(false);
        }
    };

    let Some(terminal) = terminal_output_from_lines(&lines) else {
        return Ok(false);
    };

    info!(
        component = COMPONENT_SESSION_RUNTIME,
        event = "session_stdout_pump_recovered",
        thread_key = %thread_key,
        execution_id = %execution.execution_id,
        sandbox_id,
        mode = "recorded_output",
        "recovered detached stdout pump from recorded sandbox output"
    );
    let _ = ctx
        .store
        .append_event(
            thread_key,
            Some(&execution.execution_id),
            "session.stdout_pump_recovered",
            json!({ "sandbox_id": sandbox_id, "mode": "recorded_output" }),
        )
        .await;
    record_terminal_output(
        ctx,
        thread_key,
        sandbox_id,
        &execution.execution_id,
        terminal,
    )
    .await?;
    Ok(true)
}

async fn fail_detached_execution(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    execution_id: &str,
    detail: &str,
) {
    let error = format!("sandbox stdout closed before terminal output; {detail}");
    if let Err(record_error) = record_terminal_output(
        ctx,
        thread_key,
        sandbox_id,
        execution_id,
        TerminalOutput::Failed { error },
    )
    .await
    {
        warn!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_stdout_detached_fail_record_failed",
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
            error = %record_error,
            "failed to record detached stdout failure"
        );
    }
}

async fn run_stdout_pump(
    ctx: RuntimeContext,
    thread_key: ThreadKey,
    sandbox_id: &str,
    stdout: SandboxRead,
    _guard: SandboxIoGuard,
) -> Result<StdoutPumpEnd, SessionRuntimeError> {
    let span = info_span!(
        "centaur.api_rs.session.stdout_pump",
        component = COMPONENT_SESSION_RUNTIME,
        event = "session_stdout_pump",
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.sandbox_id" = sandbox_id,
        thread_key = %thread_key,
        sandbox_id,
    );
    set_span_parent_trace(
        &span,
        &thread_trace_id(&thread_key),
        &thread_trace_parent_span_id(&thread_key),
    );
    async {
        ensure_thread_trace_root_span(&thread_key);
        let mut stdout = FramedRead::new(stdout, LinesCodec::new());
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_stdout_pump_started",
            thread_key = %thread_key,
            sandbox_id,
            "session stdout pump started"
        );
        let mut output_state = StdoutPumpState::default();
        let mut lost_stdout_ownership = HashSet::new();
        let mut line_count = 0_u64;
        while let Some(line) = stdout.next().await {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    let message = stdout_pump_error_message(&error);
                    record_stdout_pump_failure(&ctx, &thread_key, sandbox_id, message).await?;
                    return Ok(StdoutPumpEnd::Idle);
                }
            };
            line_count += 1;
            let output_value = serde_json::from_str::<Value>(&line).ok();
            if let Some(harness_thread_id) = harness_thread_id_from_output_line(&line)
                && let Err(error) = ctx
                    .store
                    .update_harness_thread_id(&thread_key, Some(&harness_thread_id))
                    .await
            {
                warn!(%thread_key, %harness_thread_id, %error, "failed to persist harness thread id");
            }
            let active_execution = ctx.store.active_execution_for_thread(&thread_key).await?;
            let execution_id = active_execution
                .as_ref()
                .map(|execution| execution.execution_id.as_str());
            let Some(output_execution_id) = output_state.execution_for_line(execution_id, &line)
            else {
                continue;
            };
            if lost_stdout_ownership.contains(&output_execution_id) {
                continue;
            }
            let first_token_execution = active_execution
                .as_ref()
                .filter(|execution| {
                    execution.execution_id == output_execution_id
                        && output_state.should_record_first_token(
                            &output_execution_id,
                            output_value.as_ref(),
                        )
                })
                .cloned();
            let execution_span = ctx
                .execution_spans
                .lock()
                .await
                .get(&output_execution_id)
                .cloned();
            let output_span = output_state.stdout_span_for_execution(
                execution_span.as_ref(),
                &thread_key,
                sandbox_id,
                &output_execution_id,
            );
            let Some(output_event) =
                append_output_line(&ctx, &thread_key, &output_execution_id, &line)
                    .instrument(output_span.clone())
                    .await?
            else {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "session_stdout_owner_lost",
                    thread_key = %thread_key,
                    execution_id = %output_execution_id,
                    sandbox_id,
                    stdout_owner_id = %ctx.stdout_owner_id,
                    "stdout pump no longer owns execution output; suppressing further rows"
                );
                lost_stdout_ownership.insert(output_execution_id.clone());
                output_state.forget(&output_execution_id);
                continue;
            };
            if let Some(execution) = first_token_execution {
                record_first_token_observation(
                    &ctx,
                    &thread_key,
                    &execution,
                    &output_event,
                    &mut output_state,
                )
                .await;
            }
            if let Some(value) = output_value.as_ref() {
                output_state.record_codex_app_server_spans(
                    &output_span,
                    &thread_key,
                    sandbox_id,
                    &output_execution_id,
                    value,
                );
            }
            if let Some(execution) = active_execution
                && execution.execution_id == output_execution_id
                && let Some(terminal) = output_state.observe(&output_execution_id, &line)
            {
                record_terminal_output(
                    &ctx,
                    &thread_key,
                    sandbox_id,
                    &output_execution_id,
                    terminal,
                )
                .instrument(output_span)
                .await?;
                ctx.execution_spans.lock().await.remove(&output_execution_id);
                output_state.forget(&output_execution_id);
            }
        }
        let active_execution = ctx.store.active_execution_for_thread(&thread_key).await?;
        ctx.store
            .append_event(
                &thread_key,
                None,
                "session.stdout_eof",
                json!({
                    "sandbox_id": sandbox_id,
                }),
            )
            .await?;
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_stdout_pump_completed",
            thread_key = %thread_key,
            sandbox_id,
            output_line_count = line_count,
            "session stdout pump completed"
        );
        match active_execution {
            Some(execution) => Ok(StdoutPumpEnd::EofActiveExecution {
                execution: Box::new(execution),
                lines_pumped: line_count,
            }),
            None => Ok(StdoutPumpEnd::Idle),
        }
    }
    .instrument(span)
    .await
}

async fn record_stdout_pump_failure(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    error: String,
) -> Result<(), SessionRuntimeError> {
    let active_execution = ctx.store.active_execution_for_thread(thread_key).await?;
    let execution_id = active_execution
        .as_ref()
        .map(|execution| execution.execution_id.as_str());
    ctx.store
        .append_event(
            thread_key,
            execution_id,
            "session.stdout_pump_failed",
            json!({
                "sandbox_id": sandbox_id,
                "error": error.as_str(),
                "terminalized_execution": execution_id.is_some(),
            }),
        )
        .await?;
    if let Some(execution) = active_execution {
        record_terminal_output(
            ctx,
            thread_key,
            sandbox_id,
            &execution.execution_id,
            TerminalOutput::Failed { error },
        )
        .await?;
    }
    Ok(())
}

async fn record_first_token_observation(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    execution: &SessionExecution,
    output_event: &SessionEvent,
    output_state: &mut StdoutPumpState,
) {
    match ctx
        .store
        .execution_event_exists(&execution.execution_id, SESSION_FIRST_TOKEN_EVENT)
        .await
    {
        Ok(true) => {
            output_state.mark_first_token_recorded(&execution.execution_id);
            return;
        }
        Ok(false) => {}
        Err(error) => {
            warn!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_first_token_marker_check_failed",
                thread_key = %thread_key,
                execution_id = %execution.execution_id,
                %error,
                "failed to check existing first-token marker"
            );
        }
    }

    let Some(latency) = first_token_latency(execution, output_event) else {
        output_state.mark_first_token_recorded(&execution.execution_id);
        return;
    };
    let harness_label = match ctx.store.get_session(thread_key).await {
        Ok(session) => session.harness_type.to_string(),
        Err(error) => {
            warn!(%thread_key, %error, "failed to load session for first-token metric labels");
            "unknown".to_owned()
        }
    };
    let latency_ms = duration_millis_u64(latency);
    if let Err(error) = ctx
        .store
        .append_event(
            thread_key,
            Some(&execution.execution_id),
            SESSION_FIRST_TOKEN_EVENT,
            json!({
                "execution_id": execution.execution_id.as_str(),
                "thread_key": thread_key.as_str(),
                "harness_type": harness_label.as_str(),
                "latency_ms": latency_ms,
                "output_event_id": output_event.event_id,
            }),
        )
        .await
    {
        warn!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_first_token_marker_append_failed",
            thread_key = %thread_key,
            execution_id = %execution.execution_id,
            output_event_id = output_event.event_id,
            %error,
            "failed to append first-token marker"
        );
    }
    record_session_first_token_latency(&harness_label, latency);
    output_state.mark_first_token_recorded(&execution.execution_id);
    info!(
        component = COMPONENT_SESSION_RUNTIME,
        event = "session_first_token_observed",
        thread_key = %thread_key,
        execution_id = %execution.execution_id,
        harness_type = %harness_label,
        latency_ms,
        output_event_id = output_event.event_id,
        "session first answer token observed"
    );
}

fn first_token_latency(
    execution: &SessionExecution,
    output_event: &SessionEvent,
) -> Option<Duration> {
    let started_at = execution.started_at.unwrap_or(execution.created_at);
    (output_event.created_at - started_at).try_into().ok()
}

#[derive(Default)]
struct StdoutPumpState {
    final_answer_text_by_execution: HashMap<String, String>,
    first_token_recorded_by_execution: HashSet<String>,
    turn_execution_by_id: HashMap<String, String>,
    item_execution_by_id: HashMap<String, String>,
    tool_call_by_id: HashMap<String, ToolCallLabels>,
    stdout_span_by_execution: HashMap<String, Span>,
}

impl StdoutPumpState {
    fn execution_for_line(
        &mut self,
        active_execution_id: Option<&str>,
        line: &str,
    ) -> Option<String> {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return active_execution_id.map(ToOwned::to_owned);
        };

        if let Some(known_execution_id) = self.known_execution_for_value(&value) {
            if active_execution_id == Some(known_execution_id.as_str()) {
                self.remember_value_execution(&value, &known_execution_id);
                return Some(known_execution_id);
            }
            if terminal_output(
                &value,
                self.final_answer_text_by_execution
                    .get(&known_execution_id)
                    .map(String::as_str)
                    .unwrap_or(""),
            )
            .is_some()
            {
                self.forget(&known_execution_id);
            }
            return None;
        }

        let active_execution_id = active_execution_id?;
        self.remember_value_execution(&value, active_execution_id);
        Some(active_execution_id.to_owned())
    }

    fn observe(&mut self, execution_id: &str, line: &str) -> Option<TerminalOutput> {
        let value: Value = serde_json::from_str(line).ok()?;
        if let Some(update) = output_line_final_answer_text(&value) {
            let text = self
                .final_answer_text_by_execution
                .entry(execution_id.to_owned())
                .or_default();
            match update {
                FinalAnswerTextUpdate::Append(delta) => text.push_str(&delta),
                FinalAnswerTextUpdate::Replace(canonical) => *text = canonical,
            }
        }
        terminal_output(
            &value,
            self.final_answer_text_by_execution
                .get(execution_id)
                .map(String::as_str)
                .unwrap_or(""),
        )
    }

    fn should_record_first_token(&self, execution_id: &str, value: Option<&Value>) -> bool {
        if self
            .first_token_recorded_by_execution
            .contains(execution_id)
            || self
                .final_answer_text_by_execution
                .get(execution_id)
                .is_some_and(|text| !text.trim().is_empty())
        {
            return false;
        }

        let Some(value) = value else {
            return false;
        };
        if output_line_final_answer_text(value).is_some() {
            return true;
        }
        matches!(
            terminal_output(value, ""),
            Some(TerminalOutput::Completed {
                result_text: Some(_),
                ..
            })
        )
    }

    fn mark_first_token_recorded(&mut self, execution_id: &str) {
        self.first_token_recorded_by_execution
            .insert(execution_id.to_owned());
    }

    fn forget(&mut self, execution_id: &str) {
        self.final_answer_text_by_execution.remove(execution_id);
        self.first_token_recorded_by_execution.remove(execution_id);
        let tool_ids_to_forget = self
            .item_execution_by_id
            .iter()
            .filter(|&(_item_id, mapped_execution_id)| mapped_execution_id == execution_id)
            .map(|(item_id, _mapped_execution_id)| item_id.clone())
            .collect::<Vec<_>>();
        self.turn_execution_by_id
            .retain(|_, mapped_execution_id| mapped_execution_id != execution_id);
        self.item_execution_by_id
            .retain(|_, mapped_execution_id| mapped_execution_id != execution_id);
        self.stdout_span_by_execution.remove(execution_id);
        for item_id in tool_ids_to_forget {
            self.tool_call_by_id.remove(&item_id);
        }
    }

    fn stdout_span_for_execution(
        &mut self,
        parent: Option<&Span>,
        thread_key: &ThreadKey,
        sandbox_id: &str,
        execution_id: &str,
    ) -> Span {
        if let Some(span) = self.stdout_span_by_execution.get(execution_id) {
            return span.clone();
        }
        let span = new_stdout_pump_span(parent, thread_key, sandbox_id, execution_id);
        self.stdout_span_by_execution
            .insert(execution_id.to_owned(), span.clone());
        span
    }

    fn record_codex_app_server_spans(
        &mut self,
        parent: &Span,
        thread_key: &ThreadKey,
        sandbox_id: &str,
        execution_id: &str,
        value: &Value,
    ) {
        record_codex_app_server_event_span(parent, thread_key, sandbox_id, execution_id, value);
        for event in tool_call_span_events(value, &mut self.tool_call_by_id) {
            record_codex_app_server_tool_span(parent, thread_key, sandbox_id, execution_id, &event);
        }
    }

    fn known_execution_for_value(&self, value: &Value) -> Option<String> {
        for turn_id in turn_ids(value) {
            if let Some(execution_id) = self.turn_execution_by_id.get(&turn_id) {
                return Some(execution_id.clone());
            }
        }
        for item_id in item_ids(value) {
            if let Some(execution_id) = self.item_execution_by_id.get(&item_id) {
                return Some(execution_id.clone());
            }
        }
        None
    }

    fn remember_value_execution(&mut self, value: &Value, execution_id: &str) {
        for turn_id in turn_ids(value) {
            self.turn_execution_by_id
                .insert(turn_id, execution_id.to_owned());
        }
        for item_id in item_ids(value) {
            self.item_execution_by_id
                .insert(item_id, execution_id.to_owned());
        }
    }
}

fn new_stdout_pump_span(
    parent: Option<&Span>,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    execution_id: &str,
) -> Span {
    if let Some(parent) = parent {
        info_span!(
            parent: parent,
            "centaur.api_rs.session.stdout_pump",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_stdout_pump",
            "centaur.thread_key" = thread_key.as_str(),
            "centaur.execution_id" = execution_id,
            "centaur.sandbox_id" = sandbox_id,
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
        )
    } else {
        info_span!(
            "centaur.api_rs.session.stdout_pump",
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_stdout_pump",
            "centaur.thread_key" = thread_key.as_str(),
            "centaur.execution_id" = execution_id,
            "centaur.sandbox_id" = sandbox_id,
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolCallLabels {
    kind: String,
    name: String,
    method: String,
}

#[derive(Clone, Debug, PartialEq)]
struct ToolCallSpanEvent {
    labels: ToolCallLabels,
    status: &'static str,
    duration: Option<Duration>,
}

fn record_codex_app_server_event_span(
    parent: &Span,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    execution_id: &str,
    value: &Value,
) {
    let event_type = sandbox_output_event_type(value);
    let source = sandbox_output_source(value);
    let item = protocol_item(value);
    let item_type = item
        .and_then(|item| string_at_path(item, &["type"]))
        .unwrap_or_default();
    let turn_id = turn_ids(value).into_iter().next().unwrap_or_default();
    let item_id = item_ids(value).into_iter().next().unwrap_or_default();

    let span = info_span!(
        parent: parent,
        "centaur.api_rs.codex_app_server.event",
        component = COMPONENT_SESSION_RUNTIME,
        event = "codex_app_server_event",
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.execution_id" = execution_id,
        "centaur.sandbox_id" = sandbox_id,
        "codex_app_server.source" = source,
        "codex_app_server.event_type" = event_type,
        "codex_app_server.item_type" = item_type.as_str(),
        "codex_app_server.turn_id" = turn_id.as_str(),
        "codex_app_server.item_id" = item_id.as_str(),
    );
    let _entered = span.enter();
}

fn record_codex_app_server_tool_span(
    parent: &Span,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    execution_id: &str,
    event: &ToolCallSpanEvent,
) {
    let duration_ms = event
        .duration
        .map(|duration| duration.as_secs_f64() * 1000.0);
    let span = info_span!(
        parent: parent,
        "centaur.api_rs.codex_app_server.tool_call",
        component = COMPONENT_SESSION_RUNTIME,
        event = "codex_app_server_tool_call",
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.execution_id" = execution_id,
        "centaur.sandbox_id" = sandbox_id,
        "tool.kind" = event.labels.kind.as_str(),
        "tool.name" = event.labels.name.as_str(),
        "tool.method" = event.labels.method.as_str(),
        "tool.status" = event.status,
        "tool.duration_ms" = tracing::field::Empty,
    );
    if let Some(duration_ms) = duration_ms {
        span.record("tool.duration_ms", duration_ms);
    }
    let _entered = span.enter();
}

fn sandbox_output_event_type(value: &Value) -> &str {
    value
        .get("method")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
        .filter(|event_type| !event_type.trim().is_empty())
        .unwrap_or("json")
}

fn sandbox_output_source(value: &Value) -> &str {
    if value.get("method").and_then(Value::as_str).is_some() {
        return "codex_app_server";
    }
    match value.get("type").and_then(Value::as_str) {
        Some(event_type)
            if event_type.starts_with("item.")
                || event_type.starts_with("turn.")
                || event_type.starts_with("thread.") =>
        {
            "codex_app_server"
        }
        Some("system")
            if value
                .get("subtype")
                .and_then(Value::as_str)
                .is_some_and(|subtype| subtype.starts_with("wrapper_")) =>
        {
            "codex_app_server"
        }
        Some("assistant" | "user" | "tool") => "harness",
        Some(_) | None => "sandbox",
    }
}

fn tool_call_span_events(
    value: &Value,
    known_tool_calls: &mut HashMap<String, ToolCallLabels>,
) -> Vec<ToolCallSpanEvent> {
    let mut events = Vec::new();
    let event_type = sandbox_output_event_type(value);

    if matches!(event_type, "item/started" | "item.started")
        && let Some(item) = protocol_item(value)
        && let Some(labels) = tool_labels_from_item(item)
    {
        remember_tool_call_labels(item, &labels, known_tool_calls);
        events.push(ToolCallSpanEvent {
            labels,
            status: "started",
            duration: None,
        });
    }

    if matches!(event_type, "item/completed" | "item.completed")
        && let Some(item) = protocol_item(value)
    {
        let item_id = string_at_path(item, &["id"]);
        let labels = tool_labels_from_item(item).or_else(|| {
            item_id
                .as_deref()
                .and_then(|item_id| known_tool_calls.get(item_id).cloned())
        });
        if let Some(labels) = labels {
            let status = completed_tool_status(item);
            if let Some(item_id) = item_id {
                known_tool_calls.remove(&item_id);
            }
            events.push(ToolCallSpanEvent {
                labels,
                status,
                duration: duration_from_ms_value(
                    item.get("durationMs").or_else(|| item.get("duration_ms")),
                ),
            });
        }
    }

    if matches!(
        event_type,
        "item/mcpToolCall/progress" | "item.mcpToolCall.progress"
    ) {
        let labels = progress_item_id(value)
            .and_then(|item_id| known_tool_calls.get(&item_id).cloned())
            .unwrap_or_else(|| ToolCallLabels {
                kind: "mcp".to_owned(),
                name: "unknown".to_owned(),
                method: "unknown".to_owned(),
            });
        events.push(ToolCallSpanEvent {
            labels,
            status: "progress",
            duration: None,
        });
    }

    for tool_use in anthropic_tool_uses(value) {
        let labels = ToolCallLabels {
            kind: "anthropic".to_owned(),
            name: string_at_path(tool_use, &["name"]).unwrap_or_else(|| "unknown".to_owned()),
            method: "call".to_owned(),
        };
        if let Some(tool_id) = string_at_path(tool_use, &["id"]) {
            known_tool_calls.insert(tool_id, labels.clone());
        }
        events.push(ToolCallSpanEvent {
            labels,
            status: "started",
            duration: None,
        });
    }

    for tool_result in anthropic_tool_results(value) {
        let labels = string_at_path(tool_result, &["tool_use_id"])
            .and_then(|tool_use_id| known_tool_calls.remove(&tool_use_id))
            .unwrap_or_else(|| ToolCallLabels {
                kind: "anthropic".to_owned(),
                name: "unknown".to_owned(),
                method: "call".to_owned(),
            });
        events.push(ToolCallSpanEvent {
            labels,
            status: if tool_result
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "failed"
            } else {
                "completed"
            },
            duration: None,
        });
    }

    events
}

fn protocol_item(value: &Value) -> Option<&Value> {
    value
        .get("params")
        .and_then(|params| params.get("item"))
        .or_else(|| value.get("item"))
}

fn tool_labels_from_item(item: &Value) -> Option<ToolCallLabels> {
    let item_type = string_at_path(item, &["type"])?;
    match item_type.as_str() {
        "mcpToolCall" | "mcp_tool_call" => Some(ToolCallLabels {
            kind: "mcp".to_owned(),
            name: string_at_path(item, &["tool"]).unwrap_or_else(|| "unknown".to_owned()),
            method: string_at_path(item, &["server"]).unwrap_or_else(|| "call".to_owned()),
        }),
        "dynamicToolCall" | "dynamic_tool_call" => Some(ToolCallLabels {
            kind: "dynamic".to_owned(),
            name: string_at_path(item, &["tool"]).unwrap_or_else(|| "unknown".to_owned()),
            method: string_at_path(item, &["namespace"]).unwrap_or_else(|| "call".to_owned()),
        }),
        "collabAgentToolCall" | "collab_agent_tool_call" => Some(ToolCallLabels {
            kind: "collab_agent".to_owned(),
            name: string_at_path(item, &["tool"]).unwrap_or_else(|| "agent".to_owned()),
            method: "call".to_owned(),
        }),
        _ => None,
    }
}

fn remember_tool_call_labels(
    item: &Value,
    labels: &ToolCallLabels,
    known_tool_calls: &mut HashMap<String, ToolCallLabels>,
) {
    if let Some(item_id) = string_at_path(item, &["id"]) {
        known_tool_calls.insert(item_id, labels.clone());
    }
}

fn completed_tool_status(item: &Value) -> &'static str {
    if item
        .get("success")
        .and_then(Value::as_bool)
        .is_some_and(|success| !success)
        || item.get("error").is_some()
    {
        return "failed";
    }

    if let Some(exit_code) = item.get("exitCode").and_then(Value::as_i64) {
        return if exit_code == 0 {
            "completed"
        } else {
            "failed"
        };
    }

    match item
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
    {
        "failed" | "error" | "cancelled" | "declined" => "failed",
        "inProgress" | "in_progress" | "running" => "started",
        _ => "completed",
    }
}

fn duration_from_ms_value(value: Option<&Value>) -> Option<Duration> {
    let millis = value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_u64().map(|millis| millis as f64))
            .or_else(|| value.as_i64().map(|millis| millis as f64))
    })?;
    if millis.is_finite() && millis >= 0.0 {
        Some(Duration::from_secs_f64(millis / 1000.0))
    } else {
        None
    }
}

fn progress_item_id(value: &Value) -> Option<String> {
    [
        &["params", "itemId"][..],
        &["params", "item_id"][..],
        &["itemId"][..],
        &["item_id"][..],
    ]
    .into_iter()
    .filter_map(|path| string_at_path(value, path))
    .next()
}

fn anthropic_tool_uses(value: &Value) -> Vec<&Value> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    content_blocks(value)
        .into_iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_use"))
        .collect()
}

fn anthropic_tool_results(value: &Value) -> Vec<&Value> {
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("user" | "tool")
    ) {
        return Vec::new();
    }
    content_blocks(value)
        .into_iter()
        .filter(|part| {
            part.get("type").and_then(Value::as_str) == Some("tool_result")
                || part.get("tool_use_id").and_then(Value::as_str).is_some()
        })
        .collect()
}

fn content_blocks(value: &Value) -> Vec<&Value> {
    value
        .get("content")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("content"))
        })
        .and_then(Value::as_array)
        .map(|values| values.iter().collect())
        .unwrap_or_default()
}

#[derive(Debug, Eq, PartialEq)]
enum TerminalOutput {
    Completed {
        reason: &'static str,
        result_text: Option<String>,
    },
    Cancelled {
        reason: &'static str,
    },
    Failed {
        error: String,
    },
}

async fn record_terminal_output(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    sandbox_id: &str,
    execution_id: &str,
    terminal: TerminalOutput,
) -> Result<(), SessionRuntimeError> {
    let mut failure_class = None;
    let (terminal_execution, terminal_status) = match terminal {
        TerminalOutput::Completed {
            reason,
            result_text,
        } => {
            let Some(execution) = ctx
                .store
                .complete_execution_if_active_and_stdout_owner(execution_id, &ctx.stdout_owner_id)
                .await?
            else {
                return Ok(());
            };
            let mut payload = json!({
                "execution_id": execution_id,
                "thread_key": thread_key.as_str(),
                "completion_reason": reason,
            });
            if let (Some(result_text), Some(object)) =
                (result_text.as_deref(), payload.as_object_mut())
            {
                object.insert("result_text".to_owned(), json!(result_text));
            }
            ctx.store
                .append_event(
                    thread_key,
                    Some(execution_id),
                    "session.execution_completed",
                    payload,
                )
                .await?;
            (execution, "completed")
        }
        TerminalOutput::Cancelled { reason } => {
            let Some(execution) = ctx
                .store
                .cancel_execution_if_active_and_stdout_owner(
                    execution_id,
                    &ctx.stdout_owner_id,
                    reason,
                )
                .await?
            else {
                return Ok(());
            };
            ctx.store
                .append_event(
                    thread_key,
                    Some(execution_id),
                    "session.execution_cancelled",
                    json!({
                        "execution_id": execution_id,
                        "thread_key": thread_key.as_str(),
                        "reason": reason,
                    }),
                )
                .await?;
            (execution, "cancelled")
        }
        TerminalOutput::Failed { error } => {
            failure_class = Some(terminal_failure_class(&error));
            let Some(execution) = ctx
                .store
                .fail_execution_if_active_and_stdout_owner(
                    execution_id,
                    &ctx.stdout_owner_id,
                    &error,
                )
                .await?
            else {
                return Ok(());
            };
            ctx.store
                .append_event(
                    thread_key,
                    Some(execution_id),
                    "session.execution_failed",
                    json!({
                        "execution_id": execution_id,
                        "thread_key": thread_key.as_str(),
                        "error": error.as_str(),
                    }),
                )
                .await?;
            (execution, "failed")
        }
    };
    ctx.execution_spans.lock().await.remove(execution_id);
    if let Err(error) = ctx
        .store
        .touch_sandbox_activity(thread_key, sandbox_id)
        .await
    {
        warn!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_sandbox_activity_touch_failed",
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
            %error,
            "failed to touch sandbox activity after terminal output"
        );
    }
    record_finished_execution_metric(
        &ctx.store,
        thread_key,
        &terminal_execution,
        terminal_status,
        failure_class,
    )
    .await;
    if let Some(idle_timeout) = idle_timeout_from_execution(&terminal_execution) {
        spawn_idle_pause(
            ctx.clone(),
            thread_key.clone(),
            terminal_execution.execution_id,
            sandbox_id.to_owned(),
            idle_timeout,
        );
    }
    Ok(())
}

fn spawn_max_duration_failure(
    ctx: RuntimeContext,
    thread_key: ThreadKey,
    execution_id: String,
    max_duration: Duration,
    idle_timeout: Option<Duration>,
) {
    tokio::spawn(async move {
        sleep(max_duration).await;
        if let Err(error) = record_max_duration_failure(
            &ctx,
            &thread_key,
            &execution_id,
            max_duration,
            idle_timeout,
        )
        .await
        {
            warn!(%thread_key, %execution_id, %error, "max duration failure task failed");
        }
    });
}

fn spawn_stdout_owner_renewer(ctx: RuntimeContext, execution_id: String) {
    tokio::spawn(async move {
        loop {
            sleep(STDOUT_OWNER_RENEW_INTERVAL).await;
            match ctx
                .store
                .renew_stdout_owner(&execution_id, &ctx.stdout_owner_id, STDOUT_OWNER_LEASE)
                .await
            {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "session_stdout_owner_renew_failed",
                        execution_id,
                        stdout_owner_id = %ctx.stdout_owner_id,
                        %error,
                        "failed to renew stdout owner lease"
                    );
                    break;
                }
            }
        }
    });
}

async fn record_max_duration_failure(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    execution_id: &str,
    max_duration: Duration,
    idle_timeout: Option<Duration>,
) -> Result<(), SessionRuntimeError> {
    let max_duration_ms = duration_millis_u64(max_duration);
    let error = format!("execution exceeded max_duration_ms={max_duration_ms}");
    let Some(execution) = ctx
        .store
        .fail_execution_if_active_and_stdout_owner(execution_id, &ctx.stdout_owner_id, &error)
        .await?
    else {
        return Ok(());
    };
    ctx.execution_spans.lock().await.remove(execution_id);
    if let Err(error) = ctx.store.touch_session_sandbox_activity(thread_key).await {
        warn!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_sandbox_activity_touch_failed",
            thread_key = %thread_key,
            execution_id,
            %error,
            "failed to touch sandbox activity after max duration"
        );
    }
    ctx.store
        .append_event(
            thread_key,
            Some(execution_id),
            "session.execution_failed",
            json!({
                "execution_id": execution_id,
                "thread_key": thread_key.as_str(),
                "error": error,
                "reason": "max_duration_exceeded",
                "max_duration_ms": max_duration_ms,
            }),
        )
        .await?;
    record_finished_execution_metric(
        &ctx.store,
        thread_key,
        &execution,
        "failed",
        Some("timeout"),
    )
    .await;
    if let Some(idle_timeout) = idle_timeout.or_else(|| idle_timeout_from_execution(&execution))
        && let Some(sandbox_id) = ctx.store.get_session(thread_key).await?.sandbox_id
    {
        spawn_idle_pause(
            ctx.clone(),
            thread_key.clone(),
            execution_id.to_owned(),
            sandbox_id,
            idle_timeout,
        );
    }
    Ok(())
}

fn spawn_idle_pause(
    ctx: RuntimeContext,
    thread_key: ThreadKey,
    execution_id: String,
    sandbox_id: String,
    idle_timeout: Duration,
) {
    tokio::spawn(async move {
        sleep(idle_timeout).await;
        if let Err(error) =
            record_idle_pause(&ctx, &thread_key, &execution_id, &sandbox_id, idle_timeout).await
        {
            warn!(%thread_key, %execution_id, %sandbox_id, %error, "idle pause task failed");
        }
    });
}

async fn record_idle_pause(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    execution_id: &str,
    sandbox_id: &str,
    idle_timeout: Duration,
) -> Result<(), SessionRuntimeError> {
    let latest_execution = ctx.store.latest_execution_for_thread(thread_key).await?;
    let session = ctx.store.get_session(thread_key).await?;
    if !should_pause_idle_sandbox(
        &session,
        latest_execution.as_ref(),
        execution_id,
        sandbox_id,
    ) {
        return Ok(());
    }

    let id = SandboxId::new(sandbox_id);
    match ctx.manager.status(&id).await {
        Ok(SandboxStatus::Suspended | SandboxStatus::Stopped | SandboxStatus::Gone) => {
            return Ok(());
        }
        Ok(SandboxStatus::Running | SandboxStatus::Created) => {}
        Ok(SandboxStatus::Unknown(_)) => return Ok(()),
        Err(SandboxError::NotFound(_)) => return Ok(()),
        Err(error) => {
            record_idle_pause_failure(
                &ctx.store,
                thread_key,
                execution_id,
                sandbox_id,
                idle_timeout,
                &error.to_string(),
            )
            .await?;
            return Err(SessionRuntimeError::Sandbox(error));
        }
    }

    ctx.sandbox_pipes.remove(sandbox_id);
    match ctx.manager.pause(&id).await {
        Ok(()) => {
            ctx.store
                .append_event(
                    thread_key,
                    Some(execution_id),
                    "session.sandbox_paused",
                    json!({
                        "execution_id": execution_id,
                        "thread_key": thread_key.as_str(),
                        "sandbox_id": sandbox_id,
                        "reason": "idle_timeout",
                        "idle_timeout_ms": duration_millis_u64(idle_timeout),
                    }),
                )
                .await?;
        }
        Err(error) => {
            record_idle_pause_failure(
                &ctx.store,
                thread_key,
                execution_id,
                sandbox_id,
                idle_timeout,
                &error.to_string(),
            )
            .await?;
            return Err(SessionRuntimeError::Sandbox(error));
        }
    }
    Ok(())
}

async fn record_idle_pause_failure(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: &str,
    sandbox_id: &str,
    idle_timeout: Duration,
    error: &str,
) -> Result<(), SessionRuntimeError> {
    store
        .append_event(
            thread_key,
            Some(execution_id),
            "session.sandbox_pause_failed",
            json!({
                "execution_id": execution_id,
                "thread_key": thread_key.as_str(),
                "sandbox_id": sandbox_id,
                "reason": "idle_timeout",
                "idle_timeout_ms": duration_millis_u64(idle_timeout),
                "error": error,
            }),
        )
        .await?;
    Ok(())
}

fn should_pause_idle_sandbox(
    session: &Session,
    latest_execution: Option<&SessionExecution>,
    execution_id: &str,
    sandbox_id: &str,
) -> bool {
    if session.sandbox_id.as_deref() != Some(sandbox_id) {
        return false;
    }
    let Some(execution) = latest_execution else {
        return false;
    };
    if execution.execution_id != execution_id {
        return false;
    }
    matches!(
        execution.status,
        ExecutionStatus::Completed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn clean_persona_id(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn upsert_spec_env(spec: &mut SandboxSpec, name: &str, value: String) {
    if let Some(existing) = spec.env.iter_mut().find(|env| env.name == name) {
        existing.value = value;
    } else {
        spec.env
            .push(centaur_sandbox_core::EnvVar::new(name, value));
    }
}

fn sandbox_capabilities_match(
    existing: Option<&SessionSandboxCapabilities>,
    desired: &SessionSandboxCapabilities,
) -> bool {
    existing.map_or_else(
        || desired.is_default_enabled(),
        |existing| existing == desired,
    )
}

fn sandbox_repo_cache_access_from_principal(
    principal: &centaur_iron_control::Principal,
) -> SessionRepoCacheAccess {
    match principal
        .labels
        .get(SANDBOX_REPO_CACHE_LABEL)
        .map(|value| value.trim().to_ascii_lowercase())
    {
        Some(value) if value == "all" => SessionRepoCacheAccess::All,
        Some(value) if value == "public" => SessionRepoCacheAccess::Public,
        Some(_) => SessionRepoCacheAccess::None,
        None => SessionRepoCacheAccess::from_legacy_enabled(principal.sandbox_repo_cache_enabled),
    }
}

fn sandbox_capabilities_from_principal(
    principal: &centaur_iron_control::Principal,
) -> SessionSandboxCapabilities {
    SessionSandboxCapabilities {
        repo_cache: sandbox_repo_cache_access_from_principal(principal),
        observability_enabled: principal.sandbox_observability_enabled,
        api_server_enabled: principal.sandbox_api_server_enabled,
    }
}

fn apply_sandbox_capabilities(spec: &mut SandboxSpec, capabilities: &SessionSandboxCapabilities) {
    spec.capabilities = BackendSandboxCapabilities {
        repo_cache: match capabilities.repo_cache {
            SessionRepoCacheAccess::None => RepoCacheAccess::None,
            SessionRepoCacheAccess::Public => RepoCacheAccess::Public,
            SessionRepoCacheAccess::All => RepoCacheAccess::All,
        },
        observability_enabled: capabilities.observability_enabled,
        api_server_enabled: capabilities.api_server_enabled,
    };
    upsert_spec_env(
        spec,
        "CENTAUR_SANDBOX_REPO_CACHE_ENABLED",
        capabilities.repo_cache_enabled().to_string(),
    );
    upsert_spec_env(
        spec,
        "CENTAUR_SANDBOX_REPO_CACHE_ACCESS",
        capabilities.repo_cache.as_str().to_owned(),
    );
    upsert_spec_env(
        spec,
        "CENTAUR_SANDBOX_OBSERVABILITY_ENABLED",
        capabilities.observability_enabled.to_string(),
    );
    upsert_spec_env(
        spec,
        "CENTAUR_SANDBOX_API_SERVER_ENABLED",
        capabilities.api_server_enabled.to_string(),
    );
    match capabilities.repo_cache {
        SessionRepoCacheAccess::None => {
            spec.mounts
                .retain(|mount| mount.target_path != SANDBOX_REPOS_MOUNT_PATH);
            remove_spec_env(spec, CENTAUR_SKILL_DIRS_ENV);
        }
        SessionRepoCacheAccess::Public => {
            scope_repo_cache_mounts_to_public(spec);
            scope_skill_dirs_to_public(spec);
        }
        SessionRepoCacheAccess::All => {
            remove_spec_env(spec, CENTAUR_PUBLIC_SKILL_DIRS_ENV);
        }
    }
    remove_spec_env(spec, CENTAUR_PUBLIC_SKILL_DIRS_ENV);
    if !capabilities.observability_enabled {
        append_spec_env_csv(spec, "TOOL_BLOCKLIST", OBSERVABILITY_TOOL_BLOCKLIST);
    }
}

fn scope_repo_cache_mounts_to_public(spec: &mut SandboxSpec) {
    for mount in spec
        .mounts
        .iter_mut()
        .filter(|mount| mount.target_path == SANDBOX_REPOS_MOUNT_PATH)
    {
        match &mut mount.kind {
            centaur_sandbox_core::MountKind::Bind { source_path } => {
                *source_path = format!(
                    "{}/{}",
                    source_path.trim_end_matches('/'),
                    PUBLIC_REPO_CACHE_SUBPATH
                );
            }
            centaur_sandbox_core::MountKind::NamedVolume(_) => {
                mount.sub_path = Some(PUBLIC_REPO_CACHE_SUBPATH.to_owned());
            }
            centaur_sandbox_core::MountKind::EmptyDir => {}
        }
    }
}

fn scope_skill_dirs_to_public(spec: &mut SandboxSpec) {
    let public_skill_dirs = spec
        .env
        .iter()
        .find(|env| env.name == CENTAUR_PUBLIC_SKILL_DIRS_ENV)
        .map(|env| env.value.trim().to_owned())
        .filter(|value| !value.is_empty());
    match public_skill_dirs {
        Some(public_skill_dirs) => upsert_spec_env(spec, CENTAUR_SKILL_DIRS_ENV, public_skill_dirs),
        None => remove_spec_env(spec, CENTAUR_SKILL_DIRS_ENV),
    }
}

fn append_spec_env_csv(spec: &mut SandboxSpec, name: &str, values: &str) {
    let existing = spec
        .env
        .iter()
        .find(|env| env.name == name)
        .map(|env| env.value.as_str())
        .unwrap_or("");
    let mut merged = existing
        .split(',')
        .chain(values.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .fold(Vec::<String>::new(), |mut acc, value| {
            if !acc.iter().any(|existing| existing == value) {
                acc.push(value.to_owned());
            }
            acc
        });
    merged.sort();
    upsert_spec_env(spec, name, merged.join(","));
}

fn apply_persona_spec_env(mut spec: SandboxSpec, persona: Option<&PersonaContext>) -> SandboxSpec {
    for name in [
        "AGENT_PERSONA",
        "CENTAUR_PERSONA_ID",
        "CENTAUR_PERSONA_PROMPT_HASH",
        "CENTAUR_PERSONA_SOURCE_PATH",
        "CENTAUR_PERSONA_SOURCE_REF",
    ] {
        remove_spec_env(&mut spec, name);
    }
    let Some(persona) = persona else {
        return spec;
    };
    upsert_spec_env(&mut spec, "AGENT_PERSONA", persona.persona_id.clone());
    upsert_spec_env(&mut spec, "CENTAUR_PERSONA_ID", persona.persona_id.clone());
    upsert_spec_env(
        &mut spec,
        "CENTAUR_PERSONA_PROMPT_HASH",
        persona.prompt_hash.clone(),
    );
    upsert_spec_env(
        &mut spec,
        "CENTAUR_PERSONA_SOURCE_PATH",
        persona.source_path.clone(),
    );
    if let Some(source_ref) = persona.source_ref.as_ref() {
        upsert_spec_env(&mut spec, "CENTAUR_PERSONA_SOURCE_REF", source_ref.clone());
    }
    spec
}

fn remove_spec_env(spec: &mut SandboxSpec, name: &str) {
    spec.env.retain(|env| env.name != name);
}

fn add_persona_metadata(metadata: &mut Value, context: &PersonaContext) {
    if let Value::Object(object) = metadata {
        object.insert("persona".to_owned(), json!(context));
    }
}

async fn record_finished_execution_metric(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution: &SessionExecution,
    status: &'static str,
    failure_class: Option<&'static str>,
) {
    let harness_label = match store.get_session(thread_key).await {
        Ok(session) => session.harness_type.to_string(),
        Err(error) => {
            warn!(%thread_key, %error, "failed to load session for execution metric labels");
            "unknown".to_owned()
        }
    };
    record_session_execution_finished(&harness_label, status, execution_duration(execution));
    if let Some(failure_class) = failure_class {
        record_session_failure(&harness_label, failure_class);
    }
}

fn execution_duration(execution: &SessionExecution) -> Option<Duration> {
    let started_at = execution.started_at.unwrap_or(execution.created_at);
    let completed_at = execution.completed_at?;
    (completed_at - started_at).try_into().ok()
}

fn runtime_error_failure_class(error: &SessionRuntimeError) -> &'static str {
    match error {
        SessionRuntimeError::BadRequest(_) => "bad_request",
        SessionRuntimeError::ShuttingDown => "shutting_down",
        SessionRuntimeError::Store(_) => "store",
        SessionRuntimeError::Sandbox(SandboxError::NotFound(_)) => "sandbox_not_found",
        SessionRuntimeError::Sandbox(SandboxError::Unsupported { .. }) => "sandbox_unsupported",
        SessionRuntimeError::Sandbox(SandboxError::NotReady(_)) => "sandbox_not_ready",
        SessionRuntimeError::Sandbox(SandboxError::Io { .. }) => "sandbox_io",
        SessionRuntimeError::Sandbox(SandboxError::Backend { .. }) => "sandbox_backend",
        SessionRuntimeError::Sandbox(SandboxError::InvalidSpec(_)) => "sandbox_invalid_spec",
        SessionRuntimeError::IronControl(_) => "iron_control",
        SessionRuntimeError::WarmPool(_) => "warm_pool",
        SessionRuntimeError::CapacityExceeded { .. } => "capacity",
    }
}

fn terminal_failure_class(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("max_duration") || error.contains("timeout") || error.contains("timed out") {
        return "timeout";
    }
    if error.contains("execution orphaned") {
        return "orphaned";
    }
    if error.contains("sandbox stdout") || error.contains("stdout closed") {
        return "sandbox_io";
    }
    "harness"
}

fn should_attach_session_pipe(status: &SandboxStatus) -> bool {
    status.can_open_io()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExistingSandboxAction {
    Reuse,
    ResumeOrReplace,
    Replace,
}

fn existing_sandbox_action(status: &SandboxStatus) -> ExistingSandboxAction {
    match status {
        SandboxStatus::Running => ExistingSandboxAction::Reuse,
        SandboxStatus::Created | SandboxStatus::Suspended => ExistingSandboxAction::ResumeOrReplace,
        SandboxStatus::Stopped | SandboxStatus::Gone | SandboxStatus::Unknown(_) => {
            ExistingSandboxAction::Replace
        }
    }
}

fn is_event_stream_attach_race(error: &SessionRuntimeError) -> bool {
    matches!(
        error,
        SessionRuntimeError::Sandbox(SandboxError::NotReady(_))
    )
}

fn terminal_output(value: &Value, prior_final_answer_text: &str) -> Option<TerminalOutput> {
    let method = value.get("method").and_then(Value::as_str);
    let event_type = value.get("type").and_then(Value::as_str);

    if matches!(method, Some("error" | "turn/failed"))
        || matches!(event_type, Some("error" | "turn.failed"))
    {
        return Some(TerminalOutput::Failed {
            error: terminal_error_text(value),
        });
    }

    if method == Some("turn/completed") {
        return Some(completed_turn_terminal_output(
            value,
            prior_final_answer_text,
        ));
    }

    match event_type {
        Some("turn.completed") => Some(completed_turn_terminal_output(
            value,
            prior_final_answer_text,
        )),
        Some("turn.done") => Some(completed_terminal_output(value, "turn_done")),
        Some("result") => {
            if result_is_failure(value) {
                Some(TerminalOutput::Failed {
                    error: terminal_error_text(value),
                })
            } else {
                Some(completed_terminal_output(value, "result"))
            }
        }
        _ => None,
    }
}

fn completed_turn_terminal_output(value: &Value, prior_final_answer_text: &str) -> TerminalOutput {
    match turn_completion_status(value).as_deref() {
        Some("completed" | "succeeded" | "success") | None => {
            completed_terminal_output_with_fallback(
                value,
                "turn_completed",
                prior_final_answer_text,
            )
        }
        Some("interrupted") if prior_final_answer_text.trim().is_empty() => {
            TerminalOutput::Cancelled {
                reason: "turn_interrupted",
            }
        }
        Some(_status) if !prior_final_answer_text.trim().is_empty() => {
            completed_terminal_output_with_fallback(
                value,
                "turn_completed",
                prior_final_answer_text,
            )
        }
        Some(status) => TerminalOutput::Failed {
            error: format!("turn completed with status {status} before final answer"),
        },
    }
}

fn completed_terminal_output(value: &Value, reason: &'static str) -> TerminalOutput {
    completed_terminal_output_with_fallback(value, reason, "")
}

fn completed_terminal_output_with_fallback(
    value: &Value,
    reason: &'static str,
    fallback_text: &str,
) -> TerminalOutput {
    let result_text = terminal_payload_text(value).trim().to_owned();
    let result_text = if result_text.is_empty() {
        fallback_text.trim().to_owned()
    } else {
        result_text
    };
    TerminalOutput::Completed {
        reason,
        result_text: (!result_text.is_empty()).then_some(result_text),
    }
}

fn turn_completion_status(value: &Value) -> Option<String> {
    [
        &["turn", "status"][..],
        &["params", "turn", "status"][..],
        &["status"][..],
        &["params", "status"][..],
    ]
    .into_iter()
    .filter_map(|path| string_at_path(value, path))
    .next()
}

enum FinalAnswerTextUpdate {
    Append(String),
    Replace(String),
}

fn output_line_final_answer_text(value: &Value) -> Option<FinalAnswerTextUpdate> {
    let method = value.get("method").and_then(Value::as_str);
    let event_type = value.get("type").and_then(Value::as_str);
    if matches!(method, Some("item/agentMessage/delta"))
        || matches!(event_type, Some("item.agentMessage.delta"))
    {
        let text = terminal_payload_text(value).trim().to_owned();
        return (!text.is_empty()).then_some(FinalAnswerTextUpdate::Append(text));
    }
    if event_type == Some("assistant") {
        let text = terminal_payload_text(value).trim().to_owned();
        return (!text.is_empty()).then_some(FinalAnswerTextUpdate::Replace(text));
    }
    if matches!(method, Some("item/completed")) || matches!(event_type, Some("item.completed")) {
        let item = value
            .get("item")
            .or_else(|| value.get("params").and_then(|params| params.get("item")));
        if let Some(item) = item
            && matches!(
                item.get("type").and_then(Value::as_str),
                Some("agentMessage" | "agent_message")
            )
            && matches!(
                item.get("phase").and_then(Value::as_str),
                Some("final_answer" | "answer") | None
            )
        {
            let text = terminal_payload_text(item).trim().to_owned();
            return (!text.is_empty()).then_some(FinalAnswerTextUpdate::Replace(text));
        }
    }
    None
}

fn turn_ids(value: &Value) -> Vec<String> {
    [
        &["turn_id"][..],
        &["turnId"][..],
        &["turn", "id"][..],
        &["params", "turnId"][..],
        &["params", "turn", "id"][..],
    ]
    .into_iter()
    .filter_map(|path| string_at_path(value, path))
    .collect()
}

fn item_ids(value: &Value) -> Vec<String> {
    [
        &["item_id"][..],
        &["itemId"][..],
        &["item", "id"][..],
        &["params", "itemId"][..],
        &["params", "item", "id"][..],
    ]
    .into_iter()
    .filter_map(|path| string_at_path(value, path))
    .collect()
}

fn string_at_path(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    let text = current.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn result_is_failure(value: &Value) -> bool {
    matches!(
        value.get("subtype").and_then(Value::as_str),
        Some("error" | "failure" | "failed")
    )
}

fn terminal_error_text(value: &Value) -> String {
    for key in ["error", "message", "result", "text"] {
        if let Some(text) = value.get(key).and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            return text.trim().to_owned();
        }
    }
    terminal_payload_text(value)
        .trim()
        .to_owned()
        .if_empty("terminal harness output reported failure")
}

fn terminal_payload_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(terminal_payload_text)
            .find(|text| !text.trim().is_empty())
            .unwrap_or_default(),
        Value::Object(object) => {
            for key in [
                "result",
                "result_text",
                "text",
                "final_text",
                "message",
                "delta",
                "content",
                "params",
            ] {
                if let Some(text) = object.get(key).map(terminal_payload_text)
                    && !text.trim().is_empty()
                {
                    return text;
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

trait StringExt {
    fn if_empty(self, fallback: &str) -> String;
}

impl StringExt for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_owned()
        } else {
            self
        }
    }
}

async fn drain_stderr(mut stderr: SandboxRead) -> Result<(), SessionRuntimeError> {
    io::copy(&mut stderr, &mut io::sink())
        .await
        .map_err(|err| {
            SessionRuntimeError::Sandbox(SandboxError::io_source("drain stderr", err))
        })?;
    Ok(())
}

async fn write_input_lines(
    pipe: &SessionPipe,
    input_lines: &[String],
    thread_key: &ThreadKey,
    execution_id: &str,
    sandbox_id: Option<&str>,
) -> Result<(), SessionRuntimeError> {
    let sandbox_id = sandbox_id.unwrap_or("");
    let span = info_span!(
        "centaur.api_rs.sandbox.write_input",
        component = COMPONENT_SESSION_RUNTIME,
        event = "sandbox_write_input",
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.execution_id" = execution_id,
        "centaur.sandbox_id" = sandbox_id,
        thread_key = %thread_key,
        execution_id,
        sandbox_id,
        input_line_count = input_lines.len(),
    );
    async {
        let mut stdin = pipe.stdin.lock().await;
        for line in input_lines {
            stdin.send(line).await.map_err(codec_error_to_runtime)?;
        }
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "sandbox_write_input_completed",
            thread_key = %thread_key,
            execution_id,
            sandbox_id,
            input_line_count = input_lines.len(),
            "sandbox input written"
        );
        Ok(())
    }
    .instrument(span)
    .await
}

/// Trace identity injected into sandbox stdin lines so the Rust harness server
/// can configure the harness OTLP export. Without a `trace_id` or `traceparent`
/// on the first turn, Codex exports no `session_task.turn` spans and Laminar
/// has no token usage to price into cost.
#[derive(Clone, Debug)]
struct SessionTraceContext {
    /// Stable per-thread trace id, derived from the thread key (UUIDv5) so it
    /// needs no persisted state and survives API restarts.
    trace_id: String,
    /// W3C traceparent of the current execution span, when the OpenTelemetry
    /// layer is active. Lets harness spans join the execution's trace.
    traceparent: Option<String>,
}

impl SessionTraceContext {
    fn new(thread_key: &ThreadKey, execution_span: Option<&Span>) -> Self {
        Self {
            trace_id: thread_trace_id(thread_key),
            traceparent: execution_span.and_then(centaur_telemetry::traceparent_for_span),
        }
    }
}

/// Deterministic per-thread trace id: one trace identity per thread without a
/// `thread_traces` table (derive, don't store).
pub fn thread_trace_id(thread_key: &ThreadKey) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("centaur:thread:{}", thread_key.as_str()).as_bytes(),
    )
    .to_string()
}

fn ensure_thread_trace_root_span(thread_key: &ThreadKey) {
    let trace_id = thread_trace_id(thread_key);
    let root_span_id = thread_trace_parent_span_id(thread_key);
    let thread_key = thread_key.as_str().to_owned();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = export_thread_trace_root_span(&trace_id, &root_span_id, &thread_key).await;
        });
    }
}

pub fn thread_trace_parent_span_id(thread_key: &ThreadKey) -> String {
    let digest = Sha256::digest(format!("centaur:thread-parent:{}", thread_key.as_str()));
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    if bytes.iter().all(|byte| *byte == 0) {
        bytes[7] = 1;
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn input_lines_with_session_context(
    thread_key: &ThreadKey,
    trace: &SessionTraceContext,
    input_lines: &[String],
) -> Vec<String> {
    input_lines
        .iter()
        .map(|line| input_line_with_session_context(thread_key, trace, line))
        .collect()
}

fn input_line_with_session_context(
    thread_key: &ThreadKey,
    trace: &SessionTraceContext,
    line: &str,
) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(line) else {
        return line.to_owned();
    };
    let Value::Object(map) = &mut value else {
        return line.to_owned();
    };
    map.entry("thread_key")
        .or_insert_with(|| Value::String(thread_key.as_str().to_owned()));
    map.entry("trace_id")
        .or_insert_with(|| Value::String(trace.trace_id.clone()));
    if let Some(traceparent) = &trace.traceparent {
        map.entry("traceparent")
            .or_insert_with(|| Value::String(traceparent.clone()));
    }
    merge_session_context(map, session_context_for_thread(thread_key));
    serde_json::to_string(&value).unwrap_or_else(|_| line.to_owned())
}

fn merge_session_context(
    map: &mut serde_json::Map<String, Value>,
    context: Option<serde_json::Map<String, Value>>,
) {
    let Some(context) = context else {
        return;
    };
    let entry = map
        .entry("session_context")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Value::Object(existing) = entry else {
        return;
    };
    for (key, value) in context {
        existing.entry(key).or_insert(value);
    }
}

fn session_context_for_thread(thread_key: &ThreadKey) -> Option<serde_json::Map<String, Value>> {
    let slack = slack_context_for_thread(thread_key)?;
    let mut context = serde_json::Map::new();
    context.insert("platform".to_owned(), Value::String("slack".to_owned()));
    context.insert("slack".to_owned(), Value::Object(slack));
    Some(context)
}

fn slack_context_for_thread(thread_key: &ThreadKey) -> Option<serde_json::Map<String, Value>> {
    let parts = thread_key.as_str().split(':').collect::<Vec<_>>();
    let (team_id, channel_id, thread_ts) = match parts.as_slice() {
        ["slack", channel_id, thread_ts] => (None, *channel_id, *thread_ts),
        ["slack", team_id, channel_id, thread_ts] => (Some(*team_id), *channel_id, *thread_ts),
        [channel_id, thread_ts] if is_slack_conversation_id(channel_id) => {
            (None, *channel_id, *thread_ts)
        }
        _ => return None,
    };
    if channel_id.is_empty() || thread_ts.is_empty() {
        return None;
    }

    let mut slack = serde_json::Map::new();
    if let Some(team_id) = team_id.filter(|value| !value.is_empty()) {
        slack.insert("team_id".to_owned(), Value::String(team_id.to_owned()));
    }
    slack.insert(
        "channel_id".to_owned(),
        Value::String(channel_id.to_owned()),
    );
    slack.insert("thread_ts".to_owned(), Value::String(thread_ts.to_owned()));
    Some(slack)
}

fn is_slack_conversation_id(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'C' | b'D' | b'G'))
}

fn steering_input_lines(
    thread_key: &ThreadKey,
    messages: &[SessionMessageInput],
    message_ids: &[String],
) -> Vec<String> {
    messages
        .iter()
        .zip(message_ids)
        .filter_map(|(message, message_id)| steering_input_line(thread_key, message, message_id))
        .collect()
}

fn steering_input_line(
    thread_key: &ThreadKey,
    message: &SessionMessageInput,
    message_id: &str,
) -> Option<String> {
    if message.role != MessageRole::User {
        return None;
    }
    serde_json::to_string(&json!({
        "type": "user",
        "thread_key": thread_key.as_str(),
        "trace_metadata": {
            "source": "session.append_messages",
            "action": "steer_active_execution",
            "message_id": message_id,
            "metadata": message.metadata.clone(),
        },
        "message": {
            "role": message.role.as_ref(),
            "content": message.parts.clone(),
        },
    }))
    .ok()
}

fn interrupt_input_line(thread_key: &ThreadKey, reason: &str) -> String {
    serde_json::to_string(&json!({
        "type": "interrupt",
        "thread_key": thread_key.as_str(),
        "trace_metadata": {
            "source": "session.interrupt_active_execution",
            "action": "interrupt_active_execution",
            "reason": reason,
        },
    }))
    .expect("interrupt input line serializes")
}

async fn append_output_line(
    ctx: &RuntimeContext,
    thread_key: &ThreadKey,
    execution_id: &str,
    line: &str,
) -> Result<Option<SessionEvent>, SessionRuntimeError> {
    let safe_line = redact_sensitive_text(line);
    let event = ctx
        .store
        .append_event_if_stdout_owner(
            thread_key,
            execution_id,
            &ctx.stdout_owner_id,
            STDOUT_OWNER_LEASE,
            SESSION_OUTPUT_LINE_EVENT,
            Value::String(safe_line),
        )
        .await?;
    Ok(event)
}

fn redact_sensitive_text(input: &str) -> String {
    let bearer_redacted = redact_bearer_tokens(input);
    let env_redacted = redact_sensitive_env_assignments(&bearer_redacted);
    redact_prefixed_tokens(&env_redacted)
}

fn redact_bearer_tokens(input: &str) -> String {
    const BEARER: &str = "bearer ";
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;

    while let Some(relative) = lower[index..].find(BEARER) {
        let start = index + relative;
        let token_start = start + BEARER.len();
        let token_end = consume_sensitive_token(input, token_start);
        out.push_str(&input[index..token_start]);
        if token_end > token_start {
            out.push_str("[REDACTED_TOKEN]");
            index = token_end;
        } else {
            index = token_start;
        }
    }

    out.push_str(&input[index..]);
    out
}

fn redact_sensitive_env_assignments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut index = 0;

    while let Some(relative) = input[index..].find('=') {
        let equals = index + relative;
        let key_start = env_key_start(input, equals);
        let key = &input[key_start..equals];
        out.push_str(&input[index..=equals]);
        if is_sensitive_env_key(key) {
            let token_start = equals + 1;
            let token_end = consume_sensitive_token(input, token_start);
            if token_end > token_start {
                out.push_str("[REDACTED_TOKEN]");
                index = token_end;
                continue;
            }
        }
        index = equals + 1;
    }

    out.push_str(&input[index..]);
    out
}

fn redact_prefixed_tokens(input: &str) -> String {
    const PREFIXES: &[&str] = &[
        "sbx1.",
        "xoxa-",
        "xoxb-",
        "xoxp-",
        "xoxr-",
        "xoxs-",
        "sk-ant-",
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
    ];

    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if let Some(prefix) = PREFIXES
            .iter()
            .find(|prefix| input[index..].starts_with(**prefix))
        {
            let token_end = consume_sensitive_token(input, index + prefix.len());
            out.push_str("[REDACTED_TOKEN]");
            index = token_end;
            continue;
        }

        let ch = input[index..].chars().next().expect("valid char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }

    out
}

fn consume_sensitive_token(input: &str, start: usize) -> usize {
    let mut end = start;
    for (relative, ch) in input[start..].char_indices() {
        if !is_sensitive_token_char(ch) {
            break;
        }
        end = start + relative + ch.len_utf8();
    }
    end
}

fn is_sensitive_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '=' | '+' | '/' | '.' | ':')
}

fn env_key_start(input: &str, equals: usize) -> usize {
    let mut start = equals;
    for (index, ch) in input[..equals].char_indices().rev() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            start = index;
        } else {
            break;
        }
    }
    start
}

fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("API_KEY")
        || upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
}

async fn execution_still_active(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: &str,
) -> bool {
    matches!(
        store.active_execution_for_thread(thread_key).await,
        Ok(Some(execution)) if execution.execution_id == execution_id
    )
}

fn is_transient_steering_startup_error(error: &SessionRuntimeError) -> bool {
    matches!(
        error,
        SessionRuntimeError::Sandbox(SandboxError::NotFound(_))
            | SessionRuntimeError::Sandbox(SandboxError::NotReady(_))
    )
}

fn harness_thread_id_from_output_line(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let event_type = value.get("type").and_then(Value::as_str);
    if event_type != Some("thread.started") {
        return None;
    }
    value
        .get("thread_id")
        .or_else(|| value.get("threadId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|thread_id| !thread_id.is_empty())
        .map(ToOwned::to_owned)
}

fn validate_input_lines(lines: &[String]) -> Result<(), SessionRuntimeError> {
    for (index, line) in lines.iter().enumerate() {
        if line.contains('\n') || line.contains('\r') {
            return Err(SessionRuntimeError::BadRequest(format!(
                "input_lines[{index}] must be one line"
            )));
        }
    }
    Ok(())
}

fn stdout_pump_error_message(error: &LinesCodecError) -> String {
    match error {
        LinesCodecError::MaxLineLengthExceeded => {
            "sandbox stdout line exceeded codec maximum length".to_owned()
        }
        LinesCodecError::Io(error) => format!("sandbox stdout I/O failed: {error}"),
    }
}

fn codec_error_to_runtime(error: LinesCodecError) -> SessionRuntimeError {
    let context = error.to_string();
    SessionRuntimeError::Sandbox(SandboxError::Io {
        context,
        source: Some(Box::new(error)),
    })
}

fn duration_options(
    idle_timeout_ms: Option<u64>,
    max_duration_ms: Option<u64>,
) -> Result<(Option<Duration>, Option<Duration>), SessionRuntimeError> {
    let idle_timeout = idle_timeout_ms.map(nonzero_duration_millis).transpose()?;
    let max_duration = max_duration_ms.map(nonzero_duration_millis).transpose()?;

    if let (Some(idle_timeout), Some(max_duration)) = (idle_timeout, max_duration)
        && idle_timeout > max_duration
    {
        return Err(SessionRuntimeError::BadRequest(
            "idle_timeout_ms must be less than or equal to max_duration_ms".to_owned(),
        ));
    }

    Ok((idle_timeout, max_duration))
}

fn nonzero_duration_millis(value: u64) -> Result<Duration, SessionRuntimeError> {
    if value == 0 {
        return Err(SessionRuntimeError::BadRequest(
            "duration values must be greater than zero".to_owned(),
        ));
    }
    Ok(Duration::from_millis(value))
}

fn tool_host_thread_key(principal_id: &str) -> Result<ThreadKey, SessionRuntimeError> {
    ThreadKey::parse(format!("mcp:{principal_id}"))
        .map_err(|error| SessionRuntimeError::BadRequest(error.to_string()))
}

/// Session/principal metadata recorded for observability; runtime behavior
/// derives from the `mcp:` thread-key prefix, not from these fields.
fn tool_host_session_metadata(principal_id: &str) -> Value {
    json!({
        "mcp_tool_host": true,
        "mcp_principal_id": principal_id,
    })
}

fn sandbox_boot_mode_for_thread(
    thread_key: &ThreadKey,
    iron_control_principal: Option<&str>,
) -> SandboxBootMode {
    let Some(thread_principal_id) = thread_key.as_str().strip_prefix("mcp:") else {
        return SandboxBootMode::Harness;
    };
    let principal_id = iron_control_principal
        .unwrap_or(thread_principal_id)
        .to_owned();
    SandboxBootMode::ToolHost { principal_id }
}

fn apply_sandbox_boot_mode(spec: &mut SandboxSpec, boot_mode: &SandboxBootMode) {
    let SandboxBootMode::ToolHost { principal_id } = boot_mode else {
        return;
    };
    spec.labels
        .insert("centaur.ai/component".to_owned(), "tool-host".to_owned());
    spec.labels
        .insert("centaur.ai/workload".to_owned(), "mcp-tool-host".to_owned());
    if !principal_id.trim().is_empty() {
        spec.iron_control_principal = Some(principal_id.to_owned());
        upsert_spec_env(spec, "CENTAUR_MCP_PRINCIPAL_ID", principal_id.to_owned());
    }
    configure_tool_host_command(spec);
}

fn configure_tool_host_command(spec: &mut SandboxSpec) {
    if should_preserve_entrypoint_for_tool_host(spec) {
        spec.command = Some(vec!["/entrypoint.sh".to_owned()]);
        spec.args = vec!["centaur-tool-host".to_owned()];
    } else {
        spec.command = Some(vec!["centaur-tool-host".to_owned()]);
        spec.args.clear();
    }
}

fn should_preserve_entrypoint_for_tool_host(spec: &SandboxSpec) -> bool {
    spec.command
        .as_ref()
        .and_then(|command| command.first())
        .is_some_and(|program| program == "/entrypoint.sh")
        || spec.args.first().is_some_and(|arg| arg == "harness-server")
}

fn execution_metadata(
    metadata: Option<Value>,
    idle_timeout_ms: Option<u64>,
    max_duration_ms: Option<u64>,
) -> Value {
    let mut metadata = default_metadata(metadata);
    if let Value::Object(object) = &mut metadata {
        if let Some(value) = idle_timeout_ms {
            object.insert("idle_timeout_ms".to_owned(), json!(value));
        }
        if let Some(value) = max_duration_ms {
            object.insert("max_duration_ms".to_owned(), json!(value));
        }
    }
    metadata
}

fn idle_timeout_from_execution(execution: &SessionExecution) -> Option<Duration> {
    execution
        .metadata
        .get("idle_timeout_ms")
        .and_then(Value::as_u64)
        .and_then(|value| nonzero_duration_millis(value).ok())
}

fn max_duration_from_execution(execution: &SessionExecution) -> Option<Duration> {
    execution
        .metadata
        .get("max_duration_ms")
        .and_then(Value::as_u64)
        .and_then(|value| nonzero_duration_millis(value).ok())
}

/// Folds recorded sandbox output the same way the live stdout pump does,
/// returning the first terminal outcome (with its accumulated final answer)
/// if the recorded history already contains the end of the turn.
fn terminal_output_from_lines(lines: &[String]) -> Option<TerminalOutput> {
    let mut final_answer_text = String::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(update) = output_line_final_answer_text(&value) {
            match update {
                FinalAnswerTextUpdate::Append(delta) => final_answer_text.push_str(&delta),
                FinalAnswerTextUpdate::Replace(canonical) => final_answer_text = canonical,
            }
        }
        if let Some(terminal) = terminal_output(&value, &final_answer_text) {
            return Some(terminal);
        }
    }
    None
}

#[derive(Debug, Error)]
pub enum SessionRuntimeError {
    #[error("{0}")]
    BadRequest(String),
    #[error("control plane is shutting down")]
    ShuttingDown,
    #[error(transparent)]
    Store(#[from] SessionStoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    IronControl(#[from] centaur_iron_control::IronControlError),
    #[error(transparent)]
    WarmPool(#[from] WarmPoolError),
    #[error(
        "sandbox running capacity exceeded during {operation}: running={running}, max_running={max_running}"
    )]
    CapacityExceeded {
        max_running: usize,
        running: usize,
        operation: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_sandbox_core::MountKind;
    use centaur_session_core::SessionStatus;
    use serde_json::json;
    use time::OffsetDateTime;

    #[test]
    fn sandbox_repo_cache_label_overrides_legacy_boolean() {
        assert_eq!(
            sandbox_repo_cache_access_from_principal(&test_principal(
                true,
                std::collections::BTreeMap::new()
            )),
            SessionRepoCacheAccess::All
        );
        assert_eq!(
            sandbox_repo_cache_access_from_principal(&test_principal(
                false,
                std::collections::BTreeMap::new()
            )),
            SessionRepoCacheAccess::None
        );
        for value in ["none", "private", "bogus"] {
            assert_eq!(
                sandbox_repo_cache_access_from_principal(&test_principal(
                    true,
                    std::collections::BTreeMap::from([(
                        SANDBOX_REPO_CACHE_LABEL.to_owned(),
                        value.to_owned(),
                    )])
                )),
                SessionRepoCacheAccess::None
            );
        }
        assert_eq!(
            sandbox_repo_cache_access_from_principal(&test_principal(
                true,
                std::collections::BTreeMap::from([(
                    SANDBOX_REPO_CACHE_LABEL.to_owned(),
                    "public".to_owned(),
                )])
            )),
            SessionRepoCacheAccess::Public
        );
        assert_eq!(
            sandbox_repo_cache_access_from_principal(&test_principal(
                false,
                std::collections::BTreeMap::from([(
                    SANDBOX_REPO_CACHE_LABEL.to_owned(),
                    "all".to_owned(),
                )])
            )),
            SessionRepoCacheAccess::All
        );
    }

    #[test]
    fn public_repo_cache_scopes_bind_mount_to_public_projection() {
        let mut spec = SandboxSpec::new("mock").mount(Mount::new(
            MountKind::Bind {
                source_path: "/var/lib/centaur/repos".to_owned(),
            },
            SANDBOX_REPOS_MOUNT_PATH,
        ));
        let capabilities = SessionSandboxCapabilities {
            repo_cache: SessionRepoCacheAccess::Public,
            observability_enabled: true,
            api_server_enabled: true,
        };

        apply_sandbox_capabilities(&mut spec, &capabilities);

        assert_eq!(spec.capabilities.repo_cache, RepoCacheAccess::Public);
        assert_eq!(
            env_value(&spec, "CENTAUR_SANDBOX_REPO_CACHE_ACCESS"),
            Some("public")
        );
        assert_eq!(
            spec.mounts[0].kind,
            MountKind::Bind {
                source_path: "/var/lib/centaur/repos/public".to_owned(),
            }
        );
        assert_eq!(spec.mounts[0].sub_path, None);
    }

    #[test]
    fn public_repo_cache_scopes_named_volume_to_public_subpath() {
        let mut spec = SandboxSpec::new("mock").mount(Mount::new(
            MountKind::NamedVolume("centaur-repo-cache".to_owned()),
            SANDBOX_REPOS_MOUNT_PATH,
        ));
        let capabilities = SessionSandboxCapabilities {
            repo_cache: SessionRepoCacheAccess::Public,
            observability_enabled: true,
            api_server_enabled: true,
        };

        apply_sandbox_capabilities(&mut spec, &capabilities);

        assert_eq!(
            spec.mounts[0].kind,
            MountKind::NamedVolume("centaur-repo-cache".to_owned())
        );
        assert_eq!(spec.mounts[0].sub_path.as_deref(), Some("public"));
    }

    #[test]
    fn public_repo_cache_scopes_skill_dirs_to_public_dirs() {
        let mut spec = SandboxSpec::new("mock")
            .env(
                CENTAUR_SKILL_DIRS_ENV,
                "/home/agent/github/acme/private/.agents/skills:\
                 /home/agent/github/acme/public/.agents/skills",
            )
            .env(
                CENTAUR_PUBLIC_SKILL_DIRS_ENV,
                "/home/agent/github/acme/public/.agents/skills",
            );
        let capabilities = SessionSandboxCapabilities {
            repo_cache: SessionRepoCacheAccess::Public,
            observability_enabled: true,
            api_server_enabled: true,
        };

        apply_sandbox_capabilities(&mut spec, &capabilities);

        assert_eq!(
            env_value(&spec, CENTAUR_SKILL_DIRS_ENV),
            Some("/home/agent/github/acme/public/.agents/skills")
        );
        assert_eq!(env_value(&spec, CENTAUR_PUBLIC_SKILL_DIRS_ENV), None);
    }

    #[test]
    fn disabled_repo_cache_removes_repo_mount() {
        let mut spec = SandboxSpec::new("mock")
            .mount(Mount::new(
                MountKind::Bind {
                    source_path: "/var/lib/centaur/repos".to_owned(),
                },
                SANDBOX_REPOS_MOUNT_PATH,
            ))
            .mount(Mount::new(MountKind::EmptyDir, "/workspace"))
            .env(
                CENTAUR_SKILL_DIRS_ENV,
                "/home/agent/github/acme/private/.agents/skills",
            )
            .env(
                CENTAUR_PUBLIC_SKILL_DIRS_ENV,
                "/home/agent/github/acme/public/.agents/skills",
            );
        let capabilities = SessionSandboxCapabilities {
            repo_cache: SessionRepoCacheAccess::None,
            observability_enabled: true,
            api_server_enabled: true,
        };

        apply_sandbox_capabilities(&mut spec, &capabilities);

        assert_eq!(spec.capabilities.repo_cache, RepoCacheAccess::None);
        assert_eq!(spec.mounts.len(), 1);
        assert_eq!(spec.mounts[0].target_path, "/workspace");
        assert_eq!(env_value(&spec, CENTAUR_SKILL_DIRS_ENV), None);
        assert_eq!(env_value(&spec, CENTAUR_PUBLIC_SKILL_DIRS_ENV), None);
    }

    fn test_principal(
        sandbox_repo_cache_enabled: bool,
        labels: std::collections::BTreeMap<String, String>,
    ) -> centaur_iron_control::Principal {
        centaur_iron_control::Principal {
            id: "prn_test".to_owned(),
            namespace: "default".to_owned(),
            foreign_id: Some("slack-channel-t-c".to_owned()),
            name: "Test".to_owned(),
            labels,
            sandbox_repo_cache_enabled,
            sandbox_observability_enabled: true,
            sandbox_api_server_enabled: true,
        }
    }

    #[test]
    fn persona_registry_validates_default_and_summarizes_without_prompt() {
        let registry = PersonaRegistry::new(
            [PersonaDefinition {
                id: "eng".to_owned(),
                source_root: "/repo/tools".to_owned(),
                source_path: "/repo/tools/personas/eng".to_owned(),
                source_ref: Some("abc123".to_owned()),
                prompt_hash: "sha256:prompt".to_owned(),
                prompt: "secret prompt".to_owned(),
            }],
            Some("eng".to_owned()),
            vec!["/repo/tools".to_owned()],
        )
        .unwrap();

        let summaries = registry.summaries();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "eng");
        assert!(
            serde_json::to_value(registry.get("eng").unwrap())
                .unwrap()
                .get("prompt")
                .is_none()
        );
        assert!(PersonaRegistry::new(Vec::new(), Some("missing".to_owned()), Vec::new()).is_err());
    }

    #[test]
    fn persona_registry_limits_public_access_to_public_source_roots() {
        let registry = PersonaRegistry::new(
            [
                PersonaDefinition {
                    id: "private".to_owned(),
                    source_root: "/repo/private/tools".to_owned(),
                    source_path: "/repo/private/tools/personas/private".to_owned(),
                    source_ref: None,
                    prompt_hash: "sha256:private".to_owned(),
                    prompt: "private prompt".to_owned(),
                },
                PersonaDefinition {
                    id: "public".to_owned(),
                    source_root: "/repo/public/tools".to_owned(),
                    source_path: "/repo/public/tools/personas/public".to_owned(),
                    source_ref: None,
                    prompt_hash: "sha256:public".to_owned(),
                    prompt: "public prompt".to_owned(),
                },
            ],
            Some("private".to_owned()),
            vec![
                "/repo/private/tools".to_owned(),
                "/repo/public/tools".to_owned(),
            ],
        )
        .unwrap()
        .with_public_source_roots(["/repo/public/tools".to_owned()]);

        assert_eq!(
            registry.default_persona_id_for_access(&SessionRepoCacheAccess::All),
            Some("private")
        );
        assert_eq!(
            registry.default_persona_id_for_access(&SessionRepoCacheAccess::Public),
            None
        );
        assert!(
            registry
                .context_for_access("private", false, &SessionRepoCacheAccess::Public)
                .is_err()
        );
        assert_eq!(
            registry
                .context_for_access("public", false, &SessionRepoCacheAccess::Public)
                .unwrap()
                .persona_id,
            "public"
        );
    }

    #[test]
    fn tool_host_command_preserves_sandbox_entrypoint_for_tool_setup() {
        let thread_key = ThreadKey::parse("mcp:test").unwrap();
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            [("TOOL_DIRS".to_owned(), "/app/tools".to_owned())],
            HarnessType::Codex,
        );
        let mut spec = workload.spec(&thread_key, &HarnessType::Codex, None);

        configure_tool_host_command(&mut spec);

        assert_eq!(spec.command, Some(vec!["/entrypoint.sh".to_owned()]));
        assert_eq!(spec.args, vec!["centaur-tool-host"]);
        assert_eq!(env_value(&spec, "TOOL_DIRS"), Some("/app/tools"));
    }

    #[test]
    fn turn_completed_without_answer_text_is_terminal() {
        let event = json!({
            "type": "turn.completed",
            "turn": {"id": "turn-1", "status": "completed"},
        });

        assert_eq!(
            terminal_output(&event, ""),
            Some(TerminalOutput::Completed {
                reason: "turn_completed",
                result_text: None
            })
        );
    }

    #[test]
    fn turn_completed_after_answer_text_is_terminal() {
        let delta = json!({
            "method": "item/agentMessage/delta",
            "params": {"turnId": "turn-1", "delta": "Final answer"},
        });
        let terminal = json!({
            "method": "turn/completed",
            "params": {"turn": {"id": "turn-1", "status": "completed"}},
        });

        assert!(matches!(
            output_line_final_answer_text(&delta),
            Some(FinalAnswerTextUpdate::Append(_))
        ));
        assert_eq!(
            terminal_output(&terminal, "Final answer"),
            Some(TerminalOutput::Completed {
                reason: "turn_completed",
                result_text: Some("Final answer".to_owned())
            })
        );
    }

    #[test]
    fn turn_completed_uses_completed_agent_message_text_when_terminal_is_empty() {
        let completed = json!({
            "type": "item.completed",
            "item": {
                "id": "msg-final",
                "type": "agentMessage",
                "phase": "final_answer",
                "text": "1. No new findings.\n\n2. No writes were used."
            }
        });
        let terminal = json!({
            "type": "turn.completed",
            "turn": {"id": "turn-1", "status": "completed"},
        });

        let Some(FinalAnswerTextUpdate::Replace(final_text)) =
            output_line_final_answer_text(&completed)
        else {
            panic!("completed agentMessage should replace final answer text")
        };
        assert_eq!(
            terminal_output(&terminal, &final_text),
            Some(TerminalOutput::Completed {
                reason: "turn_completed",
                result_text: Some("1. No new findings.\n\n2. No writes were used.".to_owned())
            })
        );
    }

    #[test]
    fn interrupted_turn_completed_without_answer_is_cancelled() {
        let event = json!({
            "type": "turn.completed",
            "turn": {"id": "turn-1", "status": "interrupted"},
        });

        assert_eq!(
            terminal_output(&event, ""),
            Some(TerminalOutput::Cancelled {
                reason: "turn_interrupted"
            })
        );
    }

    #[test]
    fn interrupted_turn_completed_after_answer_stays_terminal() {
        let event = json!({
            "method": "turn/completed",
            "params": {"turn": {"id": "turn-1", "status": "interrupted"}},
        });

        assert_eq!(
            terminal_output(&event, "Final answer"),
            Some(TerminalOutput::Completed {
                reason: "turn_completed",
                result_text: Some("Final answer".to_owned())
            })
        );
    }

    #[test]
    fn terminal_result_completes_even_without_prior_delta() {
        let event = json!({
            "type": "result",
            "result": {"text": "Final answer"},
        });

        assert_eq!(
            terminal_output(&event, ""),
            Some(TerminalOutput::Completed {
                reason: "result",
                result_text: Some("Final answer".to_owned())
            })
        );
    }

    #[test]
    fn turn_done_carries_terminal_result_text() {
        let event = json!({
            "type": "turn.done",
            "result": "Final answer",
        });

        assert_eq!(
            terminal_output(&event, ""),
            Some(TerminalOutput::Completed {
                reason: "turn_done",
                result_text: Some("Final answer".to_owned())
            })
        );
    }

    #[test]
    fn turn_failed_is_terminal_failure() {
        let event = json!({
            "type": "turn.failed",
            "error": "sandbox exited",
        });

        assert_eq!(
            terminal_output(&event, ""),
            Some(TerminalOutput::Failed {
                error: "sandbox exited".to_owned()
            })
        );
    }

    #[test]
    fn nested_terminal_text_is_normalized() {
        let event = json!({
            "result": {
                "message": {
                    "content": [{"type": "text", "text": "Final answer"}],
                },
            },
        });

        assert_eq!(terminal_payload_text(&event), "Final answer");
    }

    #[test]
    fn timeout_event_uses_millisecond_duration() {
        assert_eq!(duration_millis_u64(Duration::from_millis(3_000)), 3_000);
    }

    #[test]
    fn stdout_state_first_token_detection_uses_answer_text() {
        let state = StdoutPumpState::default();
        let turn_started = json!({"type": "turn.started", "turn_id": "turn-1"});
        let delta = json!({
            "type": "item.agentMessage.delta",
            "turnId": "turn-1",
            "itemId": "msg-1",
            "delta": "Hello"
        });
        let terminal_result = json!({"type": "result", "result": {"text": "Done"}});

        assert!(!state.should_record_first_token("exe-1", Some(&turn_started)));
        assert!(state.should_record_first_token("exe-1", Some(&delta)));
        assert!(state.should_record_first_token("exe-2", Some(&terminal_result)));
    }

    #[test]
    fn terminal_failure_class_is_low_cardinality() {
        assert_eq!(
            terminal_failure_class("sandbox stdout closed before terminal output"),
            "sandbox_io"
        );
        assert_eq!(
            terminal_failure_class("execution orphaned by control plane restart"),
            "orphaned"
        );
        assert_eq!(
            terminal_failure_class("turn failed: model error"),
            "harness"
        );
    }

    #[test]
    fn execution_metadata_preserves_idle_and_max_duration() {
        let metadata =
            execution_metadata(Some(json!({"source": "test"})), Some(2_000), Some(5_000));

        assert_eq!(metadata["source"], "test");
        assert_eq!(metadata["idle_timeout_ms"], 2_000);
        assert_eq!(metadata["max_duration_ms"], 5_000);
    }

    #[test]
    fn idle_timeout_is_read_from_execution_metadata() {
        let execution = session_execution(
            "exe-idle",
            ExecutionStatus::Completed,
            json!({"idle_timeout_ms": 1500}),
        );

        assert_eq!(
            idle_timeout_from_execution(&execution),
            Some(Duration::from_millis(1500))
        );
    }

    #[test]
    fn redacts_sensitive_values_from_output_lines() {
        let line = r#"{"type":"item.completed","item":{"aggregatedOutput":"Authorization: Bearer sbx1.threadpayload.signature\nSANDBOX_TOKEN=sbx1.otherpayload.othersig\nSLACK_BOT_TOKEN=xoxb-1234567890-abcdef\n"}}"#;

        let redacted = redact_sensitive_text(line);

        assert!(!redacted.contains("sbx1.threadpayload.signature"));
        assert!(!redacted.contains("sbx1.otherpayload.othersig"));
        assert!(!redacted.contains("xoxb-1234567890-abcdef"));
        assert!(redacted.contains("Authorization: Bearer [REDACTED_TOKEN]"));
        assert!(redacted.contains("SANDBOX_TOKEN=[REDACTED_TOKEN]"));
        assert!(redacted.contains("SLACK_BOT_TOKEN=[REDACTED_TOKEN]"));
    }

    #[test]
    fn codex_app_server_event_source_and_type_are_classified() {
        let app_server = json!({
            "method": "item/agentMessage/delta",
            "params": {"turnId": "turn-1", "itemId": "item-1"},
        });
        let harness = json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "redacted"}]},
        });
        let sandbox = json!({
            "type": "custom.wrapper.event",
        });

        assert_eq!(
            sandbox_output_event_type(&app_server),
            "item/agentMessage/delta"
        );
        assert_eq!(sandbox_output_source(&app_server), "codex_app_server");
        assert_eq!(sandbox_output_source(&harness), "harness");
        assert_eq!(sandbox_output_source(&sandbox), "sandbox");
    }

    #[test]
    fn codex_app_server_mcp_tool_events_emit_tool_spans() {
        let started = json!({
            "method": "item/started",
            "params": {
                "item": {
                    "id": "tool-1",
                    "type": "mcpToolCall",
                    "server": "github",
                    "tool": "list_issues"
                }
            }
        });
        let progress = json!({
            "method": "item/mcpToolCall/progress",
            "params": {"itemId": "tool-1"}
        });
        let completed = json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "id": "tool-1",
                    "durationMs": 125
                }
            }
        });
        let mut known = HashMap::new();

        assert_eq!(
            tool_call_span_events(&started, &mut known),
            vec![ToolCallSpanEvent {
                labels: ToolCallLabels {
                    kind: "mcp".to_owned(),
                    name: "list_issues".to_owned(),
                    method: "github".to_owned(),
                },
                status: "started",
                duration: None,
            }]
        );
        assert_eq!(
            tool_call_span_events(&progress, &mut known),
            vec![ToolCallSpanEvent {
                labels: ToolCallLabels {
                    kind: "mcp".to_owned(),
                    name: "list_issues".to_owned(),
                    method: "github".to_owned(),
                },
                status: "progress",
                duration: None,
            }]
        );
        assert_eq!(
            tool_call_span_events(&completed, &mut known),
            vec![ToolCallSpanEvent {
                labels: ToolCallLabels {
                    kind: "mcp".to_owned(),
                    name: "list_issues".to_owned(),
                    method: "github".to_owned(),
                },
                status: "completed",
                duration: Some(Duration::from_millis(125)),
            }]
        );
        assert!(known.is_empty());
    }

    #[test]
    fn command_execution_items_do_not_emit_tool_spans() {
        let started = json!({
            "method": "item/started",
            "params": {
                "item": {
                    "id": "cmd-1",
                    "type": "commandExecution",
                    "command": "ls -la"
                }
            }
        });
        let completed = json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "id": "cmd-1",
                    "type": "commandExecution",
                    "command": "ls -la",
                    "exitCode": 0,
                    "durationMs": 42
                }
            }
        });
        let mut known = HashMap::new();

        assert_eq!(tool_call_span_events(&started, &mut known), Vec::new());
        assert_eq!(tool_call_span_events(&completed, &mut known), Vec::new());
        assert!(known.is_empty());
    }

    #[test]
    fn anthropic_tool_use_and_result_events_emit_tool_spans() {
        let assistant = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "tool_use", "id": "use-1", "name": "todo_write", "input": {"redacted": true}}
                ]
            }
        });
        let result = json!({
            "type": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "use-1", "content": "redacted"}
            ]
        });
        let mut known = HashMap::new();

        assert_eq!(
            tool_call_span_events(&assistant, &mut known),
            vec![ToolCallSpanEvent {
                labels: ToolCallLabels {
                    kind: "anthropic".to_owned(),
                    name: "todo_write".to_owned(),
                    method: "call".to_owned(),
                },
                status: "started",
                duration: None,
            }]
        );
        assert_eq!(
            tool_call_span_events(&result, &mut known),
            vec![ToolCallSpanEvent {
                labels: ToolCallLabels {
                    kind: "anthropic".to_owned(),
                    name: "todo_write".to_owned(),
                    method: "call".to_owned(),
                },
                status: "completed",
                duration: None,
            }]
        );
        assert!(known.is_empty());
    }

    #[test]
    fn idle_pause_requires_latest_terminal_execution_and_same_sandbox() {
        let session = session_with_sandbox("asbx-1");
        let completed = session_execution("exe-1", ExecutionStatus::Completed, json!({}));
        let running = session_execution("exe-1", ExecutionStatus::Running, json!({}));
        let newer = session_execution("exe-2", ExecutionStatus::Completed, json!({}));

        assert!(should_pause_idle_sandbox(
            &session,
            Some(&completed),
            "exe-1",
            "asbx-1"
        ));
        assert!(!should_pause_idle_sandbox(
            &session,
            Some(&running),
            "exe-1",
            "asbx-1"
        ));
        assert!(!should_pause_idle_sandbox(
            &session,
            Some(&newer),
            "exe-1",
            "asbx-1"
        ));
        assert!(!should_pause_idle_sandbox(
            &session,
            Some(&completed),
            "exe-1",
            "asbx-other"
        ));
    }

    #[test]
    fn event_stream_attaches_only_to_running_sandboxes() {
        assert!(should_attach_session_pipe(&SandboxStatus::Running));
        assert!(!should_attach_session_pipe(&SandboxStatus::Created));
        assert!(!should_attach_session_pipe(&SandboxStatus::Suspended));
        assert!(!should_attach_session_pipe(&SandboxStatus::Stopped));
        assert!(!should_attach_session_pipe(&SandboxStatus::Gone));
        assert!(!should_attach_session_pipe(&SandboxStatus::Unknown(
            "other".to_owned()
        )));
    }

    #[test]
    fn existing_sandbox_action_repairs_or_replaces_non_attachable_assignments() {
        assert_eq!(
            existing_sandbox_action(&SandboxStatus::Running),
            ExistingSandboxAction::Reuse
        );
        assert_eq!(
            existing_sandbox_action(&SandboxStatus::Suspended),
            ExistingSandboxAction::ResumeOrReplace
        );
        assert_eq!(
            existing_sandbox_action(&SandboxStatus::Created),
            ExistingSandboxAction::ResumeOrReplace
        );
        assert_eq!(
            existing_sandbox_action(&SandboxStatus::Stopped),
            ExistingSandboxAction::Replace
        );
        assert_eq!(
            existing_sandbox_action(&SandboxStatus::Gone),
            ExistingSandboxAction::Replace
        );
        assert_eq!(
            existing_sandbox_action(&SandboxStatus::Unknown("rollout missing".to_owned())),
            ExistingSandboxAction::Replace
        );
    }

    #[test]
    fn event_stream_tolerates_not_ready_attach_race() {
        let not_ready =
            SessionRuntimeError::Sandbox(SandboxError::NotReady("sandbox paused".to_owned()));
        let backend_error = SessionRuntimeError::Sandbox(SandboxError::backend("api failed"));

        assert!(is_event_stream_attach_race(&not_ready));
        assert!(!is_event_stream_attach_race(&backend_error));
    }

    #[test]
    fn steering_startup_retries_only_transient_sandbox_errors() {
        let not_ready =
            SessionRuntimeError::Sandbox(SandboxError::NotReady("sandbox starting".to_owned()));
        let not_found = SessionRuntimeError::Sandbox(SandboxError::NotFound("asbx-1".to_owned()));
        let io = SessionRuntimeError::Sandbox(SandboxError::io("stdin closed"));
        let store = SessionRuntimeError::Store(SessionStoreError::NotFound {
            thread_key: "cli:test".to_owned(),
        });

        assert!(is_transient_steering_startup_error(&not_ready));
        assert!(is_transient_steering_startup_error(&not_found));
        assert!(!is_transient_steering_startup_error(&io));
        assert!(!is_transient_steering_startup_error(&store));
    }

    #[test]
    fn stdout_state_drops_late_output_from_inactive_turn() {
        let mut state = StdoutPumpState::default();
        let started = r#"{"type":"turn.started","turn_id":"turn-old"}"#;
        let delta = r#"{"type":"item.agentMessage.delta","turnId":"turn-old","itemId":"msg-old","delta":"late"}"#;

        assert_eq!(
            state.execution_for_line(Some("exe-old"), started),
            Some("exe-old".to_owned())
        );
        assert_eq!(state.execution_for_line(None, delta), None);
        assert_eq!(state.execution_for_line(Some("exe-new"), delta), None);
    }

    #[test]
    fn stdout_state_uses_final_agent_message_when_turn_completed_is_textless() {
        let mut state = StdoutPumpState::default();
        let started = r#"{"type":"turn.started","turn_id":"turn-1"}"#;
        let delta = r#"{"type":"item.agentMessage.delta","turnId":"turn-1","itemId":"msg-final","delta":"draft"}"#;
        let completed = r#"{"type":"item.completed","item":{"id":"msg-final","type":"agentMessage","phase":"final_answer","text":"Final canonical answer."}}"#;
        let terminal =
            r#"{"type":"turn.completed","turn":{"id":"turn-1","status":"completed"},"usage":null}"#;

        assert_eq!(
            state.execution_for_line(Some("exe-1"), started),
            Some("exe-1".to_owned())
        );
        assert_eq!(state.observe("exe-1", delta), None);
        assert_eq!(state.observe("exe-1", completed), None);
        assert_eq!(
            state.observe("exe-1", terminal),
            Some(TerminalOutput::Completed {
                reason: "turn_completed",
                result_text: Some("Final canonical answer.".to_owned())
            })
        );
    }

    #[test]
    fn steering_input_lines_forward_only_user_messages() {
        let thread_key = ThreadKey::parse("cli:test-steering").unwrap();
        let messages = vec![
            SessionMessageInput {
                client_message_id: None,
                role: MessageRole::User,
                parts: vec![json!({"type": "text", "text": "steer now"})],
                metadata: json!({"platform": "test"}),
            },
            SessionMessageInput {
                client_message_id: None,
                role: MessageRole::Assistant,
                parts: vec![json!({"type": "text", "text": "do not echo assistant"})],
                metadata: json!({}),
            },
        ];
        let message_ids = vec!["msg-user".to_owned(), "msg-assistant".to_owned()];

        let lines = steering_input_lines(&thread_key, &messages, &message_ids);
        assert_eq!(lines.len(), 1);

        let value: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["thread_key"], "cli:test-steering");
        assert_eq!(value["trace_metadata"]["action"], "steer_active_execution");
        assert_eq!(value["trace_metadata"]["message_id"], "msg-user");
        assert_eq!(value["message"]["content"][0]["text"], "steer now");
    }

    #[test]
    fn harness_thread_id_is_extracted_from_thread_started_output() {
        assert_eq!(
            harness_thread_id_from_output_line(
                r#"{"type":"thread.started","thread_id":"codex-thread-1"}"#
            ),
            Some("codex-thread-1".to_owned())
        );
        assert_eq!(
            harness_thread_id_from_output_line(
                r#"{"type":"thread.started","threadId":"codex-thread-2"}"#
            ),
            Some("codex-thread-2".to_owned())
        );
        assert_eq!(
            harness_thread_id_from_output_line(r#"{"type":"turn.started","turn_id":"turn-1"}"#),
            None
        );
    }

    #[test]
    fn codex_workload_applies_mounts_to_sandbox_spec() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            [("CENTAUR_API_URL".to_owned(), "http://api:8000".to_owned())],
            HarnessType::Codex,
        )
        .mount(
            Mount::new(
                MountKind::Bind {
                    source_path: "/host/github".to_owned(),
                },
                "/home/agent/github",
            )
            .read_only(),
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();

        let spec = workload.spec(&thread_key, &HarnessType::Codex, None);

        assert_eq!(spec.mounts.len(), 1);
        assert_eq!(spec.mounts[0].target_path, "/home/agent/github");
        assert!(spec.mounts[0].read_only);
        assert_eq!(
            spec.mounts[0].kind,
            MountKind::Bind {
                source_path: "/host/github".to_owned(),
            }
        );
    }

    #[test]
    fn codex_workload_reflects_resolved_persona_in_sandbox_spec() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            [("AGENT_PERSONA".to_owned(), "stale".to_owned())],
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        let persona = test_persona_context("eng");

        let spec = workload.spec(&thread_key, &HarnessType::Codex, Some(&persona));

        assert_eq!(env_value(&spec, "AGENT_PERSONA"), Some("eng"));
        assert_eq!(env_value(&spec, "CENTAUR_PERSONA_ID"), Some("eng"));
        assert_eq!(
            env_value(&spec, "CENTAUR_PERSONA_PROMPT_HASH"),
            Some("sha256:prompt")
        );
        assert_eq!(
            env_value(&spec, "CENTAUR_PERSONA_SOURCE_REF"),
            Some("abc123")
        );
        assert_eq!(env_value(&workload.warm_spec(), "AGENT_PERSONA"), None);
    }

    #[test]
    fn codex_workload_does_not_inject_stale_continue_thread_id() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            Vec::new(),
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();

        let spec = workload.spec(&thread_key, &HarnessType::Codex, None);

        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "CODEX_CONTINUE_THREAD_ID")
                .map(|env| env.value.as_str()),
            None
        );
        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "AMP_CONTINUE_THREAD_ID")
                .map(|env| env.value.as_str()),
            None
        );
    }

    #[test]
    fn codex_warm_spec_starts_profileless() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            [("CENTAUR_API_URL".to_owned(), "http://api:8000".to_owned())],
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();

        let claimed_spec = workload.spec(&thread_key, &HarnessType::ClaudeCode, None);
        let warm_spec = workload.warm_spec();

        assert_eq!(
            env_value(&claimed_spec, "CENTAUR_THREAD_KEY"),
            Some(thread_key.as_str())
        );
        assert_eq!(env_value(&warm_spec, "CENTAUR_THREAD_KEY"), None);
    }

    #[test]
    fn warm_workload_key_ignores_claimed_thread_key() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            [("CENTAUR_API_URL".to_owned(), "http://api:8000".to_owned())],
            HarnessType::Codex,
        );
        let first_thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        let second_thread_key = ThreadKey::parse("chat:C456:1780000000.000001").unwrap();

        assert_ne!(
            sandbox_spec_key(&workload.spec(&first_thread_key, &HarnessType::ClaudeCode, None)),
            sandbox_spec_key(&workload.spec(&second_thread_key, &HarnessType::ClaudeCode, None))
        );
        assert_eq!(
            sandbox_spec_key(&workload.warm_spec()),
            sandbox_spec_key(&workload.warm_spec())
        );
    }

    #[test]
    fn codex_workload_pins_harness_via_container_args() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            Vec::new(),
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();

        let codex_spec = workload.spec(&thread_key, &HarnessType::Codex, None);
        let claude_spec = workload.spec(&thread_key, &HarnessType::ClaudeCode, None);
        let amp_spec = workload.spec(&thread_key, &HarnessType::Amp, None);

        assert_eq!(codex_spec.args, vec!["harness-server", "codex"]);
        assert_eq!(claude_spec.args, vec!["harness-server", "claude-code"]);
        assert_eq!(amp_spec.args, vec!["harness-server", "amp"]);
        // The image entrypoint must be preserved: only CMD is overridden.
        assert_eq!(codex_spec.command, None);
    }

    #[test]
    fn codex_workload_labels_session_sandbox_for_observability() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            Vec::new(),
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();

        let spec = workload.spec(&thread_key, &HarnessType::ClaudeCode, None);

        assert_eq!(
            spec.labels.get("centaur.ai/component").map(String::as_str),
            Some("session-sandbox")
        );
        assert_eq!(
            spec.labels.get("centaur.ai/harness").map(String::as_str),
            Some("claudecode")
        );
    }

    #[test]
    fn warm_spec_uses_workload_default_harness() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            Vec::new(),
            HarnessType::Codex,
        );

        assert_eq!(
            workload.warm_spec().args,
            vec!["harness-server", "codex"],
            "warm sandboxes boot the configured default harness"
        );
        // A session on a different harness produces a different spec, so a
        // warm claim for it would hand over the wrong harness.
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        assert_eq!(
            workload
                .spec(&thread_key, &HarnessType::ClaudeCode, None)
                .args,
            vec!["harness-server", "claude-code"]
        );
    }

    #[test]
    fn input_line_with_session_context_enriches_json_objects() {
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        let trace = SessionTraceContext::new(&thread_key, None);

        let line = input_line_with_session_context(&thread_key, &trace, r#"{"type":"user"}"#);
        let value: Value = serde_json::from_str(&line).unwrap();

        assert_eq!(value["type"], "user");
        assert_eq!(value["thread_key"], thread_key.as_str());
        assert_eq!(value["trace_id"], trace.trace_id);
        // Without an OpenTelemetry layer there is no traceparent to forward.
        assert!(value.get("traceparent").is_none());
        assert!(value.get("session_context").is_none());
    }

    #[test]
    fn input_line_with_session_context_adds_slack_thread_context() {
        let thread_key = ThreadKey::parse("slack:T123:C123:1780000000.000000").unwrap();
        let trace = SessionTraceContext::new(&thread_key, None);

        let line = input_line_with_session_context(&thread_key, &trace, r#"{"type":"user"}"#);
        let value: Value = serde_json::from_str(&line).unwrap();

        assert_eq!(value["session_context"]["platform"], "slack");
        assert_eq!(value["session_context"]["slack"]["team_id"], "T123");
        assert_eq!(value["session_context"]["slack"]["channel_id"], "C123");
        assert_eq!(
            value["session_context"]["slack"]["thread_ts"],
            "1780000000.000000"
        );
    }

    #[test]
    fn input_line_with_session_context_preserves_existing_session_context() {
        let thread_key = ThreadKey::parse("slack:T123:C123:1780000000.000000").unwrap();
        let trace = SessionTraceContext::new(&thread_key, None);

        let line = input_line_with_session_context(
            &thread_key,
            &trace,
            r#"{"type":"user","session_context":{"requester":{"github_handle":"@ada"},"platform":"custom"}}"#,
        );
        let value: Value = serde_json::from_str(&line).unwrap();

        assert_eq!(
            value["session_context"]["requester"]["github_handle"],
            "@ada"
        );
        assert_eq!(value["session_context"]["platform"], "custom");
        assert_eq!(value["session_context"]["slack"]["channel_id"], "C123");
    }

    #[test]
    fn input_line_with_session_context_preserves_existing_fields_and_non_json() {
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        let trace = SessionTraceContext {
            trace_id: thread_trace_id(&thread_key),
            traceparent: Some("00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_owned()),
        };

        let line = input_line_with_session_context(
            &thread_key,
            &trace,
            r#"{"type":"user","thread_key":"chat:existing","trace_id":"caller-trace"}"#,
        );
        let value: Value = serde_json::from_str(&line).unwrap();

        assert_eq!(value["thread_key"], "chat:existing");
        assert_eq!(value["trace_id"], "caller-trace");
        assert_eq!(
            value["traceparent"],
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
        );
        assert_eq!(
            input_line_with_session_context(&thread_key, &trace, "raw"),
            "raw"
        );
    }

    #[test]
    fn thread_trace_id_is_deterministic_per_thread() {
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        let other = ThreadKey::parse("chat:C456:1780000000.000000").unwrap();

        assert_eq!(thread_trace_id(&thread_key), thread_trace_id(&thread_key));
        assert_ne!(thread_trace_id(&thread_key), thread_trace_id(&other));
        // The wrapper parses this with uuid.UUID(...): must stay a canonical UUID.
        assert!(uuid::Uuid::parse_str(&thread_trace_id(&thread_key)).is_ok());
        assert_eq!(
            thread_trace_parent_span_id(&thread_key),
            thread_trace_parent_span_id(&thread_key)
        );
        assert_ne!(
            thread_trace_parent_span_id(&thread_key),
            thread_trace_parent_span_id(&other)
        );
        assert_eq!(thread_trace_parent_span_id(&thread_key).len(), 16);
        assert_ne!(thread_trace_parent_span_id(&thread_key), "0000000000000000");
    }

    fn session_with_sandbox(sandbox_id: &str) -> Session {
        let thread_key = ThreadKey::parse("cli:test-idle").unwrap();
        let now = OffsetDateTime::now_utc();
        Session {
            thread_key,
            title: None,
            sandbox_id: Some(sandbox_id.to_owned()),
            sandbox_capabilities: None,
            harness_type: HarnessType::Codex,
            harness_thread_id: None,
            persona_id: None,
            status: SessionStatus::Idle,
            iron_control_principal: None,
            sandbox_last_active_at: Some(now),
            created_at: now,
            updated_at: now,
        }
    }

    fn session_execution(
        execution_id: &str,
        status: ExecutionStatus,
        metadata: serde_json::Value,
    ) -> SessionExecution {
        let thread_key = ThreadKey::parse("cli:test-idle").unwrap();
        let now = OffsetDateTime::now_utc();
        SessionExecution {
            execution_id: execution_id.to_owned(),
            idempotency_key: None,
            thread_key,
            status,
            metadata,
            error: None,
            created_at: now,
            updated_at: now,
            started_at: Some(now),
            completed_at: Some(now),
        }
    }

    fn env_value<'a>(spec: &'a SandboxSpec, name: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .find(|env| env.name == name)
            .map(|env| env.value.as_str())
    }

    fn test_persona_context(persona_id: &str) -> PersonaContext {
        PersonaContext {
            persona_id: persona_id.to_owned(),
            source_root: "/repo/tools".to_owned(),
            source_path: format!("/repo/tools/personas/{persona_id}"),
            source_ref: Some("abc123".to_owned()),
            prompt_hash: "sha256:prompt".to_owned(),
            defaulted: false,
            overlay_chain: vec!["/repo/tools".to_owned()],
        }
    }
}

/// Integration tests for orphaned-execution adoption. They need a real
/// Postgres; set `SESSION_RUNTIME_TEST_DATABASE_URL` to run them (they skip
/// silently otherwise, mirroring `ABSURD_TEST_DATABASE_URL` in absurd-sdk).
#[cfg(test)]
mod adoption_tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use centaur_sandbox_core::{ObservedSandbox, SandboxHandle, SandboxIo, SandboxResult};
    use tokio::io::{AsyncWriteExt, DuplexStream};

    use super::*;

    /// The adoption scan is database-wide, so concurrently running tests
    /// would adopt each other's executions. Serialize the module; every test
    /// fully terminalizes its own executions before releasing the lock.
    static TEST_LOCK: Mutex<()> = Mutex::const_new(());

    struct MockBackend {
        ios: Mutex<VecDeque<SandboxIo>>,
        recorded_output: std::sync::Mutex<Vec<String>>,
        open_count: AtomicUsize,
        status: std::sync::Mutex<SandboxStatus>,
        observed_statuses: std::sync::Mutex<BTreeMap<String, SandboxStatus>>,
        create_id: String,
        created_specs: std::sync::Mutex<Vec<SandboxSpec>>,
        resume_fails: AtomicBool,
        stopped: std::sync::Mutex<Vec<String>>,
        proxy_ensures: std::sync::Mutex<Vec<(String, String)>>,
        missing_on_stop: std::sync::Mutex<BTreeSet<String>>,
    }

    impl MockBackend {
        fn new(status: SandboxStatus, recorded_output: Vec<String>) -> Self {
            Self {
                ios: Mutex::new(VecDeque::new()),
                recorded_output: std::sync::Mutex::new(recorded_output),
                open_count: AtomicUsize::new(0),
                status: std::sync::Mutex::new(status),
                observed_statuses: std::sync::Mutex::new(BTreeMap::new()),
                create_id: "mock-sbx".to_owned(),
                created_specs: std::sync::Mutex::new(Vec::new()),
                resume_fails: AtomicBool::new(false),
                stopped: std::sync::Mutex::new(Vec::new()),
                proxy_ensures: std::sync::Mutex::new(Vec::new()),
                missing_on_stop: std::sync::Mutex::new(BTreeSet::new()),
            }
        }

        async fn push_io(&self, io: SandboxIo) {
            self.ios.lock().await.push_back(io);
        }

        fn opens(&self) -> usize {
            self.open_count.load(Ordering::SeqCst)
        }

        fn set_recorded_output(&self, recorded_output: Vec<String>) {
            *self.recorded_output.lock().unwrap() = recorded_output;
        }

        fn set_status(&self, status: SandboxStatus) {
            *self.status.lock().unwrap() = status;
        }

        fn set_observed_status(&self, sandbox_id: &str, status: SandboxStatus) {
            self.observed_statuses
                .lock()
                .unwrap()
                .insert(sandbox_id.to_owned(), status);
        }

        fn status_of(&self, sandbox_id: &str) -> Option<SandboxStatus> {
            self.observed_statuses
                .lock()
                .unwrap()
                .get(sandbox_id)
                .cloned()
        }

        fn fail_resume(&self) {
            self.resume_fails.store(true, Ordering::SeqCst);
        }

        fn mark_stop_missing(&self, sandbox_id: &str) {
            self.missing_on_stop
                .lock()
                .unwrap()
                .insert(sandbox_id.to_owned());
        }

        fn stopped(&self) -> Vec<String> {
            self.stopped.lock().unwrap().clone()
        }

        fn proxy_ensures(&self) -> Vec<(String, String)> {
            self.proxy_ensures.lock().unwrap().clone()
        }

        fn created_specs(&self) -> Vec<SandboxSpec> {
            self.created_specs.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl SandboxBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            self.created_specs.lock().unwrap().push(spec);
            self.set_observed_status(&self.create_id, SandboxStatus::Running);
            Ok(SandboxHandle::new(
                SandboxId::new(self.create_id.clone()),
                "mock",
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            self.open_count.fetch_add(1, Ordering::SeqCst);
            self.ios
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| SandboxError::io("mock backend has no more ios"))
        }

        async fn read_output_since(
            &self,
            _id: &SandboxId,
            _since: Option<SystemTime>,
        ) -> SandboxResult<Vec<String>> {
            Ok(self.recorded_output.lock().unwrap().clone())
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            if let Some(status) = self.status_of(_id.as_str()) {
                return Ok(status);
            }
            Ok(self.status.lock().unwrap().clone())
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            let status = self.status(id).await?;
            Ok(ObservedSandbox::new(id.clone(), "mock", status))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(self
                .observed_statuses
                .lock()
                .unwrap()
                .iter()
                .map(|(id, status)| ObservedSandbox::new(id.as_str(), "mock", status.clone()))
                .collect())
        }

        async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
            if self.missing_on_stop.lock().unwrap().contains(id.as_str()) {
                return Err(SandboxError::NotFound(id.as_str().to_owned()));
            }
            self.stopped.lock().unwrap().push(id.as_str().to_owned());
            self.set_observed_status(id.as_str(), SandboxStatus::Stopped);
            Ok(())
        }

        async fn ensure_iron_control_proxy_resources(
            &self,
            id: &SandboxId,
            principal_id: &str,
        ) -> SandboxResult<()> {
            self.proxy_ensures
                .lock()
                .unwrap()
                .push((id.as_str().to_owned(), principal_id.to_owned()));
            Ok(())
        }

        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            self.set_observed_status(_id.as_str(), SandboxStatus::Suspended);
            Ok(())
        }

        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
            if self.resume_fails.load(Ordering::SeqCst) {
                return Err(SandboxError::NotFound(_id.as_str().to_owned()));
            }
            self.set_observed_status(_id.as_str(), SandboxStatus::Running);
            Ok(())
        }
    }

    fn mock_io() -> (SandboxIo, DuplexStream, DuplexStream) {
        let (stdin_near, stdin_far) = tokio::io::duplex(64 * 1024);
        let (stdout_near, stdout_far) = tokio::io::duplex(64 * 1024);
        let (stderr_near, _stderr_far) = tokio::io::duplex(1024);
        let io = SandboxIo::new(
            Box::pin(stdin_near),
            Box::pin(stdout_near),
            Box::pin(stderr_near),
        );
        (io, stdout_far, stdin_far)
    }

    fn completed_output_lines(result_text: &str) -> Vec<String> {
        vec![
            json!({
                "type": "item.completed",
                "item": {
                    "id": "msg-1",
                    "type": "agentMessage",
                    "text": result_text,
                    "phase": "final_answer"
                }
            })
            .to_string(),
            json!({"type": "turn.completed", "turn": {"id": "turn-1", "status": "completed"}})
                .to_string(),
        ]
    }

    fn completed_output_bytes(result_text: &str) -> Vec<u8> {
        let mut output = completed_output_lines(result_text).join("\n");
        output.push('\n');
        output.into_bytes()
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

    async fn orphaned_execution(
        store: &PgSessionStore,
        thread_key: &ThreadKey,
        sandbox_id: Option<&str>,
        running: bool,
    ) -> String {
        store
            .create_or_get_session(thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        if sandbox_id.is_some() {
            store
                .update_sandbox_id(thread_key, sandbox_id)
                .await
                .expect("set sandbox id");
        }
        let created = store
            .create_execution(thread_key, None, json!({}))
            .await
            .expect("create execution");
        let execution_id = created.execution.execution_id;
        if running {
            store
                .mark_execution_running(&execution_id)
                .await
                .expect("mark running");
        }
        execution_id
    }

    /// Ages an execution row past `PRE_SANDBOX_ORPHAN_GRACE` so adoption treats it
    /// as a genuine orphan instead of a young row racing a live execute.
    async fn backdate_execution(store: &PgSessionStore, execution_id: &str, seconds: f64) {
        let result = sqlx::query(
            "update session_executions \
             set created_at = created_at - make_interval(secs => $2), \
                 started_at = started_at - make_interval(secs => $2) \
             where execution_id = $1",
        )
        .bind(execution_id)
        .bind(seconds)
        .execute(store.pool())
        .await
        .expect("backdate execution");
        assert_eq!(result.rows_affected(), 1, "expected to backdate one row");
    }

    /// Expires an execution's stdout-owner lease in place, simulating an
    /// owner that died without releasing, deterministically (no sleeps
    /// racing real lease TTLs).
    async fn expire_stdout_lease(store: &PgSessionStore, execution_id: &str) {
        let result = sqlx::query(
            "update session_executions \
             set stdout_owner_lease_expires_at = now() - interval '1 second' \
             where execution_id = $1",
        )
        .bind(execution_id)
        .execute(store.pool())
        .await
        .expect("expire stdout lease");
        assert_eq!(result.rows_affected(), 1, "expected to expire one lease");
    }

    async fn wait_for_event(store: &PgSessionStore, thread_key: &ThreadKey, event_type: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let events = store
                .list_events_after(thread_key, 0, None, 1000)
                .await
                .expect("list events");
            if events.iter().any(|event| event.event_type == event_type) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {event_type}"
            );
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn wait_for_session_title(
        store: &PgSessionStore,
        thread_key: &ThreadKey,
        expected: &str,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let session = store.get_session(thread_key).await.expect("get session");
            if session.title.as_deref() == Some(expected) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for title");
            sleep(Duration::from_millis(25)).await;
        }
    }

    async fn events(store: &PgSessionStore, thread_key: &ThreadKey) -> Vec<SessionEvent> {
        store
            .list_events_after(thread_key, 0, None, 1000)
            .await
            .expect("list events")
    }

    fn runtime_with(store: &PgSessionStore, backend: Arc<MockBackend>) -> SessionRuntime {
        SessionRuntime::new(
            store.clone(),
            SandboxRuntime::backend(backend, SandboxSpec::new("mock")),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execution_scoped_event_stream_completes_after_terminal_event() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:stream-close-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, None, false).await;
        store
            .append_event(
                &thread_key,
                Some(&execution_id),
                "session.output.line",
                json!({ "line": "working" }),
            )
            .await
            .expect("append output event");
        store
            .append_event(
                &thread_key,
                Some(&execution_id),
                "session.execution_completed",
                json!({ "execution_id": execution_id }),
            )
            .await
            .expect("append terminal event");

        // Execution-scoped: the stream must end on its own after emitting the
        // terminal event, releasing the response and its listener connection.
        let listener = store.listen_session_events().await.expect("listener");
        let scoped = session_event_stream(
            store.clone(),
            thread_key.clone(),
            0,
            Some(execution_id.clone()),
            listener,
            tracing::Span::none(),
        );
        let emitted = tokio::time::timeout(Duration::from_secs(10), scoped.collect::<Vec<_>>())
            .await
            .expect("execution-scoped stream should complete after the terminal event");
        let kinds: Vec<_> = emitted
            .into_iter()
            .map(|result| result.expect("stream event").event_type)
            .collect();
        assert_eq!(
            kinds,
            vec!["session.output.line", "session.execution_completed"]
        );

        // Control: an unscoped stream over the same events stays open for
        // future events instead of completing.
        let listener = store.listen_session_events().await.expect("listener");
        let unscoped = session_event_stream(
            store.clone(),
            thread_key.clone(),
            0,
            None,
            listener,
            tracing::Span::none(),
        );
        let mut unscoped = std::pin::pin!(unscoped);
        for _ in 0..2 {
            unscoped
                .next()
                .await
                .expect("buffered event")
                .expect("stream event");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(300), unscoped.next())
                .await
                .is_err(),
            "unscoped stream should stay open after a terminal event"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_messages_generates_missing_session_title_once() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key = ThreadKey::parse(format!("test:title-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");

        let calls = Arc::new(AtomicUsize::new(0));
        let sources = Arc::new(Mutex::new(Vec::<String>::new()));
        let generator_started = Arc::new(tokio::sync::Notify::new());
        let generator_release = Arc::new(tokio::sync::Notify::new());
        let calls_for_generator = calls.clone();
        let sources_for_generator = sources.clone();
        let started_for_generator = generator_started.clone();
        let release_for_generator = generator_release.clone();
        let runtime = runtime_with(
            &store,
            Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new())),
        )
        .with_session_title_generator(move |source| {
            let calls = calls_for_generator.clone();
            let sources = sources_for_generator.clone();
            let started = started_for_generator.clone();
            let release = release_for_generator.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                sources.lock().await.push(source);
                started.notify_one();
                release.notified().await;
                Ok("Fix worker memory leak".to_owned())
            }
        });

        tokio::time::timeout(
            Duration::from_secs(1),
            runtime.append_messages(
                &thread_key,
                &[SessionMessageInput {
                    client_message_id: Some("first".to_owned()),
                    role: MessageRole::User,
                    parts: vec![
                        json!({
                            "type": "text",
                            "text": "# Requester Context\n\nThe Slack user who prompted this turn is Alice."
                        }),
                        json!({
                            "type": "text",
                            "text": "<@U123> please fix the memory leak in the worker"
                        }),
                    ],
                    metadata: json!({}),
                }],
            ),
        )
        .await
        .expect("append first message should not wait for title generation")
        .expect("append first message");

        generator_started.notified().await;

        let session = store.get_session(&thread_key).await.unwrap();
        assert_eq!(session.title, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            sources.lock().await.clone(),
            vec!["please fix the memory leak in the worker".to_owned()]
        );

        runtime
            .append_messages(
                &thread_key,
                &[SessionMessageInput {
                    client_message_id: Some("burst".to_owned()),
                    role: MessageRole::User,
                    parts: vec![json!({"type": "text", "text": "add more logging"})],
                    metadata: json!({}),
                }],
            )
            .await
            .expect("append burst message");

        assert_eq!(calls.load(Ordering::SeqCst), 1);

        generator_release.notify_one();
        wait_for_session_title(&store, &thread_key, "Fix worker memory leak").await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        runtime
            .append_messages(
                &thread_key,
                &[SessionMessageInput {
                    client_message_id: Some("second".to_owned()),
                    role: MessageRole::User,
                    parts: vec![json!({"type": "text", "text": "add more logging"})],
                    metadata: json!({}),
                }],
            )
            .await
            .expect("append second message");

        let session = store.get_session(&thread_key).await.unwrap();
        assert_eq!(session.title.as_deref(), Some("Fix worker memory leak"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn env_value<'a>(spec: &'a SandboxSpec, name: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .find(|env| env.name == name)
            .map(|env| env.value.as_str())
    }

    fn default_capabilities() -> SessionSandboxCapabilities {
        SessionSandboxCapabilities::default_enabled()
    }

    fn restricted_capabilities() -> SessionSandboxCapabilities {
        SessionSandboxCapabilities {
            repo_cache: SessionRepoCacheAccess::None,
            observability_enabled: false,
            api_server_enabled: false,
        }
    }

    fn runtime_with_warm_pool(
        store: &PgSessionStore,
        backend: Arc<MockBackend>,
        workload_marker: impl Into<String>,
    ) -> SessionRuntime {
        let workload_marker = Arc::new(workload_marker.into());
        let claimed_marker = workload_marker.clone();
        let warm_marker = workload_marker.clone();
        let mut runtime = SessionRuntime::new(
            store.clone(),
            SandboxRuntime::backend_with_warm_spec_factory(
                backend,
                move |_thread_key, _execution_id, _harness, _persona| {
                    SandboxSpec::new("mock")
                        .mount(Mount::new(
                            centaur_sandbox_core::MountKind::Bind {
                                source_path: "/var/lib/centaur/repos".to_owned(),
                            },
                            SANDBOX_REPOS_MOUNT_PATH,
                        ))
                        .env("WARM_POOL_TEST_MARKER", claimed_marker.as_str())
                },
                move || SandboxSpec::new("mock").env("WARM_POOL_TEST_MARKER", warm_marker.as_str()),
            ),
        );
        let warm_pool = Arc::new(WarmPoolManager::new(
            runtime.sandbox_runtime.manager.clone(),
            store.clone(),
            runtime.sandbox_runtime.warm_spec_factory.clone().unwrap(),
            runtime.sandbox_runtime.workload_key.clone().unwrap(),
            WarmPoolConfig {
                target_size: 1,
                replenish_interval: Duration::from_secs(60),
                bootstrap_iron_control_principal: None,
                max_running_sandboxes: None,
            },
        ));
        runtime.warm_pool = Some(warm_pool);
        runtime
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capability_mismatch_replaces_existing_sandbox() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:cap-replace-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        store
            .update_sandbox_assignment(&thread_key, "sbx-full", &default_capabilities())
            .await
            .expect("assign default sandbox");
        let session = store.get_session(&thread_key).await.unwrap();
        let execution_id = store
            .create_execution(&thread_key, None, json!({}))
            .await
            .expect("create execution")
            .execution
            .execution_id;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with_warm_pool(&store, backend.clone(), thread_key.as_str());
        let sandbox_id = runtime
            .ensure_session_sandbox(EnsureSessionSandboxRequest {
                thread_key: &thread_key,
                harness_type: &HarnessType::Codex,
                persona_id: None,
                existing_sandbox_id: session.sandbox_id.as_deref(),
                existing_sandbox_capabilities: session.sandbox_capabilities.as_ref(),
                iron_control_principal: None,
                desired_capabilities: &restricted_capabilities(),
                execution_id: &execution_id,
            })
            .await
            .expect("replace sandbox");

        assert_eq!(sandbox_id, "mock-sbx");
        assert_eq!(backend.stopped(), vec!["sbx-full".to_owned()]);
        let session = store.get_session(&thread_key).await.unwrap();
        assert_eq!(session.sandbox_id.as_deref(), Some("mock-sbx"));
        assert_eq!(
            session.sandbox_capabilities,
            Some(restricted_capabilities())
        );
        let spec = backend.created_specs().pop().expect("created cold spec");
        assert!(!spec.capabilities.repo_cache.enabled());
        assert!(!spec.capabilities.observability_enabled);
        assert!(!spec.capabilities.api_server_enabled);
        assert_eq!(
            env_value(&spec, "CENTAUR_SANDBOX_OBSERVABILITY_ENABLED"),
            Some("false")
        );
        assert_eq!(
            env_value(&spec, "CENTAUR_SANDBOX_API_SERVER_ENABLED"),
            Some("false")
        );
        let blocklist = env_value(&spec, "TOOL_BLOCKLIST").unwrap_or("");
        for tool in OBSERVABILITY_TOOL_BLOCKLIST.split(',') {
            assert!(blocklist.split(',').any(|blocked| blocked == tool));
        }
        assert!(
            !spec
                .mounts
                .iter()
                .any(|mount| mount.target_path == SANDBOX_REPOS_MOUNT_PATH)
        );
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .any(|event| event.event_type == "session.sandbox_capabilities_replaced")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_default_capabilities_skip_warm_pool() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:cap-warm-skip-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        let execution_id = store
            .create_execution(&thread_key, None, json!({}))
            .await
            .expect("create execution")
            .execution
            .execution_id;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with_warm_pool(&store, backend.clone(), thread_key.as_str());
        let workload_key = runtime
            .warm_pool
            .as_ref()
            .unwrap()
            .workload_key()
            .to_owned();
        let warm_sandbox_id = format!("warm-sbx-{}", uuid::Uuid::new_v4());
        store
            .insert_ready_warm_sandbox(&warm_sandbox_id, &workload_key)
            .await
            .expect("insert warm sandbox");

        let sandbox_id = runtime
            .ensure_session_sandbox(EnsureSessionSandboxRequest {
                thread_key: &thread_key,
                harness_type: &HarnessType::Codex,
                persona_id: None,
                existing_sandbox_id: None,
                existing_sandbox_capabilities: None,
                iron_control_principal: None,
                desired_capabilities: &restricted_capabilities(),
                execution_id: &execution_id,
            })
            .await
            .expect("ensure sandbox");

        assert_eq!(sandbox_id, "mock-sbx");
        assert_eq!(
            store
                .claim_ready_warm_sandbox(&workload_key, thread_key.as_str())
                .await
                .expect("warm row should remain ready"),
            Some(warm_sandbox_id)
        );
        let session = store.get_session(&thread_key).await.unwrap();
        assert_eq!(
            session.sandbox_capabilities,
            Some(restricted_capabilities())
        );
        let spec = backend.created_specs().pop().expect("created cold spec");
        assert!(!spec.capabilities.repo_cache.enabled());
        assert!(!spec.capabilities.observability_enabled);
        assert!(!spec.capabilities.api_server_enabled);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn existing_running_sandbox_ensures_proxy_before_reuse() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:proxy-reuse-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        let execution_id = store
            .create_execution(&thread_key, None, json!({}))
            .await
            .expect("create execution")
            .execution
            .execution_id;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        let sandbox_id = runtime
            .ensure_session_sandbox(EnsureSessionSandboxRequest {
                thread_key: &thread_key,
                harness_type: &HarnessType::Codex,
                persona_id: None,
                existing_sandbox_id: Some("sbx-existing"),
                existing_sandbox_capabilities: None,
                iron_control_principal: Some("principal-existing"),
                desired_capabilities: &SessionSandboxCapabilities::default_enabled(),
                execution_id: &execution_id,
            })
            .await
            .expect("reuse existing sandbox");

        assert_eq!(sandbox_id, "sbx-existing");
        assert_eq!(
            backend.proxy_ensures(),
            vec![("sbx-existing".to_owned(), "principal-existing".to_owned())]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn capacity_pressure_pauses_oldest_idle_assigned_sandbox() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        backend.set_observed_status(
            "sbx-old",
            SandboxStatus::Unknown("status temporarily unavailable".to_owned()),
        );
        backend.set_observed_status("sbx-hot", SandboxStatus::Running);
        backend.set_observed_status("sbx-stale", SandboxStatus::Gone);
        backend.set_observed_status("sbx-paused", SandboxStatus::Suspended);

        let stale_thread =
            ThreadKey::parse(format!("test:capacity-stale-{}", uuid::Uuid::new_v4())).unwrap();
        let paused_thread =
            ThreadKey::parse(format!("test:capacity-paused-{}", uuid::Uuid::new_v4())).unwrap();
        let old_thread =
            ThreadKey::parse(format!("test:capacity-old-{}", uuid::Uuid::new_v4())).unwrap();
        let hot_thread =
            ThreadKey::parse(format!("test:capacity-hot-{}", uuid::Uuid::new_v4())).unwrap();
        let trigger_thread =
            ThreadKey::parse(format!("test:capacity-trigger-{}", uuid::Uuid::new_v4())).unwrap();

        store
            .create_or_get_session(&stale_thread, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create stale session");
        store
            .update_sandbox_id(&stale_thread, Some("sbx-stale"))
            .await
            .expect("assign stale sandbox");
        store
            .create_or_get_session(&paused_thread, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create paused session");
        store
            .update_sandbox_id(&paused_thread, Some("sbx-paused"))
            .await
            .expect("assign paused sandbox");
        store
            .append_event(
                &paused_thread,
                None,
                "session.sandbox_paused",
                json!({
                    "thread_key": paused_thread.as_str(),
                    "sandbox_id": "sbx-paused",
                    "reason": "capacity_pressure",
                }),
            )
            .await
            .expect("append paused event");
        store
            .create_or_get_session(&old_thread, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create old session");
        store
            .update_sandbox_id(&old_thread, Some("sbx-old"))
            .await
            .expect("assign old sandbox");
        store
            .create_or_get_session(&hot_thread, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create hot session");
        store
            .update_sandbox_id(&hot_thread, Some("sbx-hot"))
            .await
            .expect("assign hot sandbox");
        sqlx::query(
            r#"
            update sessions
            set sandbox_last_active_at = case
                    when thread_key = $1 then now() - interval '3 hours'
                    when thread_key = $2 then now() - interval '2 hours'
                    when thread_key = $3 then now() - interval '1 hour'
                end
            where thread_key in ($1, $2, $3)
            "#,
        )
        .bind(stale_thread.as_str())
        .bind(paused_thread.as_str())
        .bind(old_thread.as_str())
        .execute(store.pool())
        .await
        .expect("age capacity candidates");

        let controller = SandboxCapacityController::new(
            store.clone(),
            Arc::new(SandboxManager::new(backend.clone())),
            Arc::new(DashMap::new()),
            SandboxCapacityConfig {
                max_running: 2,
                hot_idle_grace: Duration::from_secs(300),
            },
        );

        controller
            .run_with_capacity(&trigger_thread, "exe-trigger", "cold_create", || async {
                Ok(())
            })
            .await
            .expect("admit under capacity");

        assert_eq!(backend.status_of("sbx-old"), Some(SandboxStatus::Suspended));
        assert_eq!(backend.status_of("sbx-hot"), Some(SandboxStatus::Running));
        assert_eq!(
            store
                .get_session(&stale_thread)
                .await
                .expect("get stale session")
                .sandbox_id,
            None
        );
        assert_eq!(
            store
                .get_session(&paused_thread)
                .await
                .expect("get paused session")
                .sandbox_id
                .as_deref(),
            Some("sbx-paused")
        );
        let old_events = store
            .list_events_after(&old_thread, 0, None, 100)
            .await
            .expect("list old events");
        assert!(old_events.iter().any(|event| {
            event.event_type == "session.sandbox_paused"
                && event.payload.get("reason").and_then(Value::as_str) == Some("capacity_pressure")
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workflow_cleanup_stops_and_clears_owned_sandbox() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let workflow_run_id = format!("run-{}", uuid::Uuid::new_v4());
        let thread_key =
            ThreadKey::parse(format!("test:wf-cleanup-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(
                &thread_key,
                &HarnessType::Codex,
                None,
                json!({
                    "source": "absurd_workflow",
                    "workflow_run_id": workflow_run_id,
                    "workflow_owned_thread": true,
                }),
            )
            .await
            .expect("create session");
        store
            .update_sandbox_id(&thread_key, Some("sbx-owned"))
            .await
            .expect("set sandbox id");
        store
            .insert_ready_warm_sandbox("sbx-owned", "test-workload")
            .await
            .expect("insert warm sandbox");
        assert_eq!(
            store
                .claim_ready_warm_sandbox("test-workload", thread_key.as_str())
                .await
                .expect("claim warm sandbox"),
            Some("sbx-owned".to_owned())
        );
        assert!(
            store
                .list_referenced_sandbox_ids()
                .await
                .expect("list referenced sandboxes")
                .contains(&"sbx-owned".to_owned())
        );

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        let report = runtime
            .stop_workflow_owned_sandboxes(&workflow_run_id, "test")
            .await
            .expect("cleanup workflow sandboxes");

        assert_eq!(report.stopped, vec!["sbx-owned".to_owned()]);
        assert_eq!(backend.stopped(), vec!["sbx-owned".to_owned()]);
        assert_eq!(
            store.get_session(&thread_key).await.unwrap().sandbox_id,
            None
        );
        assert!(
            !store
                .list_referenced_sandbox_ids()
                .await
                .expect("list referenced sandboxes")
                .contains(&"sbx-owned".to_owned())
        );
        let all = events(&store, &thread_key).await;
        assert!(all.iter().any(|event| {
            event.event_type == "session.workflow_sandbox_stopped"
                && event.payload["workflow_run_id"] == json!(workflow_run_id)
                && event.payload["cleared"] == json!(true)
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workflow_cleanup_preserves_explicit_unowned_thread_key() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let workflow_run_id = format!("run-{}", uuid::Uuid::new_v4());
        let thread_key =
            ThreadKey::parse(format!("test:wf-explicit-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(
                &thread_key,
                &HarnessType::Codex,
                None,
                json!({
                    "source": "absurd_workflow",
                    "workflow_run_id": workflow_run_id,
                }),
            )
            .await
            .expect("create session");
        store
            .update_sandbox_id(&thread_key, Some("sbx-explicit"))
            .await
            .expect("set sandbox id");

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        let report = runtime
            .stop_workflow_owned_sandboxes(&workflow_run_id, "test")
            .await
            .expect("cleanup workflow sandboxes");

        assert!(report.stopped.is_empty());
        assert!(backend.stopped().is_empty());
        assert_eq!(
            store.get_session(&thread_key).await.unwrap().sandbox_id,
            Some("sbx-explicit".to_owned())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workflow_cleanup_clears_owned_sandbox_when_backend_reports_missing() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let workflow_run_id = format!("run-{}", uuid::Uuid::new_v4());
        let thread_key =
            ThreadKey::parse(format!("test:wf-missing-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(
                &thread_key,
                &HarnessType::Codex,
                None,
                json!({
                    "source": "absurd_workflow",
                    "workflow_run_id": workflow_run_id,
                    "workflow_owned_thread": true,
                }),
            )
            .await
            .expect("create session");
        store
            .update_sandbox_id(&thread_key, Some("sbx-missing"))
            .await
            .expect("set sandbox id");

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        backend.mark_stop_missing("sbx-missing");
        let runtime = runtime_with(&store, backend);
        let report = runtime
            .stop_workflow_owned_sandboxes(&workflow_run_id, "test")
            .await
            .expect("cleanup workflow sandboxes");

        assert_eq!(report.missing, vec!["sbx-missing".to_owned()]);
        assert_eq!(
            store.get_session(&thread_key).await.unwrap().sandbox_id,
            None
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resume_failure_replaces_sandbox_and_preserves_harness_thread_id() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:resume-failed-{}", uuid::Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        store
            .update_sandbox_id(&thread_key, Some("sbx-old"))
            .await
            .expect("set sandbox id");
        store
            .update_harness_thread_id(&thread_key, Some("harness-thread-1"))
            .await
            .expect("set harness thread id");
        let execution_id = store
            .create_execution(&thread_key, None, json!({}))
            .await
            .expect("create execution")
            .execution
            .execution_id;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Suspended, Vec::new()));
        backend.fail_resume();
        let runtime = runtime_with(&store, backend);
        let sandbox_id = runtime
            .ensure_session_sandbox(EnsureSessionSandboxRequest {
                thread_key: &thread_key,
                harness_type: &HarnessType::Codex,
                persona_id: None,
                existing_sandbox_id: Some("sbx-old"),
                existing_sandbox_capabilities: None,
                iron_control_principal: None,
                desired_capabilities: &SessionSandboxCapabilities::default_enabled(),
                execution_id: &execution_id,
            })
            .await
            .expect("resume failure should fall through to replacement");

        assert_eq!(sandbox_id, "mock-sbx");
        let session = store.get_session(&thread_key).await.unwrap();
        assert_eq!(session.sandbox_id, Some("mock-sbx".to_owned()));
        assert_eq!(
            session.harness_thread_id,
            Some("harness-thread-1".to_owned())
        );
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .any(|event| event.event_type == "session.sandbox_resume_failed")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_pipe_ensure_opens_one_io_per_sandbox() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:pipe-race-{}", uuid::Uuid::new_v4())).unwrap();
        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let (first_io, _first_stdout, _first_stdin) = mock_io();
        let (second_io, _second_stdout, _second_stdin) = mock_io();
        backend.push_io(first_io).await;
        backend.push_io(second_io).await;

        let runtime = runtime_with(&store, backend.clone());
        let (first, second) = tokio::join!(
            runtime.ensure_session_pipe(&thread_key, "sbx-pipe-race"),
            runtime.ensure_session_pipe(&thread_key, "sbx-pipe-race"),
        );

        first.expect("first pipe ensure should succeed");
        second.expect("second pipe ensure should reuse the first pipe");
        assert_eq!(backend.opens(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdout_eof_recovers_terminal_output_from_recorded_logs() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:eof-recorded-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-recorded"), true).await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let (io, stdout, _stdin) = mock_io();
        backend.push_io(io).await;

        let runtime = runtime_with(&store, backend.clone());
        runtime
            .ensure_session_pipe(&thread_key, "sbx-recorded")
            .await
            .expect("open initial pipe");
        backend.set_recorded_output(completed_output_lines("Recovered from pod logs."));
        drop(stdout);

        wait_for_event(&store, &thread_key, "session.execution_completed").await;
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .any(|event| event.event_type == "session.stdout_pump_recovered"),
            "expected recorded-output recovery event"
        );
        assert!(
            !all.iter()
                .any(|event| event.event_type == "session.stdout_pump_reattached"),
            "recorded terminal output should avoid a live reattach"
        );
        assert!(
            !all.iter()
                .any(|event| event.event_type == "session.execution_failed"),
            "stdout eof should not fail an active execution when logs contain a terminal turn"
        );
        let completed = all
            .iter()
            .find(|event| event.event_type == "session.execution_completed")
            .expect("completed event");
        assert_eq!(
            completed.payload["result_text"].as_str(),
            Some("Recovered from pod logs.")
        );
        assert_eq!(backend.opens(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdout_eof_reattaches_and_delivers_late_terminal_output() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:eof-reattach-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-reattach"), true).await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let (first_io, mut first_stdout, _first_stdin) = mock_io();
        let (second_io, mut second_stdout, _second_stdin) = mock_io();
        backend.push_io(first_io).await;
        backend.push_io(second_io).await;

        let runtime = runtime_with(&store, backend.clone());
        runtime
            .ensure_session_pipe(&thread_key, "sbx-reattach")
            .await
            .expect("open initial pipe");
        first_stdout
            .write_all(b"{\"type\":\"thread.started\",\"thread_id\":\"mock-thread\"}\n")
            .await
            .unwrap();
        drop(first_stdout);

        wait_for_event(&store, &thread_key, "session.stdout_pump_reattached").await;
        second_stdout
            .write_all(&completed_output_bytes("Completed after reattach."))
            .await
            .unwrap();

        wait_for_event(&store, &thread_key, "session.execution_completed").await;
        let all = events(&store, &thread_key).await;
        assert!(
            !all.iter()
                .any(|event| event.event_type == "session.execution_failed"),
            "reattached stdout should not produce the old false failure"
        );
        let completed = all
            .iter()
            .find(|event| event.event_type == "session.execution_completed")
            .expect("completed event");
        assert_eq!(
            completed.payload["result_text"].as_str(),
            Some("Completed after reattach.")
        );
        assert_eq!(backend.opens(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdout_eof_fails_when_sandbox_no_longer_accepts_io() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:eof-gone-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-gone"), true).await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let (io, stdout, _stdin) = mock_io();
        backend.push_io(io).await;

        let runtime = runtime_with(&store, backend.clone());
        runtime
            .ensure_session_pipe(&thread_key, "sbx-gone")
            .await
            .expect("open initial pipe");
        backend.set_status(SandboxStatus::Gone);
        drop(stdout);

        wait_for_event(&store, &thread_key, "session.execution_failed").await;
        let all = events(&store, &thread_key).await;
        let failed = all
            .iter()
            .find(|event| event.event_type == "session.execution_failed")
            .expect("failed event");
        let error = failed.payload["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("sandbox stdout closed before terminal output"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("sandbox no longer accepts io"),
            "expected sandbox status detail: {error}"
        );
        assert!(
            !all.iter()
                .any(|event| event.event_type == "session.stdout_pump_reattached"),
            "gone sandbox should not reattach"
        );
        assert_eq!(backend.opens(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn adopts_finished_turn_from_recorded_sandbox_output() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-logs-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;

        let backend = Arc::new(MockBackend::new(
            SandboxStatus::Running,
            vec![
                json!({"type": "item.completed", "item": {"id": "msg-1", "type": "agentMessage", "text": "Done: pushed commit abc123.", "phase": "final_answer"}}).to_string(),
                json!({"type": "turn.completed", "turn": {"id": "turn-1", "status": "completed"}}).to_string(),
            ],
        ));
        let runtime = runtime_with(&store, backend.clone());
        runtime.adopt_orphaned_executions().await;

        wait_for_event(&store, &thread_key, "session.execution_completed").await;
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .any(|event| event.event_type == "session.execution_adopted"),
            "expected an adoption event"
        );
        let completed = all
            .iter()
            .find(|event| event.event_type == "session.execution_completed")
            .expect("completed event");
        assert_eq!(
            completed.payload["result_text"].as_str(),
            Some("Done: pushed commit abc123.")
        );
        // The terminal came from recorded output; no live attach was needed.
        assert_eq!(backend.opens(), 0);
        let session = store.get_session(&thread_key).await.unwrap();
        assert_ne!(session.status.as_ref(), "failed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn adopts_live_when_recorded_output_has_no_terminal() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-live-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let (io, mut stdout, _stdin) = mock_io();
        backend.push_io(io).await;

        let runtime = runtime_with(&store, backend.clone());
        runtime.adopt_orphaned_executions().await;
        assert_eq!(backend.opens(), 1);

        stdout
            .write_all(
                b"{\"type\":\"turn.completed\",\"turn\":{\"id\":\"turn-1\",\"status\":\"completed\"}}\n",
            )
            .await
            .unwrap();
        wait_for_event(&store, &thread_key, "session.execution_completed").await;
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter().any(|event| {
                event.event_type == "session.execution_adopted"
                    && event.payload["mode"] == json!("live_attach")
            }),
            "expected a live adoption event"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fails_orphans_whose_sandbox_is_gone() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-gone-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Gone, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        runtime.adopt_orphaned_executions().await;

        wait_for_event(&store, &thread_key, "session.execution_failed").await;
        let all = events(&store, &thread_key).await;
        let failed = all
            .iter()
            .find(|event| event.event_type == "session.execution_failed")
            .expect("failed event");
        let error = failed.payload["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("execution orphaned by control plane restart"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("sandbox no longer accepts io"),
            "expected status detail: {error}"
        );
        assert_eq!(backend.opens(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fails_queued_orphans_that_never_received_input() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-queued-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-mock"), false).await;

        // The one-shot scan has no later tick to revisit skipped rows, so it
        // fails queued orphans immediately regardless of age.
        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        runtime.adopt_orphaned_executions().await;

        wait_for_event(&store, &thread_key, "session.execution_failed").await;
        let all = events(&store, &thread_key).await;
        let failed = all
            .iter()
            .find(|event| event.event_type == "session.execution_failed")
            .expect("failed event");
        let error = failed.payload["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("orphaned before input was sent"),
            "unexpected error: {error}"
        );
        assert_eq!(backend.opens(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn periodic_scan_skips_young_pre_sandbox_executions_until_grace_passes() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-young-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), false).await;

        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        let mut state = OrphanAdoptionState::default();
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;

        // A queued row younger than the grace window may belong to a live
        // execute_session mid-transition; a periodic scan must leave it
        // alone and revisit it later.
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .all(|event| event.event_type != "session.execution_failed"),
            "young queued execution must not be failed"
        );
        let active = store
            .list_active_executions()
            .await
            .expect("list active executions");
        assert!(
            active
                .iter()
                .any(|execution| execution.execution_id == execution_id),
            "young queued execution must stay active"
        );

        // Once the row ages past the grace window, a later tick fails it.
        backdate_execution(&store, &execution_id, 300.0).await;
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        wait_for_event(&store, &thread_key, "session.execution_failed").await;

        // A newly running execution can still be waiting for its warm
        // sandbox assignment. It gets the same periodic grace, but is failed
        // if it remains unassigned after the grace window.
        let running_thread =
            ThreadKey::parse(format!("test:adopt-young-running-{}", uuid::Uuid::new_v4())).unwrap();
        let running_execution = orphaned_execution(&store, &running_thread, None, true).await;
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        assert!(
            events(&store, &running_thread)
                .await
                .iter()
                .all(|event| event.event_type != "session.execution_failed"),
            "young running execution must survive sandbox assignment"
        );

        backdate_execution(&store, &running_execution, 300.0).await;
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        wait_for_event(&store, &running_thread, "session.execution_failed").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn adopts_deferred_execution_after_lease_expires() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-deferred-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;
        store
            .claim_stdout_owner(
                &execution_id,
                "other-control-plane",
                Duration::from_secs(60),
            )
            .await
            .expect("claim lease for other owner");

        let backend = Arc::new(MockBackend::new(
            SandboxStatus::Running,
            vec![
                json!({"type": "item.completed", "item": {"id": "msg-1", "type": "agentMessage", "text": "Done: recovered after handoff.", "phase": "final_answer"}}).to_string(),
                json!({"type": "turn.completed", "turn": {"id": "turn-1", "status": "completed"}}).to_string(),
            ],
        ));
        let runtime = runtime_with(&store, backend.clone());

        // While another control plane holds the stdout-owner lease the scan
        // must defer instead of stealing the execution.
        runtime.adopt_orphaned_executions().await;
        wait_for_event(&store, &thread_key, "session.execution_adoption_deferred").await;
        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .all(|event| event.event_type != "session.execution_completed"),
            "deferred execution must not be terminalized"
        );

        // Once the lease expires (owner died without releasing), a later
        // scan adopts the execution and recovers the recorded terminal. The
        // expiry is forced in the database rather than slept through so slow
        // test databases cannot turn the first scan into the adopting one.
        expire_stdout_lease(&store, &execution_id).await;
        runtime.adopt_orphaned_executions().await;
        wait_for_event(&store, &thread_key, "session.execution_adopted").await;
        wait_for_event(&store, &thread_key, "session.execution_completed").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn periodic_scan_ignores_executions_owned_by_this_process() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-own-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;
        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend.clone());
        assert!(
            store
                .claim_stdout_owner(
                    &execution_id,
                    &runtime.stdout_owner_id,
                    Duration::from_secs(60)
                )
                .await
                .expect("claim as this control plane")
        );

        // A healthy execution owned by the scanning process must be skipped
        // silently: no deferral event, no sandbox status probe.
        let mut state = OrphanAdoptionState::default();
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;

        let all = events(&store, &thread_key).await;
        assert!(
            all.iter().all(|event| {
                event.event_type != "session.execution_adoption_deferred"
                    && event.event_type != "session.execution_adopted"
                    && event.event_type != "session.execution_failed"
            }),
            "self-owned execution must not be touched by the scan"
        );
        store
            .fail_execution_if_active(&execution_id, "test cleanup")
            .await
            .expect("terminalize execution");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawned_adoption_loop_recovers_orphans() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-loop-{}", uuid::Uuid::new_v4())).unwrap();
        orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;

        let backend = Arc::new(MockBackend::new(
            SandboxStatus::Running,
            vec![
                json!({"type": "item.completed", "item": {"id": "msg-1", "type": "agentMessage", "text": "Done: recovered by the loop.", "phase": "final_answer"}}).to_string(),
                json!({"type": "turn.completed", "turn": {"id": "turn-1", "status": "completed"}}).to_string(),
            ],
        ));
        let runtime = runtime_with(&store, backend.clone());
        runtime.spawn_orphan_adoption(Duration::from_millis(50));

        wait_for_event(&store, &thread_key, "session.execution_adopted").await;
        wait_for_event(&store, &thread_key, "session.execution_completed").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn periodic_scans_record_deferral_once() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:adopt-dedup-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;
        store
            .claim_stdout_owner(
                &execution_id,
                "other-control-plane",
                Duration::from_secs(60),
            )
            .await
            .expect("claim lease for other owner");

        let backend = Arc::new(MockBackend::new(
            SandboxStatus::Running,
            vec![
                json!({"type": "item.completed", "item": {"id": "msg-1", "type": "agentMessage", "text": "Done: recovered after release.", "phase": "final_answer"}}).to_string(),
                json!({"type": "turn.completed", "turn": {"id": "turn-1", "status": "completed"}}).to_string(),
            ],
        ));
        let runtime = runtime_with(&store, backend.clone());

        // Repeated periodic scans over the same held lease must record the
        // deferral event only once.
        let mut state = OrphanAdoptionState::default();
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        let all = events(&store, &thread_key).await;
        let deferrals = all
            .iter()
            .filter(|event| event.event_type == "session.execution_adoption_deferred")
            .count();
        assert_eq!(deferrals, 1, "deferral event must be recorded once");

        // Releasing the lease (a clean shutdown handoff) lets the next scan
        // adopt immediately; this also terminalizes the execution before the
        // test releases TEST_LOCK.
        store
            .release_stdout_owner(&execution_id, "other-control-plane")
            .await
            .expect("release lease");
        runtime
            .run_orphan_adoption_scan(&mut state, Some(PRE_SANDBOX_ORPHAN_GRACE))
            .await;
        wait_for_event(&store, &thread_key, "session.execution_completed").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_handoff_releases_owned_leases() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:handoff-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;
        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend);
        assert!(
            store
                .claim_stdout_owner(
                    &execution_id,
                    &runtime.stdout_owner_id,
                    Duration::from_secs(60)
                )
                .await
                .expect("claim as this control plane")
        );

        runtime.handoff_owned_executions(Duration::ZERO).await;

        wait_for_event(&store, &thread_key, "session.stdout_owner_released").await;
        // The lease is immediately claimable by a peer control plane; without
        // the handoff it would only expire after the lease TTL.
        assert!(
            store
                .claim_stdout_owner(&execution_id, "peer-control-plane", Duration::from_secs(5))
                .await
                .expect("peer claims released lease")
        );
        store
            .fail_execution_if_active(&execution_id, "test cleanup")
            .await
            .expect("terminalize execution");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_handoff_waits_for_executions_to_finish() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let thread_key =
            ThreadKey::parse(format!("test:handoff-wait-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;
        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend);
        assert!(
            store
                .claim_stdout_owner(
                    &execution_id,
                    &runtime.stdout_owner_id,
                    Duration::from_secs(60)
                )
                .await
                .expect("claim as this control plane")
        );

        // The execution finishes while the drain is waiting; no lease should
        // be released and no handoff event recorded.
        let completer_store = store.clone();
        let completer_id = execution_id.clone();
        let completer = tokio::spawn(async move {
            sleep(Duration::from_millis(300)).await;
            completer_store
                .complete_execution_if_active(&completer_id)
                .await
                .expect("complete execution")
        });
        runtime
            .handoff_owned_executions(Duration::from_secs(5))
            .await;
        let completed = completer.await.expect("completer task");
        assert!(
            completed.is_some(),
            "the completer, not the handoff, must terminalize the execution"
        );

        let all = events(&store, &thread_key).await;
        assert!(
            all.iter()
                .all(|event| event.event_type != "session.stdout_owner_released"),
            "finished execution must not be handed off"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_fences_new_stdout_claims() {
        let Some(store) = test_store().await else {
            return;
        };
        let _serial = TEST_LOCK.lock().await;
        let backend = Arc::new(MockBackend::new(SandboxStatus::Running, Vec::new()));
        let runtime = runtime_with(&store, backend);

        // Nothing owned: the handoff returns immediately but still flips
        // the shutdown fence.
        runtime.handoff_owned_executions(Duration::ZERO).await;

        let thread_key =
            ThreadKey::parse(format!("test:handoff-fence-{}", uuid::Uuid::new_v4())).unwrap();
        let execution_id = orphaned_execution(&store, &thread_key, Some("sbx-mock"), true).await;
        let error = runtime
            .claim_stdout_owner(&execution_id)
            .await
            .expect_err("claims after shutdown must be rejected");
        assert!(
            matches!(error, SessionRuntimeError::ShuttingDown),
            "unexpected error: {error}"
        );
        store
            .fail_execution_if_active(&execution_id, "test cleanup")
            .await
            .expect("terminalize execution");
    }
}
