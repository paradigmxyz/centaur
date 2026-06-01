use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use reqwest::Client;
use serde_json::Value;
use tokio::{
    sync::mpsc,
    time::{self, Duration as TokioDuration},
};

use crate::{
    SseFrame, StdinEvent, append_user_message, execute_input_lines, is_terminal_event, next_frame,
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
    client: Client,
    thread_key: String,
    session_url: String,
    events_response: reqwest::Response,
    options: TuiOptions,
) -> Result<()> {
    let _terminal_restore = TerminalRestore::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let (terminal_tx, mut terminal_rx) = mpsc::channel(64);
    let _terminal_reader = TerminalEventReader::spawn(terminal_tx);
    let (frame_tx, mut frame_rx) = mpsc::channel(256);
    let (notice_tx, mut notice_rx) = mpsc::channel(64);

    spawn_stream_task(events_response, frame_tx, notice_tx.clone());

    let mut app = TuiApp::new(thread_key, options.debug_visible);
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
                    &session_url,
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
    response: reqwest::Response,
    frame_tx: mpsc::Sender<SseFrame>,
    notice_tx: mpsc::Sender<TuiNotice>,
) {
    tokio::spawn(async move {
        if let Err(error) = stream_sse_frames(response, frame_tx).await {
            let _ = notice_tx
                .send(TuiNotice::Error(format!("stream error: {error:#}")))
                .await;
        }
    });
}

async fn stream_sse_frames(
    response: reqwest::Response,
    frame_tx: mpsc::Sender<SseFrame>,
) -> Result<()> {
    let mut chunks = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.context("read event stream")?;
        buffer.push_str(std::str::from_utf8(&chunk).context("event stream is not UTF-8")?);

        while let Some((frame_end, separator_len)) = next_frame(&buffer) {
            let frame = buffer[..frame_end].to_owned();
            buffer.drain(..frame_end + separator_len);

            if let Some(event) = SseFrame::parse(&frame) {
                if frame_tx.send(event).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

fn handle_terminal_event(
    app: &mut TuiApp,
    event: TerminalInput,
    client: &Client,
    session_url: &str,
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
            return submit_input_line(app, line, client, session_url, options, notice_tx);
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(ch) => {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                app.input.push(ch);
            }
        }
        _ => {}
    }

    Ok(false)
}

fn submit_input_line(
    app: &mut TuiApp,
    line: String,
    client: &Client,
    session_url: &str,
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
        session_url.to_owned(),
        options.idle_timeout_ms,
        options.max_duration_ms,
        notice_tx,
    );
    Ok(false)
}

fn spawn_send_task(
    event: StdinEvent,
    client: Client,
    session_url: String,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
    notice_tx: mpsc::Sender<TuiNotice>,
) {
    tokio::spawn(async move {
        let notice =
            match send_stdin_event(event, client, session_url, idle_timeout_ms, max_duration_ms)
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
    client: Client,
    session_url: String,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Result<String> {
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
            Ok("message sent".to_owned())
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
            Ok("input line sent".to_owned())
        }
        StdinEvent::InputLines(lines) => {
            let count = lines.len();
            execute_input_lines(
                &client,
                &session_url,
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
        enable_raw_mode().context("enable terminal raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, Hide).context("enter alternate screen")?;
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
            main_lines: vec![
                "session ready".to_owned(),
                "type a message and press Enter; use F2 for raw events".to_owned(),
            ],
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
                self.push_main_line("system: sending raw input line".to_owned());
                self.status = "sending raw input line".to_owned();
            }
            StdinEvent::InputLines(lines) => {
                self.push_main_line(format!("system: sending {} raw input lines", lines.len()));
                self.status = "sending raw input lines".to_owned();
            }
            StdinEvent::Quit => {}
        }
    }

    fn handle_notice(&mut self, notice: TuiNotice) {
        match notice {
            TuiNotice::Info(message) => self.status = message,
            TuiNotice::Error(message) => {
                self.status = format!("error: {message}");
                self.push_main_line(format!("error: {message}"));
            }
        }
    }

    fn handle_sse_frame(&mut self, frame: SseFrame, options: &TuiOptions) -> bool {
        self.last_event_id = frame.id.clone();
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
            self.push_main_line("system: execution started".to_owned());
            self.status = "execution started".to_owned();
        } else if is_terminal_event(&frame.event) {
            self.current_agent_line = None;
            self.push_main_line(format!("system: {}", frame.event));
            self.status = frame.event.clone();
            if options.exit_on_terminal {
                return true;
            }
        }

        false
    }

    fn handle_output_line(&mut self, data: &str) {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            self.push_main_line(format!("stdout: {data}"));
            return;
        };

        let Some(event_type) = value.get("type").and_then(Value::as_str) else {
            self.push_main_line(format!("stdout: {}", compact_json(&value, 240)));
            return;
        };

        match event_type {
            "item.agentMessage.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    self.append_agent_delta(delta);
                }
            }
            "item.completed" => {
                if let Some(text) = completed_agent_text(&value) {
                    if self.current_agent_line.is_none() {
                        self.push_main_line(format!("assistant: {text}"));
                    }
                    self.current_agent_line = None;
                }
            }
            "turn.completed" => {
                self.current_agent_line = None;
                self.push_main_line("system: turn completed".to_owned());
                self.status = "turn completed".to_owned();
            }
            "result" => {
                self.current_agent_line = None;
                if let Some(result) = value.get("result").and_then(Value::as_str) {
                    self.push_main_line(format!("result: {result}"));
                } else {
                    self.push_main_line(format!("result: {}", compact_json(&value, 240)));
                }
            }
            "system" => {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("event");
                self.push_main_line(format!("system: {subtype}"));
            }
            other => {
                self.push_main_line(format!("event: {other}"));
            }
        }
    }

    fn append_agent_delta(&mut self, delta: &str) {
        if self.current_agent_line.is_none() {
            self.push_main_line("assistant: ".to_owned());
            self.current_agent_line = self.main_lines.len().checked_sub(1);
        }
        if let Some(index) = self.current_agent_line {
            if let Some(line) = self.main_lines.get_mut(index) {
                line.push_str(delta);
            }
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

fn completed_agent_text(value: &Value) -> Option<&str> {
    let item = value.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
        return None;
    }
    item.get("text").and_then(Value::as_str)
}

fn format_debug_frame(frame: &SseFrame) -> String {
    let id = frame.id.as_deref().unwrap_or("-");
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
