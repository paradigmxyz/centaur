use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(10);
const KEY: &str = "test-thread";

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("centaur-omp-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("home")).unwrap();
        Self(root)
    }

    fn session_dir(&self, key: &str) -> PathBuf {
        self.0.join(format!("{:x}", Sha256::digest(key.as_bytes())))
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Bridge {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Receiver<Value>,
    timeout: Duration,
}

impl Bridge {
    fn spawn(root: &TestRoot, key: Option<&str>, env: &[(&str, &str)]) -> Self {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_omp_rpc.py");
        Self::with_binary(root, key, fixture.as_os_str(), env, TIMEOUT)
    }

    fn with_binary(
        root: &TestRoot,
        key: Option<&str>,
        binary: &OsStr,
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_harness-server"));
        command
            .arg("omp")
            .current_dir(&root.0)
            .env("CENTAUR_OMP_BIN", binary)
            .env("CENTAUR_OMP_SESSION_ROOT", &root.0)
            .env("FAKE_OMP_LOG", root.0.join("rpc.jsonl"))
            .env("HOME", root.0.join("home"))
            .env_remove("CENTAUR_THREAD_KEY")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(key) = key {
            command.env("CENTAUR_THREAD_KEY", key);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = serde_json::from_str(&line.unwrap()).expect("JSON-only harness stdout");
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            output,
            timeout,
        }
    }

    fn send(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &value).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn until(&self, mut predicate: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + self.timeout;
        let mut values = Vec::new();
        loop {
            let value = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("bridge ended: {error}; values={values:#?}"));
            let done = predicate(&value);
            values.push(value);
            if done {
                return values;
            }
        }
    }

    fn begin(&mut self, text: &str) -> Vec<Value> {
        self.send(json!({"type": "user", "text": text}));
        self.until(|value| method(value) == "turn/started")
    }

    fn turn(&mut self, text: &str) -> Vec<Value> {
        self.send(json!({"type": "user", "text": text}));
        self.finish()
    }

    fn finish(&self) -> Vec<Value> {
        self.until(|value| method(value) == "turn/completed")
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn method(value: &Value) -> &str {
    value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn completed(values: &[Value]) -> &Value {
    assert_items_complete(values);
    values
        .iter()
        .find(|value| method(value) == "turn/completed")
        .expect("turn/completed")
}

/// Every item a turn starts must complete as the same kind, and every streamed
/// delta must target a started item of its kind: consumers render an open or
/// mismatched item as unfinished output.
fn assert_items_complete(values: &[Value]) {
    let started: Vec<(&str, &str)> = items(values, "item/started").collect();
    let completed: Vec<(&str, &str)> = items(values, "item/completed").collect();
    let open: Vec<_> = started
        .iter()
        .filter(|item| !completed.contains(item))
        .collect();
    assert!(
        open.is_empty(),
        "items started but never completed: {open:?}"
    );
    // Before the first turn/started, a list collected mid-turn can hold deltas
    // for items whose start it never saw.
    let Some(first) = values
        .iter()
        .position(|value| method(value) == "turn/started")
    else {
        return;
    };
    for (delta, kind) in [
        ("item/agentMessage/delta", "agentMessage"),
        ("item/reasoning/textDelta", "reasoning"),
    ] {
        for value in values[first..]
            .iter()
            .filter(|value| method(value) == delta)
        {
            let id = value
                .pointer("/params/itemId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                started.contains(&(kind, id)),
                "{delta} targets {id}, which is not a started {kind} item"
            );
        }
    }
}

fn items<'a>(values: &'a [Value], event: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
    values
        .iter()
        .filter(move |value| method(value) == event)
        .filter_map(|value| {
            let item = value.pointer("/params/item")?;
            Some((item.get("type")?.as_str()?, item.get("id")?.as_str()?))
        })
}

fn status(values: &[Value]) -> &str {
    completed(values)
        .pointer("/params/turn/status")
        .and_then(Value::as_str)
        .unwrap()
}

fn deltas(values: &[Value]) -> String {
    values
        .iter()
        .filter(|value| method(value) == "item/agentMessage/delta")
        .filter_map(|value| value.pointer("/params/delta").and_then(Value::as_str))
        .collect()
}

fn reasoning(values: &[Value]) -> String {
    values
        .iter()
        .filter(|value| method(value) == "item/reasoning/textDelta")
        .filter_map(|value| value.pointer("/params/delta").and_then(Value::as_str))
        .collect()
}

fn identity(values: &[Value]) -> (String, String) {
    let text = deltas(values);
    let (pid, session) = text
        .strip_prefix("fake response pid=")
        .unwrap()
        .split_once(" session=")
        .unwrap();
    (pid.to_string(), session.to_string())
}

#[test]
fn fake_omp_background_work_stays_in_the_original_turn() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let first = bridge.turn("__background__");
    thread::sleep(Duration::from_millis(200));
    bridge.send(json!({"type": "user", "text": "__model__", "model": "openai/next-model"}));
    let second = bridge.finish();
    assert_eq!(deltas(&first), "foreground background follow-up");
    assert_eq!(status(&first), "completed");
    assert_eq!(deltas(&second), "openai/next-model");
    assert_eq!(status(&second), "completed");
}

#[test]
fn fake_omp_resumes_from_a_session_directory_per_thread() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let values = bridge.turn("native directory");
    assert_eq!(status(&values), "completed");
    assert!(root.session_dir(KEY).is_dir());
    let initial = identity(&values);
    drop(bridge);
    let mut resumed = Bridge::spawn(&root, Some(KEY), &[]);
    let next = identity(&resumed.turn("after restart"));
    assert_ne!(initial.0, next.0);
    assert_eq!(initial.1, next.1);
}

