use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use centaur_api_server::client::{CentaurClient, SseEventStream};
use centaur_session_core::ThreadKey;
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::Value;
use tokio::{
    sync::mpsc,
    time::{self, Duration as TokioDuration},
};

use crate::{
    SseFrame, StdinEvent, append_user_message, execute_input_lines, is_terminal_event,
    output_type_matches, parse_json_or_string, user_input_line,
};

const MAX_MAIN_LINES: usize = 1_000;
const MAX_DEBUG_LINES: usize = 1_000;

pub(crate) struct TuiOptions {
    pub(crate) debug_visible: bool,
    pub(crate) idle_timeout_ms: u64,
    pub(crate) max_duration_ms: u64,
    pub(crate) exit_on_terminal: bool,
    pub(crate) exit_on_output_type: Option<String>,
}

pub(crate) async fn run(
    client: CentaurClient,
    thread_key: ThreadKey,
    events: SseEventStream,
    options: TuiOptions,
) -> Result<()> {
    let _terminal_restore = TerminalRestore::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let (terminal_tx, mut terminal_rx) = mpsc::channel(64);
    let _terminal_reader = TerminalEventReader::spawn(terminal_tx);
    let (frame_tx, mut frame_rx) = mpsc::channel(256);
    let (notice_tx, mut notice_rx) = mpsc::channel(64);

    spawn_stream_task(events, frame_tx, notice_tx.clone());

    let mut app = TuiApp::new(thread_key.to_string(), options.debug_visible);
    let mut tick = time::interval(TokioDuration::from_millis(100));

    loop {
        terminal.draw(|frame| draw(frame, &app))?;

        tokio::select! {
            _ = tick.tick() => {}
            Some(event) = terminal_rx.recv() => {
                if handle_terminal_event(
                    &mut app,
                    event,
                    &client,
                    &thread_key,
                    &options,
                    notice_tx.clone(),
                )? {
                    break;
                }
            }
            Some(frame) = frame_rx.recv() => {
                if app.handle_sse_frame(frame, &options) {
                    break;
                }
            }
            Some(notice) = notice_rx.recv() => {
                app.handle_notice(notice);
            }
            else => break,
        }
    }

    Ok(())
}

fn spawn_stream_task(
    stream: SseEventStream,
    frame_tx: mpsc::Sender<SseFrame>,
    notice_tx: mpsc::Sender<TuiNotice>,
) {
    tokio::spawn(async move {
        if let Err(error) = stream_sse_frames(stream, frame_tx).await {
            let _ = notice_tx
                .send(TuiNotice::Error(format!("stream error: {error:#}")))
                .await;
        }
    });
}

async fn stream_sse_frames(
    mut stream: SseEventStream,
    frame_tx: mpsc::Sender<SseFrame>,
) -> Result<()> {
    while let Some(frame) = stream.next().await {
        let frame = frame.wrap_err("read event stream")?;
        if frame_tx.send(frame).await.is_err() {
            return Ok(());
        }
    }

    Ok(())
}

fn handle_terminal_event(
    app: &mut TuiApp,
    event: TerminalInput,
    client: &CentaurClient,
    thread_key: &ThreadKey,
    options: &TuiOptions,
    notice_tx: mpsc::Sender<TuiNotice>,
) -> Result<bool> {
    let TerminalInput::Key(key) = event else {
        return Ok(false);
    };

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.clear(),
        KeyCode::F(2) => app.toggle_debug(),
        KeyCode::Enter => {
            let line = app.input.trim().to_owned();
            app.input.clear();
            return submit_input_line(app, line, client, thread_key, options, notice_tx);
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            app.input.push(ch);
        }
        _ => {}
    }

    Ok(false)
}

fn submit_input_line(
    app: &mut TuiApp,
    line: String,
    client: &CentaurClient,
    thread_key: &ThreadKey,
    options: &TuiOptions,
    notice_tx: mpsc::Sender<TuiNotice>,
) -> Result<bool> {
    if line.is_empty() {
        return Ok(false);
    }
    match line.as_str() {
        "/debug" => {
            app.toggle_debug();
            return Ok(false);
        }
        "/clear" => {
            app.clear();
            return Ok(false);
        }
        "/help" => {
            app.status =
                "Enter sends | F2 or /debug toggles events | /input raw | /quit exits".to_owned();
            return Ok(false);
        }
        _ => {}
    }

    let Some(event) = StdinEvent::parse(&line)? else {
        return Ok(false);
    };
    if matches!(event, StdinEvent::Quit) {
        return Ok(true);
    }

    app.note_submitted_event(&event);
    spawn_send_task(
        event,
        client.clone(),
        thread_key.clone(),
        options.idle_timeout_ms,
        options.max_duration_ms,
        notice_tx,
    );
    Ok(false)
}

