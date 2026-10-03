use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

const KEY: &str = "slack:C123:123.456";
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/omp");

/// Answers startup before replaying recordings from OMP 18.5.0 against a
/// loopback provider. The background gate pauses before session_settled.
fn fake_omp(dir: &std::path::Path) -> PathBuf {
    let script = r##"#!/bin/sh
printf '%s|%s\n' "$$" "$*" >> '@DIR@/argv'
while IFS= read -r line; do
    printf '%s\n' "$line" >> '@DIR@/stdin'
    kind=${line##*\"type\":\"}
    kind=${kind%%\"*}
    if [ "$OMP_TEST_FAULT" = "$kind" ]; then
        printf '{"type":"response","id":"centaur-%s","command":"%s","success":false,"error":"startup rejected"}\n' "$kind" "$kind"
        continue
    fi
    case "$kind" in
    negotiate_protocol)
        case "$OMP_TEST_FAULT" in
        eof) exit 0 ;;
        v1) printf '%s\n' '{"id":"centaur-negotiate_protocol","type":"response","command":"negotiate_protocol","success":true,"data":{"protocolVersion":1}}' ;;
        *)
            printf '%s\n' '{"type":"message_end","messageId":"early","message":{"role":"assistant","content":[{"type":"text","text":"not part of the turn"}]}}' '{"id":"unrelated","type":"response","command":"negotiate_protocol","success":true}'
            printf '%s\n' '{"id":"centaur-negotiate_protocol","type":"response","command":"negotiate_protocol","success":true,"data":{"protocolVersion":2}}' ;;
        esac ;;
    open_session)
        if [ "$OMP_TEST_FAULT" = cancelled ]; then cancelled=true; else cancelled=false; fi
        printf '{"id":"centaur-open_session","type":"response","command":"open_session","success":true,"data":{"cancelled":%s}}\n' "$cancelled" ;;
    set_model)
        case "$line" in *'"modelId":"missing"'*) success=false ;; *) success=true ;; esac
        printf '{"id":"centaur-set_model","type":"response","command":"set_model","success":%s,"error":"model not found"}\n' "$success" ;;
    prompt)
        printf '%s\n' '{"type":"response","command":"prompt","success":true}'
        case "$line" in
        *'"message":"slow"'*) printf '%s\n' '{"type":"tool_execution_start","toolCallId":"slow","toolName":"bash","args":{"command":"sleep 60"}}' ;;
        *'"message":"gated_background"'*)
            while IFS= read -r frame; do
                printf '%s\n' "$frame"
                case "$frame" in *'"sessionSettled":false'*)
                    : > '@DIR@/paused'
                    while [ ! -f '@DIR@/release' ]; do sleep 0.01; done ;;
                esac
            done < '@FIXTURES@/background.jsonl' ;;
        *'"message":"thinking_tool"'*) cat '@FIXTURES@/thinking_tool.jsonl' ;;
        *'"message":"tool"'*) cat '@FIXTURES@/tool.jsonl' ;;
        *'"message":"provider_error"'*) cat '@FIXTURES@/provider_error.jsonl' ;;
        *'"message":"truncated_stream"'*) cat '@FIXTURES@/truncated_stream.jsonl' ;;
        *'"message":"custom"'*) cat '@DIR@/turn.jsonl' ;;
        *) cat '@FIXTURES@/text.jsonl' ;;
        esac ;;
    *) printf '{"type":"response","id":"centaur-%s","command":"%s","success":true}\n' "$kind" "$kind" ;;
    esac
done
"##.replace("@DIR@", &dir.to_string_lossy()).replace("@FIXTURES@", FIXTURES);
    let path = dir.join("omp");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Server {
    dir: PathBuf,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<Value>,
    timeout: Duration,
}

impl Server {
    fn new(extra_env: &[(&str, &str)]) -> Self {
        Self::mode(extra_env, "blocks")
    }