#[test]
fn fake_omp_passes_the_requested_provider_and_model() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.send(json!({"type": "user", "text": "__model__", "model": "openai/fake-model"}));
    assert_eq!(deltas(&bridge.finish()), "openai/fake-model");
    bridge.send(
        json!({"type": "user", "text": "__model__", "model": "second-model", "provider": "google"}),
    );
    assert_eq!(deltas(&bridge.finish()), "google/second-model");
    assert_eq!(deltas(&bridge.turn("__model__")), "google/second-model");
}

#[test]
fn fake_omp_inherits_the_provider_environment() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[("OPENAI_API_KEY", "dummy")]);
    assert_eq!(deltas(&bridge.turn("__provider_env__")), "inherited");
}

#[test]
fn fake_omp_resume_separates_long_thread_keys() {
    let root = TestRoot::new();
    let a = format!("{}{}", "x".repeat(256), "a".repeat(256));
    let b = format!("{}{}", "x".repeat(256), "b".repeat(256));
    for (key, word) in [(&a, "ALPHA"), (&b, "BETA")] {
        let mut bridge = Bridge::spawn(&root, Some(key), &[]);
        assert_eq!(
            deltas(&bridge.turn(&format!("__remember:{word}"))),
            "remembered"
        );
    }
    for (key, word) in [(&a, "ALPHA"), (&b, "BETA")] {
        let mut bridge = Bridge::spawn(&root, Some(key), &[]);
        assert_eq!(deltas(&bridge.turn("__recall__")), word);
    }
    let mut first = Bridge::spawn(&root, None, &[]);
    first.turn("__remember:UNKEYED");
    drop(first);
    let mut second = Bridge::spawn(&root, None, &[]);
    assert_eq!(deltas(&second.turn("__recall__")), "forgotten");
}

#[test]
fn fake_omp_rejects_malformed_frames_then_resumes() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.turn("__remember:CONTEXT");
    for prompt in [
        "__unknown_event__",
        "__unknown_update__",
        "__unknown_control__",
        "__malformed__",
        "__oversize__",
    ] {
        assert_eq!(status(&bridge.turn(prompt)), "failed", "{prompt}");
        assert_eq!(deltas(&bridge.turn("__recall__")), "CONTEXT", "{prompt}");
    }
}

