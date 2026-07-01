use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use centaur_session_core::{SessionEvent, ThreadKey, ThreadKeyError};
use centaur_session_runtime::SESSION_OUTPUT_LINE_EVENT;
use centaur_session_sqlx::{PgSessionStore, SessionEventNotification, SessionStoreError};
use reqwest::StatusCode;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::time::sleep;
use tracing::{debug, info, warn};

pub(crate) const SESSION_ACTIVITY_SUMMARY_EVENT: &str = "session.activity_summary";

const SYSTEM_PROMPT: &str = "\
You write live status text for a software agent. Use only the supplied event facts. \
Write one short, conversational first-person present-tense sentence under 45 characters, \
including spaces, as if you are the agent. Describe the goal you are working toward, \
not the exact command, file path, ID, flag, or implementation step you are using. \
Avoid mechanics like running tests, reading output, building images, checking logs, \
or watching rollouts unless they are the user's explicit goal. If the facts are mostly \
mechanics, infer the higher-level outcome and omit those mechanics. Prefer short \
outcomes like \"I'm checking the fix\" or \"I'm getting the preview ready\". \
Do not mention tests, output, builds, logs, rollouts, commands, paths, IDs, or flags unless the user asked for them. \
Use user-facing words like fix, preview, update, or summary behavior instead of \
infrastructure words like server, deployment, or rollout. Do not refer to \"the agent\". \
Never write more than 45 characters. No markdown, no quotes, no event IDs, and no speculation.";

#[derive(Clone)]
pub(crate) struct ActivitySummaryConfig {
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) max_facts: usize,
    pub(crate) max_output_tokens: u16,
    pub(crate) min_interval: Duration,
    pub(crate) model: String,
    pub(crate) timeout: Duration,
}

pub(crate) struct ActivitySummaryWorker {
    client: ActivitySummaryClient,
    config: ActivitySummaryConfig,
    states: HashMap<String, ExecutionActivity>,
    store: PgSessionStore,
}

impl ActivitySummaryWorker {
    pub(crate) fn new(
        store: PgSessionStore,
        config: ActivitySummaryConfig,
    ) -> Result<Self, ActivitySummaryError> {
        Ok(Self {
            client: ActivitySummaryClient::new(&config)?,
            config,
            states: HashMap::new(),
            store,
        })
    }

