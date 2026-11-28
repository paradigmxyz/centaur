//! The hand daemon served to a real nanocode brain-side attachment.

use std::{future::Future, net::SocketAddr, path::PathBuf, pin::Pin, time::Duration};

use centaur_hand::{Config, RunningHand};
use nanocode::{
    AttachmentError, CodeModeHost, Runner, RunnerPool, RunnerStatus, tool_contract::ToolContext,
};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "test-hand-token";

struct Fixture {
    _dir: TempDir,
    hand: RunningHand,
    token: String,
    workspace: PathBuf,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, TOKEN).expect("token file");
    let workspace = dir.path().join("workspace");
    let hand = centaur_hand::start(Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        health_listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        token_file,
        workspace: workspace.clone(),
        name: Some("test-hand".into()),
    })
    .await
    .expect("hand starts");
    Fixture {
        _dir: dir,
        hand,
        token: TOKEN.into(),
        workspace,
    }
}

fn hand_url(hand: &RunningHand) -> String {
    format!("ws://{}", hand.listen())
}

/// Run a brain-side closure on nanocode's executor, which is not tokio.
async fn on_nanocode<T, F>(work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> Pin<Box<dyn Future<Output = T> + Send>> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let runtime = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("nanocode executor");
        runtime.block_on(work())
    })
    .await
    .expect("brain thread")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_is_ready_and_a_shell_call_runs_in_the_workspace() {
    let fixture = fixture().await;
    assert_eq!(health_status(fixture.hand.health()).await, 200);

    let url = hand_url(&fixture.hand);
    let token = fixture.token.clone();
    let workspace = fixture.workspace.clone();
    let attached =
        on_nanocode(move || Box::pin(async move { attach_and_run(url, token).await })).await;

    match attached {
        Ok(names) => {
            assert!(names.iter().any(|name| name == "exec_command"), "{names:?}");
            assert!(names.iter().any(|name| name == "Bash"), "{names:?}");
            assert!(names.iter().any(|name| name == "Read"), "{names:?}");
        }
        Err(error) => panic!("brain attachment failed: {error}"),
    }

    let file = workspace.join("f");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !file.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hand never wrote {}",
            file.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi");
}

async fn attach_and_run(url: String, token: String) -> Result<Vec<String>, String> {
    let pool = RunnerPool::new();
    let runner = Runner::connect(url.clone())
        .token(token.clone())
        .name("test-hand");
    let handle = pool
        .connect(runner)
        .await
        .map_err(|error| format!("pool attach: {error}"))?;
    let info = handle.info();
    if info.status != RunnerStatus::Ready {
        return Err(format!("hand was not ready: {:?}", info.status));
    }
    if info.tool_count == 0 {
        return Err("the hand advertised no tools".into());
    }
    handle
        .disconnect()
        .await
        .map_err(|error| format!("pool disconnect: {error}"))?;

    // A fresh attach proves the listener keeps serving after a disconnect.
    let handle = pool
        .connect(
            Runner::connect(url.clone())
                .token(token.clone())
                .name("test-hand"),
        )
        .await
        .map_err(|error| format!("pool reconnect: {error}"))?;

    let executors = pool.code_mode_executors();
    let host = executors
        .connected("test-hand")
        .map_err(|error| format!("hand host: {error}"))?;
    let names = host
        .tool_definitions("test-session")
        .map_err(|error| format!("tool definitions: {error}"))?
        .into_iter()
        .map(|definition| definition.name().to_owned())
        .collect::<Vec<_>>();
    let context = ToolContext::new("test-model", "test-session", "test-call", &[], 4096);
    let output = executors
        .call_on(
            "test-hand",
            "Bash",
            json!({"command": "printf hi > f"}),
            context,
        )
        .await;
    if !output.success {
        return Err(format!("Bash failed: {:?}", output.output));
    }

    handle
        .disconnect()
        .await
        .map_err(|error| format!("final disconnect: {error}"))?;
    Ok(names)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_token_is_rejected() {
    let fixture = fixture().await;
    let url = hand_url(&fixture.hand);
    let error = on_nanocode(move || {
        Box::pin(async move {
            let pool = RunnerPool::new();
            pool.connect(
                Runner::connect(url)
                    .token("not-the-token")
                    .name("wrong-token"),
            )
            .await
            .map(drop)
        })
    })
    .await
    .expect_err("the hand accepted a wrong bearer");
    assert!(
        matches!(error, AttachmentError::Authentication(_)),
        "unexpected attachment error: {error}"
    );

    // The rejected connection did not take the hand down.
    assert_eq!(health_status(fixture.hand.health()).await, 200);
    let url = hand_url(&fixture.hand);
    let token = fixture.token.clone();
    let ok = on_nanocode(move || {
        Box::pin(async move {
            let pool = RunnerPool::new();
            pool.connect(Runner::connect(url).token(token).name("after-reject"))
                .await
                .map(drop)
        })
    })
    .await;
    assert!(ok.is_ok(), "the hand rejected the right token: {ok:?}");
}

#[test]
fn missing_or_empty_token_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing");
    assert!(centaur_hand::read_token(&missing).is_err());

    let empty = dir.path().join("empty");
    std::fs::write(&empty, "  \n").expect("empty token");
    assert!(centaur_hand::read_token(&empty).is_err());

    let token = dir.path().join("token");
    std::fs::write(&token, "  token-value\n").expect("token");
    assert_eq!(centaur_hand::read_token(&token).unwrap(), "token-value");
}

async fn health_status(address: SocketAddr) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect health");
    stream
        .write_all(
            format!("GET /healthz HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("write health request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read health response");
    response
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or_else(|| panic!("unparsable health response: {response:?}"))
}
