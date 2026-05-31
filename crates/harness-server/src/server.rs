use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use codex_app_server_protocol::{
    ApprovalsReviewer, AskForApproval, ClientResponse, InitializeResponse, JSONRPCError,
    JSONRPCErrorError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse, RequestId, SandboxPolicy,
    ServerNotification, ThreadResumeParams, ThreadResumeResponse, ThreadStartParams,
    ThreadStartResponse, TurnInterruptParams, TurnInterruptResponse, TurnStartParams,
    TurnStartResponse, TurnStatus, TurnSteerParams, TurnSteerResponse, UserInput,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::amp::AmpHarness;
use crate::claude::ClaudeCodeHarness;
use crate::codex::CodexHarnessServer;
use crate::traits::{
    AppServerNormalizer, AppServerRuntime, HarnessChild, HarnessKind, HarnessServer,
    NormalizedEvent, ThreadState,
};
use crate::turn::{BridgeConfig, CodexTurnNormalizer};
use crate::util::{absolute_path, default_codex_home, write_value};
use crate::wire::notification_to_wire_value;
use crate::{HarnessServerError, Result};

pub fn server_for(kind: HarnessKind) -> Box<dyn AppServerRuntime> {
    match kind {
        HarnessKind::Codex => Box::new(CodexHarnessServer),
        HarnessKind::ClaudeCode => Box::new(AppServerNormalizer::new(ClaudeCodeHarness)),
        HarnessKind::Amp => Box::new(AppServerNormalizer::new(AmpHarness)),
    }
}

pub fn run_harness_server(kind: HarnessKind) -> Result<()> {
    server_for(kind).run_stdio()
}

pub fn run_validate_jsonrpc() -> Result<()> {
    let stdin = io::stdin();
    for raw in stdin.lock().lines() {
        let line = raw?;
        if line.trim().is_empty() {
            continue;
        }
        let message: JSONRPCMessage = serde_json::from_str(&line)?;
        if let JSONRPCMessage::Notification(notification) = message {
            let _typed = codex_app_server_protocol::ServerNotification::try_from(notification)
                .map_err(|error| HarnessServerError::InvalidServerNotification {
                    message: error.to_string(),
                })?;
        }
    }
    Ok(())
}

pub(crate) fn run_app_server<H: HarnessServer>(harness: &H) -> Result<()> {
    let (request_tx, request_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for raw in stdin.lock().lines() {
            let Ok(line) = raw else {
                break;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let message = match serde_json::from_str::<JSONRPCMessage>(trimmed) {
                Ok(message) => message,
                Err(error) => {
                    eprintln!("invalid JSON-RPC message: {error}");
                    continue;
                }
            };
            let JSONRPCMessage::Request(request) = message else {
                continue;
            };
            if request_tx.send(request).is_err() {
                break;
            }
        }
    });

    let mut stdout = io::stdout().lock();
    let mut threads: HashMap<String, ThreadState> = HashMap::new();

    while let Ok(request) = request_rx.recv() {
        if let Err(error) = handle_request(harness, request, &request_rx, &mut threads, &mut stdout)
        {
            eprintln!("request failed: {error:#}");
        }
    }

    Ok(())
}

fn handle_request<H: HarnessServer, W: Write>(
    harness: &H,
    request: JSONRPCRequest,
    request_rx: &Receiver<JSONRPCRequest>,
    threads: &mut HashMap<String, ThreadState>,
    stdout: &mut W,
) -> Result<()> {
    match request.method.as_str() {
        "initialize" => {
            let response = InitializeResponse {
                user_agent: "harness-server".to_string(),
                codex_home: absolute_path(
                    env::var_os("CODEX_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(default_codex_home),
                )?,
                platform_family: env::consts::FAMILY.to_string(),
                platform_os: env::consts::OS.to_string(),
            };
            write_client_response(
                stdout,
                ClientResponse::Initialize {
                    request_id: request.id,
                    response,
                },
            )
        }
        "thread/start" => {
            let params: ThreadStartParams = request_params(request.params)?;
            let cwd = params
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or(env::current_dir()?);
            let cwd = if cwd.is_absolute() {
                cwd
            } else {
                env::current_dir()?.join(cwd)
            };
            let state = harness.thread_state(&params, cwd.clone());
            let thread_id = state.id.clone();
            let normalizer = normalizer_for(harness, &state, "turn-placeholder");
            let response = ThreadStartResponse {
                thread: normalizer.thread_snapshot()?,
                model: state.model.clone(),
                model_provider: state.model_provider.clone(),
                service_tier: state.service_tier.clone(),
                cwd: absolute_path(cwd)?,
                runtime_workspace_roots: Vec::new(),
                instruction_sources: Vec::new(),
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox: SandboxPolicy::DangerFullAccess,
                active_permission_profile: None,
                reasoning_effort: None,
            };
            threads.insert(thread_id, state);
            write_client_response(
                stdout,
                ClientResponse::ThreadStart {
                    request_id: request.id,
                    response,
                },
            )
        }
        "thread/resume" => {
            let params: ThreadResumeParams = request_params(request.params)?;
            let thread_id = params.thread_id.clone();
            if !threads.contains_key(&thread_id) {
                threads.insert(thread_id.clone(), resumed_thread_state(harness, &params)?);
            }
            let state = threads
                .get_mut(&thread_id)
                .expect("thread state inserted or existed");
            apply_resume_overrides(state, &params)?;
            let normalizer = normalizer_for(harness, &state, "turn-placeholder");
            let mut thread = normalizer.thread_snapshot()?;
            if !params.exclude_turns {
                thread.turns = state.completed_turns.clone();
            }
            let response = ThreadResumeResponse {
                thread,
                model: state.model.clone(),
                model_provider: state.model_provider.clone(),
                service_tier: state.service_tier.clone(),
                cwd: absolute_path(state.cwd.clone())?,
                runtime_workspace_roots: Vec::new(),
                instruction_sources: Vec::new(),
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox: SandboxPolicy::DangerFullAccess,
                active_permission_profile: None,
                reasoning_effort: None,
                initial_turns_page: None,
            };
            write_client_response(
                stdout,
                ClientResponse::ThreadResume {
                    request_id: request.id,
                    response,
                },
            )
        }
        "turn/start" => {
            let params: TurnStartParams = request_params(request.params)?;
            let state = threads.get_mut(&params.thread_id).ok_or_else(|| {
                HarnessServerError::UnknownThread {
                    thread_id: params.thread_id.clone(),
                }
            })?;
            let turn_id = format!("turn-{}", Uuid::new_v4().simple());
            let mut normalizer = normalizer_for(harness, state, &turn_id);
            let response = TurnStartResponse {
                turn: normalizer.turn_snapshot(TurnStatus::InProgress),
            };
            write_client_response(
                stdout,
                ClientResponse::TurnStart {
                    request_id: request.id,
                    response,
                },
            )?;
            for notification in normalizer.start_notifications(!state.thread_started_sent)? {
                if matches!(notification, ServerNotification::ThreadStarted(_)) {
                    state.thread_started_sent = true;
                }
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            for notification in normalizer
                .emit_user_message(params.client_user_message_id.clone(), params.input.clone())?
            {
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            let outcome = run_harness_turn(
                harness,
                state,
                &params.input,
                &mut normalizer,
                stdout,
                request_rx,
            );
            match outcome {
                Ok(Some(turn)) => state.completed_turns.push(turn),
                Ok(None) => {}
                Err(error) => {
                    let message = error.to_string();
                    let normalized = NormalizedEvent::Error {
                        message: message.clone(),
                    };
                    for notification in normalizer.process_event(&normalized)? {
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                    if let Some(notification) = normalizer.finish_turn(Some(message))? {
                        if let ServerNotification::TurnCompleted(completed) = &notification {
                            state.completed_turns.push(completed.turn.clone());
                        }
                        write_value(stdout, &notification_to_wire_value(&notification)?)?;
                    }
                }
            }
            Ok(())
        }
        "turn/interrupt" => {
            let _params: TurnInterruptParams = request_params(request.params)?;
            write_client_response(
                stdout,
                ClientResponse::TurnInterrupt {
                    request_id: request.id,
                    response: TurnInterruptResponse {},
                },
            )
        }
        "turn/steer" => write_error(
            stdout,
            request.id,
            -32600,
            "no active turn to steer".to_string(),
        ),
        _ => write_error(
            stdout,
            request.id,
            -32601,
            format!("method not found: {}", request.method),
        ),
    }
}

fn normalizer_for<H: HarnessServer>(
    harness: &H,
    state: &ThreadState,
    turn_id: &str,
) -> CodexTurnNormalizer {
    let mut config = BridgeConfig::new(state.id.clone(), turn_id.to_string());
    config.cwd = state.cwd.clone();
    config.cli_version = harness.cli_version().to_string();
    config.model_provider = state.model_provider.clone();
    CodexTurnNormalizer::new(config)
}

fn resumed_thread_state<H: HarnessServer>(
    harness: &H,
    params: &ThreadResumeParams,
) -> Result<ThreadState> {
    let cwd = params
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    let cwd = if cwd.is_absolute() {
        cwd
    } else {
        env::current_dir()?.join(cwd)
    };
    Ok(ThreadState {
        id: params.thread_id.clone(),
        cwd,
        model: params
            .model
            .clone()
            .unwrap_or_else(|| harness.default_model()),
        model_provider: params
            .model_provider
            .clone()
            .unwrap_or_else(|| harness.default_model_provider().to_string()),
        service_tier: params.service_tier.clone().flatten(),
        harness_session_id: Some(params.thread_id.clone()),
        completed_turns: Vec::new(),
        process: None,
        thread_started_sent: false,
    })
}

fn apply_resume_overrides(state: &mut ThreadState, params: &ThreadResumeParams) -> Result<()> {
    if let Some(model) = &params.model {
        state.model = model.clone();
    }
    if let Some(model_provider) = &params.model_provider {
        state.model_provider = model_provider.clone();
    }
    if let Some(service_tier) = &params.service_tier {
        state.service_tier.clone_from(service_tier);
    }
    if let Some(cwd) = &params.cwd {
        let cwd = PathBuf::from(cwd);
        state.cwd = if cwd.is_absolute() {
            cwd
        } else {
            env::current_dir()?.join(cwd)
        };
    }
    Ok(())
}

fn handle_active_turn_request<H: HarnessServer, W: Write>(
    harness: &H,
    process: &mut HarnessChild,
    normalizer: &mut CodexTurnNormalizer,
    request: JSONRPCRequest,
    stdout: &mut W,
) -> Result<()> {
    match request.method.as_str() {
        "turn/steer" => {
            let params: TurnSteerParams = request_params(request.params)?;
            if params.thread_id != normalizer.thread_id() {
                write_error(
                    stdout,
                    request.id,
                    -32600,
                    format!("unknown threadId {}", params.thread_id),
                )?;
                return Ok(());
            }
            if params.expected_turn_id != normalizer.turn_id() {
                write_error(
                    stdout,
                    request.id,
                    -32600,
                    format!(
                        "expected active turn id `{}` but found `{}`",
                        params.expected_turn_id,
                        normalizer.turn_id()
                    ),
                )?;
                return Ok(());
            }
            process
                .stdin
                .write_all(&harness.stdin_for_steer(&params.input)?)?;
            process.stdin.flush()?;
            write_client_response(
                stdout,
                ClientResponse::TurnSteer {
                    request_id: request.id,
                    response: TurnSteerResponse {
                        turn_id: normalizer.turn_id().to_string(),
                    },
                },
            )?;
            for notification in normalizer
                .emit_user_message(params.client_user_message_id.clone(), params.input.clone())?
            {
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            Ok(())
        }
        "turn/interrupt" => {
            write_client_response(
                stdout,
                ClientResponse::TurnInterrupt {
                    request_id: request.id,
                    response: TurnInterruptResponse {},
                },
            )?;
            Ok(())
        }
        _ => {
            write_error(
                stdout,
                request.id,
                -32600,
                format!("cannot handle {} while a turn is active", request.method),
            )?;
            Ok(())
        }
    }
}

fn run_harness_turn<H: HarnessServer, W: Write>(
    harness: &H,
    state: &mut ThreadState,
    input: &[UserInput],
    normalizer: &mut CodexTurnNormalizer,
    stdout: &mut W,
    request_rx: &Receiver<JSONRPCRequest>,
) -> Result<Option<codex_app_server_protocol::Turn>> {
    ensure_harness_process(harness, state)?;
    let process = state
        .process
        .as_mut()
        .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
    process.stdin.write_all(&harness.stdin_for_turn(input)?)?;
    process.stdin.flush()?;

    let mut last_session_id = state.harness_session_id.clone();
    let mut event_normalizer = H::EventNormalizer::default();
    let mut completed_turn = None;
    loop {
        while let Ok(request) = request_rx.try_recv() {
            handle_active_turn_request(harness, process, normalizer, request, stdout)?;
        }

        let line = match process.stdout.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line?,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let status = process.child.wait()?;
                return Err(HarnessServerError::HarnessExited {
                    kind: harness.kind(),
                    status,
                    stderr: String::new(),
                });
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event = harness.parse_stdout_line(trimmed)?;
        let normalized_events = harness.normalize_events(&mut event_normalizer, event)?;
        let mut terminal = false;
        for normalized in normalized_events {
            if let Some(session_id) = normalized.session_id() {
                last_session_id = Some(session_id.to_string());
                state.harness_session_id = Some(session_id.to_string());
            }
            for notification in normalizer.process_event(&normalized)? {
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            terminal |= normalized.is_terminal()
                || (harness.finish_turn_on_assistant_end_turn()
                    && normalized.is_assistant_end_turn());
        }
        if terminal {
            if let Some(notification) = normalizer.finish_turn(None)? {
                if let ServerNotification::TurnCompleted(completed) = &notification {
                    completed_turn = Some(completed.turn.clone());
                }
                write_value(stdout, &notification_to_wire_value(&notification)?)?;
            }
            break;
        }
    }

    if let Some(session_id) = last_session_id {
        state.harness_session_id = Some(session_id);
    }
    if let Some(notification) = normalizer.finish_turn(None)? {
        if let ServerNotification::TurnCompleted(completed) = &notification {
            completed_turn = Some(completed.turn.clone());
        }
        write_value(stdout, &notification_to_wire_value(&notification)?)?;
    }
    Ok(completed_turn)
}

fn ensure_harness_process<H: HarnessServer>(harness: &H, state: &mut ThreadState) -> Result<()> {
    if state.process.is_some() {
        return Ok(());
    }

    let mut command = harness.command_for_turn(state);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&state.cwd)
        .spawn()
        .map_err(|source| HarnessServerError::SpawnHarness {
            cwd: state.cwd.clone(),
            source,
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(HarnessServerError::HarnessStdoutUnavailable)?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(HarnessServerError::HarnessStderrUnavailable)?;
    std::thread::spawn(move || {
        let mut parent_stderr = io::stderr().lock();
        let _ = io::copy(&mut stderr, &mut parent_stderr);
    });
    let (stdout_tx, stdout_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = io::BufReader::new(stdout);
        for raw in reader.lines() {
            let should_stop = raw.is_err();
            if stdout_tx.send(raw).is_err() || should_stop {
                break;
            }
        }
    });

    state.process = Some(HarnessChild {
        child,
        stdin,
        stdout: stdout_rx,
    });
    Ok(())
}

fn request_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T> {
    serde_json::from_value(params.unwrap_or_else(|| json!({})))
        .map_err(|source| HarnessServerError::InvalidParams { source })
}

fn write_client_response<W: Write>(stdout: &mut W, response: ClientResponse) -> Result<()> {
    let (id, result) = response.into_jsonrpc_parts()?;
    write_value(
        stdout,
        &serde_json::to_value(JSONRPCMessage::Response(JSONRPCResponse { id, result }))?,
    )
}

fn write_error<W: Write>(stdout: &mut W, id: RequestId, code: i64, message: String) -> Result<()> {
    write_value(
        stdout,
        &serde_json::to_value(JSONRPCMessage::Error(JSONRPCError {
            id,
            error: JSONRPCErrorError {
                code,
                message,
                data: None,
            },
        }))?,
    )
}
