use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

use centaur_session_core::{MessageRole, SessionEvent, ThreadKey, ThreadKeyError};
use centaur_session_runtime::SESSION_OUTPUT_LINE_EVENT;
use centaur_session_sqlx::{PgSessionStore, SessionEventNotification, SessionStoreError};
use clap::ValueEnum;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
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
    pub(crate) provider: ActivitySummaryProvider,
    pub(crate) proxy_url: Option<String>,
    pub(crate) proxy_ca_cert: Option<PathBuf>,
    pub(crate) max_facts: usize,
    pub(crate) max_output_tokens: u16,
    pub(crate) min_interval: Duration,
    pub(crate) model: String,
    /// `None` omits the parameter, for servers that reject an unknown field.
    pub(crate) reasoning_effort: Option<String>,
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
            provider = ?self.config.provider,
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
            Ok(Some(summary)) => summary,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(
                    %error,
                    provider = ?self.config.provider,
                    disabled = self.client.backoff.disabled,
                    retry_after_secs = self.client.backoff.retry_at.map(|at| at.saturating_duration_since(Instant::now()).as_secs()),
                    "failed to generate session activity summary"
                );
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
                    "provider": self.config.provider.to_possible_value().map(|value| value.get_name().to_owned()),
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
    reasoning_effort: Option<String>,
    responses_url: String,
    provider: ActivitySummaryProvider,
    backoff: SummaryBackoff,
}

