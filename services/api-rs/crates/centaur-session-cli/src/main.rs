use std::str::FromStr;

use centaur_api_server::{
    client::{CentaurClient, SseEvent as ApiSseEvent, SseEventStream},
    types::{AppendMessagesRequest, CreateSessionRequest, ExecuteSessionRequest},
};
use centaur_session_core::{HarnessType, MessageRole, SessionMessageInput, ThreadKey};
use clap::{Parser, ValueEnum};
use eyre::{Result, WrapErr, bail, eyre};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::{
    io::{self, AsyncBufReadExt, BufReader},
    task::JoinHandle,
};
use uuid::Uuid;

mod tui;

const DEFAULT_MESSAGE: &str = "Reply with exactly PONG and nothing else.";

pub(crate) type SseFrame = ApiSseEvent;

#[derive(Debug, Parser)]
#[command(about = "Create, execute, or attach to a Centaur session")]
struct Args {
    #[arg(long, env = "CENTAUR_API_URL", default_value = "http://127.0.0.1:8080")]
    api_url: ApiBaseUrl,

    #[arg(long)]
    thread_key: Option<ThreadKeyArg>,

    #[arg(long)]
    attach: bool,

    #[arg(long, value_enum, default_value = "codex")]
    harness_type: HarnessTypeArg,

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
    exit_on_output_type: Option<OutputEventType>,

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
    if generated_thread_key {
        eprintln!("thread_key={}", thread_key.as_str());
    }
    let client = CentaurClient::new(args.api_url.as_str());

    if attach_mode {
        let events = client
            .stream_events(&thread_key, args.after_event_id)
            .await
            .wrap_err("open event stream")?;
        if args.tui {
            return tui::run(client, thread_key, events, tui_options(&args)).await;
        }
        return run_stream_and_optional_stdin(
            client,
            thread_key,
            events,
            stream_run_options(&args),
        )
        .await;
    }

    client
        .create_session(
            &thread_key,
            CreateSessionRequest {
                harness_type: args.harness_type.into(),
                persona_id: None,
                metadata: Some(json!({
                    "source": "centaur-session-cli",
                })),
            },
        )
        .await
        .wrap_err("create session")?;

    let initial_input_lines = if should_send_initial_turn(&args) {
        let input_lines = session_input_lines(&args)?;
        let message = message_text(&args);
        append_user_message(&client, &thread_key, message)
            .await
            .wrap_err("append message")?;
        Some(input_lines)
    } else {
        None
    };

    let events = client
        .stream_events(&thread_key, args.after_event_id)
        .await
        .wrap_err("open event stream")?;

    if let Some(input_lines) = initial_input_lines {
        execute_input_lines(
            &client,
            &thread_key,
            input_lines,
            args.idle_timeout_ms,
            args.max_duration_ms,
        )
        .await
        .wrap_err("execute initial turn")?;
    }

    if args.tui {
        return tui::run(client, thread_key, events, tui_options(&args)).await;
    }

    run_stream_and_optional_stdin(client, thread_key, events, stream_run_options(&args)).await
}

fn tui_options(args: &Args) -> tui::TuiOptions {
    tui::TuiOptions {
        debug_visible: args.debug,
        idle_timeout_ms: args.idle_timeout_ms,
        max_duration_ms: args.max_duration_ms,
        exit_on_terminal: args.exit_on_terminal,
        exit_on_output_type: args
            .exit_on_output_type
            .as_ref()
            .map(OutputEventType::as_str)
            .map(ToOwned::to_owned),
    }
}

