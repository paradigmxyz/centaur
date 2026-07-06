use std::env;
use std::io::{self, BufRead, Write};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError},
};
use std::thread;
use std::time::Duration;

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};

use crate::otel;
use crate::server::{BlocksCommand, BlocksState, parse_blocks_line_with_state, write_blocks_error};
use crate::util::write_value;
use crate::{AppServerRuntime, HarnessServerError, Result};

#[derive(Debug, Clone, Copy)]
pub struct CodexHarnessServer {
    fallback_model_provider: &'static str,
}

impl CodexHarnessServer {
    pub fn codex() -> Self {
        Self {
            fallback_model_provider: "openai",
        }
    }

    fn default_model(&self) -> Option<String> {
        env::var("CODEX_MODEL")
            .ok()
            .or_else(|| env::var("OPENROUTER_MODEL").ok())
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
    }

    fn model_provider_for(&self, provider_override: Option<&str>, model: Option<&str>) -> String {
        provider_override
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                env::var("CODEX_MODEL_PROVIDER")
                    .ok()
                    .map(|provider| provider.trim().to_owned())
                    .filter(|provider| !provider.is_empty())
            })
            .or_else(|| {
                model
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .filter(|model| model.contains('/'))
                    .map(|_| "openrouter".to_string())
            })
            .or_else(|| {
                env::var("OPENROUTER_MODEL")
                    .ok()
                    .map(|model| model.trim().to_owned())
                    .filter(|model| !model.is_empty())
                    .map(|_| "openrouter".to_string())
            })
            .unwrap_or_else(|| self.fallback_model_provider.to_string())
    }
}

impl AppServerRuntime for CodexHarnessServer {
    fn run_stdio(&self) -> Result<()> {
        let bin = codex_bin();
        let mut child = ProcessCommand::new(&bin)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| HarnessServerError::SpawnCodex {
                bin: bin.clone(),
                source,
            })?;

        let mut child_stdin = child
            .stdin
            .take()
            .ok_or(HarnessServerError::CodexStdinUnavailable)?;
        let _stdin_thread = thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            io::copy(&mut stdin, &mut child_stdin)
        });

        let mut child_stderr = child
            .stderr
            .take()
            .ok_or(HarnessServerError::CodexStderrUnavailable)?;
        let stderr_thread = thread::spawn(move || {
            let mut stderr = io::stderr().lock();
            io::copy(&mut child_stderr, &mut stderr)
        });

        let mut child_stdout = child
            .stdout
            .take()
            .ok_or(HarnessServerError::CodexStdoutUnavailable)?;
        {
            let mut stdout = io::stdout().lock();
            io::copy(&mut child_stdout, &mut stdout)?;
            stdout.flush()?;
        }

        let status = child.wait()?;
        let _ = stderr_thread.join();
        if !status.success() {
            return Err(HarnessServerError::CodexExited { status });
        }
        Ok(())
    }
}

