use anyhow::{Context, Result, bail};
use centaur_session_core::ThreadKey;
use clap::Parser;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::{
    io::{self, AsyncBufReadExt, BufReader},
    task::JoinHandle,
};
use uuid::Uuid;

mod tui;

const DEFAULT_MESSAGE: &str = "Reply with exactly PONG and nothing else.";

#[derive(Debug, Parser)]
#[command(about = "Create, execute, or attach to a Centaur session")]
struct Args {
    #[arg(long, env = "CENTAUR_API_URL", default_value = "http://127.0.0.1:8080")]
    api_url: String,

    #[arg(long)]
    thread_key: Option<String>,

    #[arg(long)]
    attach: bool,

    #[arg(long, default_value = "codex")]
    harness_type: String,

    #[arg(long)]
    message: Option<String>,

    #[arg(long = "input-line")]
    input_lines: Vec<String>,

    #[arg(long, default_value_t = 1_000)]
    idle_timeout_ms: u64,

    #[arg(long, default_value_t = 60_000)]
    max_duration_ms: u64,

    #[arg(long, default_value_t = 0)]
    after_event_id: i64,

    #[arg(long)]
    all_events: bool,

    #[arg(long)]
    exit_on_terminal: bool,

    #[arg(long)]
    exit_on_output_type: Option<String>,

    #[arg(long, alias = "stdin")]
    stdin_events: bool,

    #[arg(long)]
    tui: bool,

    #[arg(long)]
    debug: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let attach_mode = attach_mode(&args);
    validate_mode(&args, attach_mode)?;
    let (thread_key, generated_thread_key) = thread_key_arg(&args, attach_mode)?;
    let thread_key = ThreadKey::parse(thread_key)?;
    if generated_thread_key {
        eprintln!("thread_key={}", thread_key.as_str());
    }
    let client = Client::new();
    let base_url = args.api_url.trim_end_matches('/').to_owned();
    let session_url = session_url(&base_url, thread_key.as_str());

    if attach_mode {
        let events_response = open_event_stream(&client, &session_url, args.after_event_id).await?;
        if args.tui {
            return tui::run(
                client,
                thread_key.as_str().to_owned(),
                session_url,
                events_response,
                tui::TuiOptions {
                    debug_visible: args.debug,
                    idle_timeout_ms: args.idle_timeout_ms,
                    max_duration_ms: args.max_duration_ms,
                    exit_on_terminal: args.exit_on_terminal,
                    exit_on_output_type: args.exit_on_output_type,
                },
            )
            .await;
        }
        return run_stream_and_optional_stdin(
            client,
            session_url,
            events_response,
            args.all_events,
            args.exit_on_terminal,
            args.exit_on_output_type,
            args.stdin_events,
            args.idle_timeout_ms,
            args.max_duration_ms,
        )
        .await;
    }

    post_json(
        &client,
        &session_url,
        json!({
            "harness_type": args.harness_type,
            "metadata": {
                "source": "centaur-session-cli",
            },
        }),
    )
    .await
    .context("create session")?;

    let initial_input_lines = if should_send_initial_turn(&args) {
        let input_lines = session_input_lines(&args)?;
        let message = message_text(&args);
        append_user_message(&client, &session_url, message)
            .await
            .context("append message")?;
        Some(input_lines)
    } else {
        None
    };

    let events_response = open_event_stream(&client, &session_url, args.after_event_id).await?;

    if let Some(input_lines) = initial_input_lines {
        execute_input_lines(
            &client,
            &session_url,
            input_lines,
            args.idle_timeout_ms,
            args.max_duration_ms,
        )
        .await
        .context("execute initial turn")?;
    }

    if args.tui {
        return tui::run(
            client,
            thread_key.as_str().to_owned(),
            session_url,
            events_response,
            tui::TuiOptions {
                debug_visible: args.debug,
                idle_timeout_ms: args.idle_timeout_ms,
                max_duration_ms: args.max_duration_ms,
                exit_on_terminal: args.exit_on_terminal,
                exit_on_output_type: args.exit_on_output_type,
            },
        )
        .await;
    }

    run_stream_and_optional_stdin(
        client,
        session_url,
        events_response,
        args.all_events,
        args.exit_on_terminal,
        args.exit_on_output_type,
        args.stdin_events,
        args.idle_timeout_ms,
        args.max_duration_ms,
    )
    .await
}

fn attach_mode(args: &Args) -> bool {
    args.attach
        || (args.after_event_id > 0
            && args.thread_key.is_some()
            && args.message.is_none()
            && args.input_lines.is_empty())
}

fn validate_mode(args: &Args, attach_mode: bool) -> Result<()> {
    if attach_mode && args.thread_key.is_none() {
        bail!("attach mode requires --thread-key");
    }
    if args.attach && (args.message.is_some() || !args.input_lines.is_empty()) {
        bail!("--attach does not accept --message or --input-line");
    }
    if args.tui && args.stdin_events {
        bail!("--tui cannot be combined with --stdin-events");
    }
    Ok(())
}

