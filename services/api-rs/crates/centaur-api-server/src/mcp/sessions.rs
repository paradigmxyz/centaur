//! MCP tools that let an authenticated Console user drive their own coding
//! sessions without a chat surface. Vocabulary follows tmux: `new`, `list`,
//! `send`, `peek`, and `interrupt`.
//!
//! Each session lives under `mcpsession:<principal_oid>:<name>` and is bound
//! to the token's own principal, so its sandbox uses the same credentials and
//! tool policy as that user's MCP tool host. The principal segment in the
//! thread key scopes every lookup to the caller; the stored principal is
//! checked again before any read or write.

use std::time::Duration;

use centaur_session_core::{
    ExecutionStatus, HarnessType, MessageRole, Session, SessionEvent, SessionMessageInput,
    ThreadKey,
};
use centaur_session_runtime::{
    ExecuteSessionInput, HarnessConflictPolicy, SESSION_OUTPUT_LINE_EVENT, SessionRuntimeError,
};
use centaur_session_sqlx::SessionStoreError;
use futures_util::{StreamExt as _, pin_mut};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};
use tracing::Span;
use uuid::Uuid;

use super::{
    McpPrincipal, McpToolCallOutcome, invalid_mcp_v2_arguments, mcp_text_result,
    record_mcp_tool_correlation, record_mcp_tool_method, static_env,
};
use crate::{ApiError, routes::AppState};

pub(super) const MCP_SESSIONS_SCOPE: &str = "mcp:sessions";
const MCP_SESSION_NAMESPACE: &str = "mcpsession";
const MAX_SESSION_NAME_CHARS: usize = 64;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Stay well under common MCP client tool-call timeouts (Codex: 60s).
const MAX_PEEK_WAIT_MS: u64 = 25_000;
const DEFAULT_PEEK_LIMIT: usize = 20;
const MAX_PEEK_LIMIT: usize = 200;
const PEEK_PAGE_SIZE: i64 = 500;
/// Bound the history replayed by one peek so a long session cannot turn a
/// call into an unbounded scan; callers page forward with the cursor.
const MAX_PEEK_SCAN_EVENTS: usize = 20_000;
const DEFAULT_LIST_LIMIT: i64 = 20;
const MAX_LIST_LIMIT: i64 = 100;
const MAX_ENTRY_TEXT_CHARS: usize = 4_000;
const MAX_COMMAND_OUTPUT_CHARS: usize = 1_000;

const TOOL_NEW: &str = "centaur_session_new";
const TOOL_LIST: &str = "centaur_session_list";
const TOOL_SEND: &str = "centaur_session_send";
const TOOL_PEEK: &str = "centaur_session_peek";
const TOOL_INTERRUPT: &str = "centaur_session_interrupt";

pub(super) fn mcp_sessions_enabled() -> bool {
    static CELL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    static_env(&CELL, "CENTAUR_MCP_SESSIONS_ENABLED")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

pub(super) fn mcp_session_tool_name(name: &str) -> bool {
    matches!(
        name,
        TOOL_NEW | TOOL_LIST | TOOL_SEND | TOOL_PEEK | TOOL_INTERRUPT
    )
}

/// Session tools are listed only when the deployment enables them and the
/// token was granted the sessions scope.
pub(super) fn mcp_session_tools_visible(scopes: &[String]) -> bool {
    mcp_sessions_enabled() && has_sessions_scope(scopes)
}

pub(super) fn has_sessions_scope(scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|scope| scope == "*" || scope == "mcp:*" || scope == MCP_SESSIONS_SCOPE)
}

