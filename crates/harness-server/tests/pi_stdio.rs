use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};
use uuid::Uuid;

/// Turns recorded from `pi --mode rpc` 1.0.0: commentary text and a bash call
/// in one message, then a final answer; and one codemode script running two
/// bash calls in parallel, then text.
const TOOL_TURN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pi/tool_turn.jsonl"
);
const CODEMODE_TURN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pi/codemode_turn.jsonl"
);

/// Scripted `pi --mode rpc`: logs its argv, environment, and stdin, and
/// replays a recorded turn for each prompt. A `slow` prompt never finishes.
fn fake_pi(dir: &Path) -> PathBuf {
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "printf '%s|%s\\n' \"$ANTHROPIC_API_KEY\" \"$*\" >> '{dir}/argv'; ",
            "while IFS= read -r line; do ",
            "printf '%s\\n' \"$line\" >> '{dir}/stdin'; ",
            "case \"$line\" in ",
            "*'\"type\":\"prompt\"'*) ",
            "printf '%s\\n' '{{\"type\":\"response\",\"command\":\"prompt\",\"success\":true,\"data\":{{\"disposition\":\"started\"}}}}'; ",
            "case \"$line\" in *codemode*) cat '{codemode}' ;; *slow*) ;; *) cat '{tool}' ;; esac ;; ",
            "*) printf '%s\\n' '{{\"type\":\"response\",\"command\":\"set_thinking_level\",\"success\":true}}' ;; ",
            "esac; done"
        ),
        dir = dir.display(),
        tool = TOOL_TURN,
        codemode = CODEMODE_TURN,
    );
    let path = dir.join("pi");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Server {
    stdin: ChildStdin,
    stdout: Receiver<Value>,
}

impl Server {
    fn send(&mut self, line: Value) {
        writeln!(self.stdin, "{line}").unwrap();
    }

    fn user(&mut self, text: &str, extra: Value) {
        let mut line = json!({
            "type": "user",
            "thread_key": "slack:C123:123.456",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        });
        line.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.send(line);
    }

    /// Notifications up to and including the next one with `method`.
    fn read_until(&mut self, method: &str) -> Vec<Value> {
        let mut notifications = Vec::new();
        loop {
            let value = self
                .stdout
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no {method}: {notifications:#?}"));
            let done = value["method"] == method;
            notifications.push(value);
            if done {
                return notifications;
            }
        }
    }

    fn turn(&mut self, text: &str, extra: Value) -> Vec<Value> {
        self.user(text, extra);
        self.read_until("turn/completed")
    }
}

fn items<'a>(turn: &'a [Value], method: &str) -> Vec<&'a Value> {
    turn.iter()
        .filter(|value| value["method"] == method)
        .map(|value| &value["params"]["item"])
        .collect()
}

fn status(turn: &[Value]) -> &Value {
    &turn.last().unwrap()["params"]["turn"]["status"]
}

