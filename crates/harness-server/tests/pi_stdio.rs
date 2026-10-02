use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

/// A turn recorded from `pi --mode rpc` 1.0.0: one bash tool call, then text.
const TOOL_TURN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pi/tool_turn.jsonl"
);

/// Scripted `pi --mode rpc`: logs its argv, API key, and stdin, acknowledges
/// commands, and replays the recorded turn for each prompt.
fn fake_pi(dir: &Path) -> PathBuf {
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "printf '%s %s\\n' \"$ANTHROPIC_API_KEY\" \"$*\" >> '{dir}/argv'; ",
            "while IFS= read -r line; do ",
            "printf '%s\\n' \"$line\" >> '{dir}/stdin'; ",
            "case \"$line\" in ",
            "*'\"type\":\"prompt\"'*) ",
            "printf '%s\\n' '{{\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{{\"disposition\":\"started\"}}}}'; ",
            "cat '{fixture}' ;; ",
            "*) printf '%s\\n' '{{\"type\":\"response\",\"command\":\"set_thinking_level\",\"success\":true}}' ;; ",
            "esac; done"
        ),
        dir = dir.display(),
        fixture = TOOL_TURN,
    );
    let path = dir.join("pi");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn user_line(text: &str, extra: Value) -> Value {
    let mut line = json!({
        "type": "user",
        "thread_key": "slack:C123:123.456",
        "message": {"role": "user", "content": [{"type": "text", "text": text}]},
    });
    line.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    line
}

#[test]
fn pi_blocks_turns_stream_tools_and_text_and_respawn_on_model_change() {
    let dir: PathBuf = std::env::temp_dir().join(format!("pi-stdio-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("pi")
        .env("PI_BIN", fake_pi(&dir))
        .env_remove("CENTAUR_PI_APP_BRIDGE_COMMAND")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("PI_MODEL")
        .env_remove("PI_THINKING")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let stdout = server.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx
                .send(serde_json::from_str::<Value>(&line).unwrap())
                .is_err()
            {
                break;
            }
        }
    });
    let mut run_turn = |line: Value| -> Vec<Value> {
        writeln!(stdin, "{line}").unwrap();
        let mut notifications = Vec::new();
        loop {
            let value = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("turn/completed");
            let done = value["method"] == "turn/completed";
            notifications.push(value);
            if done {
                return notifications;
            }
        }
    };

    let first = run_turn(user_line("run echo hi", json!({"reasoning": "high"})));
    let second = run_turn(user_line("again", json!({"model": "openai/gpt-5.5"})));
    drop(stdin);
    server.wait().unwrap();

    for turn in [&first, &second] {
        let completed = turn.last().unwrap();
        assert_eq!(
            completed["params"]["turn"]["status"], "completed",
            "{turn:#?}"
        );
        let items: Vec<&Value> = turn
            .iter()
            .filter(|value| value["method"] == "item/completed")
            .map(|value| &value["params"]["item"])
            .collect();
        assert!(
            items.iter().any(|item| item["type"] == "commandExecution"
                && item["command"] == "echo hi"
                && item["aggregatedOutput"] == "hi\n"
                && item["exitCode"] == 0),
            "{items:#?}"
        );
        assert!(
            items
                .iter()
                .any(|item| item["type"] == "agentMessage" && item["text"] == "DONE"),
            "{items:#?}"
        );
    }

    let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        argv,
        [
            "ANTHROPIC_API_KEY --mode rpc --continue --thinking medium",
            "ANTHROPIC_API_KEY --mode rpc --continue --model openai/gpt-5.5 --thinking medium",
        ]
    );
    let stdin: Vec<Value> = std::fs::read_to_string(dir.join("stdin"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        stdin,
        [
            json!({"type": "set_thinking_level", "level": "high"}),
            json!({"type": "prompt", "message": "run echo hi"}),
            json!({"type": "prompt", "message": "again"}),
        ]
    );

    let _ = std::fs::remove_dir_all(dir);
}
