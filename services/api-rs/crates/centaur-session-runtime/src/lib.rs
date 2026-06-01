use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use centaur_sandbox_core::{
    SandboxBackend, SandboxError, SandboxId, SandboxIoGuard, SandboxRead, SandboxSpec,
    SandboxStatus, SandboxWrite,
};
use centaur_sandbox_manager::SandboxManager;
use centaur_session_core::{
    HarnessType, Session, SessionEvent, SessionExecution, SessionMessageInput, ThreadKey,
};
use centaur_session_sqlx::{
    PgSessionStore, SessionEventListener, SessionStoreError, default_metadata,
};
use futures_util::{SinkExt, Stream, StreamExt, stream};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io,
    sync::Mutex,
    time::{Instant, Interval, MissedTickBehavior, interval_at},
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};
use tracing::warn;

pub const SESSION_OUTPUT_LINE_EVENT: &str = "session.output.line";

const MAX_SESSION_OUTPUT_LINE_BYTES: usize = 1024 * 1024;
const EVENT_STREAM_SAFETY_POLL_INTERVAL: Duration = Duration::from_secs(30);

type SandboxSpecFactory = Arc<dyn Fn(&ThreadKey, &str) -> SandboxSpec + Send + Sync>;
type SessionInputSink = FramedWrite<SandboxWrite, LinesCodec>;

#[derive(Clone)]
pub struct SessionRuntime {
    store: PgSessionStore,
    sandbox_runtime: SandboxRuntime,
    sandbox_pipes: Arc<Mutex<HashMap<String, SessionPipe>>>,
}

#[derive(Clone)]
pub struct SandboxRuntime {
    manager: Arc<SandboxManager>,
    spec_factory: SandboxSpecFactory,
}

#[derive(Clone, Debug)]
pub enum SandboxWorkloadMode {
    MockAppServer {
        image: String,
    },
    CodexAppServer {
        image: String,
        env: Vec<(String, String)>,
    },
}

#[derive(Debug)]
pub struct ExecuteSessionInput {
    pub metadata: Option<Value>,
    pub input_lines: Vec<String>,
    pub idle_timeout_ms: Option<u64>,
    pub max_duration_ms: Option<u64>,
}

#[derive(Clone)]
struct SessionPipe {
    stdin: Arc<Mutex<SessionInputSink>>,
}

struct EventStreamState {
    store: PgSessionStore,
    thread_key: ThreadKey,
    after_event_id: i64,
    pending: VecDeque<SessionEvent>,
    listener: SessionEventListener,
    safety_tick: Interval,
    done: bool,
}