impl ActivitySummaryClient {
    fn new(config: &ActivitySummaryConfig) -> Result<Self, ActivitySummaryError> {
        let mut client = reqwest::Client::builder()
            .timeout(config.timeout)
            // Do not inherit a chat/sandbox proxy or follow redirects with credentials.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none());
        if let Some(url) = &config.proxy_url {
            client = client
                .proxy(reqwest::Proxy::all(url).map_err(|_| ActivitySummaryError::Configuration)?);
            if let Some(path) = &config.proxy_ca_cert {
                let pem = std::fs::read(path).map_err(|_| ActivitySummaryError::Configuration)?;
                let cert = reqwest::Certificate::from_pem(&pem)
                    .map_err(|_| ActivitySummaryError::Configuration)?;
                client = client.add_root_certificate(cert);
            }
        }
        let client = client
            .build()
            .map_err(|_| ActivitySummaryError::Configuration)?;
        let endpoint = match config.provider {
            ActivitySummaryProvider::Openai | ActivitySummaryProvider::Codex => "responses",
            ActivitySummaryProvider::Anthropic | ActivitySummaryProvider::Claude => "messages",
        };
        let responses_url = format!("{}/{endpoint}", config.base_url.trim_end_matches('/'));
        let url =
            reqwest::Url::parse(&responses_url).map_err(|_| ActivitySummaryError::Configuration)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ActivitySummaryError::Configuration);
        }
        Ok(Self {
            api_key: config.api_key.clone(),
            client,
            max_output_tokens: config.max_output_tokens,
            model: config.model.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
            responses_url,
            provider: config.provider,
            backoff: SummaryBackoff::default(),
        })
    }

    /// The summary budget is small, so a server that resolves an absent effort
    /// to its highest level spends the whole budget reasoning and returns an
    /// `incomplete` response with no message. Sending an explicit effort avoids
    /// depending on the server's default; `None` omits it for servers that
    /// reject the field.
    fn request_body(&self, prompt: &str) -> Value {
        if matches!(
            self.provider,
            ActivitySummaryProvider::Anthropic | ActivitySummaryProvider::Claude
        ) {
            let system = if self.provider == ActivitySummaryProvider::Claude {
                json!([
                    {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."},
                    {"type": "text", "text": SYSTEM_PROMPT},
                ])
            } else {
                json!(SYSTEM_PROMPT)
            };
            return json!({
                "model": self.model,
                "system": system,
                "messages": [{"role": "user", "content": prompt}],
                "max_tokens": self.max_output_tokens,
            });
        }
        let mut body = json!({
            "model": self.model.as_str(),
            "instructions": SYSTEM_PROMPT,
            "input": prompt,
            "max_output_tokens": self.max_output_tokens,
            "store": false,
        });
        if self.provider == ActivitySummaryProvider::Codex {
            // The subscription endpoint requires SSE and structured input and
            // rejects the public API's max_output_tokens parameter.
            body.as_object_mut()
                .expect("object")
                .remove("max_output_tokens");
            body["stream"] = json!(true);
            body["input"] =
                json!([{"role": "user", "content": [{"type": "input_text", "text": prompt}]}]);
        }
        if let Some(effort) = &self.reasoning_effort
            && let Some(object) = body.as_object_mut()
        {
            object.insert("reasoning".to_owned(), json!({ "effort": effort }));
        }
        body
    }

    async fn summarize(&mut self, prompt: &str) -> Result<Option<String>, ActivitySummaryError> {
        if !self.backoff.ready(Instant::now()) {
            return Ok(None);
        }
        match self.request_summary(prompt).await {
            Ok(summary) => {
                self.backoff = SummaryBackoff::default();
                Ok(Some(summary))
            }
            Err(error) => {
                self.backoff.record_failure(&error, Instant::now());
                Err(error)
            }
        }
    }

    async fn request_summary(&self, prompt: &str) -> Result<String, ActivitySummaryError> {
        let mut request = self
            .client
            .post(&self.responses_url)
            .json(&self.request_body(prompt));
        request = match self.provider {
            ActivitySummaryProvider::Openai => request.bearer_auth(&self.api_key),
            ActivitySummaryProvider::Codex => request.header("accept", "text/event-stream"),
            ActivitySummaryProvider::Anthropic => request
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", &self.api_key),
            ActivitySummaryProvider::Claude => request
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
                // Match the subscription protocol used by the sandbox's pinned CLI.
                .header("user-agent", "claude-cli/2.1.281")
                .header("accept", "application/json")
                .header("x-app", "cli"),
        };
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|seconds| Duration::from_secs(seconds.min(86400)));
            let body = response.json::<Value>().await.unwrap_or(Value::Null);
            return Err(ActivitySummaryError::ProviderStatus {
                status,
                quota_exhausted: quota_exhausted(&body),
                retry_after,
            });
        }
        if self.provider == ActivitySummaryProvider::Codex {
            let mut stream = response.bytes_stream().eventsource();
            let mut text = String::new();
            while let Some(event) = stream.next().await {
                let event = event.map_err(|_| ActivitySummaryError::Stream)?;
                if event.data == "[DONE]" {
                    break;
                }
                let value = serde_json::from_str::<Value>(&event.data)?;
                match value.get("type").and_then(Value::as_str) {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                            text.push_str(delta);
                        }
                    }
                    Some("response.completed") => {
                        return value
                            .get("response")
                            .and_then(extract_response_text)
                            .or_else(|| (!text.is_empty()).then_some(text))
                            .ok_or(ActivitySummaryError::MissingOutputText);
                    }
                    Some("response.incomplete") => return Err(ActivitySummaryError::Incomplete),
                    Some("error" | "response.failed") => {
                        let quota_exhausted =
                            quota_exhausted(value.get("response").unwrap_or(&value));
                        return Err(ActivitySummaryError::ProviderStatus {
                            status: if quota_exhausted {
                                StatusCode::TOO_MANY_REQUESTS
                            } else {
                                StatusCode::BAD_GATEWAY
                            },
                            quota_exhausted,
                            retry_after: None,
                        });
                    }
                    _ => {}
                }
            }
            return Err(ActivitySummaryError::Stream);
        }
        let body = response.text().await?;
        let value = serde_json::from_str::<Value>(&body)?;
        if value
            .get("incomplete_details")
            .is_some_and(|details| !details.is_null())
        {
            return Err(ActivitySummaryError::Incomplete);
        }
        if matches!(
            self.provider,
            ActivitySummaryProvider::Anthropic | ActivitySummaryProvider::Claude
        ) {
            if value.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
                return Err(ActivitySummaryError::Incomplete);
            }
            return extract_response_text(&json!({"output": [value]}))
                .ok_or(ActivitySummaryError::MissingOutputText);
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

