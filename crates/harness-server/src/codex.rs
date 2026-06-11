use std::env;
use std::io::{self, BufRead, Write};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};

use crate::server::{BlocksCommand, parse_blocks_line, write_blocks_error};
use crate::util::write_value;
use crate::{AppServerRuntime, HarnessServerError, Result};

#[derive(Debug, Default)]
pub struct CodexHarnessServer;

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

pub(crate) fn run_codex_blocks_server() -> Result<()> {
    let mut codex = CodexJsonRpcChild::spawn()?;
    let mut stdout = io::stdout().lock();
    let mut request_id = 1_i64;
    let mut thread_id: Option<String> = None;

    let initialize_id = next_request_id(&mut request_id);
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
    )?;
    codex.read_response_or_forward(initialize_id, &mut stdout)?;

    let stdin = io::stdin();
    for raw in stdin.lock().lines() {
        let line = raw?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match parse_blocks_line(trimmed) {
            Ok(BlocksCommand::User {
                input,
                client_user_message_id,
                model,
            }) => {
                if let Err(error) = run_codex_user_turn(
                    &mut codex,
                    &mut stdout,
                    &mut request_id,
                    &mut thread_id,
                    input,
                    client_user_message_id,
                    model,
                ) {
                    let fallback_thread_id = thread_id.as_deref().unwrap_or("codex");
                    eprintln!("Codex blocks turn failed: {error:#}");
                    write_blocks_error(&mut stdout, fallback_thread_id, "turn", error.to_string())?;
                }
            }
            Ok(BlocksCommand::Interrupt) => {
                eprintln!(
                    "Codex blocks interrupt ignored: no active stdin reader while a turn runs"
                );
            }
            Err(error) => {
                eprintln!("invalid Codex blocks input: {error:#}");
                write_blocks_error(
                    &mut stdout,
                    thread_id.as_deref().unwrap_or("codex"),
                    "input",
                    error.to_string(),
                )?;
            }
        }
    }

    Ok(())
}

fn run_codex_user_turn<W: Write>(
    codex: &mut CodexJsonRpcChild,
    stdout: &mut W,
    request_id: &mut i64,
    thread_id: &mut Option<String>,
    input: Vec<UserInput>,
    client_user_message_id: Option<String>,
    model: Option<String>,
) -> Result<()> {
    if thread_id.is_none() {
        *thread_id = Some(start_or_resume_thread(codex, stdout, request_id)?);
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

    let turn_request_id = next_request_id(request_id);
    codex.send_request(turn_request_id, "turn/start", params)?;
    let result = codex.read_response_or_forward(turn_request_id, stdout)?;
    let turn_id = result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            HarnessServerError::Protocol("turn/start response missing turn.id".to_string())
        })?
        .to_string();
    codex.read_until_turn_terminal(stdout, thread_id.as_deref().unwrap_or_default(), &turn_id)
}

fn start_or_resume_thread<W: Write>(
    codex: &mut CodexJsonRpcChild,
    stdout: &mut W,
    request_id: &mut i64,
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
                "excludeTurns": false,
            }),
        )
    };

    let id = next_request_id(request_id);
    codex.send_request(id, method, params)?;
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
            let mut parent_stderr = io::stderr().lock();
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

    fn send_request(&mut self, id: i64, method: &str, params: Value) -> Result<()> {
        self.write_value(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))
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

    fn read_until_turn_terminal<W: Write>(
        &mut self,
        stdout: &mut W,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        loop {
            let value = self.read_value()?;
            if is_server_request(&value) {
                self.send_error_response(&value)?;
                continue;
            }
            if notification_method(&value).is_some() {
                let terminal = is_terminal_notification(&value, thread_id, turn_id);
                write_value(stdout, &value)?;
                if terminal {
                    break;
                }
            }
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
}

impl Drop for CodexJsonRpcChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