fn thread_key_arg(args: &Args, attach_mode: bool) -> Result<(String, bool)> {
    match (&args.thread_key, attach_mode) {
        (Some(thread_key), _) => Ok((thread_key.clone(), false)),
        (None, true) => bail!("--attach requires --thread-key"),
        (None, false) => Ok((format!("cli:{}", Uuid::new_v4().simple()), true)),
    }
}

async fn post_json(client: &Client, url: &str, payload: Value) -> Result<Value> {
    let response = client
        .post(url)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = response.status();
    let body = response.text().await?;
    ensure_success(status, body.clone()).with_context(|| format!("POST {url}"))?;
    serde_json::from_str(&body).with_context(|| format!("decode response from {url}"))
}

fn ensure_success(status: reqwest::StatusCode, body: String) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    bail!("HTTP {status}: {body}");
}

async fn ensure_response_success(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await?;
    bail!("HTTP {status}: {body}");
}

async fn open_event_stream(
    client: &Client,
    session_url: &str,
    after_event_id: i64,
) -> Result<reqwest::Response> {
    let events_url = format!("{session_url}/events?after_event_id={after_event_id}");
    let events_response = client
        .get(&events_url)
        .send()
        .await
        .context("open event stream")?;
    ensure_response_success(events_response)
        .await
        .context("open event stream")
}

pub(crate) async fn append_user_message(
    client: &Client,
    session_url: &str,
    text: &str,
) -> Result<()> {
    post_json(
        client,
        &format!("{session_url}/messages"),
        json!({
            "messages": [{
                "role": "user",
                "parts": [{"type": "text", "text": text}],
                "metadata": {
                    "source": "centaur-session-cli",
                },
            }],
        }),
    )
    .await
    .context("append message")?;
    Ok(())
}

pub(crate) async fn execute_input_lines(
    client: &Client,
    session_url: &str,
    input_lines: Vec<String>,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Result<()> {
    post_json(
        client,
        &format!("{session_url}/execute"),
        json!({
            "metadata": {
                "source": "centaur-session-cli",
            },
            "input_lines": input_lines,
            "idle_timeout_ms": idle_timeout_ms,
            "max_duration_ms": max_duration_ms,
        }),
    )
    .await
    .context("execute session")?;
    Ok(())
}

async fn run_stream_and_optional_stdin(
    client: Client,
    session_url: String,
    events_response: reqwest::Response,
    all_events: bool,
    exit_on_terminal: bool,
    exit_on_output_type: Option<String>,
    stdin_events: bool,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Result<()> {
    let stream_future = stream_output_lines(
        events_response,
        all_events,
        exit_on_terminal,
        exit_on_output_type,
    );
    tokio::pin!(stream_future);

    if !stdin_events {
        return stream_future.await;
    }

    let mut stdin_task = spawn_stdin_events(client, session_url, idle_timeout_ms, max_duration_ms);

    tokio::select! {
        stream_result = &mut stream_future => {
            stdin_task.abort();
            stream_result
        }
        stdin_result = &mut stdin_task => {
            stdin_result.context("join stdin event task")??;
            stream_future.await
        }
    }
}

fn spawn_stdin_events(
    client: Client,
    session_url: String,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(io::stdin()).lines();
        while let Some(line) = lines.next_line().await.context("read stdin event")? {
            let event = match StdinEvent::parse(&line)? {
                Some(event) => event,
                None => continue,
            };
            match event {
                StdinEvent::Message(text) => {
                    append_user_message(&client, &session_url, &text).await?;
                    execute_input_lines(
                        &client,
                        &session_url,
                        vec![user_input_line(&text)?],
                        idle_timeout_ms,
                        max_duration_ms,
                    )
                    .await?;
                }
                StdinEvent::InputLine(line) => {
                    execute_input_lines(
                        &client,
                        &session_url,
                        vec![line],
                        idle_timeout_ms,
                        max_duration_ms,
                    )
                    .await?;
                }
                StdinEvent::InputLines(lines) => {
                    execute_input_lines(
                        &client,
                        &session_url,
                        lines,
                        idle_timeout_ms,
                        max_duration_ms,
                    )
                    .await?;
                }
                StdinEvent::Quit => break,
            }
        }
        Ok(())
    })
}

async fn stream_output_lines(
    response: reqwest::Response,
    all_events: bool,
    exit_on_terminal: bool,
    exit_on_output_type: Option<String>,
) -> Result<()> {
    let mut chunks = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.context("read event stream")?;
        buffer.push_str(std::str::from_utf8(&chunk).context("event stream is not UTF-8")?);

        while let Some((frame_end, separator_len)) = next_frame(&buffer) {
            let frame = buffer[..frame_end].to_owned();
            buffer.drain(..frame_end + separator_len);

            let Some(event) = SseFrame::parse(&frame) else {
                continue;
            };

            if event.event == "session.output.line" {
                println!(
                    "{}\t{}",
                    event.id.as_deref().unwrap_or("unknown"),
                    event.data
                );
                if output_type_matches(&event.data, exit_on_output_type.as_deref()) {
                    return Ok(());
                }
            } else if all_events {
                let data = parse_json_or_string(&event.data);
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "sse_event": event.event,
                        "id": event.id,
                        "data": data,
                    }))?
                );
            }

            if exit_on_terminal && is_terminal_event(&event.event) {
                return Ok(());
            }
        }
    }

    Ok(())
}