#[test]
fn fake_omp_local_results_do_not_poison_the_next_turn() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    for prompt in ["__local__", "__local_result__"] {
        assert_eq!(deltas(&bridge.turn(prompt)), "local command completed");
        let next = bridge.turn("after local result");
        assert_eq!(status(&next), "completed");
        identity(&next);
    }
}

#[test]
fn fake_omp_command_output_before_interrupt_is_not_a_final_answer() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let mut values = bridge.begin("__command_then_abort__");
    values.extend(bridge.until(|value| {
        method(value) == "item/completed"
            && value.pointer("/params/item/type") == Some(&json!("dynamicToolCall"))
    }));
    bridge.send(json!({"type": "interrupt"}));
    values.extend(bridge.finish());
    assert_eq!(status(&values), "interrupted");
    assert_eq!(deltas(&values), "");
    assert!(
        values
            .iter()
            .any(|value| value.pointer("/params/item/contentItems/0/text")
                == Some(&json!("command output before abort")))
    );
}

#[test]
fn fake_omp_steers_and_consumes_late_control_responses() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let initial = identity(&bridge.turn("before controls"));
    for prompt in ["__steer__", "__late_steer__", "__late_abort__"] {
        let mut values = bridge.begin(prompt);
        if prompt == "__late_abort__" {
            bridge.send(json!({"type": "interrupt"}));
        } else {
            bridge.send(json!({"type": "user", "text": "steering update", "client_user_message_id": "steer-item"}));
        }
        values.extend(bridge.finish());
        if prompt == "__late_abort__" {
            assert_eq!(status(&values), "interrupted");
        } else {
            assert_eq!(status(&values), "completed");
            assert!(deltas(&values).starts_with("steered"));
            assert!(
                completed(&values)
                    .pointer("/params/turn/items")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item.get("clientId") == Some(&json!("steer-item")))
            );
        }
        bridge.send(json!({"type": "user", "text": "after control", "provider": "anthropic", "model": "fake-model-2"}));
        assert_eq!(identity(&bridge.finish()), initial);
    }
}

#[test]
fn fake_omp_forced_abort_and_child_exit_resume_native_context() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.turn("__remember:SURVIVES");
    let initial = identity(&bridge.turn("before abort"));
    bridge.begin("__ignore_abort__");
    bridge.send(json!({"type": "interrupt"}));
    assert_eq!(status(&bridge.finish()), "interrupted");
    assert_eq!(deltas(&bridge.turn("__recall__")), "SURVIVES");
    let after = identity(&bridge.turn("after abort"));
    assert_ne!(initial.0, after.0);
    assert_eq!(initial.1, after.1);
    let status = Command::new("kill")
        .args(["-KILL", &after.0])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p", &after.0])
            .output()
            .unwrap();
        let state = String::from_utf8(output.stdout).unwrap();
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            break;
        }
        assert!(Instant::now() < deadline, "fake child did not exit");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(deltas(&bridge.turn("__recall__")), "SURVIVES");
}

#[test]
fn fake_omp_bounds_unacknowledged_controls() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.begin("__silent_controls__");
    for _ in 0..1025 {
        bridge.send(json!({"type": "user", "text": "steer"}));
    }
    let failed = bridge.finish();
    assert_eq!(status(&failed), "failed");
    assert!(
        completed(&failed)
            .to_string()
            .contains("pending control response limit")
    );
}