impl SessionRuntime {
    pub fn new(store: PgSessionStore, sandbox_runtime: SandboxRuntime) -> Self {
        Self {
            store,
            sandbox_runtime,
            sandbox_pipes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn create_or_get_session(
        &self,
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
        metadata: Option<Value>,
    ) -> Result<Session, SessionRuntimeError> {
        Ok(self
            .store
            .create_or_get_session(thread_key, harness_type, default_metadata(metadata))
            .await?)
    }

    pub async fn append_messages(
        &self,
        thread_key: &ThreadKey,
        messages: &[SessionMessageInput],
    ) -> Result<Vec<String>, SessionRuntimeError> {
        if messages.is_empty() {
            return Err(SessionRuntimeError::BadRequest(
                "messages must not be empty".to_owned(),
            ));
        }
        Ok(self.store.append_messages(thread_key, messages).await?)
    }

    pub async fn execute_session(
        &self,
        thread_key: &ThreadKey,
        input: ExecuteSessionInput,
    ) -> Result<SessionExecution, SessionRuntimeError> {
        let session = self.store.get_session(thread_key).await?;
        validate_input_lines(&input.input_lines)?;
        validate_duration_options(&input)?;

        let execution = self
            .store
            .create_execution(thread_key, default_metadata(input.metadata))
            .await?;
        let execution = self
            .store
            .mark_execution_running(&execution.execution_id)
            .await?;
        let sandbox_id = self
            .ensure_session_sandbox(
                thread_key,
                session.sandbox_id.as_deref(),
                &execution.execution_id,
            )
            .await?;

        self.store
            .append_event(
                thread_key,
                Some(&execution.execution_id),
                "session.execution_started",
                json!({
                    "execution_id": execution.execution_id,
                    "thread_key": thread_key.as_str(),
                    "input_line_count": input.input_lines.len(),
                }),
            )
            .await?;

        let write_result = match self.ensure_session_pipe(thread_key, &sandbox_id).await {
            Ok(pipe) => write_input_lines(&pipe, &input.input_lines).await,
            Err(error) => Err(error),
        };

        match write_result {
            Ok(()) => {}
            Err(error) => {
                let error_message = error.to_string();
                let _ = self
                    .store
                    .append_event(
                        thread_key,
                        Some(&execution.execution_id),
                        "session.execution_failed",
                        json!({
                            "execution_id": execution.execution_id,
                            "thread_key": thread_key.as_str(),
                            "error": error_message,
                        }),
                    )
                    .await;
                let _ = self
                    .store
                    .fail_execution(&execution.execution_id, &error_message)
                    .await;
                return Err(error);
            }
        }

        self.store
            .append_event(
                thread_key,
                Some(&execution.execution_id),
                "session.execution_completed",
                json!({
                    "execution_id": execution.execution_id,
                    "thread_key": thread_key.as_str(),
                    "completion_reason": "input_accepted",
                }),
            )
            .await?;

        Ok(self
            .store
            .complete_execution(&execution.execution_id)
            .await?)
    }

    pub async fn stream_events(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
    ) -> Result<
        impl Stream<Item = Result<SessionEvent, SessionRuntimeError>> + use<>,
        SessionRuntimeError,
    > {
        let session = self.store.get_session(thread_key).await?;
        if let Some(sandbox_id) = session.sandbox_id.as_deref() {
            self.ensure_session_pipe(thread_key, sandbox_id).await?;
        }

        let listener = self.store.listen_session_events().await?;

        Ok(session_event_stream(
            self.store.clone(),
            thread_key.clone(),
            after_event_id,
            listener,
        ))
    }

    async fn ensure_session_sandbox(
        &self,
        thread_key: &ThreadKey,
        existing_sandbox_id: Option<&str>,
        execution_id: &str,
    ) -> Result<String, SessionRuntimeError> {
        if let Some(sandbox_id) = existing_sandbox_id {
            let id = SandboxId::new(sandbox_id);
            match self.sandbox_runtime.manager.status(&id).await {
                Ok(SandboxStatus::Running | SandboxStatus::Created) => {
                    return Ok(sandbox_id.to_owned());
                }
                Ok(_) | Err(SandboxError::NotFound(_)) => {}
                Err(error) => return Err(SessionRuntimeError::Sandbox(error)),
            }
        }

        let spec = (self.sandbox_runtime.spec_factory)(thread_key, execution_id);
        let handle = self.sandbox_runtime.manager.create_running(spec).await?;
        self.store
            .update_sandbox_id(thread_key, Some(handle.id.as_str()))
            .await?;
        Ok(handle.id.into_string())
    }

    async fn ensure_session_pipe(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: &str,
    ) -> Result<SessionPipe, SessionRuntimeError> {
        if let Some(pipe) = self.sandbox_pipes.lock().await.get(sandbox_id).cloned() {
            return Ok(pipe);
        }

        let io = self
            .sandbox_runtime
            .manager
            .open_io(&SandboxId::new(sandbox_id))
            .await?
            .into_parts();
        let pipe = SessionPipe {
            stdin: Arc::new(Mutex::new(FramedWrite::new(
                io.stdin,
                LinesCodec::new_with_max_length(MAX_SESSION_OUTPUT_LINE_BYTES),
            ))),
        };

        self.sandbox_pipes
            .lock()
            .await
            .insert(sandbox_id.to_owned(), pipe.clone());
        let store = self.store.clone();
        let thread_key = thread_key.clone();
        let pump_key = sandbox_id.to_owned();
        let sandbox_pipes = self.sandbox_pipes.clone();
        let stdout = io.stdout;
        let stderr = io.stderr;
        let guard = io.guard;
        let stderr_key = pump_key.clone();

        tokio::spawn(async move {
            let result =
                run_stdout_pump(store.clone(), thread_key.clone(), &pump_key, stdout, guard).await;
            if let Err(error) = result {
                warn!(%pump_key, %error, "session stdout pump failed");
                let _ = store
                    .append_event(
                        &thread_key,
                        None,
                        "session.stdout_pump_failed",
                        json!({
                            "sandbox_id": pump_key.as_str(),
                            "error": error.to_string(),
                        }),
                    )
                    .await;
            }
            sandbox_pipes.lock().await.remove(&pump_key);
        });

        tokio::spawn(async move {
            if let Err(error) = drain_stderr(stderr).await {
                warn!(%stderr_key, %error, "session stderr drain failed");
            }
        });

        Ok(pipe)
    }
}

impl SandboxRuntime {
    pub fn backend(backend: Arc<dyn SandboxBackend>, spec: SandboxSpec) -> Self {
        let spec_factory = move |_thread_key: &ThreadKey, _execution_id: &str| spec.clone();
        Self::backend_with_spec_factory(backend, spec_factory)
    }

    pub fn backend_with_workload(
        backend: Arc<dyn SandboxBackend>,
        workload: SandboxWorkloadMode,
    ) -> Self {
        Self::backend_with_spec_factory(backend, move |thread_key, _execution_id| {
            workload.spec(thread_key)
        })
    }

    pub fn backend_with_spec_factory<F>(backend: Arc<dyn SandboxBackend>, spec_factory: F) -> Self
    where
        F: Fn(&ThreadKey, &str) -> SandboxSpec + Send + Sync + 'static,
    {
        Self {
            manager: Arc::new(SandboxManager::new(backend)),
            spec_factory: Arc::new(spec_factory),
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
    ) -> Self {
        Self::CodexAppServer {
            image: image.into(),
            env: env.into_iter().collect(),
        }
    }

    fn spec(&self, thread_key: &ThreadKey) -> SandboxSpec {
        match self {
            Self::MockAppServer { image } => SandboxSpec::new(image)
                .command(["/bin/sh", "-lc"])
                .args([mock_app_server_script()]),
            Self::CodexAppServer { image, env } => {
                let mut spec =
                    SandboxSpec::new(image).env("CENTAUR_THREAD_KEY", thread_key.as_str());
                for (name, value) in env {
                    spec = spec.env(name.clone(), value.clone());
                }
                spec
            }
        }
    }
}

fn mock_app_server_script() -> &'static str {
    r#"while IFS= read -r line; do
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
  printf '{"type":"item.agentMessage.delta","turnId":"%s","session_id":"mock-codex-thread","delta":"PONG %s"}\n' "$turn_id" "$turn_index"
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
    listener: SessionEventListener,
) -> impl Stream<Item = Result<SessionEvent, SessionRuntimeError>> {
    stream::unfold(
        EventStreamState {
            store,
            thread_key,
            after_event_id,
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
        },
        |mut state| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    state.after_event_id = event.event_id;
                    return Some((Ok(event), state));
                }
                if state.done {
                    return None;
                }
                match state
                    .store
                    .list_events_after(&state.thread_key, state.after_event_id, 100)
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
                    },
                    Ok(events) => state.pending = events.into(),
                    Err(error) => {
                        state.done = true;
                        return Some((Err(SessionRuntimeError::Store(error)), state));
                    }
                }
            }
        },
    )
}