pub(crate) fn output_type_matches(data: &str, expected_type: Option<&str>) -> bool {
    let Some(expected_type) = expected_type else {
        return false;
    };
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|event_type| event_type == expected_type)
        })
        .unwrap_or(false)
}

fn session_input_lines(args: &Args) -> Result<Vec<String>> {
    if !args.input_lines.is_empty() {
        return Ok(args.input_lines.clone());
    }
    let message = message_text(args);
    Ok(vec![user_input_line(message)?])
}

fn should_send_initial_turn(args: &Args) -> bool {
    args.message.is_some() || !args.input_lines.is_empty() || (!args.stdin_events && !args.tui)
}

pub(crate) fn user_input_line(text: &str) -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "type": "user",
        "message": {
            "content": [{"type": "text", "text": text}],
        },
    }))?)
}

fn message_text(args: &Args) -> &str {
    args.message.as_deref().unwrap_or(DEFAULT_MESSAGE)
}

fn session_url(base_url: &str, thread_key: &str) -> String {
    format!("{base_url}/api/session/{}", urlencoding::encode(thread_key))
}

pub(crate) fn next_frame(buffer: &str) -> Option<(usize, usize)> {
    let lf = buffer.find("\n\n").map(|index| (index, 2));
    let crlf = buffer.find("\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(frame), None) | (None, Some(frame)) => Some(frame),
        (None, None) => None,
    }
}

pub(crate) fn parse_json_or_string(data: &str) -> Value {
    serde_json::from_str(data).unwrap_or_else(|_| Value::String(data.to_owned()))
}

pub(crate) fn is_terminal_event(event: &str) -> bool {
    matches!(
        event,
        "session.execution_completed" | "session.execution_failed" | "session.execution_cancelled"
    )
}

#[derive(Debug)]
pub(crate) enum StdinEvent {
    Message(String),
    InputLine(String),
    InputLines(Vec<String>),
    Quit,
}

impl StdinEvent {
    pub(crate) fn parse(line: &str) -> Result<Option<Self>> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        if matches!(line, "/quit" | "/exit") {
            return Ok(Some(Self::Quit));
        }
        if let Some(text) = line.strip_prefix("/message ") {
            return Ok(Some(Self::Message(text.trim().to_owned())));
        }
        if let Some(raw_line) = line.strip_prefix("/input ") {
            return Ok(Some(Self::InputLine(raw_line.trim().to_owned())));
        }
        if let Some(raw_lines) = line.strip_prefix("/execute ") {
            return parse_execute_command(raw_lines.trim()).map(Some);
        }
        if line.starts_with('/') {
            bail!("unknown stdin command: {line}");
        }
        if line.starts_with('{') {
            return parse_json_stdin_event(line).map(Some);
        }
        Ok(Some(Self::Message(line.to_owned())))
    }
}

fn parse_execute_command(value: &str) -> Result<StdinEvent> {
    if value.starts_with('[') {
        let lines =
            serde_json::from_str::<Vec<String>>(value).context("parse /execute JSON array")?;
        return Ok(StdinEvent::InputLines(lines));
    }
    Ok(StdinEvent::InputLine(value.to_owned()))
}

fn parse_json_stdin_event(line: &str) -> Result<StdinEvent> {
    let value = serde_json::from_str::<Value>(line).context("parse stdin JSON event")?;
    match value.get("type").and_then(Value::as_str) {
        Some("message") => {
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .context("stdin message event requires string field `text`")?;
            Ok(StdinEvent::Message(text.to_owned()))
        }
        Some("input_line") => {
            let raw_line = value
                .get("line")
                .and_then(Value::as_str)
                .context("stdin input_line event requires string field `line`")?;
            Ok(StdinEvent::InputLine(raw_line.to_owned()))
        }
        Some("execute") => {
            let lines = value
                .get("input_lines")
                .and_then(Value::as_array)
                .context("stdin execute event requires array field `input_lines`")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .context("stdin execute input_lines must be strings")
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(StdinEvent::InputLines(lines))
        }
        Some("quit" | "exit") => Ok(StdinEvent::Quit),
        _ => Ok(StdinEvent::InputLine(line.to_owned())),
    }
}

#[derive(Debug)]
pub(crate) struct SseFrame {
    pub(crate) id: Option<String>,
    pub(crate) event: String,
    pub(crate) data: String,
}

impl SseFrame {
    pub(crate) fn parse(frame: &str) -> Option<Self> {
        let frame = frame.replace("\r\n", "\n");
        let mut id = None;
        let mut event = "message".to_owned();
        let mut data = Vec::new();

        for line in frame.lines() {
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(value) = line.strip_prefix("id:") {
                id = Some(value.trim_start().to_owned());
            } else if let Some(value) = line.strip_prefix("event:") {
                event = value.trim_start().to_owned();
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push(value.trim_start().to_owned());
            }
        }

        if data.is_empty() {
            return None;
        }

        Some(Self {
            id,
            event,
            data: data.join("\n"),
        })
    }
}