pub(super) fn mcp_session_tools() -> Vec<Value> {
    let session_property = json!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_SESSION_NAME_CHARS,
        "description": "Session name returned by centaur_session_new or centaur_session_list.",
    });
    vec![
        json!({
            "name": TOOL_NEW,
            "description": concat!(
                "Start a new Centaur coding session in its own sandbox and send it a first prompt. ",
                "The session runs as you, with the same credentials and tools as your Centaur DMs. ",
                "Returns immediately; use centaur_session_peek to watch progress."
            ),
            "inputSchema": {
                "type": "object",
                "required": ["prompt"],
                "additionalProperties": false,
                "properties": {
                    "prompt": {"type": "string", "minLength": 1},
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_SESSION_NAME_CHARS,
                        "pattern": "^[A-Za-z0-9][A-Za-z0-9._-]*$",
                        "description": "Optional stable name, for example a Linear issue ID. Generated when omitted. Fails if the name is already in use.",
                    },
                    "harness": {
                        "type": "string",
                        "enum": ["codex", "claudecode", "amp", "nanocodex", "pi", "omp", "hermes"],
                        "description": "Agent harness. Defaults to codex.",
                    },
                    "persona": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Optional persona ID. Defaults to the deployment default persona.",
                    },
                },
            },
        }),
        json!({
            "name": TOOL_LIST,
            "description": "List your MCP-started Centaur sessions, most recently active first, with each session's latest turn status.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT},
                },
            },
        }),
        json!({
            "name": TOOL_SEND,
            "description": concat!(
                "Send a message to a session. Starts a new turn when the session is idle; when a ",
                "turn is running the message is delivered to it as steering. Returns immediately."
            ),
            "inputSchema": {
                "type": "object",
                "required": ["session", "message"],
                "additionalProperties": false,
                "properties": {
                    "session": session_property,
                    "message": {"type": "string", "minLength": 1},
                },
            },
        }),
        json!({
            "name": TOOL_PEEK,
            "description": concat!(
                "Read a session's recent activity: agent messages, commands, file changes, tool ",
                "calls, and turn status. Pass the returned next_after_event_id back as ",
                "after_event_id to read only newer activity; when more_available is true, call ",
                "again with that cursor to continue. Set wait_ms to block until the running turn ",
                "finishes or the wait elapses."
            ),
            "inputSchema": {
                "type": "object",
                "required": ["session"],
                "additionalProperties": false,
                "properties": {
                    "session": session_property,
                    "after_event_id": {"type": "integer", "minimum": 0},
                    "wait_ms": {"type": "integer", "minimum": 0, "maximum": MAX_PEEK_WAIT_MS},
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_PEEK_LIMIT,
                        "description": "Maximum entries to return, keeping the most recent. Defaults to 20.",
                    },
                    "raw": {
                        "type": "boolean",
                        "description": "Return raw session events instead of the condensed view.",
                    },
                },
            },
        }),
        json!({
            "name": TOOL_INTERRUPT,
            "description": "Interrupt a session's running turn, like Ctrl-C. The session stays available for centaur_session_send.",
            "inputSchema": {
                "type": "object",
                "required": ["session"],
                "additionalProperties": false,
                "properties": {
                    "session": session_property,
                },
            },
        }),
    ]
}

