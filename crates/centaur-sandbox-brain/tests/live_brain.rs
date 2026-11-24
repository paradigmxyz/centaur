//! Contract test against a running centaur-brain. Ignored by default because
//! it makes a real model call:
//!
//! ```sh
//! CENTAUR_BRAIN_TEST_URL=http://127.0.0.1:8790 \
//!   cargo test -p centaur-sandbox-brain --test live_brain -- --ignored
//! ```

use std::time::{Duration, SystemTime};

use centaur_sandbox_brain::{BrainSandboxBackend, RoutingSandboxBackend};
use centaur_sandbox_core::{SandboxBackend, SandboxSpec, SandboxStatus};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
#[ignore = "needs a running centaur-brain and a model provider"]
async fn a_turn_round_trips_through_a_live_brain() {
    let url = std::env::var("CENTAUR_BRAIN_TEST_URL").expect("set CENTAUR_BRAIN_TEST_URL");
    let backend = BrainSandboxBackend::new(url).unwrap();
    let thread_key = format!("contract:{}", uuid::Uuid::new_v4());
    let spec = SandboxSpec::new("unused")
        .label("centaur.ai/harness", "nanocode")
        .env("CENTAUR_THREAD_KEY", &thread_key);
    let started = SystemTime::now();

    let handle = backend.create(spec).await.unwrap();
    assert!(handle.id.as_str().starts_with("brain-"));
    assert_eq!(
        backend.status(&handle.id).await.unwrap(),
        SandboxStatus::Running
    );

    let parts = backend.open_io(&handle.id).await.unwrap().into_parts();
    let mut stdin = parts.stdin;
    let mut stdout = BufReader::new(parts.stdout).lines();
    let line = json!({
        "type": "user",
        "thread_key": thread_key,
        "message": {"role": "user", "content": [{"type": "text", "text": "Reply with exactly: contract-ok"}]},
    });
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .await
        .unwrap();
    stdin.flush().await.unwrap();

    let mut answer = String::new();
    let terminal = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(line) = stdout.next_line().await.unwrap() {
            let event: Value = serde_json::from_str(&line).unwrap();
            match event["method"].as_str() {
                Some("item/agentMessage/delta") => {
                    answer.push_str(event["params"]["delta"].as_str().unwrap_or_default());
                }
                Some("turn/completed") => {
                    return event["params"]["turn"]["status"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                }
                _ => {}
            }
        }
        panic!("stdout ended before the turn did");
    })
    .await
    .unwrap();
    assert_eq!(terminal, "completed", "answer so far: {answer}");
    assert!(answer.contains("contract-ok"), "answer: {answer}");
    drop(stdin);
    drop(stdout);
    drop(parts.guard);

    let replay = backend
        .read_output_since(&handle.id, Some(started))
        .await
        .unwrap();
    assert!(replay.iter().any(|line| line.contains("turn/completed")));

    let routing = RoutingSandboxBackend::new(
        std::sync::Arc::new(BrainSandboxBackend::new("http://127.0.0.1:9").unwrap()),
        Some(std::sync::Arc::new(backend)),
    );
    routing.stop(&handle.id).await.unwrap();
    assert_eq!(
        routing.status(&handle.id).await.unwrap(),
        SandboxStatus::Gone
    );
}