async fn run_stdout_pump(
    store: PgSessionStore,
    thread_key: ThreadKey,
    sandbox_id: &str,
    stdout: SandboxRead,
    _guard: SandboxIoGuard,
) -> Result<(), SessionRuntimeError> {
    let mut stdout = FramedRead::new(
        stdout,
        LinesCodec::new_with_max_length(MAX_SESSION_OUTPUT_LINE_BYTES),
    );
    while let Some(line) = stdout.next().await {
        let line = line.map_err(codec_error_to_runtime)?;
        append_output_line(&store, &thread_key, None, &line).await?;
    }
    store
        .append_event(
            &thread_key,
            None,
            "session.stdout_eof",
            json!({
                "sandbox_id": sandbox_id,
            }),
        )
        .await?;
    Ok(())
}

async fn drain_stderr(mut stderr: SandboxRead) -> Result<(), SessionRuntimeError> {
    io::copy(&mut stderr, &mut io::sink())
        .await
        .map_err(|err| {
            SessionRuntimeError::Sandbox(SandboxError::Io(format!("drain stderr: {err}")))
        })?;
    Ok(())
}

async fn write_input_lines(
    pipe: &SessionPipe,
    input_lines: &[String],
) -> Result<(), SessionRuntimeError> {
    let mut stdin = pipe.stdin.lock().await;
    for line in input_lines {
        stdin.send(line).await.map_err(codec_error_to_runtime)?;
    }
    Ok(())
}

async fn append_output_line(
    store: &PgSessionStore,
    thread_key: &ThreadKey,
    execution_id: Option<&str>,
    line: &str,
) -> Result<(), SessionRuntimeError> {
    store
        .append_event(
            thread_key,
            execution_id,
            SESSION_OUTPUT_LINE_EVENT,
            Value::String(line.to_owned()),
        )
        .await?;
    Ok(())
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

fn codec_error_to_runtime(error: LinesCodecError) -> SessionRuntimeError {
    SessionRuntimeError::Sandbox(SandboxError::Io(error.to_string()))
}

fn validate_duration_options(input: &ExecuteSessionInput) -> Result<(), SessionRuntimeError> {
    let idle_timeout = input
        .idle_timeout_ms
        .map(nonzero_duration_millis)
        .transpose()?;
    let max_duration = input
        .max_duration_ms
        .map(nonzero_duration_millis)
        .transpose()?;

    if let (Some(idle_timeout), Some(max_duration)) = (idle_timeout, max_duration)
        && idle_timeout > max_duration
    {
        return Err(SessionRuntimeError::BadRequest(
            "idle_timeout_ms must be less than or equal to max_duration_ms".to_owned(),
        ));
    }

    Ok(())
}

fn nonzero_duration_millis(value: u64) -> Result<Duration, SessionRuntimeError> {
    if value == 0 {
        return Err(SessionRuntimeError::BadRequest(
            "duration values must be greater than zero".to_owned(),
        ));
    }
    Ok(Duration::from_millis(value))
}

#[derive(Debug, Error)]
pub enum SessionRuntimeError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Store(#[from] SessionStoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
}