#[test]
fn pi_blocks_turns_render_tools_and_codemode_and_respawn_on_model_change_and_interrupt() {
    let dir: PathBuf = std::env::temp_dir().join(format!("pi-stdio-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("pi")
        .env("CENTAUR_PI_BIN", fake_pi(&dir))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CENTAUR_PI_MODEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
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
    let mut server = Server {
        stdin: child.stdin.take().unwrap(),
        stdout: rx,
    };

    let first = server.turn("run echo hi", json!({"reasoning": "high"}));
    let second = server.turn("again", json!({"model": "openai/gpt-5.5"}));
    let unknown = server.turn("bad model", json!({"model": "openai/o3"}));
    let codemode = server.turn("use codemode", json!({}));
    server.user("slow", json!({}));
    server.read_until("turn/started");
    server.send(json!({"type": "interrupt", "thread_key": "slack:C123:123.456"}));
    let interrupted = server.read_until("turn/completed");
    let after_interrupt = server.turn("after interrupt", json!({}));
    drop(server);
    child.wait().unwrap();

    for turn in [&first, &second, &after_interrupt] {
        assert_eq!(status(turn), "completed", "{turn:#?}");
        // Text is only phased once its message ends: commentary before the
        // tool call, then the final answer.
        let started: Vec<(&Value, &Value)> = items(turn, "item/started")
            .into_iter()
            .filter(|item| item["type"] == "agentMessage")
            .map(|item| (&item["id"], &item["phase"]))
            .collect();
        let completed: Vec<(&Value, &Value, &Value)> = items(turn, "item/completed")
            .into_iter()
            .filter(|item| item["type"] == "agentMessage")
            .map(|item| (&item["id"], &item["text"], &item["phase"]))
            .collect();
        assert_eq!(
            completed
                .iter()
                .map(|(_, text, phase)| (*text, *phase))
                .collect::<Vec<_>>(),
            [
                (&json!("Checking now."), &json!("commentary")),
                (&json!("DONE"), &json!("final_answer")),
            ],
            "{turn:#?}"
        );
        assert_eq!(
            started,
            completed
                .iter()
                .map(|(id, _, phase)| (*id, *phase))
                .collect::<Vec<_>>()
        );
        assert!(
            items(turn, "item/completed")
                .iter()
                .any(|item| item["type"] == "commandExecution"
                    && item["command"] == "echo hi"
                    && item["aggregatedOutput"] == "hi\n"
                    && item["exitCode"] == 0),
            "{turn:#?}"
        );
    }

    // An unsupported model fails its turn without reaching Pi, with wording
    // Slack recognizes to clear the thread's sticky model; the next turn runs
    // the last working model again.
    assert_eq!(status(&unknown), "failed", "{unknown:#?}");
    assert_eq!(
        unknown.last().unwrap()["params"]["turn"]["error"]["message"],
        "unsupported model `openai/o3` for Pi; see the supported models in the Pi harness docs"
    );

    // Codemode's nested calls render as their own command items.
    assert_eq!(status(&codemode), "completed");
    let completed = items(&codemode, "item/completed");
    let commands: Vec<(&Value, &Value)> = completed
        .iter()
        .filter(|item| item["type"] == "commandExecution")
        .map(|item| (&item["command"], &item["aggregatedOutput"]))
        .collect();
    assert_eq!(
        commands,
        [
            (&json!("echo a"), &json!("a\n")),
            (&json!("echo b"), &json!("b\n"))
        ],
        "{completed:#?}"
    );
    assert!(
        completed
            .iter()
            .any(|item| item["type"] == "dynamicToolCall"
                && item["tool"] == "codemode"
                && item["status"] == "completed"),
        "{completed:#?}"
    );

    assert_eq!(status(&interrupted), "interrupted", "{interrupted:#?}");

    // A spawn per model, and a respawn after the failed turn and after the
    // interrupt killed Pi; the session id is stable across them.
    let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        argv,
        [
            "|--mode rpc --approve --session-id centaur-slack-C123-123.456 \
             --thinking medium --tools read,bash,edit,write,codemode",
            "|--mode rpc --approve --session-id centaur-slack-C123-123.456 \
             --model openai/gpt-5.5 --thinking medium --tools read,bash,edit,write,codemode",
            "|--mode rpc --approve --session-id centaur-slack-C123-123.456 \
             --model openai/gpt-5.5 --thinking medium --tools read,bash,edit,write,codemode",
            "|--mode rpc --approve --session-id centaur-slack-C123-123.456 \
             --model openai/gpt-5.5 --thinking medium --tools read,bash,edit,write,codemode",
        ]
    );
    let stdin: Vec<Value> = std::fs::read_to_string(dir.join("stdin"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        // The interrupt can kill Pi before it logs the slow prompt.
        .filter(|line: &Value| line["message"] != "slow")
        .collect();
    assert_eq!(
        stdin,
        [
            json!({"type": "set_thinking_level", "level": "high"}),
            json!({"type": "prompt", "message": "run echo hi"}),
            json!({"type": "prompt", "message": "again"}),
            json!({"type": "prompt", "message": "use codemode"}),
            json!({"type": "prompt", "message": "after interrupt"}),
        ]
    );

    let _ = std::fs::remove_dir_all(dir);
}
