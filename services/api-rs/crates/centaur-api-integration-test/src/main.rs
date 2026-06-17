use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use centaur_session_core::HarnessType;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::{Client as HttpClient, StatusCode};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};
use uuid::Uuid;

const DEFAULT_API_URL: &str = "http://127.0.0.1:18080";
const SOURCE_PATH: &str = "services/api-rs/crates/centaur-api-integration-test/src/main.rs";
const TEST_MODEL: &str = "gpt-api-integration-test";

#[tokio::main]
async fn main() -> Result<()> {
    let base_url = env::var("CENTAUR_API_URL")
        .unwrap_or_else(|_| DEFAULT_API_URL.to_owned())
        .trim_end_matches('/')
        .to_owned();
    let http = HttpClient::new();

    let mut results = Vec::new();

    let line = line!() + 1;
    record_result(
        &mut results,
        "API health endpoint responds",
        line,
        wait_for_health(&http, &base_url).await,
    );

    let line = line!() + 1;
    record_result(
        &mut results,
        "Harness wire values match the API contract",
        line,
        test_harness_wire_values(&http, &base_url).await,
    );

    let line = line!() + 1;
    record_result(
        &mut results,
        "Session execute forwards model context and completes",
        line,
        test_session_turn(&http, &base_url).await,
    );

    let line = line!() + 1;
    record_result(
        &mut results,
        "Workflows API runs added workflows and cancels removed workflows",
        line,
        test_workflows_api(&http, &base_url).await,
    );

    let line = line!() + 1;
    record_result(
        &mut results,
        "Metrics expose API request counters",
        line,
        test_metrics(&http, &base_url).await,
    );

    write_report(&results)?;

    if results.iter().all(|result| result.passed) {
        println!("centaur-api integration test passed");
        Ok(())
    } else {
        bail!("centaur-api integration test failed")
    }
}

#[derive(Debug)]
struct TestResult {
    summary: &'static str,
    line: u32,
    passed: bool,
}

fn record_result(
    results: &mut Vec<TestResult>,
    summary: &'static str,
    line: u32,
    result: Result<()>,
) {
    match result {
        Ok(()) => results.push(TestResult {
            summary,
            line,
            passed: true,
        }),
        Err(error) => {
            eprintln!("{summary}: {error:#}");
            results.push(TestResult {
                summary,
                line,
                passed: false,
            });
        }
    }
}

fn write_report(results: &[TestResult]) -> Result<()> {
    let report = render_report(results);
    println!("{report}");
    if let Ok(path) = env::var("API_INTEGRATION_TEST_REPORT")
        && !path.trim().is_empty()
    {
        fs::write(&path, report)
            .with_context(|| format!("write integration test report {path}"))?;
    }
    Ok(())
}

fn render_report(results: &[TestResult]) -> String {
    let mut report = String::from("| Test | Result |\n| --- | --- |\n");
    for result in results {
        let status = if result.passed { "Passed" } else { "Failed" };
        report.push_str(&format!(
            "| [{}]({}) | {status} |\n",
            result.summary,
            source_link(result.line)
        ));
    }
    report
}

fn source_link(line: u32) -> String {
    match (
        env::var("GITHUB_SERVER_URL"),
        env::var("GITHUB_REPOSITORY"),
        env::var("API_INTEGRATION_TEST_SOURCE_SHA").or_else(|_| env::var("GITHUB_SHA")),
    ) {
        (Ok(server), Ok(repository), Ok(sha))
            if !server.is_empty() && !repository.is_empty() && !sha.is_empty() =>
        {
            format!("{server}/{repository}/blob/{sha}/{SOURCE_PATH}#L{line}")
        }
        _ => format!("{SOURCE_PATH}#L{line}"),
    }
}

