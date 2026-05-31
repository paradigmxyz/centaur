use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use codex_app_server_protocol::{JSONRPCMessage, ServerNotification};
use serde_json::{Value, json};

#[test]
fn claude_app_server_streams_codex_v2_notifications() {
    let fake_claude = concat!(
        "printf '%s\\n' ",
        "'{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"claude-session\"}' ",
        "'{\"type\":\"assistant\",\"is_partial\":true,\"message\":{\"id\":\"msg_1\",\"content\":[{\"type\":\"text\",\"text\":\"hel\"}]}}' ",
        "'{\"type\":\"assistant\",\"is_partial\":true,\"message\":{\"id\":\"msg_1\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}' ",
        "'{\"type\":\"assistant\",\"is_partial\":false,\"message\":{\"id\":\"msg_1\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}' ",
        "'{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"hello\"}'"
    );

    let run = run_bridge_turn(BridgeTurnConfig {
        server_args: vec!["claude-code"],
        command_override_env: "CENTAUR_CLAUDE_APP_BRIDGE_COMMAND",
        command_override: Some(fake_claude.to_string()),
        model: None,
        prompt: "say hello".to_string(),
        timeout: Duration::from_secs(10),
    });

    assert_eq!(
        run.terminal_status.as_deref(),
        Some("completed"),
        "terminal error: {:?}; methods={:?}",
        run.terminal_error,
        run.methods
    );
    assert_eq!(run.agent_text, "hello");
    assert_codex_v2_turn(&run);
}

#[test]
fn amp_app_server_streams_codex_v2_notifications() {
    let fake_amp = concat!(
        "printf '%s\\n' ",
        "'{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"T-amp-session\"}' ",
        "'{\"type\":\"assistant\",\"is_partial\":true,\"message\":{\"id\":\"msg_1\",\"content\":[{\"type\":\"text\",\"text\":\"am\"}]}}' ",
        "'{\"type\":\"assistant\",\"is_partial\":true,\"message\":{\"id\":\"msg_1\",\"content\":[{\"type\":\"text\",\"text\":\"amp\"}]}}' ",
        "'{\"type\":\"assistant\",\"is_partial\":false,\"message\":{\"id\":\"msg_1\",\"content\":[{\"type\":\"text\",\"text\":\"amp\"}]}}' ",
        "'{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"amp\"}'"
    );

    let run = run_bridge_turn(BridgeTurnConfig {
        server_args: vec!["amp"],
        command_override_env: "CENTAUR_AMP_APP_BRIDGE_COMMAND",
        command_override: Some(fake_amp.to_string()),
        model: Some("deep".to_string()),
        prompt: "say amp".to_string(),
        timeout: Duration::from_secs(10),
    });

    assert_eq!(
        run.terminal_status.as_deref(),
        Some("completed"),
        "terminal error: {:?}; methods={:?}",
        run.terminal_error,
        run.methods
    );
    assert_eq!(run.agent_text, "amp");
    assert_codex_v2_turn(&run);
}

#[test]
#[ignore = "runs the real Claude Code CLI and may make network/auth calls"]
fn real_claude_code_app_server_emits_codex_v2_notifications() {
    let claude_bin = std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
    let version = Command::new(&claude_bin)
        .arg("--version")
        .output()
        .expect("Claude Code CLI must be installed; set CLAUDE_BIN if it is not on PATH");
    assert!(
        version.status.success(),
        "Claude Code CLI version check failed: status={}; stderr={}",
        version.status,
        String::from_utf8_lossy(&version.stderr)
    );

    let model = std::env::var("CENTAUR_REAL_CLAUDE_MODEL").unwrap_or_else(|_| "sonnet".to_string());
    let run = run_bridge_turn(BridgeTurnConfig {
        server_args: vec!["claude-code"],
        command_override_env: "CENTAUR_CLAUDE_APP_BRIDGE_COMMAND",
        command_override: None,
        model: Some(model),
        prompt: "Reply with exactly: CENTAUR_CLAUDE_APP_SERVER_OK".to_string(),
        timeout: Duration::from_secs(180),
    });
    eprintln!("Claude Code output: {}", run.agent_text.trim());

    assert_eq!(
        run.terminal_status.as_deref(),
        Some("completed"),
        "terminal error: {:?}; methods={:?}",
        run.terminal_error,
        run.methods
    );
    assert!(
        !run.agent_text.trim().is_empty(),
        "real Claude Code turn completed without agent text; methods={:?}",
        run.methods
    );
    assert_codex_v2_turn(&run);
}

#[derive(Debug)]
struct BridgeTurnConfig {
    server_args: Vec<&'static str>,
    command_override_env: &'static str,
    command_override: Option<String>,
    model: Option<String>,
    prompt: String,
    timeout: Duration,
}

#[derive(Debug)]
struct BridgeTurnRun {
    thread_id: String,
    methods: Vec<String>,
    agent_text: String,
    terminal_status: Option<String>,
    terminal_error: Option<String>,
}

