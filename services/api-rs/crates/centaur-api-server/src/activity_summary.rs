use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use centaur_session_core::{MessageRole, SessionEvent, ThreadKey, ThreadKeyError};
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
Write one first-person present-tense sentence of at most 40 characters, including \
spaces, as if you are the agent. The hard limit is 45 characters: anything longer is \
thrown away, so when in doubt cut words and use the shortest name for things. \
Describe the current step or latest finding, not the overall session goal: say what \
you are doing or learned right now, like \"I'm computing TPS from blocks\", \
\"I found the chain config\", or \"I'm blocked on metrics access\". Take the newest \
facts labeled commentary, plan, or tool as the current step; earlier facts are only \
context. Name one specific thing from the facts (a chain, PR, partner, tool, or \
topic); avoid generic words like details, info, items, update, or summary, and avoid \
repeating the session goal word for word. Each status must say something new \
compared to the previous status sentence; if you cannot, output exactly SKIP. If the \
facts only show setup, help output, dependency installs, builds, command output, \
logs, tests, or other mechanics, output exactly SKIP. Do not mention commands, \
paths, IDs, or flags. Do not refer to \"the agent\". No markdown, no quotes, no \
event IDs, and no speculation.";

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
        let goal = if self.states.contains_key(execution_id) {
            None
        } else {
            self.activity_goal_context(&event.thread_key).await?
        };
        let now = Instant::now();
        let publish = {
            let state = self
                .states
                .entry(execution_id.to_owned())
                .or_insert_with(|| ExecutionActivity::new(self.config.max_facts, goal));
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
        if self
            .states
            .get(execution_id)
            .and_then(|state| state.last_summary.as_deref())
            .is_some_and(|last| summaries_are_similar(last, &summary))
        {
            debug!(summary, "discarded redundant session activity summary");
            return Ok(());
        }

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

    async fn activity_goal_context(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<String>, ActivitySummaryError> {
        if let Some(title) = self.store.get_session_title(thread_key).await?
            && let Some(title) = clean_goal_text(&title)
        {
            return Ok(Some(title));
        }

        let messages = self.store.list_messages(thread_key).await?;
        let goal = messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message_parts_text(&message.parts));
        Ok(goal.and_then(|goal| clean_goal_text(&goal)))
    }
}

#[derive(Debug)]
struct ExecutionActivity {
    facts: VecDeque<ActivityFact>,
    goal: Option<String>,
    last_attempt_at: Option<Instant>,
    last_published_signature: Option<String>,
    last_summary: Option<String>,
    max_facts: usize,
}

impl ExecutionActivity {
    fn new(max_facts: usize, goal: Option<String>) -> Self {
        Self {
            facts: VecDeque::with_capacity(max_facts),
            goal,
            last_attempt_at: None,
            last_published_signature: None,
            last_summary: None,
            max_facts,
        }
    }

