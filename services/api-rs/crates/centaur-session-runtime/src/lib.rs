use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime},
};

use centaur_iron_control::SessionRegistrar;
use centaur_sandbox_core::{
    Mount, SandboxBackend, SandboxError, SandboxId, SandboxIoGuard, SandboxRead, SandboxSpec,
    SandboxStatus, SandboxWrite,
};
use centaur_sandbox_manager::{
    SandboxManager, SandboxReaper, SandboxReaperConfig, WarmPoolConfig, WarmPoolError,
    WarmPoolManager, WarmSandboxSpecFactory,
};
use centaur_session_core::{
    ExecutionStatus, HarnessType, MessageRole, Session, SessionEvent, SessionExecution,
    SessionMessageInput, ThreadKey,
};
use centaur_session_sqlx::{
    PgSessionStore, SessionEventListener, SessionStoreError, default_metadata,
};
use centaur_telemetry::{
    record_sandbox_warm_pool_claim, record_session_execution_finished,
    record_session_execution_started, record_session_failure, record_session_first_token_latency,
};
use dashmap::DashMap;
use futures_util::{SinkExt, Stream, StreamExt, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io,
    sync::Mutex,
    time::{Instant, Interval, MissedTickBehavior, interval_at, sleep},
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};
use tracing::{Instrument, Span, error, info, info_span, warn};

pub const SESSION_OUTPUT_LINE_EVENT: &str = "session.output.line";
pub const SESSION_FIRST_TOKEN_EVENT: &str = "session.first_token";

const EVENT_STREAM_SAFETY_POLL_INTERVAL: Duration = Duration::from_secs(30);
const STEERING_STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const STEERING_STARTUP_RETRY_TIMEOUT: Duration = Duration::from_secs(15);
const COMPONENT_SESSION_RUNTIME: &str = "session_runtime";

type SandboxSpecFactory = Arc<dyn Fn(&ThreadKey, &str, &HarnessType) -> SandboxSpec + Send + Sync>;
type SessionInputSink = FramedWrite<SandboxWrite, LinesCodec>;
type ExecutionSpanRegistry = Arc<Mutex<HashMap<String, Span>>>;
type SessionPipeMap = Arc<DashMap<String, SessionPipe>>;
type SessionPipeOpenLocks = Arc<DashMap<String, Arc<Mutex<()>>>>;

#[derive(Clone)]
pub struct SessionRuntime {
    store: PgSessionStore,
    sandbox_runtime: SandboxRuntime,
    sandbox_pipes: SessionPipeMap,
    sandbox_pipe_open_locks: SessionPipeOpenLocks,
    execution_spans: ExecutionSpanRegistry,
    iron_control: Option<SessionRegistrar>,
    warm_pool: Option<Arc<WarmPoolManager>>,
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

#[derive(Debug)]
pub struct ExecuteSessionInput {
    pub idempotency_key: Option<String>,
    pub metadata: Option<Value>,
    pub input_lines: Vec<String>,
    pub idle_timeout_ms: Option<u64>,
    pub max_duration_ms: Option<u64>,
}

#[derive(Clone)]
struct SessionPipe {
    stdin: Arc<Mutex<SessionInputSink>>,
}

/// Shared handles threaded through background session tasks (stdout pump,
/// terminal-output recording, max-duration failure, idle pause).
#[derive(Clone)]
struct RuntimeContext {
    store: PgSessionStore,
    manager: Arc<SandboxManager>,
    sandbox_pipes: SessionPipeMap,
    execution_spans: ExecutionSpanRegistry,
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

impl SessionRuntime {
    pub fn new(store: PgSessionStore, sandbox_runtime: SandboxRuntime) -> Self {
        Self {
            store,
            sandbox_runtime,
            sandbox_pipes: Arc::new(DashMap::new()),
            sandbox_pipe_open_locks: Arc::new(DashMap::new()),
            execution_spans: Arc::new(Mutex::new(HashMap::new())),
            iron_control: None,
            warm_pool: None,
        }
    }