#[test]
fn fake_omp_replays_captured_omp_18_3_1_turns() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let text = bridge.turn("__replay:text");
    assert_eq!(status(&text), "completed");
    assert_eq!(deltas(&text), "Hello from the loopback mock.");
    let tool = bridge.turn("__replay:tool");
    assert_eq!(status(&tool), "completed");
    for event in ["item/started", "item/completed"] {
        assert!(tool.iter().any(|value| method(value) == event
            && value.pointer("/params/item/tool") == Some(&json!("bash"))));
    }
    assert_eq!(deltas(&tool), "The bash tool printed e2e-ok.");
    let thinking = bridge.turn("__replay:thinking_tool");
    assert_eq!(status(&thinking), "completed");
    assert_eq!(deltas(&thinking), "The bash tool printed e2e-ok.");
    assert_eq!(
        reasoning(&thinking),
        "The user wants bash. Run echo e2e-ok first.The command printed e2e-ok."
    );
    for (name, error) in [
        ("provider_error", "Mock provider rejected the request"),
        ("truncated_stream", "stream ended before message_stop"),
    ] {
        let values = bridge.turn(&format!("__replay:{name}"));
        assert_eq!(status(&values), "failed");
        let message = completed(&values)
            .pointer("/params/turn/error/message")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(message.contains(error), "{message}");
        assert!(!message.contains("raw-http-request"), "{message}");
    }
    assert_eq!(
        deltas(&bridge.turn("__replay:text")),
        "Hello from the loopback mock."
    );
    let background = bridge.turn("__replay:background");
    assert_eq!(status(&background), "completed");
    assert_eq!(
        deltas(&background),
        "The command is running in the background.The background command finished: e2e-background-ok."
    );
}

#[test]
fn fake_omp_accepts_blocks_attachments() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.send(json!({"type": "user", "content": [{"type": "attachment", "name": "note.txt", "mimeType": "text/plain", "attachment_type": "document", "dataBase64": "aGVsbG8="}]}));
    assert_eq!(deltas(&bridge.finish()), "document path accepted");
}

#[test]
fn fake_omp_forwards_every_image_attachment() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let images: Vec<_> = (0..9)
        .map(|_| json!({"type": "image", "url": "data:image/png;base64,aA=="}))
        .collect();
    let mut content = vec![json!({"type": "text", "text": "__image__", "text_elements": []})];
    content.extend(images);
    bridge.send(json!({"type": "user", "content": content}));
    assert_eq!(deltas(&bridge.finish()), "image count=9");
}

#[test]
fn fake_omp_forwards_large_image_attachments() {
    use base64::Engine;
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let data = base64::engine::general_purpose::STANDARD.encode(vec![0; 4 * 1024 * 1024 + 1]);
    bridge.send(json!({"type": "user", "content": [
        {"type": "text", "text": "__image__", "text_elements": []},
        {"type": "image", "url": format!("data:image/png;base64,{data}")}
    ]}));
    assert_eq!(deltas(&bridge.finish()), "image count=1");
}

fn rpc_log(root: &TestRoot) -> Vec<Value> {
    std::fs::read_to_string(root.0.join("rpc.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn turn_error(values: &[Value]) -> &str {
    completed(values)
        .pointer("/params/turn/error/message")
        .unwrap()
        .as_str()
        .unwrap()
}

#[test]
fn fake_omp_lazy_spawn_negotiates_and_binds_the_input_thread() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, None, &[]);
    bridge.send(json!({"type": "invalid"}));
    bridge.until(|value| method(value) == "error");
    assert!(
        !root.0.join("rpc.jsonl").exists(),
        "OMP must not spawn at boot"
    );
    bridge.send(json!({"type": "user", "thread_key": "warm-pool/thread", "text": "__model__", "model": "openai/desired"}));
    let values = bridge.finish();
    assert_eq!(deltas(&values), "openai/desired");
    assert_eq!(
        completed(&values).pointer("/params/threadId"),
        Some(&json!("warm-pool/thread"))
    );
    assert!(root.session_dir("warm-pool/thread").is_dir());
    let log = rpc_log(&root);
    assert_eq!(log[0]["argv"], json!(["--mode", "rpc", "--no-ui"]));
    assert_eq!(
        log.iter()
            .map(|frame| frame["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "spawn",
            "negotiate_protocol",
            "set_event_filter",
            "open_session",
            "get_state",
            "set_model",
            "prompt"
        ]
    );
    assert_eq!(log[1]["protocolVersion"], 2);
    assert_eq!(
        log[2]["events"],
        json!([
            "agent_start",
            "message_start",
            "message_update",
            "message_end",
            "tool_execution_start",
            "tool_execution_end",
            "notice"
        ])
    );
    assert_eq!(
        log[3]["sessionDir"],
        json!(root.session_dir("warm-pool/thread"))
    );
}

#[test]
fn fake_omp_line_key_overrides_environment_and_cannot_change() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some("environment-thread"), &[]);
    bridge.send(json!({"type": "user", "thread_key": KEY, "text": "__remember:BOUND"}));
    assert_eq!(status(&bridge.finish()), "completed");
    assert!(root.session_dir(KEY).is_dir());
    assert!(!root.session_dir("environment-thread").exists());
    bridge.send(json!({"type": "user", "thread_key": "other", "text": "wrong thread"}));
    let failed = bridge.finish();
    assert_eq!(status(&failed), "failed");
    assert!(turn_error(&failed).contains("thread_key changed"));
    assert_eq!(deltas(&bridge.turn("__recall__")), "BOUND");
    assert!(
        rpc_log(&root)
            .iter()
            .filter(|frame| frame["type"] == "open_session")
            .all(|frame| frame["sessionDir"] == json!(root.session_dir(KEY)))
    );
}

