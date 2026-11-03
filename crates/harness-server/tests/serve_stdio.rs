//! `harness-server serve` binds to a session from its first stdin line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn serve_binds_harness_env_and_files_then_ignores_repeated_binds() {
    let dir = temp_dir();
    let hook_log = dir.join("hook.log");
    let persona = dir.join("home/AGENTS_PERSONA.md");
    let hook = dir.join("bind-hook.sh");
    write_executable(
        &hook,
        &format!(
            "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' \"$CENTAUR_BIND_HARNESS\" \"$CENTAUR_BIND_CHANGED_ENV\" \"$CENTAUR_THREAD_KEY\" \"$(cat {})\" >> {}\n",
            quote(&persona),
            quote(&hook_log)
        ),
    );
    // The fake harness only comes from the bind env, so a completed turn
    // proves the bound env reached the harness.
    let fake_claude = concat!(
        "printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fake\"}'; ",
        "while IFS= read -r _; do ",
        "printf '%s\\n' '{\"type\":\"assistant\",\"is_partial\":false,\"message\":{\"id\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"bound\"}]}}'; ",
        "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"bound\"}'; ",
        "done"
    );
    let bind = json!({
        "type": "sandbox.bind",
        "harness": "claude-code",
        "env": {
            "CENTAUR_CLAUDE_APP_BRIDGE_COMMAND": fake_claude,
            "CENTAUR_THREAD_KEY": "test:thread",
        },
        "unset": ["CENTAUR_BIND_TEST_UNSET"],
        "files": [{"target_path": persona, "contents": "persona prompt"}],
    })
    .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("serve")
        .current_dir(&dir)
        .env("CENTAUR_SANDBOX_BIND_HOOK", &hook)
        .env("CENTAUR_BIND_TEST_UNSET", "warm")
        .env_remove("CENTAUR_CLAUDE_APP_BRIDGE_COMMAND")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn harness-server serve");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.expect("stdout line")).is_err() {
                break;
            }
        }
    });
    let user = json!({"type": "user", "text": "hi"}).to_string();

    // The control plane resends the bind line on every attach.
    writeln!(stdin, "{bind}\n{user}").expect("write first turn");
    wait_for_turn_completed(&rx);
    writeln!(stdin, "{bind}\n{user}").expect("write second turn");
    wait_for_turn_completed(&rx);
    drop(stdin);

    assert!(child.wait().expect("wait for serve").success());
    let hook_runs = std::fs::read_to_string(&hook_log).expect("read hook log");
    let lines = hook_runs.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "the hook runs once per process: {lines:?}");
    let [harness, changed, thread_key, persona_prompt] =
        lines[0].split('|').collect::<Vec<_>>()[..]
    else {
        panic!("unexpected hook log line {:?}", lines[0]);
    };
    assert_eq!(harness, "claude-code");
    let mut changed = changed.split(',').collect::<Vec<_>>();
    changed.sort_unstable();
    assert_eq!(
        changed,
        [
            "CENTAUR_BIND_TEST_UNSET",
            "CENTAUR_CLAUDE_APP_BRIDGE_COMMAND",
            "CENTAUR_THREAD_KEY"
        ]
    );
    assert_eq!(thread_key, "test:thread");
    assert_eq!(persona_prompt, "persona prompt");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn serve_exits_when_the_first_line_is_not_a_bind() {
    let output = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(b"{\"type\":\"user\",\"text\":\"hi\"}\n")?;
            child.wait_with_output()
        })
        .expect("run harness-server serve");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "nothing reaches the protocol stream"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("sandbox bind failed"));
}

fn wait_for_turn_completed(rx: &mpsc::Receiver<String>) {
    loop {
        let line = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("turn/completed before timeout");
        let value: Value = serde_json::from_str(&line).expect("JSON stdout");
        match value.get("method").and_then(Value::as_str) {
            Some("turn/completed") => return,
            Some("error") => panic!("harness reported an error: {value}"),
            _ => {}
        }
    }
}

fn temp_dir() -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("harness-server-serve-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