    fn context(&self) -> RuntimeContext {
        RuntimeContext {
            store: self.store.clone(),
            manager: self.sandbox_runtime.manager.clone(),
            sandbox_pipes: self.sandbox_pipes.clone(),
            execution_spans: self.execution_spans.clone(),
        }
    }

    /// Attach an iron-control registrar so each new session upserts its
    /// principal and assigns the configured roles.
    pub fn with_iron_control(mut self, registrar: SessionRegistrar) -> Self {
        self.iron_control = Some(registrar);
        self
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

    /// Spawn the background reaper that stops sandboxes whose idle pause or
    /// total lifetime expired. No-op when both TTLs are disabled.
    pub fn with_sandbox_reaper(self, config: SandboxReaperConfig) -> Self {
        if !config.is_enabled() {
            return self;
        }
        SandboxReaper::new(self.sandbox_runtime.manager.clone(), config).spawn();
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
        let result = async {
            info!(
                component = COMPONENT_SESSION_RUNTIME,
                event = "session_create_or_get_started",
                thread_key = %thread_key,
                harness_type = %harness_type,
                iron_control_enabled = self.iron_control.is_some(),
                "creating or loading session"
            );
            // Read slack_user_id before `metadata` is consumed below; it keys the
            // 1:1 DM principal and is only known here at session creation.
            let slack_user_id = metadata
                .as_ref()
                .and_then(|metadata| metadata.get("slack_user_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            // The human-readable conversation name the chat bot resolved
            // (Slack channel/DM, or Discord channel), used as the principal's
            // display name. Read it here for the same reason, before `metadata`
            // is consumed below.
            let conversation_name = metadata
                .as_ref()
                .and_then(|metadata| {
                    metadata
                        .get("slack_conversation_name")
                        .or_else(|| metadata.get("discord_conversation_name"))
                })
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let mut harness_switched = false;
            let session = match self
                .store
                .create_or_get_session(
                    thread_key,
                    harness_type,
                    persona_id,
                    default_metadata(metadata),
                )
                .await
            {
                Ok(session) => session,
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
            if let Some(registrar) = &self.iron_control {
                // iron-control is the source of truth for the session's egress
                // proxy: without a registered principal the proxy has no identity
                // to bind to, so a registration failure must fail session creation
                // rather than silently boot a sandbox with a non-functional proxy.
                let principal = registrar
                    .register_session(
                        thread_key.as_str(),
                        slack_user_id.as_deref(),
                        conversation_name.as_deref(),
                    )
                    .await?;
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
        let result = async {
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
        Ok(message_ids)
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
        let result = async {
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

            let sandbox_id = match self
                .ensure_session_sandbox(
                    thread_key,
                    &session.harness_type,
                    session.sandbox_id.as_deref(),
                    session.iron_control_principal.as_deref(),
                    &execution.execution_id,
                )
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

            let pipe = match self.ensure_session_pipe(thread_key, &sandbox_id).await {
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
        if let Ok(execution) = self
            .store
            .fail_execution(execution_id, &error_message)
            .await
        {
            record_finished_execution_metric(
                &self.store,
                thread_key,
                &execution,
                "failed",
                Some(runtime_error_failure_class(error)),
            )
            .await;
        }
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
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
        existing_sandbox_id: Option<&str>,
        iron_control_principal: Option<&str>,
        execution_id: &str,
    ) -> Result<String, SessionRuntimeError> {
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
        );
        let ensure_started = Instant::now();
        let result = async {
            if let Some(sandbox_id) = existing_sandbox_id {
                let id = SandboxId::new(sandbox_id);
                match self.sandbox_runtime.manager.status(&id).await {
                    Ok(status) => match existing_sandbox_action(&status) {
                        ExistingSandboxAction::Reuse => {
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
                            match self.sandbox_runtime.manager.resume(&id).await {
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
                                Err(error) => {
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

            // Warm sandboxes are pre-booted with the workload's default
            // harness; a session on any other harness needs a cold sandbox.
            let warm_harness_matches = self
                .sandbox_runtime
                .warm_harness
                .as_ref()
                .is_none_or(|warm| warm == harness_type);
            if !warm_harness_matches && self.warm_pool.is_some() {
                record_sandbox_warm_pool_claim("harness_mismatch");
            }
            if let Some(warm_pool) = self.warm_pool.as_ref().filter(|_| warm_harness_matches) {
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
                            .update_sandbox_id(thread_key, Some(sandbox_id.as_str()))
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

            let mut spec =
                (self.sandbox_runtime.spec_factory)(thread_key, execution_id, harness_type);
            if let Some(principal) = iron_control_principal {
                spec.iron_control_principal = Some(principal.to_owned());
            }
            let create_started = Instant::now();
            let handle = self.sandbox_runtime.manager.create_running(spec).await?;
            let startup_duration = create_started.elapsed();
            let ready_duration = ensure_started.elapsed();
            span.record("centaur.sandbox_id", handle.id.as_str());
            span.record("sandbox_id", handle.id.as_str());
            self.store
                .update_sandbox_id(thread_key, Some(handle.id.as_str()))
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
            let pipe = SessionPipe {
                stdin: Arc::new(Mutex::new(FramedWrite::new(io.stdin, LinesCodec::new()))),
            };

            self.sandbox_pipes
                .insert(sandbox_id.to_owned(), pipe.clone());
            let ctx = self.context();
            let thread_key = thread_key.clone();
            let pump_thread_key = thread_key.clone();
            let pump_key = sandbox_id.to_owned();
            let pump_pipe = pipe.clone();
            let stdout = io.stdout;
            let stderr = io.stderr;
            let guard = io.guard;
            let stderr_key = pump_key.clone();

            tokio::spawn(async move {
                let result = run_stdout_pump(
                    ctx.clone(),
                    pump_thread_key.clone(),
                    &pump_key,
                    stdout,
                    guard,
                )
                .await;
                if let Err(error) = result {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "session_stdout_pump_failed",
                        thread_key = %pump_thread_key,
                        sandbox_id = %pump_key,
                        %error,
                        "session stdout pump failed"
                    );
                    let _ = ctx
                        .store
                        .append_event(
                            &pump_thread_key,
                            None,
                            "session.stdout_pump_failed",
                            json!({
                                "sandbox_id": pump_key.as_str(),
                                "error": error.to_string(),
                            }),
                        )
                        .await;
                }
                ctx.sandbox_pipes
                    .remove_if(&pump_key, |_sandbox_id, current| {
                        Arc::ptr_eq(&current.stdin, &pump_pipe.stdin)
                    });
            });

            tokio::spawn(async move {
                if let Err(error) = drain_stderr(stderr).await {
                    warn!(
                        component = COMPONENT_SESSION_RUNTIME,
                        event = "session_stderr_drain_failed",
                        sandbox_id = %stderr_key,
                        %error,
                        "session stderr drain failed"
                    );
                }
            });

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
        let executions = match self.store.list_active_executions().await {
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
            return;
        }
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "execution_adoption_scan",
            orphan_count = executions.len(),
            "adopting executions orphaned by a previous control plane process"
        );
        for execution in executions {
            if let Err(error) = self.adopt_orphaned_execution(&execution).await {
                warn!(
                    component = COMPONENT_SESSION_RUNTIME,
                    event = "execution_adoption_failed",
                    thread_key = %execution.thread_key,
                    execution_id = %execution.execution_id,
                    %error,
                    "failed to adopt orphaned execution; will retry on next startup"
                );
            }
        }
    }

    async fn adopt_orphaned_execution(
        &self,
        execution: &SessionExecution,
    ) -> Result<(), SessionRuntimeError> {
        let thread_key = &execution.thread_key;
        let execution_id = execution.execution_id.as_str();
        if execution.status == ExecutionStatus::Queued {
            // Input is only written after an execution is marked running, so
            // a queued orphan never reached the harness: nothing can come.
            self.fail_orphaned_execution(
                thread_key,
                execution_id,
                "",
                "orphaned before input was sent",
            )
            .await;
            return Ok(());
        }
        let session = self.store.get_session(thread_key).await?;
        let Some(sandbox_id) = session.sandbox_id.as_deref() else {
            self.fail_orphaned_execution(
                thread_key,
                execution_id,
                "",
                "orphaned with no sandbox assigned",
            )
            .await;
            return Ok(());
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
            return Ok(());
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
            return Ok(());
        }

        // No terminal in the recorded output: treat the turn as still in
        // flight. Re-attach the stdout pump and re-arm the remaining
        // max-duration budget so an adopted-but-silent turn stays bounded.
        self.ensure_session_pipe(thread_key, sandbox_id).await?;
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
        Ok(())
    }

    async fn fail_orphaned_execution(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        sandbox_id: &str,
        detail: &str,
    ) {
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
            move |_thread_key: &ThreadKey, _execution_id: &str, _harness: &HarnessType| {
                spec.clone()
            };
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
            move |thread_key, _execution_id, harness| workload.spec(thread_key, harness),
            move || warm_workload.warm_spec(),
        );
        runtime.warm_harness = warm_harness;
        runtime
    }

    pub fn backend_with_spec_factory<F>(backend: Arc<dyn SandboxBackend>, spec_factory: F) -> Self
    where
        F: Fn(&ThreadKey, &str, &HarnessType) -> SandboxSpec + Send + Sync + 'static,
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
        F: Fn(&ThreadKey, &str, &HarnessType) -> SandboxSpec + Send + Sync + 'static,
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

    fn spec(&self, thread_key: &ThreadKey, harness: &HarnessType) -> SandboxSpec {
        self.spec_for(Some(thread_key), harness)
    }

    fn warm_spec(&self) -> SandboxSpec {
        match self {
            Self::MockAppServer { .. } => self.spec_for(None, &HarnessType::Codex),
            Self::CodexAppServer { harness, .. } => self.spec_for(None, harness),
        }
    }

    fn spec_for(&self, thread_key: Option<&ThreadKey>, harness: &HarnessType) -> SandboxSpec {
        match self {
            Self::MockAppServer { image } => SandboxSpec::new(image)
                .command(["/bin/sh", "-lc"])
                .args([mock_app_server_script()])
                .env("CENTAUR_HARNESS_TYPE", harness.as_ref()),
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
                    if let Some((channel_id, thread_ts)) =
                        slack_destination_from_thread_key(thread_key.as_str())
                    {
                        spec = spec
                            .env("SLACK_CHANNEL_ID", channel_id)
                            .env("SLACK_THREAD_TS", thread_ts);
                    }
                }
                for mount in mounts {
                    spec = spec.mount(mount.clone());
                }
                for (name, value) in env {
                    spec = spec.env(name.clone(), value.clone());
                }
                spec
            }
        }
    }
}

fn slack_destination_from_thread_key(thread_key: &str) -> Option<(&str, &str)> {
    let parts = thread_key.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        ["slack", channel_id, thread_ts] if !channel_id.is_empty() && !thread_ts.is_empty() => {
            Some((*channel_id, *thread_ts))
        }
        ["slack", _team_id, channel_id, thread_ts]
            if !channel_id.is_empty() && !thread_ts.is_empty() =>
        {
            Some((*channel_id, *thread_ts))
        }
        _ => None,
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

async fn run_stdout_pump(
    ctx: RuntimeContext,
    thread_key: ThreadKey,
    sandbox_id: &str,
    stdout: SandboxRead,
    _guard: SandboxIoGuard,
) -> Result<(), SessionRuntimeError> {
    let span = info_span!(
        "centaur.api_rs.session.stdout_pump",
        component = COMPONENT_SESSION_RUNTIME,
        event = "session_stdout_pump",
        "centaur.thread_key" = thread_key.as_str(),
        "centaur.sandbox_id" = sandbox_id,
        thread_key = %thread_key,
        sandbox_id,
    );
    async {
        let mut stdout = FramedRead::new(stdout, LinesCodec::new());
        info!(
            component = COMPONENT_SESSION_RUNTIME,
            event = "session_stdout_pump_started",
            thread_key = %thread_key,
            sandbox_id,
            "session stdout pump started"
        );
        let mut output_state = StdoutPumpState::default();
        let mut line_count = 0_u64;
        while let Some(line) = stdout.next().await {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    let message = stdout_pump_error_message(&error);
                    record_stdout_pump_failure(&ctx, &thread_key, sandbox_id, message).await?;
                    return Ok(());
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
            let output_event =
                append_output_line(&ctx.store, &thread_key, Some(&output_execution_id), &line)
                .instrument(output_span.clone())
                .await?;
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
        if let Some(execution) = ctx.store.active_execution_for_thread(&thread_key).await? {
            let execution_span = ctx
                .execution_spans
                .lock()
                .await
                .get(&execution.execution_id)
                .cloned();
            let output_span = output_state.stdout_span_for_execution(
                execution_span.as_ref(),
                &thread_key,
                sandbox_id,
                &execution.execution_id,
            );
            record_terminal_output(
                &ctx,
                &thread_key,
                sandbox_id,
                &execution.execution_id,
                TerminalOutput::Failed {
                    error: "sandbox stdout closed before terminal output".to_owned(),
                },
            )
            .instrument(output_span)
            .await?;
            ctx.execution_spans.lock().await.remove(&execution.execution_id);
            output_state.forget(&execution.execution_id);
        }
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
        Ok(())
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
            let Some(execution) = ctx.store.complete_execution_if_active(execution_id).await?
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
        TerminalOutput::Failed { error } => {
            failure_class = Some(terminal_failure_class(&error));
            let Some(execution) = ctx
                .store
                .fail_execution_if_active(execution_id, &error)
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
        .fail_execution_if_active(execution_id, &error)
        .await?
    else {
        return Ok(());
    };
    ctx.execution_spans.lock().await.remove(execution_id);
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
        SessionRuntimeError::Store(_) => "store",
        SessionRuntimeError::Sandbox(SandboxError::NotFound(_)) => "sandbox_not_found",
        SessionRuntimeError::Sandbox(SandboxError::Unsupported { .. }) => "sandbox_unsupported",
        SessionRuntimeError::Sandbox(SandboxError::NotReady(_)) => "sandbox_not_ready",
        SessionRuntimeError::Sandbox(SandboxError::Io { .. }) => "sandbox_io",
        SessionRuntimeError::Sandbox(SandboxError::Backend { .. }) => "sandbox_backend",
        SessionRuntimeError::Sandbox(SandboxError::InvalidSpec(_)) => "sandbox_invalid_spec",
        SessionRuntimeError::IronControl(_) => "iron_control",
        SessionRuntimeError::WarmPool(_) => "warm_pool",
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
fn thread_trace_id(thread_key: &ThreadKey) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("centaur:thread:{}", thread_key.as_str()).as_bytes(),
    )
    .to_string()
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
    serde_json::to_string(&value).unwrap_or_else(|_| line.to_owned())
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

async fn append_output_line(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: Option<&str>,
    line: &str,
) -> Result<SessionEvent, SessionRuntimeError> {
    let safe_line = redact_sensitive_text(line);
    let event = store
        .append_event(
            thread_key,
            execution_id,
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
    #[error(transparent)]
    Store(#[from] SessionStoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    IronControl(#[from] centaur_iron_control::IronControlError),
    #[error(transparent)]
    WarmPool(#[from] WarmPoolError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_sandbox_core::MountKind;
    use centaur_session_core::SessionStatus;
    use serde_json::json;
    use time::OffsetDateTime;

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
    fn interrupted_turn_completed_without_answer_is_failure() {
        let event = json!({
            "type": "turn.completed",
            "turn": {"id": "turn-1", "status": "interrupted"},
        });

        assert_eq!(
            terminal_output(&event, ""),
            Some(TerminalOutput::Failed {
                error: "turn completed with status interrupted before final answer".to_owned()
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

        let spec = workload.spec(&thread_key, &HarnessType::Codex);

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
    fn codex_workload_does_not_inject_stale_continue_thread_id() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            Vec::new(),
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();

        let spec = workload.spec(&thread_key, &HarnessType::Codex);

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

        let claimed_spec = workload.spec(&thread_key, &HarnessType::ClaudeCode);
        let warm_spec = workload.warm_spec();

        assert_eq!(
            env_value(&claimed_spec, "CENTAUR_THREAD_KEY"),
            Some(thread_key.as_str())
        );
        assert_eq!(env_value(&warm_spec, "CENTAUR_THREAD_KEY"), None);
    }

    #[test]
    fn codex_claimed_slack_spec_exports_upload_destination() {
        let workload = SandboxWorkloadMode::codex_app_server(
            "centaur-agent:latest",
            [("CENTAUR_API_URL".to_owned(), "http://api:8000".to_owned())],
            HarnessType::Codex,
        );
        let thread_key = ThreadKey::parse("slack:T123:C123:1780000000.000000").unwrap();

        let claimed_spec = workload.spec(&thread_key, &HarnessType::Codex);
        let warm_spec = workload.warm_spec();

        assert_eq!(env_value(&claimed_spec, "SLACK_CHANNEL_ID"), Some("C123"));
        assert_eq!(
            env_value(&claimed_spec, "SLACK_THREAD_TS"),
            Some("1780000000.000000")
        );
        assert_eq!(env_value(&warm_spec, "SLACK_CHANNEL_ID"), None);
        assert_eq!(env_value(&warm_spec, "SLACK_THREAD_TS"), None);
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
            sandbox_spec_key(&workload.spec(&first_thread_key, &HarnessType::ClaudeCode)),
            sandbox_spec_key(&workload.spec(&second_thread_key, &HarnessType::ClaudeCode))
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

        let codex_spec = workload.spec(&thread_key, &HarnessType::Codex);
        let claude_spec = workload.spec(&thread_key, &HarnessType::ClaudeCode);
        let amp_spec = workload.spec(&thread_key, &HarnessType::Amp);

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

        let spec = workload.spec(&thread_key, &HarnessType::ClaudeCode);

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
            workload.spec(&thread_key, &HarnessType::ClaudeCode).args,
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
    }

    fn session_with_sandbox(sandbox_id: &str) -> Session {
        let thread_key = ThreadKey::parse("cli:test-idle").unwrap();
        let now = OffsetDateTime::now_utc();
        Session {
            thread_key,
            sandbox_id: Some(sandbox_id.to_owned()),
            harness_type: HarnessType::Codex,
            harness_thread_id: None,
            persona_id: None,
            status: SessionStatus::Idle,
            iron_control_principal: None,
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
}

/// Integration tests for orphaned-execution adoption. They need a real
/// Postgres; set `SESSION_RUNTIME_TEST_DATABASE_URL` to run them (they skip
/// silently otherwise, mirroring `ABSURD_TEST_DATABASE_URL` in absurd-sdk).
#[cfg(test)]
mod adoption_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use centaur_sandbox_core::{ObservedSandbox, SandboxHandle, SandboxIo, SandboxResult};
    use tokio::io::{AsyncWriteExt, DuplexStream};

    use super::*;

    /// The adoption scan is database-wide, so concurrently running tests
    /// would adopt each other's executions. Serialize the module; every test
    /// fully terminalizes its own executions before releasing the lock.
    static TEST_LOCK: Mutex<()> = Mutex::const_new(());

    struct MockBackend {
        ios: Mutex<VecDeque<SandboxIo>>,
        recorded_output: Vec<String>,
        open_count: AtomicUsize,
        status: std::sync::Mutex<SandboxStatus>,
    }

    impl MockBackend {
        fn new(status: SandboxStatus, recorded_output: Vec<String>) -> Self {
            Self {
                ios: Mutex::new(VecDeque::new()),
                recorded_output,
                open_count: AtomicUsize::new(0),
                status: std::sync::Mutex::new(status),
            }
        }

        async fn push_io(&self, io: SandboxIo) {
            self.ios.lock().await.push_back(io);
        }

        fn opens(&self) -> usize {
            self.open_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl SandboxBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            Ok(SandboxHandle::new(SandboxId::new("mock-sbx"), "mock"))
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
            Ok(self.recorded_output.clone())
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(self.status.lock().unwrap().clone())
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            let status = self.status(id).await?;
            Ok(ObservedSandbox::new(id.clone(), "mock", status))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(Vec::new())
        }

        async fn stop(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }

        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }

        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
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
}