    fn push(&mut self, fact: ActivityFact) {
        if !fact.is_publishable() {
            return;
        }
        if self
            .facts
            .iter()
            .any(|existing| existing.kind == fact.kind && existing.text == fact.text)
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
        if !self.facts.iter().any(ActivityFact::is_publishable) {
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
        if let Some(goal) = self.goal.as_deref() {
            lines.push(format!("Session goal: {goal}"));
        }
        lines.push("Recent activity facts, oldest to newest:".to_owned());
        for fact in self.facts.iter().filter(|fact| fact.is_publishable()) {
            lines.push(format!("- {}: {}", fact.kind, fact.text));
        }
        lines.join("\n")
    }

    fn signature(&self) -> String {
        let goal = self.goal.as_deref().unwrap_or_default();
        std::iter::once(format!("goal={goal}"))
            .chain(
                self.facts
                    .iter()
                    .filter(|fact| fact.is_publishable())
                    .map(|fact| format!("{}={}", fact.kind, fact.text)),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivitySignal {
    High,
    Low,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActivityFact {
    kind: &'static str,
    signal: ActivitySignal,
    text: String,
}

impl ActivityFact {
    fn high(kind: &'static str, text: impl Into<String>) -> Self {
        Self {
            kind,
            signal: ActivitySignal::High,
            text: text.into(),
        }
    }

    fn low(kind: &'static str, text: impl Into<String>) -> Self {
        Self {
            kind,
            signal: ActivitySignal::Low,
            text: text.into(),
        }
    }

    fn is_publishable(&self) -> bool {
        self.signal == ActivitySignal::High
    }
}

fn message_parts_text(parts: &[Value]) -> Option<String> {
    let text = parts
        .iter()
        .filter_map(message_part_text)
        .collect::<Vec<_>>()
        .join(" ");
    (!text.trim().is_empty()).then_some(text)
}

fn message_part_text(part: &Value) -> Option<String> {
    if let Some(text) = part.as_str() {
        return Some(text.trim().to_owned()).filter(|text| !text.is_empty());
    }
    string_at(part, &["text"])
        .or_else(|| string_at(part, &["content"]))
        .or_else(|| string_at(part, &["title"]))
}

fn clean_goal_text(value: &str) -> Option<String> {
    let text = one_line(value, 160);
    let lower = text.to_ascii_lowercase();
    if lower.is_empty()
        || matches!(
            lower.as_str(),
            "continue" | "go on" | "ok" | "okay" | "yes" | "yep" | "sure"
        )
    {
        return None;
    }
    Some(text)
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
        "item.plan.delta" => string_field(value, &["delta", "text"])
            .map(|text| ActivityFact::high("plan", format!("planning {}", one_line(&text, 180)))),
        "item.reasoning.summaryTextDelta" | "item.reasoning.textDelta" => {
            string_field(value, &["delta", "text"])
                .map(|text| ActivityFact::high("thinking", one_line(&text, 220)))
        }
        "item.commandExecution.outputDelta" => None,
        "item.mcpToolCall.progress" => Some(ActivityFact::high("tool", progress_fact_text(value))),
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
    Some(ActivityFact::high(
        "plan",
        format!("working on {}", one_line(&strip_plan_marker(&step), 180)),
    ))
}

fn item_fact(value: &Value, normalized_event_type: &str) -> Option<ActivityFact> {
    let item = protocol_item(value)?;
    let item_type = string_at(item, &["type"]).unwrap_or_default();
    let completed = normalized_event_type == "item.completed";
    match item_type.as_str() {
        "commandExecution" | "command_execution" => {
            let command = string_at(item, &["command"]).unwrap_or_else(|| "command".to_owned());
            command_fact(&command, completed)
        }
        "fileChange" | "file_change" => Some(ActivityFact::high(
            "files",
            file_change_text(item, completed),
        )),
        "reasoning" => reasoning_item_fact(item, completed),
        "mcpToolCall" | "mcp_tool_call" | "dynamicToolCall" | "dynamic_tool_call" => {
            let name = tool_name(item);
            let action = if completed { "finished using" } else { "using" };
            Some(ActivityFact::high("tool", format!("{action} {name}")))
        }
        "agentMessage" | "agent_message" => agent_message_fact(item, completed),
        "plan" => string_at(item, &["text"]).map(|text| {
            ActivityFact::high("plan", format!("updated plan {}", one_line(&text, 180)))
        }),
        _ => None,
    }
}

fn command_fact(command: &str, completed: bool) -> Option<ActivityFact> {
    let command = unwrap_shell_command(command);
    if is_low_signal_command(&command) {
        return Some(ActivityFact::low(
            "command",
            low_signal_command_label(&command),
        ));
    }
    let tool = command_tool_name(&command)?;
    let action = if completed { "finished using" } else { "using" };
    Some(ActivityFact::high("tool", format!("{action} {tool}")))
}

fn agent_message_fact(item: &Value, completed: bool) -> Option<ActivityFact> {
    if !completed {
        return None;
    }
    let phase = string_at(item, &["phase"]).unwrap_or_default();
    if phase != "commentary" {
        return None;
    }
    let text = string_at(item, &["text"])?;
    if is_low_signal_commentary(&text) {
        return None;
    }
    Some(ActivityFact::high("commentary", one_line(&text, 220)))
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
    Some(ActivityFact::high(
        "thinking",
        if completed {
            format!("finished thinking about {}", one_line(&text, 180))
        } else {
            one_line(&text, 220)
        },
    ))
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
    Some(ActivityFact::high(
        "tool",
        format!("using {}", tool_name(tool)),
    ))
}

fn tool_result_fact(value: &Value) -> Option<ActivityFact> {
    let content = value.get("content").and_then(Value::as_array)?;
    if content.iter().any(|item| {
        string_at(item, &["type"]).as_deref() == Some("tool_result")
            || string_at(item, &["tool_use_id"]).is_some()
    }) {
        return Some(ActivityFact::low("tool", "reading tool results"));
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

fn command_tool_name(command: &str) -> Option<String> {
    let first = command
        .split_whitespace()
        .next()?
        .trim_matches(|ch| ch == '"' || ch == '\'');
    let name = first.rsplit('/').next().unwrap_or(first);
    if name.is_empty() || is_shell_or_package_command(name) {
        return None;
    }
    Some(name.to_owned())
}

fn is_low_signal_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or_default();
    lower.is_empty()
        || lower == "command"
        || lower.contains(" --help")
        || lower.ends_with(" --help")
        || lower.contains(" -h")
        || lower.contains("centaur-tools list")
        || lower.contains("centaur-tools refresh")
        || lower.contains("uv sync")
        || lower.contains("uv pip install")
        || lower.contains("pip install")
        || lower.contains("pnpm install")
        || lower.contains("npm install")
        || lower.contains("cargo build")
        || lower.contains("cargo check")
        || lower.contains("cargo test")
        || lower.contains("cargo fmt")
        || lower.contains("ruff ")
        || lower.contains("pytest")
        || lower.contains("helm template")
        || lower.contains("helm lint")
        || matches!(
            first,
            "rg" | "grep"
                | "sed"
                | "awk"
                | "cat"
                | "ls"
                | "find"
                | "git"
                | "kubectl"
                | "jq"
                | "curl"
                | "python"
                | "python3"
                | "node"
                | "sh"
                | "bash"
        )
}

fn low_signal_command_label(command: &str) -> String {
    let lower = command.to_ascii_lowercase();
    if lower.contains(" --help") || lower.ends_with(" --help") || lower.contains(" -h") {
        "checking tool help".to_owned()
    } else if lower.contains("install") || lower.contains("build") {
        "setup work".to_owned()
    } else {
        "mechanical command".to_owned()
    }
}

fn is_shell_or_package_command(name: &str) -> bool {
    matches!(
        name,
        "bash"
            | "sh"
            | "zsh"
            | "python"
            | "python3"
            | "node"
            | "bun"
            | "uv"
            | "pip"
            | "pnpm"
            | "npm"
            | "cargo"
            | "git"
            | "kubectl"
            | "rg"
            | "grep"
            | "sed"
            | "awk"
            | "cat"
            | "ls"
            | "find"
            | "jq"
            | "curl"
    )
}

fn is_low_signal_commentary(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.is_empty()
        || lower == "i'll take a look."
        || lower == "i\u{2019}ll take a look."
        || lower == "i'll check."
        || lower == "i\u{2019}ll check."
        || lower == "i'm working on it."
        || lower == "i\u{2019}m working on it."
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
    let summary = summary
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .trim_end_matches('.')
        .to_owned();
    if summary.eq_ignore_ascii_case("skip") || summary.chars().count() > 45 {
        return None;
    }
    if is_generic_summary(&summary) {
        return None;
    }
    (!summary.is_empty()).then_some(summary)
}

fn is_generic_summary(summary: &str) -> bool {
    let normalized = normalize_summary(summary);
    normalized.is_empty()
        || normalized.contains("gathering details")
        || normalized.contains("gathering info")
        || (normalized.contains("gathering") && normalized.contains("info"))
        || normalized.contains("listing available")
        || normalized.contains("available items")
        || normalized.contains("preparing your update")
        || normalized.contains("preparing your summary")
        || (normalized.contains("preparing your") && normalized.contains("summary"))
        || normalized.contains("checking the request")
        || normalized.contains("working on it")
        || normalized.contains("making progress")
        || normalized.contains("handling the task")
}

fn summaries_are_similar(previous: &str, candidate: &str) -> bool {
    let previous = summary_keywords(previous);
    let candidate = summary_keywords(candidate);
    if previous.is_empty() || candidate.is_empty() {
        return false;
    }
    let shared = candidate
        .iter()
        .filter(|word| previous.contains(*word))
        .count();
    let smaller = previous.len().min(candidate.len());
    shared * 4 >= smaller * 3
}

fn summary_keywords(summary: &str) -> Vec<String> {
    normalize_summary(summary)
        .split_whitespace()
        .filter(|word| {
            !matches!(
                *word,
                "i" | "m"
                    | "im"
                    | "i'm"
                    | "am"
                    | "the"
                    | "a"
                    | "an"
                    | "for"
                    | "to"
                    | "on"
                    | "your"
                    | "my"
                    | "this"
                    | "that"
            )
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_summary(summary: &str) -> String {
    summary
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
            ActivityFact::high("plan", "working on Add activity summary worker")
        );
    }

    #[test]
    fn drops_low_signal_command_events() {
        let fact = activity_fact_from_output_event(&event(json!({
            "method": "item/started",
            "params": {
                "item": {
                    "id": "cmd-1",
                    "type": "commandExecution",
                    "command": "/bin/bash -lc 'centaur-tools list'"
                }
            }
        })))
        .unwrap();

        assert_eq!(fact, ActivityFact::low("command", "mechanical command"));
    }

    #[test]
    fn projects_tool_command_by_tool_name() {
        let fact = activity_fact_from_output_event(&event(json!({
            "method": "item/started",
            "params": {
                "item": {
                    "id": "cmd-1",
                    "type": "commandExecution",
                    "command": "/bin/bash -lc 'websearch search --query usdG yield'"
                }
            }
        })))
        .unwrap();

        assert_eq!(fact, ActivityFact::high("tool", "using websearch"));
    }

    #[test]
    fn captures_completed_agent_commentary_as_activity() {
        let fact = activity_fact_from_output_event(&event(json!({
            "method": "item/completed",
            "params": {
                "item": {
                    "id": "msg-1",
                    "phase": "commentary",
                    "text": "I'll trace the USDG vault yield source.",
                    "type": "agentMessage"
                }
            }
        })))
        .unwrap();

        assert_eq!(
            fact,
            ActivityFact::high("commentary", "I'll trace the USDG vault yield source.")
        );
    }

    #[test]
    fn system_prompt_requires_conversational_step_status() {
        assert!(SYSTEM_PROMPT.contains("first-person"));
        assert!(SYSTEM_PROMPT.contains("at most 40 characters"));
        assert!(SYSTEM_PROMPT.contains("hard limit is 45 characters"));
        assert!(SYSTEM_PROMPT.contains("current step or latest finding"));
        assert!(SYSTEM_PROMPT.contains("not the overall session goal"));
        assert!(SYSTEM_PROMPT.contains("Name one specific thing"));
        assert!(SYSTEM_PROMPT.contains("output exactly SKIP"));
        assert!(SYSTEM_PROMPT.contains("Do not mention commands"));
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
        let mut state = ExecutionActivity::new(4, Some("Investigate USDG vault yield".to_owned()));
        let now = Instant::now();
        state.push(ActivityFact::high("tool", "using websearch"));
        assert!(state.prepare_publish(now, Duration::from_secs(8)).is_some());
        state.last_published_signature = Some(state.signature());
        assert!(
            state
                .prepare_publish(now + Duration::from_secs(9), Duration::from_secs(8))
                .is_none()
        );
    }

    #[test]
    fn skips_low_signal_only_activity() {
        let mut state = ExecutionActivity::new(4, Some("Investigate USDG vault yield".to_owned()));
        let now = Instant::now();
        state.push(ActivityFact::low("command", "checking tool help"));

        assert!(state.prepare_publish(now, Duration::from_secs(8)).is_none());
    }

    #[test]
    fn prompt_includes_session_goal() {
        let mut state = ExecutionActivity::new(4, Some("Investigate USDG vault yield".to_owned()));
        state.push(ActivityFact::high("tool", "using websearch"));

        let prompt = state.prompt();

        assert!(prompt.contains("Session goal: Investigate USDG vault yield"));
        assert!(prompt.contains("- tool: using websearch"));
    }

    #[test]
    fn sanitizes_useless_summaries() {
        assert_eq!(sanitize_summary("SKIP"), None);
        assert_eq!(
            sanitize_summary("I'm gathering details for the USDG info."),
            None
        );
        assert_eq!(
            sanitize_summary("I'm preparing your USDG vault update summary"),
            None
        );
        assert_eq!(
            sanitize_summary("I'm checking USDG yield sources."),
            Some("I'm checking USDG yield sources".to_owned())
        );
        assert_eq!(
            sanitize_summary("I'm checking a summary that is far too long for Slack status text"),
            None
        );
    }

    #[test]
    fn detects_redundant_summary_phrasing() {
        assert!(summaries_are_similar(
            "I'm checking USDG yield sources",
            "I'm checking USDG yield source"
        ));
        assert!(!summaries_are_similar(
            "I'm checking USDG yield sources",
            "I'm comparing vault contract events"
        ));
    }
}