pub(crate) fn run_codex_blocks_server(config: CodexHarnessServer) -> Result<()> {
    let mut codex: Option<CodexJsonRpcChild> = None;
    let mut stdout = io::stdout().lock();
    let mut request_id = 1_i64;
    let mut thread_id: Option<String> = None;
    // The provider the thread was started/resumed on. codex pins the provider at
    // thread start (the app-server protocol has no per-turn provider), so this
    // lets a later conflicting override be surfaced rather than silently dropped.
    let mut thread_provider: Option<String> = None;
    let (command_tx, command_rx) = mpsc::channel();
    let (active_turn_tx, active_turn_rx) = mpsc::channel();
    let turn_active = Arc::new(AtomicBool::new(false));

    {
        let turn_active = Arc::clone(&turn_active);
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut blocks_state = BlocksState::default();
            for raw in stdin.lock().lines() {
                let Ok(line) = raw else {
                    break;
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match parse_blocks_line_with_state(trimmed, &mut blocks_state) {
                    Ok(BlocksCommand::Interrupt) if turn_active.load(Ordering::SeqCst) => {
                        if active_turn_tx
                            .send(CodexActiveTurnRequest::Interrupt)
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(command @ BlocksCommand::User { .. }) => {
                        turn_active.store(true, Ordering::SeqCst);
                        if command_tx
                            .send(CodexBlocksReaderInput::Command(command))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(command) => {
                        if command_tx
                            .send(CodexBlocksReaderInput::Command(command))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        if command_tx
                            .send(CodexBlocksReaderInput::Error(error.to_string()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
    }

    while let Ok(input) = command_rx.recv() {
        match input {
            CodexBlocksReaderInput::Command(BlocksCommand::User {
                input,
                client_user_message_id,
                model,
                provider,
                reasoning,
                trace_context,
            }) => {
                let traceparent = trace_context.effective_traceparent();
                turn_active.store(true, Ordering::SeqCst);
                let result = (|| -> Result<()> {
                    if codex.is_none() {
                        otel::configure_codex_otel_for_startup(&trace_context)?;
                        let mut child = CodexJsonRpcChild::spawn()?;
                        initialize_codex(
                            &mut child,
                            &mut stdout,
                            &mut request_id,
                            traceparent.as_deref(),
                        )?;
                        codex = Some(child);
                    }
                    let model = model.or_else(|| config.default_model());
                    let model_provider =
                        config.model_provider_for(provider.as_deref(), model.as_deref());
                    run_codex_user_turn(
                        codex.as_mut().expect("codex initialized"),
                        &mut stdout,
                        &mut request_id,
                        &mut thread_id,
                        &mut thread_provider,
                        input,
                        client_user_message_id,
                        (model, model_provider),
                        provider,
                        reasoning,
                        &active_turn_rx,
                        traceparent.as_deref(),
                    )
                })();
                turn_active.store(false, Ordering::SeqCst);
                drain_codex_active_turn_requests(&active_turn_rx);
                if let Err(error) = result {
                    let fallback_thread_id = thread_id.as_deref().unwrap_or("codex");
                    eprintln!("Codex blocks turn failed: {error:#}");
                    write_blocks_error(&mut stdout, fallback_thread_id, "turn", error.to_string())?;
                }
            }
            CodexBlocksReaderInput::Command(BlocksCommand::Interrupt) => {
                eprintln!("Codex blocks interrupt ignored: no active turn runs");
            }
            CodexBlocksReaderInput::Command(BlocksCommand::AttachmentChunk) => {}
            CodexBlocksReaderInput::Error(error) => {
                eprintln!("invalid Codex blocks input: {error}");
                write_blocks_error(
                    &mut stdout,
                    thread_id.as_deref().unwrap_or("codex"),
                    "input",
                    error,
                )?;
            }
        }
    }

    Ok(())
}

enum CodexBlocksReaderInput {
    Command(BlocksCommand),
    Error(String),
}

enum CodexActiveTurnRequest {
    Interrupt,
}

fn drain_codex_active_turn_requests(rx: &Receiver<CodexActiveTurnRequest>) {
    while rx.try_recv().is_ok() {}
}

fn initialize_codex<W: Write>(
    codex: &mut CodexJsonRpcChild,
    stdout: &mut W,
    request_id: &mut i64,
    traceparent: Option<&str>,
) -> Result<()> {
    let initialize_id = next_request_id(request_id);
    codex.send_request(
        initialize_id,
        "initialize",
        json!({
            "clientInfo": {
                "name": "centaur-harness-server",
                "title": null,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": null,
        }),
        traceparent,
    )?;
    codex
        .read_response_or_forward(initialize_id, stdout)
        .map(|_| ())
}

fn run_codex_user_turn<W: Write>(
    codex: &mut CodexJsonRpcChild,
    stdout: &mut W,
    request_id: &mut i64,
    thread_id: &mut Option<String>,
    thread_provider: &mut Option<String>,
    input: Vec<UserInput>,
    client_user_message_id: Option<String>,
    model_and_provider: (Option<String>, String),
    requested_provider: Option<String>,
    reasoning: Option<String>,
    active_turn_rx: &Receiver<CodexActiveTurnRequest>,
    traceparent: Option<&str>,
) -> Result<()> {
    let (model, model_provider) = model_and_provider;
    if thread_id.is_none() {
        *thread_id = Some(start_or_resume_thread(
            codex,
            stdout,
            request_id,
            &model_provider,
            traceparent,
        )?);
        *thread_provider = Some(model_provider.clone());
    } else if let (Some(requested), Some(pinned)) =
        (requested_provider.as_deref(), thread_provider.as_deref())
        && requested != pinned
    {
        // codex pins the provider at thread start, so an explicit mid-thread
        // override (e.g. a later `--bedrock`) cannot take effect. Surface it
        // rather than silently staying on the pinned provider; switching
        // providers requires a new thread (a harness flag like `--bedrock`
        // already restarts across harnesses, but a codex->codex provider switch
        // does not).
        eprintln!(
            "Codex provider `{requested}` ignored: this thread is pinned to `{pinned}` \
             (provider is fixed at thread start; start a new thread to switch providers)"
        );
    }
    let current_thread_id = thread_id
        .as_ref()
        .expect("thread id was initialized")
        .clone();

    let mut params = json!({
        "threadId": current_thread_id,
        "input": input,
    });
    if let Some(client_user_message_id) = client_user_message_id {
        params["clientUserMessageId"] = Value::String(client_user_message_id);
    }
    if let Some(model) = model {
        params["model"] = Value::String(model);
    }
    // Per-turn reasoning effort (codex `turn/start.effort`), parsed from the
    // `-rsn` message flag. Values match codex's ReasoningEffort enum
    // (none|minimal|low|medium|high|xhigh); validation happens upstream.
    if let Some(reasoning) = reasoning {
        params["effort"] = Value::String(reasoning);
    }

    // codex occasionally fails a turn at job-registration time with a transient
    // "Engine not found" 404 (its backend engine is still warming up), reported
    // as a -32602 error notification with willRetry:false. When that happens
    // before the turn has streamed any output, re-submit the same turn rather
    // than surfacing the failure: the engine registers within a second or two
    // and the resubmitted turn succeeds. Bounded by CODEX_ENGINE_RETRY_MAX
    // (default 2; set 0 to restore the old fail-fast behavior). Turns that have
    // already streamed output (or run a tool) are never retried, since
    // re-running them would duplicate output and repeat side effects.
    let max_retries = engine_retry_max();
    let mut retries = 0u32;
    loop {
        let turn_request_id = next_request_id(request_id);
        codex.send_request(turn_request_id, "turn/start", params.clone(), traceparent)?;
        let result = codex.read_response_or_forward(turn_request_id, stdout)?;
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                HarnessServerError::Protocol("turn/start response missing turn.id".to_string())
            })?
            .to_string();
        match codex.read_until_turn_terminal(
            stdout,
            thread_id.as_deref().unwrap_or_default(),
            &turn_id,
            active_turn_rx,
            request_id,
            traceparent,
        )? {
            TurnTermination::Done => return Ok(()),
            TurnTermination::RetriableEngineError { withheld } => {
                if retries >= max_retries {
                    // Out of retry budget: release the withheld `systemError`
                    // status and error so the client sees the real failure.
                    // This is also the `CODEX_ENGINE_RETRY_MAX=0` fail-fast path.
                    for value in &withheld {
                        write_value(stdout, value)?;
                    }
                    return Ok(());
                }
                retries += 1;
                eprintln!(
                    "codex turn hit a transient engine-registration error; \
                     retrying ({retries}/{max_retries})"
                );
                thread::sleep(retry_backoff(retries));
            }
        }
    }
}

fn start_or_resume_thread<W: Write>(
    codex: &mut CodexJsonRpcChild,
    stdout: &mut W,
    request_id: &mut i64,
    model_provider: &str,
    traceparent: Option<&str>,
) -> Result<String> {
    let cwd = env::current_dir()?.display().to_string();
    let resume = env::var("CODEX_CONTINUE_THREAD_ID")
        .or_else(|_| env::var("AMP_CONTINUE_THREAD_ID"))
        .unwrap_or_default();
    let (method, params) = if resume.trim().is_empty() {
        (
            "thread/start",
            json!({
                "cwd": cwd,
                "approvalPolicy": "never",
                "approvalsReviewer": "user",
                "sandbox": "danger-full-access",
                "modelProvider": model_provider,
            }),
        )
    } else {
        (
            "thread/resume",
            json!({
                "threadId": resume.trim(),
                "cwd": cwd,
                "approvalPolicy": "never",
                "approvalsReviewer": "user",
                "sandbox": "danger-full-access",
                "modelProvider": model_provider,
                "excludeTurns": false,
            }),
        )
    };

    let id = next_request_id(request_id);
    codex.send_request(id, method, params, traceparent)?;
    let result = codex.read_response_or_forward(id, stdout)?;
    result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| HarnessServerError::Protocol(format!("{method} response missing thread.id")))
}

struct CodexJsonRpcChild {
    child: Child,
    stdin: ChildStdin,
    stdout: Receiver<io::Result<String>>,
}

impl CodexJsonRpcChild {
    fn spawn() -> Result<Self> {
        let bin = codex_bin();
        let mut child = ProcessCommand::new(&bin)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| HarnessServerError::SpawnCodex {
                bin: bin.clone(),
                source,
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or(HarnessServerError::CodexStdinUnavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(HarnessServerError::CodexStdoutUnavailable)?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or(HarnessServerError::CodexStderrUnavailable)?;
        thread::spawn(move || {
            // Unlocked handle on purpose: this child lives across turns, so
            // holding the StderrLock for the copy's lifetime would block every
            // eprintln! in the server until the child exits.
            let mut parent_stderr = io::stderr();
            let _ = io::copy(&mut stderr, &mut parent_stderr);
        });

        let (stdout_tx, stdout_rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = io::BufReader::new(stdout);
            for raw in reader.lines() {
                let should_stop = raw.is_err();
                if stdout_tx.send(raw).is_err() || should_stop {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout: stdout_rx,
        })
    }

    fn send_request(
        &mut self,
        id: i64,
        method: &str,
        params: Value,
        traceparent: Option<&str>,
    ) -> Result<()> {
        let mut payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(traceparent) = traceparent {
            payload["trace"] = json!({ "traceparent": traceparent });
        }
        self.write_value(&payload)
    }

    fn send_error_response(&mut self, request: &Value) -> Result<()> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        self.write_value(&json!({
            "id": id,
            "error": {
                "code": -32000,
                "message": "Centaur blocks mode cannot service app-server client requests",
                "data": null,
            },
        }))
    }

    fn write_value(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_response_or_forward<W: Write>(
        &mut self,
        expected_id: i64,
        stdout: &mut W,
    ) -> Result<Value> {
        loop {
            let value = self.read_value()?;
            if is_server_request(&value) {
                self.send_error_response(&value)?;
                continue;
            }
            if response_id(&value) == Some(expected_id) {
                if let Some(error) = value.get("error") {
                    return Err(HarnessServerError::Protocol(format!(
                        "Codex app-server request {expected_id} failed: {error}"
                    )));
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            if notification_method(&value).is_some() {
                write_value(stdout, &value)?;
            }
        }
    }

    /// Drives a codex turn to its terminal notification, forwarding events to
    /// `stdout`. If the turn fails with a transient engine-registration error
    /// before streaming any output, the error (and the `systemError` status that
    /// precedes it) are withheld and handed back via `RetriableEngineError` so
    /// the caller can either drop them and re-submit the turn, or forward them
    /// once its retry budget is spent.
    fn read_until_turn_terminal<W: Write>(
        &mut self,
        stdout: &mut W,
        thread_id: &str,
        turn_id: &str,
        active_turn_rx: &Receiver<CodexActiveTurnRequest>,
        request_id: &mut i64,
        traceparent: Option<&str>,
    ) -> Result<TurnTermination> {
        let mut guard = TurnGuard::default();
        let mut interrupt_request_id = None;
        loop {
            let value = match self.read_value_timeout(Duration::from_millis(50))? {
                Some(value) => value,
                None => {
                    self.forward_pending_interrupt(
                        active_turn_rx,
                        &mut interrupt_request_id,
                        request_id,
                        thread_id,
                        turn_id,
                        traceparent,
                    )?;
                    continue;
                }
            };
            if is_server_request(&value) {
                self.send_error_response(&value)?;
                continue;
            }
            if let Some(id) = response_id(&value) {
                if Some(id) == interrupt_request_id {
                    if let Some(error) = value.get("error") {
                        return Err(HarnessServerError::Protocol(format!(
                            "Codex app-server turn/interrupt request {id} failed: {error}"
                        )));
                    }
                    continue;
                }
                continue;
            }
            if notification_method(&value).is_none() {
                continue;
            }
            let terminal = is_terminal_notification(&value, thread_id, turn_id);
            match guard.observe(value, terminal) {
                GuardStep::Retry(withheld) => {
                    return Ok(TurnTermination::RetriableEngineError { withheld });
                }
                GuardStep::Forward(values) => {
                    for value in &values {
                        write_value(stdout, value)?;
                    }
                }
                GuardStep::ForwardThenDone(values) => {
                    for value in &values {
                        write_value(stdout, value)?;
                    }
                    return Ok(TurnTermination::Done);
                }
            }
            self.forward_pending_interrupt(
                active_turn_rx,
                &mut interrupt_request_id,
                request_id,
                thread_id,
                turn_id,
                traceparent,
            )?;
        }
    }

    fn forward_pending_interrupt(
        &mut self,
        active_turn_rx: &Receiver<CodexActiveTurnRequest>,
        interrupt_request_id: &mut Option<i64>,
        request_id: &mut i64,
        thread_id: &str,
        turn_id: &str,
        traceparent: Option<&str>,
    ) -> Result<()> {
        while let Ok(CodexActiveTurnRequest::Interrupt) = active_turn_rx.try_recv() {
            if interrupt_request_id.is_some() {
                eprintln!("Codex blocks interrupt ignored: interrupt already requested");
                continue;
            }
            let id = next_request_id(request_id);
            self.send_request(
                id,
                "turn/interrupt",
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                }),
                traceparent,
            )?;
            *interrupt_request_id = Some(id);
        }
        Ok(())
    }

    fn read_value(&mut self) -> Result<Value> {
        loop {
            let line = match self.stdout.recv() {
                Ok(line) => line?,
                Err(_) => {
                    let status = self.child.wait()?;
                    return Err(HarnessServerError::CodexExited { status });
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Ok(serde_json::from_str(trimmed)?);
        }
    }

    fn read_value_timeout(&mut self, timeout: Duration) -> Result<Option<Value>> {
        loop {
            let line = match self.stdout.recv_timeout(timeout) {
                Ok(line) => line?,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait()?;
                    return Err(HarnessServerError::CodexExited { status });
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Ok(Some(serde_json::from_str(trimmed)?));
        }
    }
}

impl Drop for CodexJsonRpcChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Outcome of driving a single codex turn attempt.
enum TurnTermination {
    /// The turn reached a terminal state (completed, failed, or a non-retriable
    /// error); everything that needed forwarding has been forwarded.
    Done,
    /// The turn failed with a transient engine-registration error before
    /// streaming any output. The `systemError` status and error notification
    /// were withheld so the caller can drop them and re-submit the turn, or
    /// forward them once its retry budget is spent.
    RetriableEngineError { withheld: Vec<Value> },
}

/// Per-turn notification filter. Sits between codex's stdout and the client so
/// a transient, output-free engine failure can be swallowed and retried without
/// the client ever seeing a `systemError`. Pulled out of the blocking read loop
/// so the decision logic is unit-testable.
#[derive(Default)]
struct TurnGuard {
    /// A `systemError` status change held back because it may be immediately
    /// followed by a retriable engine error we are going to swallow.
    pending_system_error: Option<Value>,
    /// Set once the turn streams real work (an item or a model round-trip), at
    /// which point retrying is no longer safe.
    streamed: bool,
}

/// What `read_until_turn_terminal` should do with the events `observe` returns.
enum GuardStep {
    /// Forward these notifications (in order) and keep reading.
    Forward(Vec<Value>),
    /// Forward these notifications (in order); the turn is then terminal.
    ForwardThenDone(Vec<Value>),
    /// Withhold these (retriable) notifications; the caller drops them and
    /// re-submits the turn, or forwards them if it is out of retry budget.
    Retry(Vec<Value>),
}

impl TurnGuard {
    fn observe(&mut self, value: Value, terminal: bool) -> GuardStep {
        // Owned so `value` can be moved into `pending_system_error`/`out` below.
        let method = notification_method(&value).unwrap_or_default().to_owned();

        // A retriable engine error before any output: withhold it (and the
        // `systemError` status we were holding) and hand both back so the caller
        // can drop them on retry or forward them once out of budget.
        if terminal && method == "error" && !self.streamed && is_retriable_engine_error(&value) {
            let mut withheld = Vec::new();
            if let Some(status) = self.pending_system_error.take() {
                withheld.push(status);
            }
            withheld.push(value);
            return GuardStep::Retry(withheld);
        }

        // We are forwarding `value`; release any held status first to preserve
        // ordering.
        let mut out = Vec::new();
        if let Some(status) = self.pending_system_error.take() {
            out.push(status);
        }

        // Defer a `systemError` status: it usually precedes the engine error,
        // and if we end up retrying we want to drop it rather than flicker a
        // failed status at the client.
        if !self.streamed && is_system_error_status(&value) {
            self.pending_system_error = Some(value);
            return GuardStep::Forward(out);
        }

        if streams_turn_output(&method) {
            self.streamed = true;
        }
        out.push(value);
        if terminal {
            GuardStep::ForwardThenDone(out)
        } else {
            GuardStep::Forward(out)
        }
    }
}

/// Maximum number of times a turn that hit a transient engine-registration
/// error is re-submitted. `CODEX_ENGINE_RETRY_MAX` overrides the default; `0`
/// disables retries (the historical fail-fast behavior).
fn engine_retry_max() -> u32 {
    parse_engine_retry_max(env::var("CODEX_ENGINE_RETRY_MAX").ok().as_deref())
}

fn parse_engine_retry_max(raw: Option<&str>) -> u32 {
    const DEFAULT: u32 = 2;
    raw.and_then(|raw| raw.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT)
}

/// Backoff before the `retry`-th re-submission: 500ms, 1s, 2s, ... capped at
/// 5s — long enough for a warming engine to finish registering.
fn retry_backoff(retry: u32) -> Duration {
    let shift = retry.saturating_sub(1).min(4);
    Duration::from_millis((500u64 << shift).min(5_000))
}

/// True for codex's transient "engine warming up" failure, which surfaces as a
/// -32602 error notification (`willRetry:false`) whose message ends in
/// "...status 404 Not Found: Engine not found". These resolve on resubmission;
/// other -32602s (genuine invalid params) are left untouched.
fn is_retriable_engine_error(value: &Value) -> bool {
    let Some(message) = value
        .pointer("/params/error/message")
        .and_then(Value::as_str)
    else {
        return false;
    };
    message.contains("Engine not found")
        || (message.contains("Job registration failed") && message.contains("404"))
}

/// True for a `thread/status/changed` notification reporting a `systemError`.
fn is_system_error_status(value: &Value) -> bool {
    notification_method(value) == Some("thread/status/changed")
        && value.pointer("/params/status/type").and_then(Value::as_str) == Some("systemError")
}

/// True for notifications that represent real turn progress (an item event or a
/// completed model round-trip). Once one is seen the turn can no longer be
/// transparently retried.
fn streams_turn_output(method: &str) -> bool {
    method.starts_with("item/") || method == "thread/tokenUsage/updated"
}

fn is_server_request(value: &Value) -> bool {
    value.get("id").is_some() && value.get("method").is_some()
}

fn response_id(value: &Value) -> Option<i64> {
    value.get("id").and_then(Value::as_i64)
}

fn notification_method(value: &Value) -> Option<&str> {
    if value.get("id").is_some() {
        return None;
    }
    value.get("method").and_then(Value::as_str)
}

fn is_terminal_notification(value: &Value, thread_id: &str, turn_id: &str) -> bool {
    match notification_method(value) {
        Some("turn/completed") | Some("turn/failed") => {
            let notification_thread = value
                .pointer("/params/threadId")
                .and_then(Value::as_str)
                .unwrap_or(thread_id);
            let notification_turn = value
                .pointer("/params/turn/id")
                .or_else(|| value.pointer("/params/turnId"))
                .and_then(Value::as_str)
                .unwrap_or(turn_id);
            notification_thread == thread_id && notification_turn == turn_id
        }
        Some("error") => true,
        _ => false,
    }
}

fn next_request_id(request_id: &mut i64) -> i64 {
    let id = *request_id;
    *request_id += 1;
    id
}

fn codex_bin() -> String {
    if let Ok(bin) = env::var("CODEX_BIN") {
        return bin;
    }

    let candidates = ["codex", "/Applications/Codex.app/Contents/Resources/codex"];
    candidates
        .iter()
        .find(|bin| codex_supports_stdio_listen(bin))
        .copied()
        .unwrap_or("codex")
        .to_string()
}

fn codex_supports_stdio_listen(bin: &str) -> bool {
    let Ok(output) = ProcessCommand::new(bin)
        .args(["app-server", "--help"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout.contains("--listen") || stderr.contains("--listen")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A non-empty explicit provider override (the `--bedrock` blocks `provider`
    // field) short-circuits before any env/model heuristic, so these assertions
    // are deterministic regardless of CODEX_MODEL_PROVIDER / OPENROUTER_MODEL.
    #[test]
    fn explicit_provider_override_wins_over_model_heuristic() {
        let codex = CodexHarnessServer::codex();
        assert_eq!(
            codex.model_provider_for(Some("amazon-bedrock"), None),
            "amazon-bedrock"
        );
        assert_eq!(
            codex.model_provider_for(Some("amazon-bedrock"), Some("anthropic/claude-fable-5")),
            "amazon-bedrock"
        );
    }

    #[test]
    fn blank_provider_override_is_ignored() {
        // A blank override falls through to the model `/`-slug heuristic, which
        // selects openrouter — i.e. the override does not pin an empty provider.
        let codex = CodexHarnessServer::codex();
        assert_eq!(
            codex.model_provider_for(Some("   "), Some("vendor/model")),
            "openrouter"
        );
    }

    fn turn_started() -> Value {
        json!({ "method": "turn/started", "params": {} })
    }

    fn system_error_status() -> Value {
        json!({
            "method": "thread/status/changed",
            "params": { "status": { "type": "systemError" } }
        })
    }

    fn engine_error() -> Value {
        json!({
            "method": "error",
            "params": {
                "error": {
                    "codexErrorInfo": "other",
                    "message": "JSON-RPC error -32602: Job registration failed: \
                                Engine bad request: Task submission failed with status \
                                404 Not Found: Engine not found"
                },
                "willRetry": false
            }
        })
    }

    fn agent_delta() -> Value {
        json!({ "method": "item/agentMessage/delta", "params": { "delta": "hi" } })
    }

    /// Runs a `(notification, is_terminal)` sequence through a `TurnGuard` and
    /// returns the methods forwarded plus, when a retry is signalled, the methods
    /// withheld for the caller to drop (on retry) or forward (out of budget).
    fn drive(events: Vec<(Value, bool)>) -> (Vec<String>, Option<Vec<String>>) {
        let mut guard = TurnGuard::default();
        let mut forwarded = Vec::new();
        for (value, terminal) in events {
            match guard.observe(value, terminal) {
                GuardStep::Retry(withheld) => {
                    return (methods(&forwarded), Some(methods(&withheld)));
                }
                GuardStep::Forward(values) => forwarded.extend(values),
                GuardStep::ForwardThenDone(values) => {
                    forwarded.extend(values);
                    return (methods(&forwarded), None);
                }
            }
        }
        (methods(&forwarded), None)
    }

    fn methods(values: &[Value]) -> Vec<String> {
        values
            .iter()
            .map(|value| value["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn parse_engine_retry_max_defaults_and_overrides() {
        assert_eq!(parse_engine_retry_max(None), 2);
        assert_eq!(parse_engine_retry_max(Some("0")), 0);
        assert_eq!(parse_engine_retry_max(Some("5")), 5);
        assert_eq!(parse_engine_retry_max(Some("  3  ")), 3);
        // Garbage falls back to the default rather than disabling retries.
        assert_eq!(parse_engine_retry_max(Some("nope")), 2);
    }

    #[test]
    fn retry_backoff_grows_and_caps() {
        assert_eq!(retry_backoff(1).as_millis(), 500);
        assert_eq!(retry_backoff(2).as_millis(), 1_000);
        assert_eq!(retry_backoff(3).as_millis(), 2_000);
        assert_eq!(retry_backoff(99).as_millis(), 5_000);
    }

    #[test]
    fn classifies_only_the_transient_engine_error() {
        assert!(is_retriable_engine_error(&engine_error()));
        assert!(is_retriable_engine_error(&json!({
            "method": "error",
            "params": { "error": { "message": "Job registration failed: ... 404 Not Found" } }
        })));
        // A genuine invalid-params -32602 is not the warmup case.
        assert!(!is_retriable_engine_error(&json!({
            "method": "error",
            "params": { "error": { "message": "JSON-RPC error -32602: bad arguments" } }
        })));
        assert!(!is_retriable_engine_error(
            &json!({ "method": "error", "params": {} })
        ));
    }

    #[test]
    fn detects_system_error_status() {
        assert!(is_system_error_status(&system_error_status()));
        assert!(!is_system_error_status(&json!({
            "method": "thread/status/changed",
            "params": { "status": { "type": "running" } }
        })));
        assert!(!is_system_error_status(&turn_started()));
    }

    #[test]
    fn withholds_output_free_engine_error_for_retry() {
        // Cold engine: status + error arrive before any output, so both are
        // withheld (handed back to the caller) and a retry is signalled. The
        // withheld pair is exactly what the caller forwards once out of budget.
        let (forwarded, withheld) = drive(vec![
            (turn_started(), false),
            (system_error_status(), false),
            (engine_error(), true),
        ]);
        assert_eq!(forwarded, vec!["turn/started"]);
        assert_eq!(
            withheld,
            Some(vec![
                "thread/status/changed".to_string(),
                "error".to_string(),
            ])
        );
    }

    #[test]
    fn never_retries_after_output_streamed() {
        // The engine dropped mid-turn after streaming: retrying would duplicate
        // output, so the error is forwarded rather than withheld.
        let (forwarded, withheld) = drive(vec![
            (agent_delta(), false),
            (system_error_status(), false),
            (engine_error(), true),
        ]);
        assert_eq!(withheld, None);
        assert_eq!(
            forwarded,
            vec!["item/agentMessage/delta", "thread/status/changed", "error"]
        );
    }

    #[test]
    fn flushes_held_status_for_non_retriable_error() {
        // A non-warmup error must not lose the systemError status we deferred.
        let other_error = json!({
            "method": "error",
            "params": { "error": { "message": "boom" } }
        });
        let (forwarded, withheld) = drive(vec![
            (turn_started(), false),
            (system_error_status(), false),
            (other_error, true),
        ]);
        assert_eq!(withheld, None);
        assert_eq!(
            forwarded,
            vec!["turn/started", "thread/status/changed", "error"]
        );
    }
}