fn stream_run_options(args: &Args) -> StreamRunOptions {
    StreamRunOptions {
        all_events: args.all_events,
        exit_on_terminal: args.exit_on_terminal,
        exit_on_output_type: args.exit_on_output_type.clone(),
        stdin_events: args.stdin_events,
        idle_timeout_ms: args.idle_timeout_ms,
        max_duration_ms: args.max_duration_ms,
    }
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

fn thread_key_arg(args: &Args, attach_mode: bool) -> Result<(ThreadKey, bool)> {
    match (&args.thread_key, attach_mode) {
        (Some(thread_key), _) => Ok((thread_key.clone().into_thread_key(), false)),
        (None, true) => bail!("--attach requires --thread-key"),
        (None, false) => Ok((
            ThreadKey::parse(format!("cli:{}", Uuid::new_v4().simple()))?,
            true,
        )),
    }
}

pub(crate) async fn append_user_message(
    client: &CentaurClient,
    thread_key: &ThreadKey,
    text: &str,
) -> Result<()> {
    client
        .append_messages(
            thread_key,
            AppendMessagesRequest {
                messages: vec![SessionMessageInput {
                    client_message_id: None,
                    role: MessageRole::User,
                    parts: vec![json!({"type": "text", "text": text})],
                    metadata: json!({
                        "source": "centaur-session-cli",
                    }),
                }],
            },
        )
        .await?;
    Ok(())
}

pub(crate) async fn execute_input_lines(
    client: &CentaurClient,
    thread_key: &ThreadKey,
    input_lines: Vec<String>,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Result<()> {
    client
        .execute_session(
            thread_key,
            ExecuteSessionRequest {
                idempotency_key: None,
                metadata: Some(json!({
                    "source": "centaur-session-cli",
                })),
                input_lines,
                idle_timeout_ms: Some(idle_timeout_ms),
                max_duration_ms: Some(max_duration_ms),
            },
        )
        .await?;
    Ok(())
}

async fn run_stream_and_optional_stdin(
    client: CentaurClient,
    thread_key: ThreadKey,
    events: SseEventStream,
    options: StreamRunOptions,
) -> Result<()> {
    let stream_future = stream_output_lines(
        events,
        options.all_events,
        options.exit_on_terminal,
        options.exit_on_output_type,
    );
    tokio::pin!(stream_future);

    if !options.stdin_events {
        return stream_future.await;
    }

    let mut stdin_task = spawn_stdin_events(
        client,
        thread_key,
        options.idle_timeout_ms,
        options.max_duration_ms,
    );

    tokio::select! {
        stream_result = &mut stream_future => {
            stdin_task.abort();
            stream_result
        }
        stdin_result = &mut stdin_task => {
            stdin_result.wrap_err("join stdin event task")??;
            stream_future.await
        }
    }
}

#[derive(Clone, Debug)]
struct StreamRunOptions {
    all_events: bool,
    exit_on_terminal: bool,
    exit_on_output_type: Option<OutputEventType>,
    stdin_events: bool,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
}

fn spawn_stdin_events(
    client: CentaurClient,
    thread_key: ThreadKey,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(io::stdin()).lines();
        while let Some(line) = lines.next_line().await.wrap_err("read stdin event")? {
            let event = match StdinEvent::parse(&line)? {
                Some(event) => event,
                None => continue,
            };
            match event {
                StdinEvent::Message(text) => {
                    append_user_message(&client, &thread_key, &text).await?;
                    execute_input_lines(
                        &client,
                        &thread_key,
                        vec![user_input_line(&text)?],
                        idle_timeout_ms,
                        max_duration_ms,
                    )
                    .await?;
                }
                StdinEvent::InputLine(line) => {
                    execute_input_lines(
                        &client,
                        &thread_key,
                        vec![line],
                        idle_timeout_ms,
                        max_duration_ms,
                    )
                    .await?;
                }
                StdinEvent::InputLines(lines) => {
                    execute_input_lines(
                        &client,
                        &thread_key,
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
    mut events: SseEventStream,
    all_events: bool,
    exit_on_terminal: bool,
    exit_on_output_type: Option<OutputEventType>,
) -> Result<()> {
    while let Some(event) = events.next().await {
        let event = event.wrap_err("read event stream")?;

        if event.event == "session.output.line" {
            println!("{}\t{}", event_id_or_unknown(&event.id), event.data);
            if output_type_matches(
                &event.data,
                exit_on_output_type.as_ref().map(|value| value.as_str()),
            ) {
                return Ok(());
            }
        } else if all_events {
            let data = parse_json_or_string(&event.data);
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "sse_event": event.event,
                    "id": optional_event_id(&event.id),
                    "data": data,
                }))?
            );
        }

        if exit_on_terminal && is_terminal_event(&event.event) {
            return Ok(());
        }
    }

    Ok(())
}

fn event_id_or_unknown(event_id: &str) -> &str {
    optional_event_id(event_id).unwrap_or("unknown")
}

fn optional_event_id(event_id: &str) -> Option<&str> {
    (!event_id.is_empty()).then_some(event_id)
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
            serde_json::from_str::<Vec<String>>(value).wrap_err("parse /execute JSON array")?;
        return Ok(StdinEvent::InputLines(lines));
    }
    Ok(StdinEvent::InputLine(value.to_owned()))
}

fn parse_json_stdin_event(line: &str) -> Result<StdinEvent> {
    let value = serde_json::from_str::<Value>(line).wrap_err("parse stdin JSON event")?;
    match value.get("type").and_then(Value::as_str) {
        Some("message") => {
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| eyre!("stdin message event requires string field `text`"))?;
            Ok(StdinEvent::Message(text.to_owned()))
        }
        Some("input_line") => {
            let raw_line = value
                .get("line")
                .and_then(Value::as_str)
                .ok_or_else(|| eyre!("stdin input_line event requires string field `line`"))?;
            Ok(StdinEvent::InputLine(raw_line.to_owned()))
        }
        Some("execute") => {
            let lines = value
                .get("input_lines")
                .and_then(Value::as_array)
                .ok_or_else(|| eyre!("stdin execute event requires array field `input_lines`"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| eyre!("stdin execute input_lines must be strings"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(StdinEvent::InputLines(lines))
        }
        Some("quit" | "exit") => Ok(StdinEvent::Quit),
        _ => Ok(StdinEvent::InputLine(line.to_owned())),
    }
}

#[derive(Clone, Debug)]
struct ApiBaseUrl(String);

impl ApiBaseUrl {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl FromStr for ApiBaseUrl {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim_end_matches('/');
        if value.is_empty() {
            return Err("api_url must not be empty".to_owned());
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug)]
struct ThreadKeyArg(ThreadKey);

impl ThreadKeyArg {
    fn into_thread_key(self) -> ThreadKey {
        self.0
    }
}

impl FromStr for ThreadKeyArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ThreadKey::parse(value)
            .map(Self)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum HarnessTypeArg {
    Codex,
    Amp,
    #[value(name = "claudecode")]
    ClaudeCode,
}

impl From<HarnessTypeArg> for HarnessType {
    fn from(value: HarnessTypeArg) -> Self {
        match value {
            HarnessTypeArg::Codex => Self::Codex,
            HarnessTypeArg::Amp => Self::Amp,
            HarnessTypeArg::ClaudeCode => Self::ClaudeCode,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OutputEventType(String);

impl OutputEventType {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl FromStr for OutputEventType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().is_empty() {
            return Err("output event type must not be empty".to_owned());
        }
        Ok(Self(value.to_owned()))
    }
}