    pub(crate) async fn run(mut self) {
        info!(
            model = %self.config.model,
            min_interval_ms = self.config.min_interval.as_millis(),
            "session activity summary worker started"
        );
        loop {
            let mut listener = match self.store.listen_session_events().await {
                Ok(listener) => listener,
                Err(error) => {
                    warn!(%error, "failed to listen for session activity events");
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            loop {
                match listener.recv().await {
                    Ok(notification) => {
                        if let Err(error) = self.process_notification(notification).await {
                            warn!(%error, "failed to process session activity event");
                        }
                    }
                    Err(error) => {
                        warn!(%error, "session activity event listener failed; reconnecting");
                        sleep(Duration::from_secs(1)).await;
                        break;
                    }
                }
            }
        }
    }

    async fn process_notification(
        &mut self,
        notification: SessionEventNotification,
    ) -> Result<(), ActivitySummaryError> {
        let thread_key = ThreadKey::parse(notification.thread_key)?;
        let events = self
            .store
            .list_events_after(
                &thread_key,
                notification.event_id.saturating_sub(1),
                None,
                8,
            )
            .await?;
        let Some(event) = events
            .into_iter()
            .find(|event| event.event_id == notification.event_id)
        else {
            return Ok(());
        };
        self.process_event(event).await
    }

    async fn process_event(&mut self, event: SessionEvent) -> Result<(), ActivitySummaryError> {
        if event.event_type == SESSION_ACTIVITY_SUMMARY_EVENT {
            return Ok(());
        }
        let Some(execution_id) = event.execution_id.as_deref() else {
            return Ok(());
        };
        if is_terminal_session_event(&event.event_type) {
            self.states.remove(execution_id);
            return Ok(());
        }
        if event.event_type != SESSION_OUTPUT_LINE_EVENT {
            return Ok(());
        }

        let Some(fact) = activity_fact_from_output_event(&event) else {
            return Ok(());
        };
        let now = Instant::now();
        let publish = {
            let state = self
                .states
                .entry(execution_id.to_owned())
                .or_insert_with(|| ExecutionActivity {
                    facts: VecDeque::with_capacity(self.config.max_facts),
                    last_attempt_at: None,
                    last_published_signature: None,
                    last_summary: None,
                    max_facts: self.config.max_facts,
                });
            state.push(fact);
            state.prepare_publish(now, self.config.min_interval)
        };

        let Some(prompt) = publish else {
            return Ok(());
        };

        let summary = match self.client.summarize(&prompt).await {
            Ok(summary) => summary,
            Err(error) => {
                warn!(%error, "failed to generate session activity summary");
                return Ok(());
            }
        };
        let Some(summary) = sanitize_summary(&summary) else {
            debug!("discarded empty session activity summary");
            return Ok(());
        };

        self.store
            .append_event(
                &event.thread_key,
                Some(execution_id),
                SESSION_ACTIVITY_SUMMARY_EVENT,
                json!({
                    "execution_id": execution_id,
                    "model": self.config.model.as_str(),
                    "source_event_id": event.event_id,
                    "summary": summary,
                }),
            )
            .await?;

        if let Some(state) = self.states.get_mut(execution_id) {
            state.last_published_signature = Some(state.signature());
            state.last_summary = Some(summary);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ExecutionActivity {
    facts: VecDeque<ActivityFact>,
    last_attempt_at: Option<Instant>,
    last_published_signature: Option<String>,
    last_summary: Option<String>,
    max_facts: usize,
}

impl ExecutionActivity {
    fn push(&mut self, fact: ActivityFact) {
        if self
            .facts
            .back()
            .is_some_and(|existing| existing.kind == fact.kind && existing.text == fact.text)
        {
            return;
        }
        self.facts.push_back(fact);
        while self.facts.len() > self.max_facts {
            self.facts.pop_front();
        }
    }

    fn prepare_publish(&mut self, now: Instant, min_interval: Duration) -> Option<String> {
        if self.facts.is_empty() {
            return None;
        }
        if self
            .last_attempt_at
            .is_some_and(|last| now.saturating_duration_since(last) < min_interval)
        {
            return None;
        }
        let signature = self.signature();
        if self
            .last_published_signature
            .as_ref()
            .is_some_and(|last| last == &signature)
        {
            return None;
        }
        self.last_attempt_at = Some(now);
        Some(self.prompt())
    }

    fn prompt(&self) -> String {
        let mut lines = Vec::new();
        if let Some(summary) = self.last_summary.as_deref() {
            lines.push(format!("Previous status sentence: {summary}"));
        }
        lines.push("Recent activity facts, oldest to newest:".to_owned());
        for fact in &self.facts {
            lines.push(format!("- {}: {}", fact.kind, fact.text));
        }
        lines.join("\n")
    }

    fn signature(&self) -> String {
        self.facts
            .iter()
            .map(|fact| format!("{}={}", fact.kind, fact.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActivityFact {
    kind: &'static str,
    text: String,
}

fn activity_fact_from_output_event(event: &SessionEvent) -> Option<ActivityFact> {
    let line = event.payload.as_str()?;
    let value = serde_json::from_str::<Value>(line).ok()?;
    activity_fact_from_value(&value)
}

fn activity_fact_from_value(value: &Value) -> Option<ActivityFact> {
    let event_type = event_type(value)?;
    let normalized = event_type.replace('/', ".");
    match normalized.as_str() {
        "turn.plan.updated" => plan_fact(value),
        "item.plan.delta" => string_field(value, &["delta", "text"]).map(|text| ActivityFact {
            kind: "plan",
            text: format!("planning {}", one_line(&text, 180)),
        }),
        "item.reasoning.summaryTextDelta" | "item.reasoning.textDelta" => {
            string_field(value, &["delta", "text"]).map(|text| ActivityFact {
                kind: "thinking",
                text: one_line(&text, 220),
            })
        }
        "item.commandExecution.outputDelta" => Some(ActivityFact {
            kind: "command",
            text: "reading command output".to_owned(),
        }),
        "item.mcpToolCall.progress" => Some(ActivityFact {
            kind: "tool",
            text: progress_fact_text(value),
        }),
        "item.started" | "item.updated" | "item.completed" => item_fact(value, &normalized),
        "assistant" => assistant_tool_fact(value),
        "tool" | "user" => tool_result_fact(value),
        _ => None,
    }
}

fn event_type(value: &Value) -> Option<String> {
    string_at(value, &["method"]).or_else(|| string_at(value, &["type"]))
}

fn plan_fact(value: &Value) -> Option<ActivityFact> {
    let plan = value
        .get("plan")
        .or_else(|| value.get("params").and_then(|params| params.get("plan")))?;
    let items = plan.as_array()?;
    let current = items
        .iter()
        .find(|item| {
            let status = string_at(item, &["status"])
                .unwrap_or_default()
                .to_ascii_lowercase();
            matches!(
                status.as_str(),
                "inprogress" | "in_progress" | "running" | "pending" | ""
            )
        })
        .or_else(|| items.last())?;
    let step = string_at(current, &["step"])
        .or_else(|| string_at(current, &["title"]))
        .or_else(|| string_at(current, &["text"]))?;
    Some(ActivityFact {
        kind: "plan",
        text: format!("working on {}", one_line(&strip_plan_marker(&step), 180)),
    })
}

fn item_fact(value: &Value, normalized_event_type: &str) -> Option<ActivityFact> {
    let item = protocol_item(value)?;
    let item_type = string_at(item, &["type"]).unwrap_or_default();
    let completed = normalized_event_type == "item.completed";
    match item_type.as_str() {
        "commandExecution" | "command_execution" => {
            let command = string_at(item, &["command"]).unwrap_or_else(|| "command".to_owned());
            let action = if completed { "finished" } else { "running" };
            Some(ActivityFact {
                kind: "command",
                text: format!(
                    "{action} {}",
                    one_line(&unwrap_shell_command(&command), 220)
                ),
            })
        }
        "fileChange" | "file_change" => Some(ActivityFact {
            kind: "files",
            text: file_change_text(item, completed),
        }),
        "reasoning" => reasoning_item_fact(item, completed),
        "mcpToolCall" | "mcp_tool_call" | "dynamicToolCall" | "dynamic_tool_call" => {
            let name = tool_name(item);
            let action = if completed { "finished using" } else { "using" };
            Some(ActivityFact {
                kind: "tool",
                text: format!("{action} {name}"),
            })
        }
        // Assistant messages are the user-visible answer/commentary stream, not
        // a useful live activity signal. Tool, plan, and command events carry
        // the actual work in progress.
        "agentMessage" | "agent_message" => None,
        "plan" => string_at(item, &["text"]).map(|text| ActivityFact {
            kind: "plan",
            text: format!("updated plan {}", one_line(&text, 180)),
        }),
        _ => None,
    }
}

fn protocol_item(value: &Value) -> Option<&Value> {
    value
        .get("item")
        .or_else(|| value.get("params").and_then(|params| params.get("item")))
}

fn reasoning_item_fact(item: &Value, completed: bool) -> Option<ActivityFact> {
    let text = string_at(item, &["text"])
        .or_else(|| array_text(item.get("summary")))
        .or_else(|| array_text(item.get("content")))?;
    Some(ActivityFact {
        kind: "thinking",
        text: if completed {
            format!("finished thinking about {}", one_line(&text, 180))
        } else {
            one_line(&text, 220)
        },
    })
}

fn file_change_text(item: &Value, completed: bool) -> String {
    let action = if completed {
        "finished editing"
    } else {
        "editing"
    };
    let paths = item
        .get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .filter_map(|change| string_at(change, &["path"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return format!("{action} files");
    }
    let unique = paths
        .into_iter()
        .fold(Vec::<String>::new(), |mut out, path| {
            if !out.contains(&path) {
                out.push(path);
            }
            out
        });
    format!("{action} {}", one_line(&unique.join(", "), 180))
}

fn progress_fact_text(value: &Value) -> String {
    let name = string_at(value, &["name"])
        .or_else(|| string_at(value, &["toolName"]))
        .or_else(|| string_at(value, &["params", "name"]))
        .or_else(|| string_at(value, &["params", "toolName"]))
        .unwrap_or_else(|| "tool".to_owned());
    format!("waiting on {name}")
}

fn assistant_tool_fact(value: &Value) -> Option<ActivityFact> {
    let content = value.get("content").and_then(Value::as_array)?;
    let tool = content
        .iter()
        .find(|item| string_at(item, &["type"]).as_deref() == Some("tool_use"))?;
    Some(ActivityFact {
        kind: "tool",
        text: format!("using {}", tool_name(tool)),
    })
}

fn tool_result_fact(value: &Value) -> Option<ActivityFact> {
    let content = value.get("content").and_then(Value::as_array)?;
    if content.iter().any(|item| {
        string_at(item, &["type"]).as_deref() == Some("tool_result")
            || string_at(item, &["tool_use_id"]).is_some()
    }) {
        return Some(ActivityFact {
            kind: "tool",
            text: "reading tool results".to_owned(),
        });
    }
    None
}

fn tool_name(item: &Value) -> String {
    string_at(item, &["name"])
        .or_else(|| string_at(item, &["toolName"]))
        .or_else(|| string_at(item, &["tool_name"]))
        .or_else(|| string_at(item, &["serverLabel"]))
        .or_else(|| string_at(item, &["server_label"]))
        .unwrap_or_else(|| "tool".to_owned())
}

fn array_text(value: Option<&Value>) -> Option<String> {
    let texts = value?
        .as_array()?
        .iter()
        .filter_map(|item| {
            if let Some(text) = item.as_str() {
                return Some(text.to_owned());
            }
            string_at(item, &["text"])
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| texts.join(" "))
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_at(value, &[*key]))
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn strip_plan_marker(value: &str) -> String {
    let mut text = value.trim();
    if let Some(rest) = text.strip_prefix("- ") {
        text = rest;
    } else if let Some(rest) = text.strip_prefix("* ") {
        text = rest;
    }
    for marker in ["[ ] ", "[x] ", "[X] "] {
        if let Some(rest) = text.strip_prefix(marker) {
            text = rest;
        }
    }
    text.trim().to_owned()
}

fn unwrap_shell_command(command: &str) -> String {
    let trimmed = command.trim();
    let Some(rest) = trimmed.strip_prefix("/bin/bash -lc ") else {
        return trimmed.to_owned();
    };
    rest.trim()
        .trim_matches(|ch| ch == '"' || ch == '\'')
        .trim()
        .to_owned()
}

fn one_line(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut out = normalized
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn sanitize_summary(summary: &str) -> Option<String> {
    let summary = one_line(summary.trim().trim_matches('"').trim_matches('\''), 180);
    (!summary.is_empty()).then_some(summary)
}

fn is_terminal_session_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "session.execution_completed"
            | "session.execution_failed"
            | "session.execution_cancelled"
            | "session.stream_error"
            | "session.stdout_pump_failed"
    )
}

#[derive(Clone)]
struct ActivitySummaryClient {
    api_key: String,
    client: reqwest::Client,
    max_output_tokens: u16,
    model: String,
    responses_url: String,
}

impl ActivitySummaryClient {
    fn new(config: &ActivitySummaryConfig) -> Result<Self, ActivitySummaryError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(ActivitySummaryError::Http)?;
        let responses_url = format!("{}/responses", config.base_url.trim_end_matches('/'));
        Ok(Self {
            api_key: config.api_key.clone(),
            client,
            max_output_tokens: config.max_output_tokens,
            model: config.model.clone(),
            responses_url,
        })
    }

    async fn summarize(&self, prompt: &str) -> Result<String, ActivitySummaryError> {
        let response = self
            .client
            .post(&self.responses_url)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model.as_str(),
                "instructions": SYSTEM_PROMPT,
                "input": prompt,
                "max_output_tokens": self.max_output_tokens,
                "store": false,
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(ActivitySummaryError::OpenAiStatus {
                body: redact_openai_error_body(&body),
                status,
            });
        }
        let value = serde_json::from_str::<Value>(&body)?;
        if let Some(reason) = string_at(&value, &["incomplete_details", "reason"]) {
            return Err(ActivitySummaryError::Incomplete { reason });
        }
        extract_response_text(&value).ok_or(ActivitySummaryError::MissingOutputText)
    }
}

fn extract_response_text(value: &Value) -> Option<String> {
    if let Some(text) = string_at(value, &["output_text"]) {
        return Some(text);
    }
    let output = value.get("output")?.as_array()?;
    let mut parts = Vec::new();
    for item in output {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for content_item in content {
            if let Some(text) = string_at(content_item, &["text"]) {
                parts.push(text);
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn redact_openai_error_body(body: &str) -> String {
    let body = one_line(body, 300);
    let marker = "Incorrect API key provided:";
    let Some(marker_index) = body.find(marker) else {
        return body;
    };
    let value_start = marker_index + marker.len();
    let value_end = body[value_start..]
        .find('.')
        .map(|offset| value_start + offset)
        .unwrap_or(body.len());
    format!(
        "{} [redacted]{}",
        body[..value_start].trim_end(),
        &body[value_end..]
    )
}

#[derive(Debug, Error)]
pub(crate) enum ActivitySummaryError {
    #[error("activity summary HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("activity summary OpenAI request failed with {status}: {body}")]
    OpenAiStatus { status: StatusCode, body: String },
    #[error("activity summary OpenAI response incomplete: {reason}")]
    Incomplete { reason: String },
    #[error("activity summary OpenAI response did not include output text")]
    MissingOutputText,
    #[error("activity summary JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("activity summary session store error: {0}")]
    Store(#[from] SessionStoreError),
    #[error("activity summary thread key error: {0}")]
    ThreadKey(#[from] ThreadKeyError),
}

#[cfg(test)]
mod tests {
    use centaur_session_core::ThreadKey;
    use time::OffsetDateTime;

    use super::*;

    fn event(line: Value) -> SessionEvent {
        SessionEvent {
            event_id: 7,
            thread_key: ThreadKey::parse("test:thread").unwrap(),
            execution_id: Some("exec-1".to_owned()),
            event_type: SESSION_OUTPUT_LINE_EVENT.to_owned(),
            payload: Value::String(line.to_string()),
            created_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn projects_plan_update_into_activity_fact() {
        let fact = activity_fact_from_output_event(&event(json!({
            "type": "turn.plan.updated",
            "plan": [
                {"step": "Inspect App Server events", "status": "completed"},
                {"step": "Add activity summary worker", "status": "in_progress"}
            ]
        })))
        .unwrap();

        assert_eq!(
            fact,
            ActivityFact {
                kind: "plan",
                text: "working on Add activity summary worker".to_owned(),
            }
        );
    }

    #[test]
    fn projects_command_event_without_output() {
        let fact = activity_fact_from_output_event(&event(json!({
            "method": "item/started",
            "params": {
                "item": {
                    "id": "cmd-1",
                    "type": "commandExecution",
                    "command": "/bin/bash -lc 'rg session.activity'"
                }
            }
        })))
        .unwrap();

        assert_eq!(
            fact,
            ActivityFact {
                kind: "command",
                text: "running rg session.activity".to_owned(),
            }
        );
    }

    #[test]
    fn ignores_agent_commentary_messages_as_activity() {
        let fact = activity_fact_from_output_event(&event(json!({
            "method": "item/started",
            "params": {
                "item": {
                    "id": "msg-1",
                    "phase": "commentary",
                    "text": "",
                    "type": "agentMessage"
                }
            }
        })));

        assert_eq!(fact, None);
    }

    #[test]
    fn system_prompt_requires_conversational_goal_status() {
        assert!(SYSTEM_PROMPT.contains("first-person"));
        assert!(SYSTEM_PROMPT.contains("under 45 characters"));
        assert!(SYSTEM_PROMPT.contains("Describe the goal"));
        assert!(SYSTEM_PROMPT.contains("not the exact"));
        assert!(SYSTEM_PROMPT.contains("Avoid mechanics"));
        assert!(SYSTEM_PROMPT.contains("infer the"));
        assert!(SYSTEM_PROMPT.contains("Do not mention tests"));
        assert!(SYSTEM_PROMPT.contains("Use user-facing words"));
        assert!(SYSTEM_PROMPT.contains("\"I'm checking the fix\""));
        assert!(SYSTEM_PROMPT.contains("Never write more than 45 characters"));
        assert!(SYSTEM_PROMPT.contains("Do not refer to \"the agent\""));
    }

    #[test]
    fn extracts_output_text_from_responses_body() {
        let text = extract_response_text(&json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "I'm inspecting events."}
                    ]
                }
            ]
        }))
        .unwrap();

        assert_eq!(text, "I'm inspecting events.");
    }

    #[test]
    fn detects_incomplete_responses_body() {
        let reason = string_at(
            &json!({
                "status": "incomplete",
                "incomplete_details": {"reason": "max_output_tokens"},
                "output": [
                    {"type": "reasoning", "content": [], "summary": []}
                ]
            }),
            &["incomplete_details", "reason"],
        )
        .unwrap();

        assert_eq!(reason, "max_output_tokens");
    }

    #[test]
    fn redacts_openai_invalid_key_errors() {
        let redacted = redact_openai_error_body(
            r#"{"error":{"message":"Incorrect API key provided: sk-svc-secret. You can find your API key at https://platform.openai.com/account/api-keys."}}"#,
        );

        assert!(redacted.contains("Incorrect API key provided: [redacted]"));
        assert!(!redacted.contains("sk-svc-secret"));
    }

    #[test]
    fn throttles_unchanged_activity() {
        let mut state = ExecutionActivity {
            facts: VecDeque::new(),
            last_attempt_at: None,
            last_published_signature: None,
            last_summary: None,
            max_facts: 4,
        };
        let now = Instant::now();
        state.push(ActivityFact {
            kind: "tool",
            text: "using github".to_owned(),
        });
        assert!(state.prepare_publish(now, Duration::from_secs(8)).is_some());
        state.last_published_signature = Some(state.signature());
        assert!(
            state
                .prepare_publish(now + Duration::from_secs(9), Duration::from_secs(8))
                .is_none()
        );
    }
}