#[test]
fn fake_omp_steering_cannot_change_threads() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.begin("__steer__");
    bridge.send(json!({"type": "user", "thread_key": "other", "text": "bad steer"}));
    let failed = bridge.finish();
    assert_eq!(status(&failed), "failed");
    assert!(turn_error(&failed).contains("thread_key changed"));
    assert!(!rpc_log(&root).iter().any(|frame| frame["type"] == "steer"));
}

#[test]
fn fake_omp_requires_v2_and_retries_startup_on_the_next_turn() {
    let root = TestRoot::new();
    let marker = root.0.join("failed-once");
    let mut bridge = Bridge::spawn(
        &root,
        Some(KEY),
        &[("FAKE_OMP_FAIL_ONCE", marker.to_str().unwrap())],
    );
    let first = bridge.turn("first");
    assert_eq!(status(&first), "failed");
    assert!(
        turn_error(&first)
            .contains("OMP RPC protocol v2 unavailable: the omp harness requires omp >= 18.3.1")
    );
    assert_eq!(status(&bridge.turn("retry")), "completed");
    assert_eq!(
        rpc_log(&root)
            .iter()
            .filter(|frame| frame["type"] == "spawn")
            .count(),
        2
    );
}

#[test]
fn fake_omp_restored_model_is_refreshed_before_overrides() {
    let root = TestRoot::new();
    let mut first = Bridge::spawn(&root, Some(KEY), &[]);
    first.send(json!({"type": "user", "text": "__model__", "model": "openai/stored"}));
    assert_eq!(deltas(&first.finish()), "openai/stored");
    drop(first);
    let mut second = Bridge::spawn(&root, Some(KEY), &[]);
    second.send(json!({"type": "user", "text": "__model__", "model": "override"}));
    assert_eq!(deltas(&second.finish()), "openai/override");
    assert_eq!(deltas(&second.turn("__model__")), "openai/override");
}

#[test]
fn fake_omp_uses_prompt_outcomes_without_completion_queries() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let first = bridge.turn("completed");
    assert_eq!(status(&first), "completed");
    let initial = identity(&first);
    assert_eq!(status(&bridge.turn("__aborted__")), "interrupted");
    let failed = bridge.turn("__error__");
    assert_eq!(status(&failed), "failed");
    assert_eq!(turn_error(&failed), "provider line one\nprovider line two");
    assert!(!failed.iter().any(|value| method(value) == "error"));
    let next = bridge.turn("after provider failure");
    assert_eq!(status(&next), "completed");
    assert_eq!(identity(&next), initial);
    assert_eq!(
        rpc_log(&root)
            .iter()
            .filter(|frame| frame["type"] == "spawn")
            .count(),
        1
    );
    assert_eq!(
        rpc_log(&root)
            .iter()
            .filter(|frame| frame["type"] == "get_state")
            .count(),
        1
    );
    let long = bridge.turn("__long_error__");
    assert_eq!(status(&long), "failed");
    assert_eq!(
        turn_error(&long),
        format!("{} [truncated]", "é".repeat(2048))
    );
}

