use axum::response::sse::Event;
use centaur_session_core::{HarnessType, Session, SessionEvent, SessionMessageInput, ThreadKey};
use centaur_session_runtime::SESSION_OUTPUT_LINE_EVENT;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateSessionRequest {
    pub harness_type: HarnessType,
    pub persona_id: Option<String>,
    pub metadata: Option<Value>,
    /// What to do when the session already exists on a different harness.
    /// Omitted or `reject`: fail with 409. `restart`: stop the old sandbox and
    /// restart the thread on the requested harness (the new harness starts
    /// with no conversational memory).
    #[serde(default)]
    pub on_harness_conflict: Option<OnHarnessConflict>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OnHarnessConflict {
    Reject,
    Restart,
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateSessionResponse {
    #[serde(flatten)]
    pub session: Session,
    /// True when this request restarted the thread onto a different harness.
    pub harness_switched: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionContextResponse {
    pub thread_key: ThreadKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackThreadContext>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SlackThreadContext {
    pub channel_id: String,
    pub thread_ts: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppendMessagesRequest {
    pub messages: Vec<SessionMessageInput>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppendMessagesResponse {
    pub ok: bool,
    pub message_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExecuteSessionRequest {
    pub idempotency_key: Option<String>,
    pub metadata: Option<Value>,
    #[serde(default)]
    pub input_lines: Vec<String>,
    pub idle_timeout_ms: Option<u64>,
    pub max_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExecuteSessionResponse {
    pub ok: bool,
    pub execution_id: String,
    pub thread_key: ThreadKey,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InterruptSessionExecutionRequest {
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InterruptSessionExecutionResponse {
    pub ok: bool,
    pub interrupted: bool,
    pub execution_id: Option<String>,
    pub thread_key: ThreadKey,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EventsQuery {
    pub after_event_id: Option<i64>,
    pub execution_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ListWorkflowRunsQuery {
    pub limit: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EmitWorkflowEventRequest {
    pub event_name: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEventName {
    OutputLine,
    ExecutionStarted,
    ExecutionCompleted,
    ExecutionFailed,
    ExecutionCancelled,
    StreamError,
    Other(String),
}

impl SessionEventName {
    pub fn as_str(&self) -> &str {
        match self {
            Self::OutputLine => SESSION_OUTPUT_LINE_EVENT,
            Self::ExecutionStarted => "session.execution_started",
            Self::ExecutionCompleted => "session.execution_completed",
            Self::ExecutionFailed => "session.execution_failed",
            Self::ExecutionCancelled => "session.execution_cancelled",
            Self::StreamError => "session.stream_error",
            Self::Other(value) => value.as_str(),
        }
    }
}

impl From<String> for SessionEventName {
    fn from(value: String) -> Self {
        match value.as_str() {
            SESSION_OUTPUT_LINE_EVENT => Self::OutputLine,
            "session.execution_started" => Self::ExecutionStarted,
            "session.execution_completed" => Self::ExecutionCompleted,
            "session.execution_failed" => Self::ExecutionFailed,
            "session.execution_cancelled" => Self::ExecutionCancelled,
            "session.stream_error" => Self::StreamError,
            _ => Self::Other(value),
        }
    }
}

impl From<&str> for SessionEventName {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

pub struct SessionSseEvent(Event);

impl TryFrom<SessionEvent> for SessionSseEvent {
    type Error = SessionEventConversionError;

    fn try_from(event: SessionEvent) -> Result<Self, Self::Error> {
        let event_id = event.event_id;
        let event_name = SessionEventName::from(event.event_type);
        let sse = Event::default()
            .id(event_id.to_string())
            .event(event_name.as_str());

        let sse = match event_name {
            SessionEventName::OutputLine => {
                let Some(line) = event.payload.as_str() else {
                    return Err(SessionEventConversionError::OutputLinePayload { event_id });
                };
                sse.data(line)
            }
            _ => sse
                .json_data(event.payload)
                .map_err(|source| SessionEventConversionError::JsonData { event_id, source })?,
        };

        Ok(Self(sse))
    }
}

impl From<SessionSseEvent> for Event {
    fn from(value: SessionSseEvent) -> Self {
        value.0
    }
}

pub fn stream_error_sse(message: impl Into<String>) -> Event {
    Event::default()
        .event(SessionEventName::StreamError.as_str())
        .json_data(serde_json::json!({ "error": message.into() }))
        .unwrap_or_else(|_| {
            Event::default()
                .event(SessionEventName::StreamError.as_str())
                .data("{}")
        })
}

#[derive(Debug, Error)]
pub enum SessionEventConversionError {
    #[error("session.output.line event {event_id} payload must be a string")]
    OutputLinePayload { event_id: i64 },
    #[error("failed to serialize session event {event_id} payload as SSE JSON: {source}")]
    JsonData { event_id: i64, source: axum::Error },
}