async fn wait_for_health(http: &HttpClient, base_url: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let url = format!("{base_url}/healthz");
    let mut last_error = String::new();

    while Instant::now() < deadline {
        match http.get(&url).send().await {
            Ok(response) if response.status() == StatusCode::OK => {
                let body = response
                    .json::<Value>()
                    .await
                    .context("parse /healthz body")?;
                if body.get("ok").and_then(Value::as_bool) == Some(true) {
                    return Ok(());
                }
                last_error = format!("unexpected /healthz body: {body}");
            }
            Ok(response) => {
                last_error = format!("/healthz returned {}", response.status());
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }
        sleep(Duration::from_millis(500)).await;
    }

    bail!("api did not become healthy at {url}: {last_error}")
}

async fn test_harness_wire_values(http: &HttpClient, base_url: &str) -> Result<()> {
    let cases = [
        (HarnessType::Codex, "codex"),
        (HarnessType::Amp, "amp"),
        (HarnessType::ClaudeCode, "claudecode"),
    ];

    for (harness_type, expected_wire_value) in cases {
        let harness_wire_value =
            serde_json::to_value(&harness_type).context("serialize harness type")?;
        let wire_value = harness_wire_value
            .as_str()
            .context("serialized harness type was not a string")?
            .to_owned();
        if wire_value != expected_wire_value {
            bail!(
                "typed harness {:?} serialized to {wire_value:?}, expected {expected_wire_value:?}",
                harness_type
            );
        }

        let thread_key = test_thread_key(format!("harness-{wire_value}"))?;
        let session = post_json_ok(
            http,
            session_url(base_url, &thread_key),
            json!({
                "harness_type": harness_wire_value,
                "metadata": {
                    "source": "centaur-api-integration-test",
                    "harness_wire_value": wire_value,
                },
            }),
        )
        .await
        .with_context(|| format!("create {wire_value} session"))?;

        if session.get("thread_key").and_then(Value::as_str) != Some(thread_key.as_str()) {
            bail!("session thread key mismatch for {wire_value}");
        }
        if session.get("harness_type").and_then(Value::as_str) != Some(wire_value.as_str()) {
            bail!(
                "session harness mismatch for {wire_value}: got {}",
                session
                    .get("harness_type")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>")
            );
        }
        if session.get("status").and_then(Value::as_str) != Some("idle") {
            bail!("new {wire_value} session was not idle: {session}");
        }
    }

    let invalid_thread_key = test_thread_key("invalid-harness")?;
    let invalid_response = http
        .post(session_url(base_url, &invalid_thread_key))
        .json(&json!({
            "harness_type": "claude-code",
            "metadata": {"source": "centaur-api-integration-test"},
        }))
        .send()
        .await
        .context("send invalid harness request")?;
    if invalid_response.status() != StatusCode::UNPROCESSABLE_ENTITY {
        let status = invalid_response.status();
        let body = invalid_response.text().await.unwrap_or_default();
        bail!("stale claude-code harness value returned {status}: {body}");
    }

    Ok(())
}

async fn test_session_turn(http: &HttpClient, base_url: &str) -> Result<()> {
    let thread_key = test_thread_key("turn")?;
    let harness_wire_value = serde_json::to_value(HarnessType::Codex)
        .context("serialize executable harness type")?
        .as_str()
        .context("serialized executable harness type was not a string")?
        .to_owned();
    post_json_ok(
        http,
        session_url(base_url, &thread_key),
        json!({
            "harness_type": HarnessType::Codex,
            "metadata": {
                    "source": "centaur-api-integration-test",
                    "purpose": "api-integration-test",
            },
            "on_harness_conflict": "restart",
        }),
    )
    .await
    .context("create executable session")?;

    let append = post_json_ok(
        http,
        format!("{}/messages", session_url(base_url, &thread_key)),
        json!({
            "messages": [
                {
                    "client_message_id": "api-integration-test-message-1",
                    "role": "user",
                    "parts": [{
                        "type": "text",
                        "text": "Reply with PONG, the model, and the harness.",
                    }],
                    "metadata": {
                        "source": "centaur-api-integration-test",
                        "model": TEST_MODEL,
                    },
                },
            ],
        }),
    )
    .await
    .context("append user message")?;
    let message_ids = append
        .get("message_ids")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    if append.get("ok").and_then(Value::as_bool) != Some(true) || message_ids != 1 {
        bail!("append response was not successful: {append:?}");
    }

    let input_line = serde_json::to_string(&json!({
        "type": "user",
        "model": TEST_MODEL,
        "trace_metadata": {
            "source": "centaur-api-integration-test",
            "action": "execute",
        },
        "message": {
            "role": "user",
            "content": [{
                "type": "text",
                "text": "Reply with PONG, the model, and the harness.",
            }],
        },
    }))
    .context("serialize execute input line")?;

    let first_execute = post_json_ok(
        http,
        format!("{}/execute", session_url(base_url, &thread_key)),
        json!({
            "idempotency_key": "api-integration-test-execute-1",
            "metadata": {
                    "source": "centaur-api-integration-test",
                    "model": TEST_MODEL,
            },
            "input_lines": [input_line],
            "idle_timeout_ms": 5_000,
            "max_duration_ms": 15_000,
        }),
    )
    .await
    .context("execute session")?;
    if first_execute.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("execute response was not ok");
    }
    if first_execute.get("thread_key").and_then(Value::as_str) != Some(thread_key.as_str()) {
        bail!("execute response thread key mismatch");
    }
    let execution_id = first_execute
        .get("execution_id")
        .and_then(Value::as_str)
        .context("execute response missing execution_id")?
        .to_owned();

    let replay = post_json_ok(
        http,
        format!("{}/execute", session_url(base_url, &thread_key)),
        json!({
            "idempotency_key": "api-integration-test-execute-1",
            "metadata": {"source": "centaur-api-integration-test", "replay": true},
            "input_lines": [],
            "idle_timeout_ms": 5_000,
            "max_duration_ms": 15_000,
        }),
    )
    .await
    .context("replay idempotent execute")?;
    if replay.get("execution_id").and_then(Value::as_str) != Some(execution_id.as_str()) {
        bail!(
            "idempotent execute returned different execution id: {} vs {}",
            replay
                .get("execution_id")
                .and_then(Value::as_str)
                .unwrap_or("<missing>"),
            execution_id
        );
    }

    let response = http
        .get(format!(
            "{}/events?after_event_id=0",
            session_url(base_url, &thread_key)
        ))
        .send()
        .await
        .context("open session event stream")?;
    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("open session event stream returned {status}: {body}");
    }
    let mut events = response.bytes_stream().eventsource();
    timeout(Duration::from_secs(20), async {
        let mut saw_output = false;
        while let Some(event) = events.next().await {
            let event = event.context("read session event")?;
            match event.event.as_str() {
                "session.output.line" => {
                    let line = parse_json(&event.data)?;
                    let line_type = line.get("type").and_then(Value::as_str);
                    if line_type == Some("item.agentMessage.delta")
                        && let Some(delta) = line.get("delta").and_then(Value::as_str)
                        && delta.contains("PONG")
                        && delta.contains(&format!("model={TEST_MODEL}"))
                        && delta.contains(&format!("harness={harness_wire_value}"))
                    {
                        saw_output = true;
                    }
                }
                "session.execution_completed" => {
                    let payload = parse_json(&event.data)?;
                    if payload.get("execution_id").and_then(Value::as_str)
                        == Some(execution_id.as_str())
                    {
                        if !saw_output {
                            bail!(
                                "execution completed before a PONG model/harness output line was observed"
                            );
                        }
                        return Ok(());
                    }
                }
                "session.execution_failed" | "session.execution_cancelled" => {
                    bail!("execution reached terminal failure event: {}", event.data);
                }
                _ => {}
            }
        }
        bail!("session event stream ended before execution completed")
    })
    .await
    .context("timed out waiting for session execution completion")??;

    Ok(())
}