#[test]
fn fake_omp_accepts_settled_between_turns_and_during_model_change() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    bridge.turn("leaves a session_settled frame queued");
    bridge.send(json!({"type": "user", "text": "__model__", "model": "openai/after-settle"}));
    let next = bridge.finish();
    assert_eq!(status(&next), "completed");
    assert_eq!(deltas(&next), "openai/after-settle");
    assert_eq!(status(&bridge.turn("after change")), "completed");
}

#[test]
fn fake_omp_rejects_unknown_prompt_result_ids_and_host_requests() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let wrong_id = bridge.turn("__wrong_id__");
    assert_eq!(status(&wrong_id), "failed");
    assert!(turn_error(&wrong_id).contains("prompt_result for unknown id not-the-prompt"));
    for kind in [
        "extension_ui_request",
        "extension_ui_cancel",
        "host_tool_call",
        "host_tool_cancel",
        "host_uri_request",
        "host_uri_cancel",
    ] {
        let values = bridge.turn(&format!("__host:{kind}"));
        assert_eq!(status(&values), "failed");
        assert!(turn_error(&values).contains(&format!("unexpected OMP host request {kind}")));
    }
    assert!(!rpc_log(&root).iter().any(|frame| matches!(
        frame["type"].as_str(),
        Some("extension_ui_response" | "host_tool_result" | "host_uri_result")
    )));
}

#[test]
fn fake_omp_injected_messages_do_not_steal_the_reply_id() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let values = bridge.turn("__injected__");
    assert_eq!(status(&values), "completed");
    let reply: String = values
        .iter()
        .filter(|value| {
            method(value) == "item/agentMessage/delta"
                && value.pointer("/params/itemId") == Some(&json!("omp-msg-1"))
        })
        .filter_map(|value| value.pointer("/params/delta").and_then(Value::as_str))
        .collect();
    assert_eq!(reply, "reply across injection");
    let items = completed(&values)
        .pointer("/params/turn/items")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["id"] == "omp-msg-1" && item["text"] == "reply across injection")
    );
    assert!(
        items
            .iter()
            .any(|item| item["id"] == "omp-msg-3" && item["text"] == "injected assistant")
    );
}

#[test]
fn fake_omp_reassembles_valid_chunks() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let values = bridge.turn("__chunk:valid");
    // LARGE_ANSWER in fake_omp_rpc.py: over 1 MiB, streamed and finished in chunks.
    let answer = "0123456789abcdé\n".repeat(90_000);
    assert_eq!(status(&values), "completed");
    assert_eq!(deltas(&values), answer);
    let items = completed(&values)
        .pointer("/params/turn/items")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["id"] == "omp-msg-1" && item["text"] == answer.as_str())
    );
}

#[test]
fn fake_omp_rejects_invalid_chunk_sequences() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    for (mode, message) in [
        ("interleaved", "sequence mismatch"),
        ("interrupted", "sequence interrupted"),
        ("gap", "sequence mismatch"),
        ("count", "sequence mismatch"),
        ("length", "byteLength mismatch"),
        ("over_cap", "reassembly limit"),
        ("utf8", "strict UTF-8"),
        ("nonobject", "one JSON object"),
        ("base64", "base64"),
        ("metadata", "metadata"),
        ("trailing", "trailing characters"),
        ("eof", "sequence interrupted"),
    ] {
        let values = bridge.turn(&format!("__chunk:{mode}"));
        assert_eq!(status(&values), "failed", "{mode}");
        assert!(
            turn_error(&values).contains(message),
            "{mode}: {}",
            turn_error(&values)
        );
    }
}