    fn mode(extra_env: &[(&str, &str)], mode: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omp-stdio-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = fake_omp(&dir);
        Self::with_binary(dir, mode, binary.as_os_str(), extra_env)
    }

    fn with_binary(dir: PathBuf, mode: &str, binary: &OsStr, extra_env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_harness-server"))
            .args(["omp", "--mode", mode])
            .current_dir(&dir)
            .env("CENTAUR_OMP_BIN", binary)
            .env("CENTAUR_OMP_SESSION_ROOT", dir.join("sessions"))
            .env_remove("CENTAUR_OMP_MODEL")
            .env_remove("CENTAUR_THREAD_KEY")
            .env_remove("OMP_TEST_FAULT")
            .envs(extra_env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            dir,
            child,
            stdin,
            stdout: rx,
            timeout: Duration::from_secs(10),
        }
    }

    fn send(&mut self, value: Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{value}").unwrap();
    }

    fn user(&mut self, message: &str, extra: Value) {
        let mut value = json!({"type": "user", "thread_key": KEY, "text": message});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.send(value);
    }

    fn until(&self, predicate: impl Fn(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + self.timeout;
        let mut values = Vec::new();
        loop {
            let value = self
                .stdout
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let done = predicate(&value);
            values.push(value);
            if done {
                return values;
            }
        }
    }

    fn read_until(&self, method: &str) -> Vec<Value> {
        self.until(|value| value["method"] == method)
    }

    fn turn(&mut self, message: &str, extra: Value) -> Vec<Value> {
        self.user(message, extra);
        self.read_until("turn/completed")
    }

    fn commands(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("stdin"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn custom(&mut self, frames: &[Value]) -> Vec<Value> {
        let content = frames
            .iter()
            .map(|frame| format!("{frame}\n"))
            .collect::<String>();
        std::fs::write(self.dir.join("turn.jsonl"), content).unwrap();
        self.turn("custom", json!({}))
    }

    fn paused(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.dir.join("paused").exists() {
            assert!(Instant::now() < deadline, "fake never paused");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn release(&self) {
        std::fs::write(self.dir.join("release"), "").unwrap();
    }

    fn close(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::write(self.dir.join("release"), "");
        self.close();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn items<'a>(turn: &'a [Value], method: &str, kind: &str) -> Vec<&'a Value> {
    turn.iter()
        .filter(|event| event["method"] == method)
        .map(|event| &event["params"]["item"])
        .filter(|item| item["type"] == kind)
        .collect()
}

fn status(turn: &[Value]) -> &Value {
    &turn.last().unwrap()["params"]["turn"]["status"]
}
fn result() -> Value {
    json!({"type": "prompt_result", "status": "completed", "sessionSettled": true})
}

#[test]
fn startup_renders_final_text_and_reasoning_restores_default() {
    let mut server = Server::new(&[("CENTAUR_OMP_MODEL", "custom/new-model")]);
    let turn = server.turn("text", json!({"reasoning": "none"}));
    assert_eq!(status(&turn), "completed");
    let messages = items(&turn, "item/completed", "agentMessage");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "Hello from the loopback mock.");
    assert_eq!(messages[0]["phase"], "final_answer");
    let first = server.commands();
    assert_eq!(
        first
            .iter()
            .map(|c| c["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "negotiate_protocol",
            "set_event_filter",
            "open_session",
            "set_cache_warming",
            "set_model",
            "set_thinking_level",
            "set_thinking_level",
            "prompt",
        ]
    );
    assert_eq!(
        first[2]["sessionDir"],
        server
            .dir
            .join("sessions/centaur-slack-C123-123.456")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(first[5]["level"], "high");
    assert_eq!(first[6]["level"], "off");
    assert_eq!(status(&server.turn("text", json!({}))), "completed");
    assert_eq!(
        &server.commands()[first.len()..],
        [
            json!({"type": "set_thinking_level", "level": "high"}),
            json!({"type": "prompt", "message": "text"})
        ]
    );
    assert_eq!(
        std::fs::read_to_string(server.dir.join("argv"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn recorded_tools_and_thinking_project_to_distinct_complete_items() {
    let mut server = Server::new(&[]);
    for scenario in ["tool", "thinking_tool"] {
        let turn = server.turn(scenario, json!({}));
        assert_eq!(status(&turn), "completed", "{turn:#?}");
        let commands = items(&turn, "item/completed", "commandExecution");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["command"], "echo e2e-ok");
        assert!(
            commands[0]["aggregatedOutput"]
                .as_str()
                .unwrap()
                .starts_with("e2e-ok\n")
        );
        assert_eq!(commands[0]["exitCode"], 0);
        assert_eq!(commands[0]["status"], "completed");
        if scenario == "thinking_tool" {
            let reasoning = items(&turn, "item/completed", "reasoning");
            assert_eq!(reasoning.len(), 2);
            assert_ne!(reasoning[0]["id"], reasoning[1]["id"]);
            assert_eq!(
                reasoning[0]["content"],
                json!(["The user wants bash. Run echo e2e-ok first."])
            );
            assert_eq!(
                reasoning[1]["content"],
                json!(["The command printed e2e-ok."])
            );
            for item in &reasoning {
                let streamed: String = turn
                    .iter()
                    .filter(|event| {
                        event["method"] == "item/reasoning/textDelta"
                            && event["params"]["itemId"] == item["id"]
                    })
                    .map(|event| event["params"]["delta"].as_str().unwrap())
                    .collect();
                assert_eq!(item["content"], json!([streamed]));
            }
            let message_id = reasoning[1]["id"]
                .as_str()
                .unwrap()
                .rsplit_once('-')
                .unwrap()
                .0;
            let thinking_delta = turn
                .iter()
                .position(|event| {
                    event["method"] == "item/reasoning/textDelta"
                        && event["params"]["itemId"] == reasoning[1]["id"]
                })
                .unwrap();
            let text_started = turn
                .iter()
                .position(|event| {
                    event["method"] == "item/started"
                        && event["params"]["item"]["type"] == "agentMessage"
                        && event["params"]["item"]["id"]
                            .as_str()
                            .unwrap()
                            .rsplit_once('-')
                            .unwrap()
                            .0
                            == message_id
                })
                .unwrap();
            assert!(
                thinking_delta < text_started,
                "thinking must precede text from the same message"
            );
        }
    }
}

#[test]
fn failed_bash_keeps_native_exit_code() {
    let mut server = Server::new(&[]);
    let turn = server.custom(&[
        json!({"type": "tool_execution_start", "toolCallId": "bash", "toolName": "bash", "args": {"command": "exit 27"}}),
        json!({"type": "tool_execution_end", "toolCallId": "bash", "isError": true, "result": {"content": [{"type": "text", "text": "Command exited with code 27"}], "details": {"exitCode": 27}}}), result(),
    ]);
    let commands = items(&turn, "item/completed", "commandExecution");
    assert_eq!(commands[0]["status"], "failed");
    assert_eq!(commands[0]["exitCode"], 27);
}

#[test]
fn provider_and_truncated_stream_errors_do_not_render_failed_attempts() {
    let mut server = Server::new(&[]);
    for (scenario, error) in [
        ("provider_error", "Mock provider rejected the request"),
        ("truncated_stream", "stream ended before message_stop"),
    ] {
        let turn = server.turn(scenario, json!({}));
        assert_eq!(status(&turn), "failed");
        assert!(
            turn.last().unwrap()["params"]["turn"]["error"]["message"]
                .as_str()
                .unwrap()
                .contains(error)
        );
        assert!(items(&turn, "item/completed", "agentMessage").is_empty());
    }
}

#[test]
fn prompt_rejection_retains_child_and_unknown_result_fails() {
    let mut server = Server::new(&[]);
    for command in ["prompt", "parse"] {
        let turn = server.custom(&[
            json!({"type": "response", "command": command, "success": false, "error": "rejected"}),
        ]);
        assert_eq!(status(&turn), "failed");
        assert_eq!(
            turn.last().unwrap()["params"]["turn"]["error"]["message"],
            format!("omp {command}: rejected")
        );
        assert_eq!(status(&server.turn("text", json!({}))), "completed");
    }
    let turn = server
        .custom(&[json!({"type": "prompt_result", "status": "unknown", "sessionSettled": true})]);
    assert_eq!(status(&turn), "failed");
    assert_eq!(
        std::fs::read_to_string(server.dir.join("argv"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn unsettled_background_output_stays_in_the_original_turn() {
    let mut server = Server::new(&[]);
    server.user("gated_background", json!({}));
    let mut turn = server.until(|event| {
        event["method"] == "item/completed" && event["params"]["item"]["type"] == "agentMessage"
    });
    server.paused();
    assert!(
        server
            .stdout
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    server.release();
    turn.extend(server.read_until("turn/completed"));
    assert_eq!(status(&turn), "completed");
    let messages = items(&turn, "item/completed", "agentMessage");
    assert_eq!(
        messages.iter().map(|m| &m["text"]).collect::<Vec<_>>(),
        [
            &json!("The command is running in the background."),
            &json!("The background command finished: e2e-background-ok.")
        ]
    );
    let id = &turn.last().unwrap()["params"]["turn"]["id"];
    for event in turn.iter().filter(|e| e["params"]["turnId"].is_string()) {
        assert_eq!(&event["params"]["turnId"], id);
    }
}

#[test]
fn startup_failures_never_send_a_prompt_and_next_turn_retries() {
    for fault in [
        "negotiate_protocol",
        "set_event_filter",
        "open_session",
        "set_cache_warming",
        "set_model",
        "set_thinking_level",
        "v1",
        "eof",
        "cancelled",
    ] {
        let mut server = Server::new(&[
            ("OMP_TEST_FAULT", fault),
            ("CENTAUR_OMP_MODEL", "custom/new-model"),
        ]);
        for _ in 0..2 {
            assert_eq!(status(&server.turn("text", json!({}))), "failed", "{fault}");
        }
        assert!(
            !server.commands().iter().any(|c| c["type"] == "prompt"),
            "{fault}"
        );
        assert_eq!(
            std::fs::read_to_string(server.dir.join("argv"))
                .unwrap()
                .lines()
                .count(),
            2,
            "{fault}"
        );
    }
}

#[test]
fn interrupt_and_model_switch_reopen_the_same_session() {
    let mut server = Server::new(&[]);
    server.user("slow", json!({}));
    server.until(|event| {
        event["method"] == "item/started" && event["params"]["item"]["type"] == "commandExecution"
    });
    server.send(json!({"type": "interrupt", "thread_key": KEY}));
    assert_eq!(status(&server.read_until("turn/completed")), "interrupted");
    assert_eq!(status(&server.turn("text", json!({}))), "completed");
    assert_eq!(
        status(&server.turn("text", json!({"model": "custom/switched"}))),
        "completed"
    );
    let commands = server.commands();
    let opened: Vec<_> = commands
        .iter()
        .filter(|c| c["type"] == "open_session")
        .collect();
    assert_eq!(opened.len(), 3);
    for open in opened {
        assert_eq!(
            open["sessionDir"],
            server
                .dir
                .join("sessions/centaur-slack-C123-123.456")
                .to_string_lossy()
                .as_ref()
        );
    }
}

#[test]
fn unavailable_model_restores_the_last_working_model() {
    let mut server = Server::new(&[("CENTAUR_OMP_MODEL", "custom/default")]);
    assert_eq!(
        status(&server.turn("text", json!({"model": "custom/working"}))),
        "completed"
    );
    let turn = server.turn("text", json!({"model": "custom/missing"}));
    assert_eq!(status(&turn), "failed");
    assert!(
        turn.last().unwrap()["params"]["turn"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported model `custom/missing` for OMP: model not found")
    );
    assert_eq!(status(&server.turn("text", json!({}))), "completed");
    let commands = server.commands();
    assert_eq!(
        commands
            .iter()
            .filter(|c| c["type"] == "set_model")
            .map(|c| c["modelId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["working", "missing", "working"]
    );
}

#[test]
fn jsonrpc_preserves_image_paths_when_attachments_cannot_be_inlined() {
    let mut server = Server::mode(&[], "jsonrpc");
    server.send(json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "omp-test", "version": "1"}}}));
    server.until(|v| v["id"] == 1);
    server.send(json!({"id": 2, "method": "thread/start", "params": {"cwd": server.dir}}));
    let response = server.until(|v| v["id"] == 2);
    let thread = response.last().unwrap()["result"]["thread"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let missing = server.dir.join("missing.png");
    let unsupported = server.dir.join("image.svg");
    let directory = server.dir.join("directory.png");
    std::fs::write(&unsupported, "<svg/>").unwrap();
    std::fs::create_dir(&directory).unwrap();
    server.send(json!({"id": 3, "method": "turn/start", "params": {"threadId": thread, "input": [
        {"type": "text", "text": "text"}, {"type": "localImage", "path": missing}, {"type": "localImage", "path": unsupported}, {"type": "localImage", "path": directory}, {"type": "image", "url": "https://example.invalid/image.png"}, {"type": "image", "url": "data:image/gif;base64,R0lGODlh"},
    ]}}));
    let turn = server.read_until("turn/completed");
    assert_eq!(status(&turn), "completed", "{turn:#?}");
    assert_eq!(
        items(&turn, "item/completed", "agentMessage")[0]["text"],
        "Hello from the loopback mock."
    );
    let commands = server.commands();
    let prompt = commands.iter().find(|c| c["type"] == "prompt").unwrap();
    assert!(
        prompt["message"]
            .as_str()
            .unwrap()
            .contains(missing.to_str().unwrap())
    );
    assert!(
        prompt["message"]
            .as_str()
            .unwrap()
            .contains("https://example.invalid/image.png")
    );
    assert_eq!(
        prompt["images"],
        json!([{"type": "image", "data": "R0lGODlh", "mimeType": "image/gif"}])
    );
    assert_eq!(
        commands
            .iter()
            .find(|c| c["type"] == "open_session")
            .unwrap()["sessionDir"],
        server
            .dir
            .join("sessions")
            .join(format!("centaur-{thread}"))
            .to_string_lossy()
            .as_ref()
    );
}

#[test]
#[ignore = "requires ANTHROPIC_API_KEY and real OMP; makes provider calls"]
fn real_omp_streaming_steer_and_resume() {
    fn resume(server: &mut Server, thread: &str, model: &str) {
        server.send(json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "real-omp-test", "version": "1"}}}));
        assert!(server.until(|v| v["id"] == 1).last().unwrap()["error"].is_null());
        server.send(json!({"id": 2, "method": "thread/resume", "params": {"threadId": thread, "model": model}}));
        assert_eq!(
            server.until(|v| v["id"] == 2).last().unwrap()["result"]["thread"]["id"],
            thread
        );
    }
    fn final_text(values: &[Value]) -> String {
        items(values, "item/completed", "agentMessage")
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect()
    }
    assert!(
        std::env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.is_empty()),
        "set ANTHROPIC_API_KEY before running real OMP tests"
    );
    let binary = std::env::var_os("CENTAUR_OMP_BIN").unwrap_or_else(|| "omp".into());
    let version = Command::new(&binary)
        .arg("--version")
        .output()
        .expect("real omp on PATH or CENTAUR_OMP_BIN");
    assert!(version.status.success(), "OMP --version failed");
    let model = std::env::var("CENTAUR_REAL_OMP_MODEL")
        .unwrap_or_else(|_| "anthropic/claude-sonnet-4-5".to_string());
    let dir = std::env::temp_dir().join(format!("real-omp-stdio-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let thread = format!("real-omp-{}", Uuid::new_v4().simple());
    let codeword = format!("OMP_MEMORY_{}", Uuid::new_v4().simple());
    let acknowledgement = format!("STEER_{}", Uuid::new_v4().simple());
    let mut server = Server::with_binary(dir.clone(), "jsonrpc", &binary, &[]);
    server.timeout = Duration::from_secs(300);
    resume(&mut server, &thread, &model);
    server.send(json!({"id": 3, "method": "turn/start", "params": {"threadId": thread, "input": [{"type": "text", "text": format!("Remember the codeword {codeword} for later. First run exactly `sleep 5` with bash. Then produce 300 numbered lines. If an update arrives, follow it.")}]}}));
    let mut values = server.until(|value| {
        value["method"] == "turn/completed"
            || (value["method"] == "item/started"
                && value["params"]["item"]["type"] == "commandExecution")
    });
    assert!(
        !values
            .iter()
            .any(|value| value["method"] == "turn/completed"),
        "turn ended before the tool started: {values:#?}"
    );
    let turn_id = values.last().unwrap()["params"]["turnId"].clone();
    server.send(json!({"id": 4, "method": "turn/steer", "params": {"threadId": thread, "expectedTurnId": turn_id, "clientUserMessageId": "real-omp-steer", "input": [{"type": "text", "text": format!("Stop the numbered listing. Remember the codeword {codeword}. Reply exactly {acknowledgement} and nothing else.")}]}}));
    values.extend(server.read_until("turn/completed"));
    assert_eq!(status(&values), "completed");
    assert_eq!(values.last().unwrap()["params"]["turn"]["id"], turn_id);
    assert_eq!(
        values.iter().find(|value| value["id"] == 4).unwrap()["result"]["turnId"],
        turn_id
    );
    assert!(
        final_text(&values).contains(&acknowledgement),
        "OMP did not apply the steering update"
    );
    assert!(
        values.last().unwrap()["params"]["turn"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["clientId"] == "real-omp-steer")
    );

    // Preserve the session directory, not the process's conversation state.
    let children = Command::new("pgrep")
        .args(["-P", &server.child.id().to_string()])
        .output()
        .expect("pgrep for the real OMP child");
    assert!(
        children.status.success(),
        "OMP child missing after the turn"
    );
    let children = String::from_utf8(children.stdout).unwrap();
    let pids: Vec<&str> = children.split_whitespace().collect();
    assert_eq!(pids.len(), 1, "expected one direct OMP child: {pids:?}");
    assert!(
        Command::new("kill")
            .arg("-KILL")
            .args(&pids)
            .status()
            .unwrap()
            .success()
    );
    server.child.kill().unwrap();
    server.child.wait().unwrap();

    let mut resumed = Server::with_binary(dir, "jsonrpc", &binary, &[]);
    resumed.timeout = Duration::from_secs(300);
    resume(&mut resumed, &thread, &model);
    resumed.send(json!({"id": 3, "method": "turn/start", "params": {"threadId": thread, "effort": "none", "input": [{"type": "text", "text": "Without using tools, what exact codeword did I ask you to remember earlier? Reply with only that codeword."}]}}));
    let recalled = resumed.read_until("turn/completed");
    assert_eq!(status(&recalled), "completed");
    assert_eq!(final_text(&recalled).trim(), codeword);
    assert!(items(&recalled, "item/completed", "reasoning").is_empty());
    resumed.send(json!({"id": 4, "method": "turn/start", "params": {"threadId": thread, "input": [{"type": "text", "text": "Without using tools, what is 17 multiplied by 23? Reply with only the number."}]}}));
    let default_level = resumed.read_until("turn/completed");
    assert_eq!(status(&default_level), "completed");
    assert!(final_text(&default_level).contains("391"));
    eprintln!(
        "real {}: steer acknowledged in final items; native resume recalled the codeword with thinking off",
        String::from_utf8_lossy(&version.stdout).trim()
    );
}