async fn test_metrics(http: &HttpClient, base_url: &str) -> Result<()> {
    let response = http
        .get(format!("{base_url}/metrics"))
        .send()
        .await
        .context("request /metrics")?;
    if response.status() != StatusCode::OK {
        bail!("/metrics returned {}", response.status());
    }
    let body = response.text().await.context("read /metrics body")?;
    for needle in [
        r#"http_server_requests_total{method="GET",route="/healthz",status="200"}"#,
        r#"http_server_requests_total{method="POST",route="/api/session/{thread_key}",status="200"}"#,
        r#"http_server_requests_total{method="POST",route="/api/session/{thread_key}/execute",status="200"}"#,
    ] {
        if !body.contains(needle) {
            bail!("missing expected metric {needle:?}");
        }
    }
    Ok(())
}

async fn test_workflows_api(http: &HttpClient, base_url: &str) -> Result<()> {
    let workflow_dir = integration_workflow_dir()?;
    fs::create_dir_all(&workflow_dir)
        .with_context(|| format!("create workflow dir {}", workflow_dir.display()))?;

    let unique = Uuid::new_v4().simple().to_string();
    let sentinel_name = format!("api_integration_sentinel_{unique}");
    let workflow_name = format!("api_integration_workflow_{unique}");
    let workflow_path = workflow_dir.join(format!("{workflow_name}.py"));

    write_sentinel_workflow(&workflow_dir, &sentinel_name)?;
    write_test_workflow(&workflow_path, &workflow_name)?;

    wait_for_workflow_schedule(http, base_url, &workflow_name, true)
        .await
        .context("wait for added workflow schedule to be discovered")?;

    let completed_run_id = create_workflow_run(
        http,
        base_url,
        &workflow_name,
        json!({
            "case": "added-workflow-run",
            "sleep_ms": 0,
        }),
    )
    .await
    .context("create added workflow run")?;
    let completed_run =
        wait_for_workflow_run_status(http, base_url, &completed_run_id, &["completed"])
            .await
            .context("wait for added workflow run completion")?;
    let output = completed_run
        .pointer("/result/output")
        .context("completed workflow run missing result output")?;
    if output.get("workflow_name").and_then(Value::as_str) != Some(workflow_name.as_str()) {
        bail!("completed workflow output did not echo workflow name: {completed_run}");
    }
    if output.pointer("/received/case").and_then(Value::as_str) != Some("added-workflow-run") {
        bail!("completed workflow output did not echo input: {completed_run}");
    }

    let removed_run_id = create_workflow_run(
        http,
        base_url,
        &workflow_name,
        json!({
            "case": "removed-workflow-run",
            "sleep_ms": 60_000,
        }),
    )
    .await
    .context("create long-running workflow run")?;
    wait_for_workflow_run_status(http, base_url, &removed_run_id, &["running"])
        .await
        .context("wait for long-running workflow run to start")?;

    fs::remove_file(&workflow_path)
        .with_context(|| format!("remove workflow file {}", workflow_path.display()))?;

    wait_for_workflow_schedule(http, base_url, &workflow_name, false)
        .await
        .context("wait for removed workflow schedule to be dropped")?;
    wait_for_workflow_run_status(http, base_url, &removed_run_id, &["cancelled"])
        .await
        .context("wait for removed workflow run to be cancelled")?;

    Ok(())
}