#[test]
fn fake_omp_enforces_the_advertised_chunk_limit() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[("FAKE_OMP_REASSEMBLY_CAP", "1048576")]);
    let values = bridge.turn("__chunk:valid");
    assert_eq!(status(&values), "failed");
    assert!(turn_error(&values).contains("reassembly limit"));
}

#[test]
fn fake_omp_aborts_active_errors_and_redacts_persistence_diagnostics() {
    let root = TestRoot::new();
    let mut bridge = Bridge::spawn(&root, Some(KEY), &[]);
    let initial = identity(&bridge.turn("before harness failure"));
    bridge.begin("__error_keeps_running__");
    bridge.until(|value| method(value) == "error");
    assert!(root.session_dir(KEY).join("error-turn-aborted").exists());
    assert_eq!(status(&bridge.finish()), "failed");
    let mut previous = identity(&bridge.turn("after active failure"));
    assert_ne!(previous.0, initial.0);
    assert_eq!(previous.1, initial.1);
    for prompt in [
        "__notice_error__",
        "__extension_error__",
        "__persistence_error__",
    ] {
        let values = bridge.turn(prompt);
        assert_eq!(status(&values), "failed");
        assert!(values.iter().any(|value| method(value) == "error"));
        assert!(!turn_error(&values).contains("private-store-path"));
        let next = identity(&bridge.turn("after harness failure"));
        assert_ne!(next.0, previous.0, "{prompt} must replace the child");
        assert_eq!(next.1, previous.1);
        previous = next;
    }
}

#[test]
#[ignore = "requires ANTHROPIC_API_KEY and real OMP; makes provider calls"]
fn real_omp_streaming_steer_and_resume() {
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
    let root = TestRoot::new();
    let codeword = format!("OMP_MEMORY_{}", Uuid::new_v4().simple());
    let acknowledgement = format!("STEER_{}", Uuid::new_v4().simple());
    let timeout = Duration::from_secs(300);
    let mut bridge = Bridge::with_binary(&root, Some(KEY), &binary, &[], timeout);
    bridge.send(json!({
        "type": "user", "model": model,
        "text": format!("Remember the codeword {codeword} for later. Do not use tools. Produce 300 lines numbered 001 through 300, each followed by the words: streaming output remains visible while a steering update arrives. If an update arrives, follow it.")
    }));
    let mut values =
        bridge.until(|value| matches!(method(value), "item/agentMessage/delta" | "turn/completed"));
    assert!(
        !values.iter().any(|value| method(value) == "turn/completed"),
        "turn ended before streaming: {values:#?}"
    );
    bridge.send(json!({
        "type": "user", "client_user_message_id": "real-omp-steer",
        "text": format!("Stop the numbered listing. Remember the codeword {codeword}. Reply exactly {acknowledgement} and nothing else.")
    }));
    values.extend(bridge.finish());
    assert_eq!(status(&values), "completed");
    let delta_count = values
        .iter()
        .filter(|value| method(value) == "item/agentMessage/delta")
        .count();
    assert!(delta_count > 1, "OMP must preserve streaming deltas");
    assert!(
        deltas(&values).contains(&acknowledgement),
        "OMP did not apply the steering update"
    );
    assert!(
        completed(&values)
            .pointer("/params/turn/items")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.get("clientId") == Some(&json!("real-omp-steer")))
    );
    eprintln!(
        "real OMP {}: streaming_deltas={delta_count}, steer=completed",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    drop(bridge);

    let mut resumed = Bridge::with_binary(&root, Some(KEY), &binary, &[], timeout);
    resumed.send(json!({
        "type": "user", "model": model,
        "text": "Without using tools, what exact codeword did I ask you to remember earlier? Reply with only that codeword."
    }));
    let recalled = resumed.finish();
    assert_eq!(status(&recalled), "completed");
    assert_eq!(deltas(&recalled).trim(), codeword);
    eprintln!("real OMP: native resume recalled the codeword after child restart");
}