fn run_bridge_turn(config: BridgeTurnConfig) -> BridgeTurnRun {
    let bin = env!("CARGO_BIN_EXE_harness-server");
    let mut command = Command::new(bin);
    command
        .args(&config.server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(raw) = config.command_override {
        command.env(config.command_override_env, raw);
    } else {
        command.env_remove(config.command_override_env);
    }

    let mut child = command.spawn().expect("spawn harness-server");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    let (line_tx, line_rx) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        for raw in BufReader::new(stdout).lines() {
            match raw {
                Ok(line) => {
                    if line_tx.send(line).is_err() {
                        break;
                    }
                }
                Err(error) => panic!("read bridge stdout: {error}"),
            }
        }
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    send(
        &mut stdin,
        json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "test", "title": null, "version": "0"},
            },
        }),
    );

    let thread_start_params = match &config.model {
        Some(model) => json!({"model": model}),
        None => json!({}),
    };
    send(
        &mut stdin,
        json!({"id": 2, "method": "thread/start", "params": thread_start_params}),
    );

    let deadline = Instant::now() + config.timeout;
    let mut methods = Vec::new();
    let mut thread_id = None;
    let mut turn_started = false;
    let mut agent_text = String::new();
    let mut terminal_status = None;
    let mut terminal_error = None;
    let mut received = Vec::new();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait_for = remaining.min(Duration::from_secs(1));
        let line = match line_rx.recv_timeout(wait_for) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        eprintln!("Codex App Server stdout JSON: {line}");
        let value: Value = serde_json::from_str(line.trim()).expect("valid JSON-RPC line");
        received.push(value.clone());
        let message: JSONRPCMessage =
            serde_json::from_value(value.clone()).expect("valid JSON-RPC message");

        match message {
            JSONRPCMessage::Notification(notification) => {
                let method = notification.method.clone();
                ServerNotification::try_from(notification).unwrap_or_else(|error| {
                    panic!(
                        "notification is not a typed Codex App Server V2 notification: {error}; value={value}"
                    )
                });
                if let Some(expected_thread_id) = thread_id.as_deref() {
                    assert_notification_thread_id(&value, expected_thread_id);
                }
                if method == "item/agentMessage/delta" {
                    if let Some(delta) = value
                        .get("params")
                        .and_then(|params| params.get("delta"))
                        .and_then(Value::as_str)
                    {
                        agent_text.push_str(delta);
                    }
                }
                if method == "turn/completed" {
                    terminal_status = value
                        .pointer("/params/turn/status")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    terminal_error = value
                        .pointer("/params/turn/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    methods.push(method);
                    break;
                }
                methods.push(method);
            }
            JSONRPCMessage::Response(_) => {
                if value.get("id").and_then(Value::as_i64) == Some(2) && !turn_started {
                    let id = value["result"]["thread"]["id"]
                        .as_str()
                        .expect("thread id")
                        .to_string();
                    thread_id = Some(id.clone());
                    send(
                        &mut stdin,
                        json!({
                            "id": 3,
                            "method": "turn/start",
                            "params": {
                                "threadId": id,
                                "input": [{"type": "text", "text": config.prompt}],
                            },
                        }),
                    );
                    turn_started = true;
                }
            }
            JSONRPCMessage::Error(error) => {
                panic!("bridge returned JSON-RPC error: {error:?}; received={received:?}");
            }
            JSONRPCMessage::Request(request) => {
                panic!("bridge emitted unexpected request: {request:?}; received={received:?}");
            }
        }
    }

    if terminal_status.is_none() {
        let _ = child.kill();
    }
    drop(stdin);
    let status = child.wait().expect("wait child");
    stdout_reader.join().expect("stdout reader");
    let stderr = stderr_reader.join().expect("stderr reader");

    assert!(
        status.success(),
        "bridge failed: {status}; stderr={stderr}; received={received:?}"
    );
    let thread_id = thread_id.unwrap_or_else(|| {
        panic!("thread/start did not return a thread id; stderr={stderr}; received={received:?}")
    });
    assert!(
        terminal_status.is_some(),
        "turn did not complete before timeout; stderr={stderr}; received={received:?}"
    );

    BridgeTurnRun {
        thread_id,
        methods,
        agent_text,
        terminal_status,
        terminal_error,
    }
}

fn assert_codex_v2_turn(run: &BridgeTurnRun) {
    assert!(
        !run.thread_id.is_empty(),
        "thread/start did not return a thread id"
    );
    assert!(
        run.methods.contains(&"item/agentMessage/delta".to_string()),
        "missing text delta; got {:?}",
        run.methods
    );
    assert!(
        run.methods.contains(&"item/completed".to_string()),
        "missing item/completed; got {:?}",
        run.methods
    );
    assert!(
        run.methods.contains(&"turn/completed".to_string()),
        "missing turn/completed; got {:?}",
        run.methods
    );
}

fn assert_notification_thread_id(value: &Value, expected_thread_id: &str) {
    if let Some(actual) = value.pointer("/params/threadId").and_then(Value::as_str) {
        assert_eq!(actual, expected_thread_id, "notification threadId mismatch");
    }
    if value.get("method").and_then(Value::as_str) == Some("thread/started")
        && let Some(actual) = value.pointer("/params/thread/id").and_then(Value::as_str)
    {
        assert_eq!(
            actual, expected_thread_id,
            "thread/started thread.id mismatch"
        );
    }
}

fn send(stdin: &mut impl Write, value: Value) {
    serde_json::to_writer(&mut *stdin, &value).expect("write JSON");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush");
}