fn spawn_send_task(
    event: StdinEvent,
    client: CentaurClient,
    thread_key: ThreadKey,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
    notice_tx: mpsc::Sender<TuiNotice>,
) {
    tokio::spawn(async move {
        let notice =
            match send_stdin_event(event, client, thread_key, idle_timeout_ms, max_duration_ms)
                .await
            {
                Ok(message) => TuiNotice::Info(message),
                Err(error) => TuiNotice::Error(format!("{error:#}")),
            };
        let _ = notice_tx.send(notice).await;
    });
}

async fn send_stdin_event(
    event: StdinEvent,
    client: CentaurClient,
    thread_key: ThreadKey,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Result<String> {
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
            Ok("message sent".to_owned())
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
            Ok("input line sent".to_owned())
        }
        StdinEvent::InputLines(lines) => {
            let count = lines.len();
            execute_input_lines(
                &client,
                &thread_key,
                lines,
                idle_timeout_ms,
                max_duration_ms,
            )
            .await?;
            Ok(format!("{count} input lines sent"))
        }
        StdinEvent::Quit => Ok("quit".to_owned()),
    }
}

fn draw(frame: &mut Frame<'_>, app: &TuiApp) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, layout[0], app);
    draw_body(frame, layout[1], app);
    draw_input(frame, layout[2], app);
    draw_footer(frame, layout[3], app);
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let debug_state = if app.debug_visible {
        "debug:on"
    } else {
        "debug:off"
    };
    let last_event = app.last_event_id.as_deref().unwrap_or("-");
    let title = Line::from(vec![
        Span::styled(
            "Centaur Session",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(app.thread_key.clone(), Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::raw(format!("event:{last_event}  {debug_state}")),
    ]);
    let help = Line::from("Enter send  F2 debug  Ctrl-U clear input  Ctrl-D quit  /help");
    frame.render_widget(
        Paragraph::new(vec![title, help]).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn draw_body(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    if app.debug_visible {
        if area.width >= 100 {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(area);
            draw_main(frame, chunks[0], app);
            draw_debug(frame, chunks[1], app);
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(area);
            draw_main(frame, chunks[0], app);
            draw_debug(frame, chunks[1], app);
        }
    } else {
        draw_main(frame, area, app);
    }
}

fn draw_main(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let lines = tail_lines(&app.main_lines, area.height.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("Session").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_debug(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let lines = tail_lines(&app.debug_lines, area.height.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("Events").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_input(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let visible = visible_input(&app.input, area.width.saturating_sub(2) as usize);
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(visible.chars().count() as u16)
        .min(area.x.saturating_add(area.width.saturating_sub(2)));
    let cursor_y = area.y.saturating_add(1);

    frame.render_widget(
        Paragraph::new(visible)
            .block(Block::default().title("Input").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    frame.render_widget(
        Paragraph::new(app.status.clone()).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn tail_lines(lines: &[String], max: usize) -> Vec<Line<'static>> {
    let start = lines.len().saturating_sub(max.max(1));
    lines[start..]
        .iter()
        .map(|line| Line::raw(line.clone()))
        .collect()
}

fn visible_input(input: &str, max_chars: usize) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(max_chars.max(1));
    chars[start..].iter().collect()
}

#[derive(Debug)]
enum TerminalInput {
    Key(KeyEvent),
    Resize,
}

struct TerminalEventReader {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TerminalEventReader {
    fn spawn(sender: mpsc::Sender<TerminalInput>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !reader_stop.load(Ordering::Relaxed) {
                let Ok(has_event) = event::poll(Duration::from_millis(100)) else {
                    continue;
                };
                if !has_event {
                    continue;
                }
                match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if sender.blocking_send(TerminalInput::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(_, _)) => {
                        if sender.blocking_send(TerminalInput::Resize).is_err() {
                            break;
                        }
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for TerminalEventReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TerminalRestore;

impl TerminalRestore {
    fn enter() -> Result<Self> {
        enable_raw_mode().wrap_err("enable terminal raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, Hide).wrap_err("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
    }
}

enum TuiNotice {
    Info(String),
    Error(String),
}

struct TuiApp {
    thread_key: String,
    input: String,
    main_lines: Vec<String>,
    debug_lines: Vec<String>,
    debug_visible: bool,
    status: String,
    last_event_id: Option<String>,
    current_agent_line: Option<usize>,
}

impl TuiApp {
    fn new(thread_key: String, debug_visible: bool) -> Self {
        Self {
            thread_key,
            input: String::new(),
            main_lines: Vec::new(),
            debug_lines: Vec::new(),
            debug_visible,
            status: "ready".to_owned(),
            last_event_id: None,
            current_agent_line: None,
        }
    }

    fn clear(&mut self) {
        self.main_lines.clear();
        self.debug_lines.clear();
        self.current_agent_line = None;
        self.status = "cleared".to_owned();
    }

    fn toggle_debug(&mut self) {
        self.debug_visible = !self.debug_visible;
        self.status = if self.debug_visible {
            "debug events visible".to_owned()
        } else {
            "debug events hidden".to_owned()
        };
    }

    fn note_submitted_event(&mut self, event: &StdinEvent) {
        match event {
            StdinEvent::Message(text) => {
                self.push_main_line(format!("you: {text}"));
                self.status = "sending message".to_owned();
            }
            StdinEvent::InputLine(_) => {
                self.status = "sending raw input line".to_owned();
            }
            StdinEvent::InputLines(lines) => {
                self.status = format!("sending {} raw input lines", lines.len());
            }
            StdinEvent::Quit => {}
        }
    }

    fn handle_notice(&mut self, notice: TuiNotice) {
        match notice {
            TuiNotice::Info(message) => self.status = message,
            TuiNotice::Error(message) => {
                self.status = format!("error: {message}");
            }
        }
    }

    fn handle_sse_frame(&mut self, frame: SseFrame, options: &TuiOptions) -> bool {
        self.last_event_id = (!frame.id.is_empty()).then_some(frame.id.clone());
        self.push_debug_line(format_debug_frame(&frame));

        if frame.event == "session.output.line" {
            self.handle_output_line(&frame.data);
            if output_type_matches(&frame.data, options.exit_on_output_type.as_deref()) {
                self.status = format!(
                    "matched output type {}",
                    options.exit_on_output_type.as_deref().unwrap_or_default()
                );
                return true;
            }
        } else if frame.event == "session.execution_started" {
            self.status = "execution started".to_owned();
        } else if is_terminal_event(&frame.event) {
            self.current_agent_line = None;
            self.status = frame.event.clone();
            if options.exit_on_terminal {
                return true;
            }
        }

        false
    }

    fn handle_output_line(&mut self, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            self.status = "received non-json output".to_owned();
            return;
        };

        let Some(event_type) = output_event_name(&value) else {
            self.status = "received output event".to_owned();
            return;
        };

        match event_type {
            "remoteControl/status/changed" => {
                if let Some(status) = value
                    .get("params")
                    .and_then(|params| params.get("status"))
                    .and_then(Value::as_str)
                {
                    self.status = format!("remote control: {status}");
                }
            }
            "thread/started" => {
                if let Some(thread_id) = value
                    .get("params")
                    .and_then(|params| params.get("thread"))
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                {
                    self.status = format!("codex thread {thread_id}");
                } else {
                    self.status = "codex thread started".to_owned();
                }
            }
            "thread/status/changed" => {
                if let Some(status) = value
                    .get("params")
                    .and_then(|params| params.get("status"))
                    .and_then(|status| status.get("type"))
                    .and_then(Value::as_str)
                {
                    self.status = format!("thread {status}");
                }
            }
            "turn/started" | "turn.started" => {
                self.current_agent_line = None;
                self.status = "turn started".to_owned();
            }
            "item/started" | "item.started" => {
                self.handle_item_event(&value, false);
            }
            "item/completed" | "item.completed" => {
                self.handle_item_event(&value, true);
            }
            "item/agentMessage/delta" | "item.agentMessage.delta" => {
                if let Some(delta) = app_server_delta(&value) {
                    self.append_agent_delta(delta);
                }
            }
            "thread/tokenUsage/updated" => {
                self.status = "token usage updated".to_owned();
            }
            "turn/completed" | "turn.completed" => {
                self.current_agent_line = None;
                self.status = "turn completed".to_owned();
            }
            "result" => {
                self.current_agent_line = None;
                self.status = "result received".to_owned();
            }
            "system" => {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("event");
                self.status = format!("system: {subtype}");
            }
            other => {
                self.status = format!("event: {other}");
            }
        }
    }

    fn handle_item_event(&mut self, value: &Value, completed: bool) {
        let Some(item) = app_server_item(value) else {
            return;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("userMessage") if completed || item.get("content").is_some() => {
                if let Some(text) = user_message_text(item) {
                    self.push_user_line_once(text);
                }
            }
            Some("agentMessage") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.is_empty() && self.current_agent_line.is_none() {
                        self.push_main_line(format!("assistant: {text}"));
                    }
                } else if !completed && self.current_agent_line.is_none() {
                    self.push_main_line("assistant: ".to_owned());
                    self.current_agent_line = self.main_lines.len().checked_sub(1);
                }
                if completed {
                    self.current_agent_line = None;
                }
            }
            _ => {}
        }
    }

    fn append_agent_delta(&mut self, delta: &str) {
        if self.current_agent_line.is_none() {
            self.push_main_line("assistant: ".to_owned());
            self.current_agent_line = self.main_lines.len().checked_sub(1);
        }
        if let Some(index) = self.current_agent_line
            && let Some(line) = self.main_lines.get_mut(index)
        {
            line.push_str(delta);
        }
    }

    fn push_user_line_once(&mut self, text: &str) {
        let line = format!("you: {text}");
        if !self
            .main_lines
            .iter()
            .rev()
            .take(8)
            .any(|existing| existing == &line)
        {
            self.push_main_line(line);
        }
    }

    fn push_main_line(&mut self, line: String) {
        self.main_lines.push(line);
        if self.main_lines.len() > MAX_MAIN_LINES {
            self.main_lines.remove(0);
            self.current_agent_line = self
                .current_agent_line
                .and_then(|index| index.checked_sub(1));
        }
    }

    fn push_debug_line(&mut self, line: String) {
        self.debug_lines.push(line);
        if self.debug_lines.len() > MAX_DEBUG_LINES {
            self.debug_lines.remove(0);
        }
    }
}

fn output_event_name(value: &Value) -> Option<&str> {
    value
        .get("method")
        .and_then(Value::as_str)
        .or_else(|| value.get("type").and_then(Value::as_str))
}

fn app_server_item(value: &Value) -> Option<&Value> {
    value
        .get("params")
        .and_then(|params| params.get("item"))
        .or_else(|| value.get("item"))
}

fn app_server_delta(value: &Value) -> Option<&str> {
    value
        .get("params")
        .and_then(|params| params.get("delta"))
        .and_then(Value::as_str)
        .or_else(|| value.get("delta").and_then(Value::as_str))
}

fn user_message_text(item: &Value) -> Option<&str> {
    item.get("content")?
        .as_array()?
        .iter()
        .find_map(|part| part.get("text").and_then(Value::as_str))
}

fn format_debug_frame(frame: &SseFrame) -> String {
    let id = if frame.id.is_empty() {
        "-"
    } else {
        frame.id.as_str()
    };
    let data = parse_json_or_string(&frame.data);
    format!(
        "{} {} {}",
        id,
        frame.event,
        truncate(&compact_json(&data, 1_000), 1_000)
    )
}

fn compact_json(value: &Value, max_chars: usize) -> String {
    let rendered = match value {
        Value::String(value) => value.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".to_owned()),
    };
    truncate(&rendered, max_chars)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_codex_app_server_events_into_transcript() {
        let mut app = TuiApp::new("cli:test".to_owned(), false);
        app.clear();

        for line in [
            r#"{"method":"thread/started","params":{"thread":{"id":"thread-1"}}}"#,
            r#"{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1"}}}"#,
            r#"{"method":"item/started","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"hello"}]}}}"#,
            r#"{"method":"item/completed","params":{"item":{"type":"userMessage","content":[{"type":"text","text":"hello"}]}}}"#,
            r#"{"method":"item/started","params":{"item":{"id":"agent-1","type":"agentMessage","text":""}}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"delta":"PO"}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"delta":"NG"}}"#,
            r#"{"method":"item/completed","params":{"item":{"id":"agent-1","type":"agentMessage","text":"PONG"}}}"#,
            r#"{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":10,"cachedInputTokens":4,"outputTokens":2,"totalTokens":12}}}}"#,
            r#"{"method":"turn/completed","params":{"turn":{"id":"turn-1"}}}"#,
        ] {
            app.handle_output_line(line);
        }

        assert_eq!(app.main_lines, vec!["you: hello", "assistant: PONG"]);
    }

    #[test]
    fn still_projects_legacy_type_delta_events() {
        let mut app = TuiApp::new("cli:test".to_owned(), false);
        app.clear();

        app.handle_output_line(r#"{"type":"item.agentMessage.delta","delta":"P"}"#);
        app.handle_output_line(r#"{"type":"item.agentMessage.delta","delta":"ONG"}"#);
        app.handle_output_line(r#"{"type":"turn.completed"}"#);

        assert_eq!(app.main_lines, vec!["assistant: PONG"]);
    }

    #[test]
    fn leaves_non_message_events_out_of_transcript() {
        let mut app = TuiApp::new("cli:test".to_owned(), false);

        for line in [
            r#"{"type":"system","subtype":"wrapper_heartbeat"}"#,
            r#"{"method":"thread/started","params":{"thread":{"id":"thread-1"}}}"#,
            r#"{"method":"turn/started","params":{"turn":{"id":"turn-1"}}}"#,
            r#"{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"totalTokens":12}}}}"#,
            r#"{"method":"turn/completed","params":{"turn":{"id":"turn-1"}}}"#,
        ] {
            app.handle_output_line(line);
        }

        assert!(app.main_lines.is_empty());
    }
}
