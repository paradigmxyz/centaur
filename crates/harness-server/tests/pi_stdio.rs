use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use serde_json::Value;
use uuid::Uuid;

#[test]
fn fake_pi_rpc_streams_a_complete_blocks_turn() {
    let dir = std::env::temp_dir().join(format!("centaur-fake-pi-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("pi");
    fs::write(
        &bin,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_state"'*) printf '{"type":"response","id":"%s","success":true,"data":{}}\n' "$id" ;;
    *'"type":"prompt"'*)
      printf '{"type":"response","id":"%s","success":true}\n' "$id"
      printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"hello"}}'
      printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"model":"fake-pi","usage":{"input":3,"output":1,"totalTokens":4},"stopReason":"stop"}}'
      printf '%s\n' '{"type":"agent_settled"}' ;;
  esac
done
"#,
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_harness-server"))
        .arg("pi")
        .env("PI_BIN", &bin)
        .env("PI_PROVIDER", "openai")
        .env("PI_MODEL", "fake-pi")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.take().unwrap(),
        r#"{{"type":"user","thread_key":"test:pi","text":"say hello"}}"#
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(dir).unwrap();

    assert!(output.status.success());
    let frames: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(frames.iter().any(|frame| {
        frame["method"] == "item/agentMessage/delta" && frame["params"]["delta"] == "hello"
    }));
    assert!(frames.iter().any(|frame| {
        frame["method"] == "turn/completed" && frame["params"]["turn"]["status"] == "completed"
    }));
}
