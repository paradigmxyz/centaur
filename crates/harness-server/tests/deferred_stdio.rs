//! `harness-server deferred`: a warm sandbox boots unbound, then the first
//! stdin line binds its harness and persona.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

const TOOL_TURN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pi/tool_turn.jsonl"
);

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Scripted `pi --mode rpc` that records, per spawn, the persona env it
/// inherited and the workspace prompt it would load, then replays a turn.
fn fake_pi(dir: &Path) -> PathBuf {
    let path = dir.join("pi");
    write_script(
        &path,
        &format!(
            concat!(
                "#!/bin/sh\n",
                "printf '%s|%s\\n' \"$CENTAUR_PERSONA_ID\" \"$(cat AGENTS.md)\" >> '{dir}/spawns'; ",
                "while IFS= read -r line; do ",
                "case \"$line\" in ",
                "*'\"type\":\"prompt\"'*) ",
                "printf '%s\\n' '{{\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{{\"disposition\":\"started\"}}}}'; ",
                "cat '{tool}' ;; ",
                "*) printf '%s\\n' '{{\"type\":\"response\",\"command\":\"set_thinking_level\",\"success\":true}}' ;; ",
                "esac; done"
            ),
            dir = dir.display(),
            tool = TOOL_TURN,
        ),
    );
    path
}

/// Stand-in for the sandbox's compose-system-prompt: base prompt plus the
/// persona prompt from the home directory.
fn fake_compose(dir: &Path) -> PathBuf {
    let path = dir.join("compose-system-prompt");
    write_script(
        &path,
        concat!(
            "#!/bin/sh\nset -e\n",
            "while [ $# -gt 0 ]; do case \"$1\" in ",
            "--home-dir) home=\"$2\"; shift 2;; ",
            "--target-prompt) target=\"$2\"; shift 2;; ",
            "*) exit 9;; esac; done\n",
            "printf 'base+%s' \"$(cat \"$home/AGENTS_PERSONA.md\")\" > \"$target\"\n",
        ),
    );
    path
}

#[test]
fn deferred_server_binds_harness_and_persona_from_first_line() {
    let dir = std::env::temp_dir().join(format!("deferred-stdio-{}", Uuid::new_v4()));
    let home = dir.join("home");
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("AGENTS.md"), "base").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("deferred")
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("CENTAUR_PI_BIN", fake_pi(&dir))
        .env("CENTAUR_COMPOSE_SYSTEM_PROMPT_BIN", fake_compose(&dir))
        .env_remove("CENTAUR_PERSONA_ID")
        .env_remove("CENTAUR_PI_MODEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel::<Value>();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    let read_until = |method: &str| {
        let mut seen = Vec::new();
        loop {
            let value = rx
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no {method}: {seen:#?}"));
            let done = value["method"] == method;
            seen.push(value);
            if done {
                return seen;
            }
        }
    };
    let mut stdin = child.stdin.take().unwrap();
    let configure = json!({
        "type": "session.configure",
        "harness": "pi",
        "persona": {"id": "eng", "prompt": "be terse", "prompt_hash": "sha256:eng"},
    });
    let user = |text: &str| {
        json!({
            "type": "user",
            "thread_key": "slack:C123:123.456",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        })
    };

    // Configure and the first turn arrive in one write: the turn must survive
    // the deferred reader's buffering.
    write!(stdin, "{configure}\n{}\n", user("first")).unwrap();
    let first = read_until("turn/completed");
    // A reattaching control plane resends the same configure: a no-op.
    write!(stdin, "{configure}\n{}\n", user("second")).unwrap();
    let second = read_until("turn/completed");
    // A different binding is rejected without disturbing the session.
    let mut other = configure.clone();
    other["harness"] = json!("codex");
    writeln!(stdin, "{other}").unwrap();
    let rejected = read_until("error");
    drop(stdin);
    child.wait().unwrap();

    for turn in [&first, &second] {
        assert_eq!(
            turn.last().unwrap()["params"]["turn"]["status"],
            "completed",
            "{turn:#?}"
        );
        assert!(
            !turn.iter().any(|value| value["method"] == "error"),
            "{turn:#?}"
        );
    }
    assert!(
        rejected.last().unwrap()["params"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("refusing to rebind"),
        "{rejected:#?}"
    );
    // Pi started once, after the persona was applied to its env and prompt.
    assert_eq!(
        std::fs::read_to_string(dir.join("spawns")).unwrap(),
        "eng|base+be terse\n"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn deferred_server_exits_when_first_line_is_not_configure() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("deferred")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.take().unwrap(),
        "{}",
        json!({"type": "user", "text": "hi"})
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected `session.configure`"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