#[derive(Debug, Error)]
pub(crate) enum ActivitySummaryError {
    // Neither upstream bodies nor URLs belong in logs: either may echo credentials.
    #[error("activity summary HTTP transport failed")]
    Http(#[from] reqwest::Error),
    #[error(
        "activity summary provider returned {status} (quota exhausted: {quota_exhausted}); requests suspended or backed off"
    )]
    ProviderStatus {
        status: StatusCode,
        quota_exhausted: bool,
        retry_after: Option<Duration>,
    },
    #[error("activity summary response incomplete")]
    Incomplete,
    #[error("activity summary response did not include output text")]
    MissingOutputText,
    #[error("activity summary JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("activity summary session store error: {0}")]
    Store(#[from] SessionStoreError),
    #[error("activity summary thread key error: {0}")]
    ThreadKey(#[from] ThreadKeyError),
    #[error("invalid activity summary endpoint, proxy, or CA configuration")]
    Configuration,
    #[error("activity summary stream ended without a completed response")]
    Stream,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ActivitySummaryProvider {
    Openai,
    Codex,
    Anthropic,
    Claude,
}

impl ActivitySummaryProvider {
    pub(crate) fn is_subscription(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ActivitySummaryBackend {
    Direct,
    IronProxy,
}

/// One circuit for the configured backend, shared across every execution in this worker.
#[derive(Clone, Default)]
struct SummaryBackoff {
    failures: u32,
    retry_at: Option<Instant>,
    disabled: bool,
}

impl SummaryBackoff {
    fn ready(&self, now: Instant) -> bool {
        !self.disabled && self.retry_at.is_none_or(|retry_at| now >= retry_at)
    }

    fn record_failure(&mut self, error: &ActivitySummaryError, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let mut delay = Duration::from_secs(30 * (1 << self.failures.saturating_sub(1).min(5)));
        if let ActivitySummaryError::ProviderStatus {
            status,
            quota_exhausted,
            retry_after,
        } = error
        {
            // Auth and invalid endpoint/model errors need an operator change.
            self.disabled = status.is_client_error()
                && !matches!(
                    *status,
                    StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
                )
                && !quota_exhausted;
            if *quota_exhausted {
                delay = delay.max(Duration::from_secs(900));
            }
            if let Some(retry_after) = retry_after {
                delay = delay.max(*retry_after);
            }
        }
        self.retry_at = Some(now + delay);
    }
}

fn quota_exhausted(value: &Value) -> bool {
    [
        value.pointer("/error/code"),
        value.pointer("/error/type"),
        value.get("code"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .any(|code| {
        matches!(
            code,
            "insufficient_quota" | "credit_balance_exhausted" | "usage_limit_reached"
        )
    })
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Bytes,
        http::{HeaderMap, Uri},
        response::IntoResponse,
        routing::post,
    };
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
    fn provider_errors_never_include_upstream_bodies() {
        let error = ActivitySummaryError::ProviderStatus {
            status: StatusCode::UNAUTHORIZED,
            quota_exhausted: false,
            retry_after: None,
        };
        assert_eq!(
            error.to_string(),
            "activity summary provider returned 401 Unauthorized (quota exhausted: false); requests suspended or backed off"
        );
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

    fn client_with_effort(effort: Option<&str>) -> ActivitySummaryClient {
        ActivitySummaryClient::new(&ActivitySummaryConfig {
            base_url: "http://localhost/v1".to_owned(),
            api_key: "key".to_owned(),
            provider: ActivitySummaryProvider::Openai,
            proxy_url: None,
            proxy_ca_cert: None,
            max_facts: 10,
            max_output_tokens: 128,
            min_interval: Duration::from_secs(1),
            model: "gpt-5.4-nano".to_owned(),
            reasoning_effort: effort.map(str::to_owned),
            timeout: Duration::from_secs(5),
        })
        .expect("client")
    }

    #[test]
    fn summary_request_carries_the_reasoning_effort() {
        let body = client_with_effort(Some("low")).request_body("prompt");
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["max_output_tokens"], 128);
    }

    /// A server that rejects an unknown field needs the parameter gone, not
    /// set to something it also does not understand.
    #[test]
    fn summary_request_omits_the_effort_when_unset() {
        let body = client_with_effort(None).request_body("prompt");
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn backoff_recovers_and_honors_quota_retry_after() {
        let now = Instant::now();
        let mut backoff = SummaryBackoff::default();
        for seconds in [30, 60, 120, 240, 480, 960, 960] {
            backoff.record_failure(&ActivitySummaryError::Stream, now);
            assert!(!backoff.ready(now + Duration::from_secs(seconds - 1)));
            assert!(backoff.ready(now + Duration::from_secs(seconds)));
        }
        backoff = SummaryBackoff::default();
        backoff.record_failure(
            &ActivitySummaryError::ProviderStatus {
                status: StatusCode::TOO_MANY_REQUESTS,
                quota_exhausted: true,
                retry_after: Some(Duration::from_secs(1800)),
            },
            now,
        );
        assert!(!backoff.ready(now + Duration::from_secs(1799)));
        assert!(backoff.ready(now + Duration::from_secs(1800)));
    }

    #[test]
    fn invalid_credentials_disable_until_restart() {
        let now = Instant::now();
        let mut backoff = SummaryBackoff::default();
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            backoff.record_failure(
                &ActivitySummaryError::ProviderStatus {
                    status,
                    quota_exhausted: false,
                    retry_after: None,
                },
                now,
            );
            assert!(!backoff.ready(now + Duration::from_secs(86400)));
        }
    }

    async fn mock_provider(
        status: StatusCode,
        body: String,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<(Uri, HeaderMap, Value)>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let app =
            Router::new().fallback(post(move |uri: Uri, headers: HeaderMap, bytes: Bytes| {
                let tx = tx.clone();
                let body = body.clone();
                async move {
                    tx.send((uri, headers, serde_json::from_slice(&bytes).unwrap()))
                        .unwrap();
                    (
                        status,
                        [
                            ("retry-after", "1800"),
                            ("content-type", "text/event-stream"),
                        ],
                        body,
                    )
                        .into_response()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, rx, server)
    }

    #[tokio::test]
    async fn direct_openai_and_anthropic_use_the_selected_protocol_and_key() {
        for provider in [
            ActivitySummaryProvider::Openai,
            ActivitySummaryProvider::Anthropic,
        ] {
            let response = match provider {
                ActivitySummaryProvider::Openai => {
                    json!({"output_text": "I'm tracing vault deposits"})
                }
                _ => {
                    json!({"content": [{"type": "text", "text": "I'm tracing vault deposits"}], "stop_reason": "end_turn"})
                }
            };
            let (url, mut requests, server) =
                mock_provider(StatusCode::OK, response.to_string()).await;
            let mut client = client_with_effort(Some("low"));
            client.provider = provider;
            client.responses_url = format!(
                "{url}/v1/{}",
                if provider == ActivitySummaryProvider::Openai {
                    "responses"
                } else {
                    "messages"
                }
            );
            assert_eq!(
                client.summarize("vault facts").await.unwrap().as_deref(),
                Some("I'm tracing vault deposits")
            );
            let (uri, headers, body) = requests.recv().await.unwrap();
            if provider == ActivitySummaryProvider::Openai {
                assert_eq!(uri.path(), "/v1/responses");
                assert_eq!(headers["authorization"], "Bearer key");
                assert_eq!(body["input"], "vault facts");
            } else {
                assert_eq!(uri.path(), "/v1/messages");
                assert_eq!(headers["x-api-key"], "key");
                assert_eq!(headers["anthropic-version"], "2023-06-01");
                assert_eq!(body["messages"][0]["content"], "vault facts");
                assert_eq!(body["max_tokens"], 128);
                assert!(body.get("reasoning").is_none());
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn subscription_calls_reach_the_explicit_proxy_without_api_credentials() {
        for provider in [
            ActivitySummaryProvider::Codex,
            ActivitySummaryProvider::Claude,
        ] {
            let response = if provider == ActivitySummaryProvider::Codex {
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"uncommitted\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"content\":[{\"type\":\"output_text\",\"text\":\"I'm tracing vault deposits\"}]}]}}\n\n".to_owned()
            } else {
                json!({"content": [{"type": "text", "text": "I'm tracing vault deposits"}], "stop_reason": "end_turn"}).to_string()
            };
            let (url, mut requests, server) = mock_provider(StatusCode::OK, response).await;
            let mut client = ActivitySummaryClient::new(&ActivitySummaryConfig {
                base_url: "http://provider.invalid/v1".to_owned(),
                api_key: "must-not-be-sent".to_owned(),
                provider,
                proxy_url: Some(url),
                proxy_ca_cert: None,
                max_facts: 10,
                max_output_tokens: 128,
                min_interval: Duration::from_secs(1),
                model: "subscription-model".to_owned(),
                reasoning_effort: Some("low".to_owned()),
                timeout: Duration::from_secs(5),
            })
            .unwrap();
            assert_eq!(
                client.summarize("vault facts").await.unwrap().as_deref(),
                Some("I'm tracing vault deposits")
            );
            let (uri, headers, body) = requests.recv().await.unwrap();
            assert_eq!(uri.host(), Some("provider.invalid"));
            assert!(headers.get("authorization").is_none());
            assert!(headers.get("x-api-key").is_none());
            assert_eq!(body["model"], "subscription-model");
            if provider == ActivitySummaryProvider::Codex {
                assert_eq!(uri.path(), "/v1/responses");
                assert_eq!(body["stream"], true);
                assert_eq!(body["input"][0]["content"][0]["text"], "vault facts");
                assert!(body.get("max_output_tokens").is_none());
            } else {
                assert_eq!(uri.path(), "/v1/messages");
                assert_eq!(
                    headers["anthropic-beta"],
                    "claude-code-20250219,oauth-2025-04-20"
                );
                assert_eq!(headers["user-agent"], "claude-cli/2.1.281");
                assert_eq!(body["system"][1]["text"], SYSTEM_PROMPT);
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn exhausted_quota_suppresses_calls_across_prompts_then_recovers() {
        let (url, mut requests, server) = mock_provider(StatusCode::TOO_MANY_REQUESTS, json!({"error": {"code": "credit_balance_exhausted", "message": "secret must not be logged"}}).to_string()).await;
        let mut client = client_with_effort(None);
        client.responses_url = url;
        let error = client.summarize("execution one").await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "activity summary provider returned 429 Too Many Requests (quota exhausted: true); requests suspended or backed off"
        );
        requests.recv().await.unwrap();
        assert_eq!(client.summarize("execution two").await.unwrap(), None);
        assert!(requests.try_recv().is_err());
        server.abort();

        let (url, mut requests, server) = mock_provider(
            StatusCode::OK,
            json!({"output_text": "I'm tracing vault deposits"}).to_string(),
        )
        .await;
        client.responses_url = url;
        client.backoff.retry_at = Some(Instant::now());
        assert!(client.summarize("execution three").await.unwrap().is_some());
        requests.recv().await.unwrap();
        assert_eq!(client.backoff.failures, 0);
        assert!(client.backoff.retry_at.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn codex_rejects_truncated_streams_without_publishing_partial_text() {
        let (url, _requests, server) = mock_provider(
            StatusCode::OK,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_owned(),
        )
        .await;
        let mut client = client_with_effort(None);
        client.provider = ActivitySummaryProvider::Codex;
        client.responses_url = url;
        assert!(matches!(
            client.summarize("facts").await,
            Err(ActivitySummaryError::Stream)
        ));
        assert!(!client.backoff.ready(Instant::now()));
        server.abort();
    }

    #[tokio::test]
    async fn codex_accepts_completed_deltas_and_backs_off_on_streamed_quota_errors() {
        for (body, expected) in [
            (
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"I'm tracing vault deposits\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test\"}}\n\n",
                Some("I'm tracing vault deposits"),
            ),
            (
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"usage_limit_reached\"}}}\n\n",
                None,
            ),
        ] {
            let (url, _requests, server) = mock_provider(StatusCode::OK, body.to_owned()).await;
            let mut client = client_with_effort(None);
            client.provider = ActivitySummaryProvider::Codex;
            client.responses_url = url;
            let result = client.summarize("facts").await;
            if let Some(expected) = expected {
                assert_eq!(result.unwrap().as_deref(), Some(expected));
            } else {
                assert!(matches!(
                    result,
                    Err(ActivitySummaryError::ProviderStatus {
                        quota_exhausted: true,
                        ..
                    })
                ));
                assert!(
                    !client
                        .backoff
                        .ready(Instant::now() + Duration::from_secs(899))
                );
            }
            server.abort();
        }
    }
}