fn integration_workflow_dir() -> Result<PathBuf> {
    let path = env::var("API_INTEGRATION_WORKFLOW_DIR")
        .context("API_INTEGRATION_WORKFLOW_DIR must point at the mounted workflow test dir")?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("API_INTEGRATION_WORKFLOW_DIR must not be empty");
    }
    Ok(PathBuf::from(trimmed))
}

fn write_sentinel_workflow(workflow_dir: &Path, workflow_name: &str) -> Result<()> {
    let path = workflow_dir.join(format!("{workflow_name}.py"));
    let source = format!(
        r#"
WORKFLOW_NAME = "{workflow_name}"


async def handler(params, ctx):
    return {{"workflow_name": ctx.workflow_name, "received": params}}
"#
    );
    fs::write(&path, source).with_context(|| format!("write sentinel workflow {}", path.display()))
}

fn write_test_workflow(path: &Path, workflow_name: &str) -> Result<()> {
    let source = format!(
        r#"
import asyncio

WORKFLOW_NAME = "{workflow_name}"
SCHEDULE = {{
    "schedule_id": "{workflow_name}",
    "interval_seconds": 3600,
    "enabled": True,
    "no_delivery": True,
    "input": {{"source": "centaur-api-integration-test"}},
}}


async def handler(params, ctx):
    sleep_ms = int(params.get("sleep_ms") or 0)
    if sleep_ms:
        await asyncio.sleep(sleep_ms / 1000)
    return {{
        "workflow_name": ctx.workflow_name,
        "run_id": ctx.run_id,
        "task_id": ctx.task_id,
        "received": params,
    }}
"#
    );
    fs::write(path, source).with_context(|| format!("write test workflow {}", path.display()))
}