pub(super) async fn mcp_session_tool_result(
    state: &AppState,
    principal: &McpPrincipal,
    name: &str,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    match name {
        TOOL_NEW => session_new(state, principal, arguments).await,
        TOOL_LIST => session_list(state, principal, arguments).await,
        TOOL_SEND => session_send(state, principal, arguments).await,
        TOOL_PEEK => session_peek(state, principal, arguments).await,
        TOOL_INTERRUPT => session_interrupt(state, principal, arguments).await,
        _ => unreachable!("session tool names are checked before dispatch"),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionNewArguments {
    prompt: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    harness: Option<HarnessType>,
    #[serde(default)]
    persona: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionListArguments {
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSendArguments {
    session: String,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPeekArguments {
    session: String,
    #[serde(default)]
    after_event_id: Option<i64>,
    #[serde(default)]
    wait_ms: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    raw: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionTargetArguments {
    session: String,
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(
    arguments: Value,
) -> Result<T, McpToolCallOutcome> {
    // MCP clients may send `null` or omit arguments for tools whose fields
    // are all optional.
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    serde_json::from_value(arguments).map_err(invalid_mcp_v2_arguments)
}

fn tool_error(message: impl Into<String>) -> McpToolCallOutcome {
    McpToolCallOutcome {
        result: mcp_text_result(message.into(), true),
        timed_out: false,
    }
}

fn tool_json(value: Value) -> Result<McpToolCallOutcome, ApiError> {
    Ok(McpToolCallOutcome {
        result: mcp_text_result(serde_json::to_string_pretty(&value)?, false),
        timed_out: false,
    })
}

fn session_thread_key_prefix(principal_id: &str) -> Result<String, ApiError> {
    let principal_id = principal_id.trim();
    if principal_id.is_empty() || principal_id.contains(':') {
        return Err(ApiError::Forbidden(
            "MCP principal cannot own sessions".to_owned(),
        ));
    }
    Ok(format!("{MCP_SESSION_NAMESPACE}:{principal_id}:"))
}

fn validate_session_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("session name must not be empty".to_owned());
    };
    if name.chars().count() > MAX_SESSION_NAME_CHARS {
        return Err(format!(
            "session name must be at most {MAX_SESSION_NAME_CHARS} characters"
        ));
    }
    if !first.is_ascii_alphanumeric()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(
            "session name must start with a letter or digit and contain only letters, digits, '.', '_', or '-'"
                .to_owned(),
        );
    }
    Ok(())
}

/// Resolve a caller-supplied session name to the caller's own thread key.
/// Returns a tool error for malformed names so the model can correct itself.
fn session_thread_key(
    principal: &McpPrincipal,
    name: &str,
) -> Result<Result<ThreadKey, McpToolCallOutcome>, ApiError> {
    let name = name.trim();
    if let Err(message) = validate_session_name(name) {
        return Ok(Err(invalid_mcp_v2_arguments(message)));
    }
    let prefix = session_thread_key_prefix(&principal.principal_id)?;
    Ok(Ok(ThreadKey::parse(format!("{prefix}{name}"))?))
}

fn session_name_from_thread_key<'a>(prefix: &str, thread_key: &'a ThreadKey) -> &'a str {
    thread_key
        .as_str()
        .strip_prefix(prefix)
        .unwrap_or(thread_key.as_str())
}

/// Load a session only if it belongs to the caller. Unknown and foreign
/// sessions are indistinguishable to the caller.
async fn owned_session(
    state: &AppState,
    principal: &McpPrincipal,
    thread_key: &ThreadKey,
    name: &str,
) -> Result<Result<Session, McpToolCallOutcome>, ApiError> {
    match state.runtime()?.session(thread_key).await {
        Ok(session)
            if session.iron_control_principal.as_deref()
                == Some(principal.principal_id.as_str()) =>
        {
            Ok(Ok(session))
        }
        Ok(_) | Err(SessionRuntimeError::Store(SessionStoreError::NotFound { .. })) => {
            Ok(Err(tool_error(format!(
                "unknown session {name:?}; call centaur_session_list"
            ))))
        }
        Err(error) => Err(error.into()),
    }
}

fn user_input_line(text: &str) -> Result<String, ApiError> {
    Ok(serde_json::to_string(&json!({
        "type": "user",
        "message": {"content": [{"type": "text", "text": text}]},
    }))?)
}

fn mcp_session_metadata(principal: &McpPrincipal, action: &str) -> Value {
    json!({
        "source": "mcp",
        "platform": "mcp",
        "action": action,
        "mcp_token_id": principal.token_id,
    })
}

fn validate_message(field: &str, text: &str) -> Result<(), McpToolCallOutcome> {
    if text.trim().is_empty() {
        return Err(invalid_mcp_v2_arguments(format!(
            "{field} must not be blank"
        )));
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(invalid_mcp_v2_arguments(format!(
            "{field} must be at most {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Append a user message and enqueue a turn. The runtime steers the message
/// into an active turn instead of starting a second one.
async fn send_turn(
    state: &AppState,
    principal: &McpPrincipal,
    thread_key: &ThreadKey,
    text: &str,
    action: &str,
) -> Result<(String, String), ApiError> {
    let runtime = state.runtime()?;
    let request_id = format!("mcp-{}", Uuid::new_v4().simple());
    runtime
        .append_messages(
            thread_key,
            &[SessionMessageInput {
                client_message_id: Some(request_id.clone()),
                role: MessageRole::User,
                parts: vec![json!({"type": "text", "text": text})],
                metadata: mcp_session_metadata(principal, action),
            }],
        )
        .await?;
    let execution = runtime
        .enqueue_session_execution(
            thread_key,
            ExecuteSessionInput {
                idempotency_key: Some(request_id),
                metadata: Some(mcp_session_metadata(principal, action)),
                input_lines: vec![user_input_line(text)?],
                idle_timeout_ms: None,
                max_duration_ms: None,
            },
        )
        .await?;
    Ok((execution.execution_id, execution.status.to_string()))
}

async fn session_new(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match parse_arguments::<SessionNewArguments>(arguments) {
        Ok(args) => args,
        Err(outcome) => return Ok(outcome),
    };
    if let Err(outcome) = validate_message("prompt", &args.prompt) {
        return Ok(outcome);
    }
    record_mcp_tool_method(&Span::current(), TOOL_NEW, "new");
    let name = match args.name.as_deref().map(str::trim) {
        Some(name) => name.to_owned(),
        None => Uuid::new_v4().simple().to_string()[..12].to_owned(),
    };
    let thread_key = match session_thread_key(principal, &name)? {
        Ok(thread_key) => thread_key,
        Err(outcome) => return Ok(outcome),
    };
    let runtime = state.runtime()?;
    match runtime.session(&thread_key).await {
        Ok(_) => {
            return Ok(tool_error(format!(
                "session {name:?} already exists; use centaur_session_send to message it"
            )));
        }
        Err(SessionRuntimeError::Store(SessionStoreError::NotFound { .. })) => {}
        Err(error) => return Err(error.into()),
    }
    let harness = args.harness.unwrap_or(HarnessType::Codex);
    let outcome = runtime
        .create_or_get_session_with_principal(
            &thread_key,
            &harness,
            args.persona
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty()),
            Some(mcp_session_metadata(principal, "create")),
            HarnessConflictPolicy::Reject,
            Some(&principal.principal_id),
        )
        .await?;
    let (execution_id, status) =
        send_turn(state, principal, &thread_key, &args.prompt, "new").await?;
    record_mcp_tool_correlation(&Span::current(), None, Some(&execution_id), None);
    tool_json(json!({
        "session": name,
        "harness": outcome.session.harness_type,
        "persona": outcome.session.persona_id,
        "unavailable_requested_persona": outcome.unavailable_requested_persona_id,
        "execution_id": execution_id,
        "status": status,
    }))
}

async fn session_list(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match parse_arguments::<SessionListArguments>(arguments) {
        Ok(args) => args,
        Err(outcome) => return Ok(outcome),
    };
    record_mcp_tool_method(&Span::current(), TOOL_LIST, "list");
    let limit = args
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let prefix = session_thread_key_prefix(&principal.principal_id)?;
    let runtime = state.runtime()?;
    let sessions = runtime
        .list_principal_sessions(&prefix, &principal.principal_id, limit)
        .await?;
    let mut entries = Vec::with_capacity(sessions.len());
    for session in sessions {
        let execution = runtime.latest_execution(&session.thread_key).await?;
        entries.push(json!({
            "session": session_name_from_thread_key(&prefix, &session.thread_key),
            "title": session.title,
            "harness": session.harness_type,
            "persona": session.persona_id,
            "status": execution_state(execution.as_ref().map(|e| &e.status)),
            "execution_id": execution.as_ref().map(|e| e.execution_id.as_str()),
            "updated_at": format_time(session.updated_at),
        }));
    }
    tool_json(json!({ "sessions": entries }))
}

async fn session_send(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match parse_arguments::<SessionSendArguments>(arguments) {
        Ok(args) => args,
        Err(outcome) => return Ok(outcome),
    };
    if let Err(outcome) = validate_message("message", &args.message) {
        return Ok(outcome);
    }
    record_mcp_tool_method(&Span::current(), TOOL_SEND, "send");
    let thread_key = match session_thread_key(principal, &args.session)? {
        Ok(thread_key) => thread_key,
        Err(outcome) => return Ok(outcome),
    };
    if let Err(outcome) = owned_session(state, principal, &thread_key, &args.session).await? {
        return Ok(outcome);
    }
    let (execution_id, status) =
        send_turn(state, principal, &thread_key, &args.message, "send").await?;
    record_mcp_tool_correlation(&Span::current(), None, Some(&execution_id), None);
    tool_json(json!({
        "session": args.session.trim(),
        "execution_id": execution_id,
        "status": status,
    }))
}

async fn session_interrupt(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match parse_arguments::<SessionTargetArguments>(arguments) {
        Ok(args) => args,
        Err(outcome) => return Ok(outcome),
    };
    record_mcp_tool_method(&Span::current(), TOOL_INTERRUPT, "interrupt");
    let thread_key = match session_thread_key(principal, &args.session)? {
        Ok(thread_key) => thread_key,
        Err(outcome) => return Ok(outcome),
    };
    if let Err(outcome) = owned_session(state, principal, &thread_key, &args.session).await? {
        return Ok(outcome);
    }
    let outcome = state
        .runtime()?
        .interrupt_active_execution(&thread_key, "Interrupted from MCP")
        .await?;
    tool_json(json!({
        "session": args.session.trim(),
        "interrupted": outcome.interrupted,
        "execution_id": outcome.execution_id,
    }))
}

async fn session_peek(
    state: &AppState,
    principal: &McpPrincipal,
    arguments: Value,
) -> Result<McpToolCallOutcome, ApiError> {
    let args = match parse_arguments::<SessionPeekArguments>(arguments) {
        Ok(args) => args,
        Err(outcome) => return Ok(outcome),
    };
    if args.after_event_id.is_some_and(|id| id < 0) {
        return Ok(invalid_mcp_v2_arguments(
            "after_event_id must not be negative",
        ));
    }
    record_mcp_tool_method(&Span::current(), TOOL_PEEK, "peek");
    let thread_key = match session_thread_key(principal, &args.session)? {
        Ok(thread_key) => thread_key,
        Err(outcome) => return Ok(outcome),
    };
    if let Err(outcome) = owned_session(state, principal, &thread_key, &args.session).await? {
        return Ok(outcome);
    }
    let runtime = state.runtime()?;
    let limit = args
        .limit
        .unwrap_or(DEFAULT_PEEK_LIMIT)
        .clamp(1, MAX_PEEK_LIMIT);
    let wait = Duration::from_millis(args.wait_ms.unwrap_or(0).min(MAX_PEEK_WAIT_MS));
    let deadline = Instant::now() + wait;
    let mut view = PeekView::new(args.after_event_id.unwrap_or(0), limit, args.raw);

    // Replay durable history in bounded pages.
    loop {
        let page = runtime
            .list_events_after(&thread_key, view.cursor, PEEK_PAGE_SIZE)
            .await?;
        let exhausted = page.len() < PEEK_PAGE_SIZE as usize;
        for event in &page {
            view.push(event);
        }
        if exhausted {
            break;
        }
        if view.scanned >= MAX_PEEK_SCAN_EVENTS {
            view.more = true;
            break;
        }
    }

    let mut execution = runtime.latest_execution(&thread_key).await?;
    let running = execution
        .as_ref()
        .is_some_and(|execution| execution_active(&execution.status));
    if !wait.is_zero() && !view.more && running {
        // Long-poll for the running turn to finish, collecting activity as it
        // streams in.
        let events = runtime
            .stream_events(&thread_key, view.cursor, None)
            .await?;
        pin_mut!(events);
        // Stops at the deadline, at stream end, or after a terminal event.
        while let Ok(Some(event)) = timeout_at(deadline, events.next()).await {
            let event = event?;
            let terminal = is_terminal_event(&event.event_type);
            view.push(&event);
            if terminal {
                break;
            }
        }
        execution = runtime.latest_execution(&thread_key).await?;
    }

    if let Some(execution) = execution.as_ref() {
        record_mcp_tool_correlation(&Span::current(), None, Some(&execution.execution_id), None);
    }
    tool_json(json!({
        "session": args.session.trim(),
        "status": execution_state(execution.as_ref().map(|e| &e.status)),
        "execution_id": execution.as_ref().map(|e| e.execution_id.as_str()),
        "error": execution.as_ref().and_then(|e| e.error.as_deref()),
        "next_after_event_id": view.cursor,
        "more_available": view.more,
        "omitted_entries": view.omitted,
        "entries": view.entries,
    }))
}

/// Accumulates the most recent condensed entries while tracking the event
/// cursor across every scanned event, including ones that condense to nothing.
struct PeekView {
    cursor: i64,
    limit: usize,
    raw: bool,
    scanned: usize,
    omitted: usize,
    more: bool,
    entries: std::collections::VecDeque<Value>,
}

impl PeekView {
    fn new(cursor: i64, limit: usize, raw: bool) -> Self {
        Self {
            cursor,
            limit,
            raw,
            scanned: 0,
            omitted: 0,
            more: false,
            entries: std::collections::VecDeque::new(),
        }
    }

    fn push(&mut self, event: &SessionEvent) {
        self.cursor = self.cursor.max(event.event_id);
        self.scanned += 1;
        let entry = if self.raw {
            Some(raw_event_entry(event))
        } else {
            condense_event(event)
        };
        if let Some(entry) = entry {
            if self.entries.len() == self.limit {
                self.entries.pop_front();
                self.omitted += 1;
            }
            self.entries.push_back(entry);
        }
    }
}

fn raw_event_entry(event: &SessionEvent) -> Value {
    json!({
        "event_id": event.event_id,
        "type": event.event_type,
        "execution_id": event.execution_id,
        "payload": event.payload,
    })
}

fn is_terminal_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "session.execution_completed" | "session.execution_failed" | "session.execution_cancelled"
    )
}

fn execution_active(status: &ExecutionStatus) -> bool {
    matches!(status, ExecutionStatus::Queued | ExecutionStatus::Running)
}

fn execution_state(status: Option<&ExecutionStatus>) -> String {
    match status {
        None => "idle".to_owned(),
        Some(status) => status.to_string(),
    }
}

fn format_time(value: time::OffsetDateTime) -> Option<String> {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// Reduce a session event to the parts a supervising agent needs, similar to
/// what a person would see in the terminal. Returns `None` for streaming
/// deltas, reasoning, and bookkeeping events.
fn condense_event(event: &SessionEvent) -> Option<Value> {
    let mut entry = match event.event_type.as_str() {
        "session.execution_started" => json!({"kind": "turn_started"}),
        "session.execution_completed" => json!({
            "kind": "turn_completed",
            "result": event.payload.get("result_text").and_then(Value::as_str).map(truncate_text),
        }),
        "session.execution_failed" => json!({
            "kind": "turn_failed",
            "error": event.payload.get("error").and_then(Value::as_str).map(truncate_text),
        }),
        "session.execution_cancelled" => json!({
            "kind": "turn_cancelled",
            "reason": event.payload.get("reason"),
        }),
        "session.steering_delivered" => json!({"kind": "message_delivered_to_running_turn"}),
        SESSION_OUTPUT_LINE_EVENT => condense_output_line(event.payload.as_str()?)?,
        _ => return None,
    };
    let object = entry.as_object_mut()?;
    object.insert("event_id".to_owned(), json!(event.event_id));
    Some(entry)
}

fn condense_output_line(line: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if value.get("type").and_then(Value::as_str) == Some("assistant.message") {
        let payload = value.get("payload")?;
        return Some(json!({
            "kind": "message",
            "phase": payload.get("phase"),
            "text": truncate_text(payload.get("text")?.as_str()?),
        }));
    }
    let completed = value.get("method").and_then(Value::as_str) == Some("item/completed")
        || value.get("type").and_then(Value::as_str) == Some("item.completed");
    if !completed {
        return None;
    }
    let item = value
        .get("item")
        .or_else(|| value.pointer("/params/item"))?;
    let text = |field: &str| item.get(field).and_then(Value::as_str).map(truncate_text);
    let entry = match item.get("type").and_then(Value::as_str)? {
        "agentMessage" | "agent_message" => json!({
            "kind": "message",
            "phase": item.get("phase"),
            "text": text("text")?,
        }),
        "userMessage" | "user_message" => json!({
            "kind": "user_message",
            "text": user_message_text(item).map(|text| truncate_text(&text)),
        }),
        "plan" => json!({"kind": "plan", "text": text("text")}),
        "commandExecution" | "command_execution" => json!({
            "kind": "command",
            "command": text("command"),
            "status": item.get("status"),
            "exit_code": item.get("exitCode").or_else(|| item.get("exit_code")),
            "output_tail": item
                .get("aggregatedOutput")
                .or_else(|| item.get("aggregated_output"))
                .and_then(Value::as_str)
                .map(|output| tail_text(output, MAX_COMMAND_OUTPUT_CHARS)),
        }),
        "fileChange" | "file_change" => json!({
            "kind": "file_change",
            "status": item.get("status"),
            "paths": item
                .get("changes")
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|change| change.get("path").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        }),
        "mcpToolCall" | "mcp_tool_call" => json!({
            "kind": "tool_call",
            "tool": qualified_tool_name(item.get("server"), item.get("tool")),
            "status": item.get("status"),
        }),
        "dynamicToolCall" | "dynamic_tool_call" => json!({
            "kind": "tool_call",
            "tool": qualified_tool_name(item.get("namespace"), item.get("tool")),
            "status": item.get("status"),
        }),
        "webSearch" | "web_search" => json!({"kind": "web_search", "query": text("query")}),
        "collabAgentToolCall" => json!({"kind": "subagent_call"}),
        _ => return None,
    };
    Some(entry)
}

fn user_message_text(item: &Value) -> Option<String> {
    let text = item
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn qualified_tool_name(namespace: Option<&Value>, tool: Option<&Value>) -> Option<String> {
    let tool = tool?.as_str()?;
    Some(match namespace.and_then(Value::as_str) {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}.{tool}"),
        _ => tool.to_owned(),
    })
}

fn truncate_text(text: &str) -> String {
    if text.chars().count() <= MAX_ENTRY_TEXT_CHARS {
        return text.to_owned();
    }
    let mut truncated = text.chars().take(MAX_ENTRY_TEXT_CHARS).collect::<String>();
    truncated.push_str("…[truncated]");
    truncated
}

fn tail_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let mut tail = String::from("[truncated]…");
    tail.extend(text.chars().skip(count - max_chars));
    tail
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn principal(principal_id: &str) -> McpPrincipal {
        McpPrincipal {
            token_id: "tok".to_owned(),
            principal_id: principal_id.to_owned(),
            console_user_email: None,
            console_user_name: None,
            name: "user".to_owned(),
            scopes: vec!["mcp:tools".to_owned(), MCP_SESSIONS_SCOPE.to_owned()],
            expires_at: None,
        }
    }

    fn event(event_id: i64, event_type: &str, payload: Value) -> SessionEvent {
        SessionEvent {
            event_id,
            thread_key: ThreadKey::parse("mcpsession:prn_a:demo").unwrap(),
            execution_id: Some("exe_1".to_owned()),
            event_type: event_type.to_owned(),
            payload,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn output_line(event_id: i64, line: Value) -> SessionEvent {
        event(
            event_id,
            SESSION_OUTPUT_LINE_EVENT,
            Value::String(line.to_string()),
        )
    }

    #[test]
    fn session_thread_keys_are_scoped_to_the_token_principal() {
        let Ok(key) = session_thread_key(&principal("prn_abc"), " LIN-123 ").unwrap() else {
            panic!("expected a valid session thread key");
        };
        assert_eq!(key.as_str(), "mcpsession:prn_abc:LIN-123");
        // Never collides with the tool-host namespace, which boots without a
        // harness.
        assert!(!key.as_str().starts_with("mcp:"));
    }

    #[test]
    fn session_names_reject_separators_and_traversal() {
        for name in [
            "",
            "a:b",
            "prn_other:x",
            "../x",
            "-lead",
            "a b",
            &"x".repeat(65),
        ] {
            assert!(
                validate_session_name(name).is_err(),
                "{name:?} should be rejected"
            );
        }
        for name in ["LIN-123", "bug_7", "a.b", "0abc"] {
            assert!(
                validate_session_name(name).is_ok(),
                "{name:?} should be accepted"
            );
        }
        assert!(
            session_thread_key(&principal("prn_abc"), "a:b")
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn principals_with_separators_cannot_own_sessions() {
        assert!(session_thread_key_prefix("prn:abc").is_err());
        assert!(session_thread_key_prefix(" ").is_err());
    }

    #[test]
    fn session_names_round_trip_through_list_prefix() {
        let prefix = session_thread_key_prefix("prn_abc").unwrap();
        let key = ThreadKey::parse(format!("{prefix}LIN-1")).unwrap();
        assert_eq!(session_name_from_thread_key(&prefix, &key), "LIN-1");
    }

    #[test]
    fn sessions_scope_accepts_explicit_and_wildcard_scopes() {
        assert!(has_sessions_scope(&[MCP_SESSIONS_SCOPE.to_owned()]));
        assert!(has_sessions_scope(&["mcp:*".to_owned()]));
        assert!(has_sessions_scope(&["*".to_owned()]));
        assert!(!has_sessions_scope(&["mcp:tools".to_owned()]));
    }

    #[test]
    fn session_tools_are_hidden_unless_enabled_and_scoped() {
        let _lock = super::super::MCP_ENV_LOCK.lock().unwrap();
        let scoped = [MCP_SESSIONS_SCOPE.to_owned()];
        // SAFETY: MCP_ENV_LOCK serializes process env mutation across MCP tests.
        unsafe { std::env::remove_var("CENTAUR_MCP_SESSIONS_ENABLED") };
        assert!(!mcp_session_tools_visible(&scoped));
        unsafe { std::env::set_var("CENTAUR_MCP_SESSIONS_ENABLED", "true") };
        assert!(mcp_session_tools_visible(&scoped));
        assert!(!mcp_session_tools_visible(&["mcp:tools".to_owned()]));
        unsafe { std::env::remove_var("CENTAUR_MCP_SESSIONS_ENABLED") };
    }

    #[test]
    fn session_tool_definitions_match_dispatch_names() {
        let tools = mcp_session_tools();
        assert_eq!(tools.len(), 5);
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(mcp_session_tool_name(name), "{name} is not dispatched");
            assert_eq!(tool["inputSchema"]["additionalProperties"], json!(false));
        }
    }

    #[test]
    fn user_input_line_is_one_json_line() {
        let line = user_input_line("fix\nthe bug").unwrap();
        assert!(!line.contains('\n'));
        let value = serde_json::from_str::<Value>(&line).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "fix\nthe bug");
    }

    #[test]
    fn condenses_codex_items_and_drops_deltas() {
        let message = output_line(
            1,
            json!({"method": "item/completed", "params": {"item": {
                "type": "agentMessage", "id": "m1", "text": "Done.", "phase": "final_answer"
            }}}),
        );
        assert_eq!(
            condense_event(&message).unwrap(),
            json!({"kind": "message", "phase": "final_answer", "text": "Done.", "event_id": 1})
        );

        let command = output_line(
            2,
            json!({"method": "item/completed", "params": {"item": {
                "type": "commandExecution", "id": "c1", "command": "cargo test",
                "status": "completed", "exitCode": 0, "aggregatedOutput": "ok", "cwd": "/w"
            }}}),
        );
        assert_eq!(
            condense_event(&command).unwrap(),
            json!({
                "kind": "command", "command": "cargo test", "status": "completed",
                "exit_code": 0, "output_tail": "ok", "event_id": 2
            })
        );

        let files = output_line(
            3,
            json!({"method": "item/completed", "params": {"item": {
                "type": "fileChange", "id": "f1", "status": "completed",
                "changes": [{"path": "src/lib.rs"}, {"path": "README.md"}]
            }}}),
        );
        assert_eq!(
            condense_event(&files).unwrap()["paths"],
            json!(["src/lib.rs", "README.md"])
        );

        let tool = output_line(
            4,
            json!({"method": "item/completed", "params": {"item": {
                "type": "mcpToolCall", "id": "t1", "server": "linear", "tool": "get_issue",
                "status": "completed"
            }}}),
        );
        assert_eq!(condense_event(&tool).unwrap()["tool"], "linear.get_issue");

        let delta = output_line(
            5,
            json!({"method": "item/agentMessage/delta", "params": {"delta": "Do"}}),
        );
        assert!(condense_event(&delta).is_none());
        let reasoning = output_line(
            6,
            json!({"method": "item/completed", "params": {"item": {
                "type": "reasoning", "id": "r1", "summary": [], "content": []
            }}}),
        );
        assert!(condense_event(&reasoning).is_none());
        assert!(condense_event(&event(7, "session.output.line", json!("not json"))).is_none());
        assert!(condense_event(&event(8, "session.sandbox_ready", json!({}))).is_none());
    }

    #[test]
    fn condenses_normalized_assistant_messages_and_turn_lifecycle() {
        let message = output_line(
            1,
            json!({"type": "assistant.message", "payload": {"text": "hi", "phase": "commentary"}}),
        );
        assert_eq!(condense_event(&message).unwrap()["text"], "hi");

        let completed = event(
            2,
            "session.execution_completed",
            json!({"execution_id": "exe_1", "result_text": "PR opened"}),
        );
        assert_eq!(
            condense_event(&completed).unwrap(),
            json!({"kind": "turn_completed", "result": "PR opened", "event_id": 2})
        );
        let failed = event(3, "session.execution_failed", json!({"error": "boom"}));
        assert_eq!(condense_event(&failed).unwrap()["error"], "boom");
        assert!(is_terminal_event("session.execution_cancelled"));
        assert!(!is_terminal_event("session.execution_started"));
    }

    #[test]
    fn peek_view_keeps_latest_entries_and_advances_cursor_past_skipped_events() {
        let mut view = PeekView::new(0, 2, false);
        view.push(&event(1, "session.execution_started", json!({})));
        view.push(&event(2, "session.sandbox_ready", json!({})));
        view.push(&event(3, "session.steering_delivered", json!({})));
        view.push(&event(4, "session.execution_completed", json!({})));
        view.push(&event(5, "session.stdout_eof", json!({})));
        assert_eq!(view.cursor, 5);
        assert_eq!(view.scanned, 5);
        assert_eq!(view.omitted, 1);
        let kinds = view
            .entries
            .iter()
            .map(|entry| entry["kind"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            ["message_delivered_to_running_turn", "turn_completed"]
        );

        let mut raw = PeekView::new(0, 10, true);
        raw.push(&event(9, "session.sandbox_ready", json!({"x": 1})));
        assert_eq!(raw.entries[0]["type"], "session.sandbox_ready");
    }

    #[test]
    fn text_helpers_bound_output() {
        let long = "x".repeat(MAX_ENTRY_TEXT_CHARS + 10);
        assert!(truncate_text(&long).ends_with("…[truncated]"));
        assert_eq!(tail_text("abcdef", 3), "[truncated]…def");
        assert_eq!(tail_text("abc", 3), "abc");
    }
}
