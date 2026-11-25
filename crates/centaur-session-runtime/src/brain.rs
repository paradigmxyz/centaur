//! HTTP client for the out-of-sandbox `nanocode` brain.
//!
//! For `nanocode` sessions the brain owns turn execution and the transcript.
//! api-rs keeps identity, authorization, sessions, and durable user messages,
//! then:
//!
//! - submits one turn per user line ([`BrainClient::submit_turn`]),
//! - proxies the brain's SSE feed to clients verbatim
//!   ([`BrainClient::events_bytes`]) and parses the same feed for in-process
//!   callers ([`BrainClient::events`]),
//! - asks the brain whether a turn is running ([`BrainClient::thread_status`]),
//!   so appended messages can steer it instead of starting another turn,
//! - asks the brain to cancel a running turn ([`BrainClient::interrupt`]).
//!
//! The event framing is exactly the api-rs `/events` framing, so ingresses and
//! workflows keep their existing contract.

use std::time::Duration;

use bytes::Bytes;
use centaur_session_core::{SessionEvent, ThreadKey};
use eventsource_stream::{Event as SseFrame, Eventsource};
use futures_util::{StreamExt, TryStreamExt, future::ready, stream::BoxStream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

use crate::{SESSION_OUTPUT_LINE_EVENT, is_terminal_execution_event};

/// Timeout for control POSTs. The event stream deliberately has none: a live
/// tail can outlive any fixed timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_CHARS: usize = 512;
/// Event type the brain uses for a mid-stream fault; it carries no event id.
const STREAM_ERROR_EVENT: &str = "session.stream_error";

/// Raw brain event bytes, as proxied to `GET /api/session/{k}/events`.
pub type BrainByteStream = BoxStream<'static, Result<Bytes, BrainError>>;
/// Parsed brain events, for in-process event consumers.
pub type BrainEventStream = BoxStream<'static, Result<SessionEvent, BrainError>>;

#[derive(Debug, Error)]
pub enum BrainError {
    #[error("brain request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("{context}: brain returned {status}: {body}")]
    Status {
        context: &'static str,
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("brain event stream failed: {0}")]
    Stream(String),
    #[error("brain URL is invalid: {0}")]
    InvalidUrl(String),
}

impl BrainError {
    /// Whether the brain definitively rejected the request. A transport or
    /// server fault may still have landed, so callers treat only client errors
    /// as "the brain never saw it".
    pub fn is_rejected(&self) -> bool {
        matches!(self, Self::Status { status, .. } if status.is_client_error())
    }
}

/// The execution id of one `nanocode` turn is the brain's request id.
///
/// A retried execute with the same idempotency key maps to the same turn:
/// `exe_` plus the first 32 hex characters of
/// `sha256("{thread_key}\n{idempotency_key}")`.
pub fn request_id(thread_key: &ThreadKey, idempotency_key: Option<&str>) -> String {
    match idempotency_key {
        Some(key) => {
            let digest = Sha256::digest(format!("{}\n{key}", thread_key.as_str()).as_bytes());
            format!("exe_{}", &hex::encode(digest)[..32])
        }
        None => new_request_id(),
    }
}

/// A fresh request id, for turns with no idempotency key and for steers.
pub fn new_request_id() -> String {
    format!("exe_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Clone, Debug)]
pub struct BrainClient {
    /// Control requests: bounded by [`REQUEST_TIMEOUT`].
    control: reqwest::Client,
    /// Event streams: no overall timeout, because a tail is unbounded.
    event_streams: reqwest::Client,
    base_url: String,
}

impl BrainClient {
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, BrainError> {
        let base_url = base_url.as_ref().trim().trim_end_matches('/');
        if base_url.is_empty() {
            return Err(BrainError::InvalidUrl(
                "brain URL must not be empty".to_owned(),
            ));
        }
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(BrainError::InvalidUrl(format!(
                "brain URL must start with http:// or https://: {base_url:?}"
            )));
        }
        let control = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        let event_streams = reqwest::Client::builder().build()?;
        Ok(Self {
            control,
            event_streams,
            base_url: base_url.to_owned(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Submit one turn (or a steer) for a thread. Idempotent by `request_id`.
    ///
    /// `tool_token` is the turn's Code Mode token: the brain forwards it with
    /// every Code Mode call, and without one the turn has no Code Mode tools.
    pub async fn submit_turn(
        &self,
        thread_key: &ThreadKey,
        request_id: &str,
        input_line: Value,
        steer: bool,
        tool_token: Option<&str>,
    ) -> Result<TurnSubmitted, BrainError> {
        let response = self
            .control
            .post(self.thread_url(thread_key, "turns"))
            .json(&SubmitTurn {
                request_id,
                input_line,
                steer,
                tool_token,
            })
            .send()
            .await?;
        let response = ensure_success(response, "submit turn").await?;
        Ok(response.json().await?)
    }

    /// The thread's unfinished turns, from the brain's durable log.
    pub async fn thread_status(&self, thread_key: &ThreadKey) -> Result<ThreadStatus, BrainError> {
        let response = self
            .control
            .get(self.thread_root_url(thread_key))
            .send()
            .await?;
        let response = ensure_success(response, "thread status").await?;
        Ok(response.json().await?)
    }

    /// Cancel the thread's running turn, if any.
    pub async fn interrupt(
        &self,
        thread_key: &ThreadKey,
        reason: &str,
    ) -> Result<TurnInterrupt, BrainError> {
        let response = self
            .control
            .post(self.thread_url(thread_key, "interrupt"))
            .json(&InterruptTurn { reason })
            .send()
            .await?;
        let response = ensure_success(response, "interrupt turn").await?;
        Ok(response.json().await?)
    }

    /// Open the thread's event stream and return its raw body bytes.
    pub async fn events_bytes(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
        execution_id: Option<&str>,
    ) -> Result<BrainByteStream, BrainError> {
        let response = self
            .open_events(thread_key, after_event_id, execution_id)
            .await?;
        Ok(response
            .bytes_stream()
            .map_err(BrainError::Transport)
            .boxed())
    }

    /// Open the thread's event stream and parse it into [`SessionEvent`]s.
    ///
    /// With an `execution_id` filter the stream ends after that execution's
    /// terminal event, exactly like an execution-scoped api-rs stream.
    pub async fn events(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
        execution_id: Option<&str>,
    ) -> Result<BrainEventStream, BrainError> {
        let bytes = self
            .events_bytes(thread_key, after_event_id, execution_id)
            .await?;
        Ok(parse_events(
            bytes,
            thread_key.clone(),
            execution_id.map(ToOwned::to_owned),
        ))
    }

    async fn open_events(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
        execution_id: Option<&str>,
    ) -> Result<reqwest::Response, BrainError> {
        let mut url = format!(
            "{}?after_event_id={after_event_id}",
            self.thread_url(thread_key, "events")
        );
        if let Some(execution_id) = execution_id {
            url.push_str("&execution_id=");
            url.push_str(&urlencoding::encode(execution_id));
        }
        let response = self
            .event_streams
            .get(url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await?;
        ensure_success(response, "open event stream").await
    }

    fn thread_root_url(&self, thread_key: &ThreadKey) -> String {
        format!(
            "{}/v1/threads/{}",
            self.base_url,
            urlencoding::encode(thread_key.as_str())
        )
    }

    fn thread_url(&self, thread_key: &ThreadKey, suffix: &str) -> String {
        format!("{}/{suffix}", self.thread_root_url(thread_key))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SubmitTurn<'a> {
    request_id: &'a str,
    input_line: Value,
    steer: bool,
    /// The turn's Code Mode token, minted by api-rs for the session's
    /// principal; absent when the turn gets no Code Mode tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_token: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct InterruptTurn<'a> {
    reason: &'a str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnSubmitted {
    pub request_id: String,
    pub status: String,
    #[serde(default)]
    pub created: bool,
    #[serde(default)]
    pub steered: bool,
}

/// A thread's unfinished turns: one running turn, if any, and its queue.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThreadStatus {
    #[serde(default)]
    pub running: Option<String>,
    #[serde(default)]
    pub queued: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnInterrupt {
    pub interrupted: bool,
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Parse the brain's SSE bytes into `SessionEvent`s.
fn parse_events(
    bytes: BrainByteStream,
    thread_key: ThreadKey,
    execution_id: Option<String>,
) -> BrainEventStream {
    let terminal_ends_stream = execution_id.is_some();
    let filter = execution_id;
    let frames = bytes
        .eventsource()
        .map(|frame| frame.map_err(|error| BrainError::Stream(error.to_string())));
    let events = frames.map(move |frame| {
        let frame = frame?;
        parse_frame(&frame, &thread_key, filter.as_deref())
    });
    Box::pin(events.scan(false, move |done, item| {
        if *done {
            return ready(None);
        }
        match item {
            Ok((event, terminal)) => {
                if terminal && terminal_ends_stream {
                    *done = true;
                }
                ready(Some(Ok(event)))
            }
            Err(error) => {
                *done = true;
                ready(Some(Err(error)))
            }
        }
    }))
}

fn parse_frame(
    frame: &SseFrame,
    thread_key: &ThreadKey,
    execution_id: Option<&str>,
) -> Result<(SessionEvent, bool), BrainError> {
    let event_type = if frame.event.is_empty() {
        "message".to_owned()
    } else {
        frame.event.clone()
    };
    let payload = parse_payload(&event_type, &frame.data);
    if event_type == STREAM_ERROR_EVENT {
        return Err(BrainError::Stream(stream_error_text(&payload)));
    }
    let event_id =
        frame.id.trim().parse::<i64>().map_err(|_| {
            BrainError::Stream(format!("brain event {event_type} carries no event id"))
        })?;
    let event_execution_id = payload
        .get("execution_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| execution_id.map(ToOwned::to_owned));
    let terminal = is_terminal_execution_event(&event_type);
    Ok((
        SessionEvent {
            event_id,
            thread_key: thread_key.clone(),
            execution_id: event_execution_id,
            event_type,
            payload,
            created_at: OffsetDateTime::now_utc(),
        },
        terminal,
    ))
}

/// `session.output.line` carries one raw harness line; every other event type
/// carries one-line JSON.
fn parse_payload(event_type: &str, data: &str) -> Value {
    if event_type == SESSION_OUTPUT_LINE_EVENT {
        return Value::String(data.to_owned());
    }
    serde_json::from_str(data).unwrap_or_else(|_| Value::String(data.to_owned()))
}

fn stream_error_text(payload: &Value) -> String {
    payload
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .or_else(|| payload.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "brain event stream failed".to_owned())
}

async fn ensure_success(
    response: reqwest::Response,
    context: &'static str,
) -> Result<reqwest::Response, BrainError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let mut body = response.text().await.unwrap_or_default();
    body = body.trim().to_owned();
    if body.chars().count() > MAX_ERROR_BODY_CHARS {
        body = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
    }
    Err(BrainError::Status {
        context,
        status,
        body,
    })
}

#[cfg(test)]
mod tests {
    use centaur_session_core::ThreadKey;
    use serde_json::json;

    use super::*;
    use crate::fake_brain::{FakeBrain, frame};

    fn thread_key() -> ThreadKey {
        ThreadKey::try_from("slack:C123:1712345678.000100".to_owned()).unwrap()
    }

    #[test]
    fn request_ids_are_stable_for_an_idempotency_key() {
        let thread_key = thread_key();
        let first = request_id(&thread_key, Some("1712345678.000100"));
        assert_eq!(
            first,
            request_id(&thread_key, Some("1712345678.000100")),
            "a retried execute must map to the same turn"
        );
        assert!(first.starts_with("exe_"));
        assert_eq!(first.len(), 4 + 32);
        assert!(first[4..].chars().all(|ch| ch.is_ascii_hexdigit()));
        // The thread key is part of the digest, so the same message id in two
        // threads is two different turns.
        let other = ThreadKey::try_from("slack:C123:1712345678.000200".to_owned()).unwrap();
        assert_ne!(first, request_id(&other, Some("1712345678.000100")));
        // No key: a fresh uuid each time.
        let generated = request_id(&thread_key, None);
        assert!(generated.starts_with("exe_"));
        assert_eq!(generated.len(), 4 + 32);
        assert_ne!(generated, request_id(&thread_key, None));
    }

    #[test]
    fn thread_urls_percent_encode_the_thread_key() {
        let client = BrainClient::new("http://brain.example:8790/").unwrap();
        assert_eq!(client.base_url(), "http://brain.example:8790");
        assert_eq!(
            client.thread_url(&thread_key(), "turns"),
            "http://brain.example:8790/v1/threads/slack%3AC123%3A1712345678.000100/turns"
        );
        assert_eq!(
            client.thread_root_url(&thread_key()),
            "http://brain.example:8790/v1/threads/slack%3AC123%3A1712345678.000100"
        );
    }

    #[tokio::test]
    async fn thread_status_maps_the_brain_response() {
        let brain = FakeBrain::start().await;
        brain.set_thread_status(json!({
            "running": "exe_running",
            "queued": ["exe_queued_a", "exe_queued_b"],
        }));
        let client = BrainClient::new(brain.url()).unwrap();

        let status = client.thread_status(&thread_key()).await.unwrap();
        assert_eq!(
            status,
            ThreadStatus {
                running: Some("exe_running".to_owned()),
                queued: vec!["exe_queued_a".to_owned(), "exe_queued_b".to_owned()],
            }
        );

        // An idle thread has no running turn and no queue.
        brain.set_thread_status(json!({"running": null, "queued": []}));
        let idle = client.thread_status(&thread_key()).await.unwrap();
        assert_eq!(idle, ThreadStatus::default());

        let requests = brain.recorded_status_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].thread_key, thread_key().as_str());
    }

    #[test]
    fn invalid_brain_urls_are_rejected() {
        assert!(matches!(
            BrainClient::new("  "),
            Err(BrainError::InvalidUrl(_))
        ));
        assert!(matches!(
            BrainClient::new("centaur-brain:8790"),
            Err(BrainError::InvalidUrl(_))
        ));
    }

    #[tokio::test]
    async fn submit_turn_posts_one_blocks_line() {
        let brain = FakeBrain::start().await;
        let client = BrainClient::new(brain.url()).unwrap();
        let line = json!({
            "type": "user",
            "thread_key": thread_key().as_str(),
            "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
        });

        let submitted = client
            .submit_turn(&thread_key(), "exe_abc", line.clone(), false, None)
            .await
            .unwrap();
        assert_eq!(
            submitted,
            TurnSubmitted {
                request_id: "exe_abc".to_owned(),
                status: "queued".to_owned(),
                created: true,
                steered: false,
            }
        );

        // Resubmitting the same request id is an idempotent replay.
        let replay = client
            .submit_turn(&thread_key(), "exe_abc", line, false, Some("token"))
            .await
            .unwrap();
        assert!(!replay.created);

        let turns = brain.recorded_turns();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].thread_key, thread_key().as_str());
        assert_eq!(turns[0].body["request_id"], "exe_abc");
        assert_eq!(turns[0].body["steer"], false);
        // No token is sent as no field, not as null.
        assert!(turns[0].body.get("tool_token").is_none());
        assert_eq!(turns[1].body["tool_token"], "token");
        assert_eq!(turns[0].body["input_line"]["type"], "user");
        assert_eq!(
            turns[0].body["input_line"]["message"]["content"][0]["text"],
            "hi"
        );
    }

    #[tokio::test]
    async fn submit_turn_reports_brain_rejections() {
        let brain = FakeBrain::start().await;
        brain.reject_turns(409, "request id already used");
        let client = BrainClient::new(brain.url()).unwrap();

        let error = client
            .submit_turn(
                &thread_key(),
                "exe_abc",
                json!({"type": "user"}),
                false,
                None,
            )
            .await
            .unwrap_err();
        assert!(error.is_rejected(), "{error}");
        assert!(error.to_string().contains("request id already used"));
    }

    #[tokio::test]
    async fn interrupt_maps_the_brain_response() {
        let brain = FakeBrain::start().await;
        brain.set_interrupt_response(json!({
            "interrupted": true,
            "request_id": "exe_running",
        }));
        let client = BrainClient::new(brain.url()).unwrap();

        let interrupted = client.interrupt(&thread_key(), "stop").await.unwrap();
        assert_eq!(
            interrupted,
            TurnInterrupt {
                interrupted: true,
                request_id: Some("exe_running".to_owned()),
            }
        );

        let interrupts = brain.recorded_interrupts();
        assert_eq!(interrupts.len(), 1);
        assert_eq!(interrupts[0].thread_key, thread_key().as_str());
        assert_eq!(interrupts[0].body["reason"], "stop");
    }

    #[tokio::test]
    async fn parsed_events_keep_ids_payloads_and_execution_association() {
        // Brain event ids are `session_log_seq * 64 + k`, so they are large and
        // sparse; nothing may assume they start at 1 or are dense.
        let base: i64 = 1_048_704;
        let brain = FakeBrain::start().await;
        brain.set_event_frames(vec![
            frame(
                base,
                "session.execution_started",
                json!({
                    "execution_id": "exe_running",
                    "thread_key": thread_key().as_str(),
                }),
            ),
            frame(
                base + 1,
                "session.output.line",
                json!({"method": "item/agentMessage/delta", "params": {"delta": "hi"}}),
            ),
            frame(
                base + 64,
                "session.execution_completed",
                json!({
                    "execution_id": "exe_running",
                    "thread_key": thread_key().as_str(),
                    "completion_reason": "turn_completed",
                    "result_text": "hi",
                }),
            ),
        ]);
        let client = BrainClient::new(brain.url()).unwrap();

        // Execution-scoped: ends after the terminal event.
        let events = client
            .events(&thread_key(), 0, Some("exe_running"))
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 3);
        let events = events.into_iter().map(Result::unwrap).collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            vec![base, base + 1, base + 64]
        );
        assert_eq!(events[0].event_type, "session.execution_started");
        // Output lines are string payloads on the in-process stream too.
        assert_eq!(events[1].event_type, SESSION_OUTPUT_LINE_EVENT);
        assert_eq!(
            events[1].payload.as_str(),
            Some(r#"{"method":"item/agentMessage/delta","params":{"delta":"hi"}}"#)
        );
        assert!(
            events
                .iter()
                .all(|event| event.execution_id.as_deref() == Some("exe_running"))
        );
        assert_eq!(events[2].payload["result_text"], "hi");

        // Session-scoped: the brain decides when the stream ends, so the
        // terminal event does not stop us early.
        let session_scoped = client
            .events(&thread_key(), 0, None)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(session_scoped.len(), 3);

        let requests = brain.recorded_event_requests();
        assert_eq!(requests[0].thread_key, thread_key().as_str());
        assert!(requests[0].query.contains("after_event_id=0"));
        assert!(requests[0].query.contains("execution_id=exe_running"));
        assert!(!requests[1].query.contains("execution_id="));
    }

    #[tokio::test]
    async fn stream_error_frames_fail_the_parsed_stream() {
        let brain = FakeBrain::start().await;
        brain.set_event_frames(vec![format!(
            "event: {STREAM_ERROR_EVENT}\ndata: {{\"error\":\"upstream died\"}}\n\n"
        )]);
        let client = BrainClient::new(brain.url()).unwrap();

        let events = client
            .events(&thread_key(), 0, None)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 1);
        assert!(
            events[0]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("upstream died")
        );
    }

    #[tokio::test]
    async fn event_bytes_are_proxied_verbatim() {
        let brain = FakeBrain::start().await;
        let frames = vec![
            frame(
                1_048_704,
                "session.execution_started",
                json!({"execution_id": "exe_a"}),
            ),
            frame(
                1_048_705,
                "session.output.line",
                json!({"method": "turn/started"}),
            ),
        ];
        brain.set_event_frames(frames.clone());
        let client = BrainClient::new(brain.url()).unwrap();

        let bytes = client
            .events_bytes(&thread_key(), 7, Some("exe_a"))
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .fold(Vec::new(), |mut all, chunk| {
                all.extend_from_slice(&chunk);
                all
            });
        assert_eq!(String::from_utf8(bytes).unwrap(), frames.concat());
    }
}