async fn create_workflow_run(
    http: &HttpClient,
    base_url: &str,
    workflow_name: &str,
    input: Value,
) -> Result<String> {
    let response = post_json_ok(
        http,
        format!("{base_url}/api/workflows/runs"),
        json!({
            "workflow_name": workflow_name,
            "input": input,
            "idempotency_key": format!("{workflow_name}-{}", Uuid::new_v4().simple()),
            "harness_type": HarnessType::Codex,
            "max_attempts": 1,
        }),
    )
    .await?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!("workflow create response was not ok: {response}");
    }
    if response.get("created").and_then(Value::as_bool) != Some(true) {
        bail!("workflow create response did not create a new run: {response}");
    }
    response
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("workflow create response missing run_id")
}

async fn wait_for_workflow_run_status(
    http: &HttpClient,
    base_url: &str,
    run_id: &str,
    expected_statuses: &[&str],
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut last_run = Value::Null;

    while Instant::now() < deadline {
        let body = get_json_ok(http, format!("{base_url}/api/workflows/runs/{run_id}")).await?;
        let run = body
            .get("run")
            .cloned()
            .context("workflow run response missing run")?;
        let status = run
            .get("status")
            .and_then(Value::as_str)
            .context("workflow run missing status")?;
        if expected_statuses.contains(&status) {
            return Ok(run);
        }
        if matches!(status, "completed" | "failed" | "cancelled") {
            bail!(
                "workflow run {run_id} reached terminal status {status}, expected one of {:?}: {run}",
                expected_statuses
            );
        }
        last_run = run;
        sleep(Duration::from_millis(250)).await;
    }

    bail!(
        "workflow run {run_id} did not reach one of {:?} before timeout; last run: {last_run}",
        expected_statuses
    )
}

async fn wait_for_workflow_schedule(
    http: &HttpClient,
    base_url: &str,
    schedule_id: &str,
    should_exist: bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_body = Value::Null;

    while Instant::now() < deadline {
        let body = get_json_ok(http, format!("{base_url}/api/workflows/schedules")).await?;
        let present = body
            .get("schedules")
            .and_then(Value::as_array)
            .context("workflow schedules response missing schedules")?
            .iter()
            .any(|schedule| {
                schedule.get("schedule_id").and_then(Value::as_str) == Some(schedule_id)
            });
        if present == should_exist {
            return Ok(());
        }
        last_body = body;
        sleep(Duration::from_millis(250)).await;
    }

    let expectation = if should_exist { "appear" } else { "disappear" };
    bail!("workflow schedule {schedule_id} did not {expectation}; last response: {last_body}")
}

fn parse_json(data: &str) -> Result<Value> {
    serde_json::from_str(data).with_context(|| format!("parse event payload as JSON: {data}"))
}

async fn get_json_ok(http: &HttpClient, url: impl AsRef<str>) -> Result<Value> {
    let response = http
        .get(url.as_ref())
        .send()
        .await
        .with_context(|| format!("GET {}", url.as_ref()))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("GET {} returned {status}: {text}", url.as_ref());
    }
    response
        .json::<Value>()
        .await
        .with_context(|| format!("parse GET {} response", url.as_ref()))
}

async fn post_json_ok(http: &HttpClient, url: impl AsRef<str>, body: Value) -> Result<Value> {
    let response = http
        .post(url.as_ref())
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {}", url.as_ref()))?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("POST {} returned {status}: {text}", url.as_ref());
    }
    response
        .json::<Value>()
        .await
        .with_context(|| format!("parse POST {} response", url.as_ref()))
}

fn test_thread_key(suffix: impl AsRef<str>) -> Result<String> {
    Ok(format!(
        "api-integration-test:{}:{}",
        Uuid::new_v4().simple(),
        suffix.as_ref()
    ))
}

fn session_url(base_url: &str, thread_key: &str) -> String {
    format!("{base_url}/api/session/{thread_key}")
}
