//! Translates nanocode agent events into the Codex app-server notifications
//! every Centaur harness speaks, through harness-server's turn normalizer.
//! Tool calls become Codex `mcpToolCall` items, which the normalizer does not
//! model, so this writes them in the app-server wire shape itself.

use std::collections::{HashMap, HashSet};

use harness_server::{
    BridgeConfig, CodexTurnNormalizer, NormalizedContent, NormalizedEvent,
    notification_to_wire_value, protocol::ServerNotification,
};
use serde_json::{Value, json};
use tracing::warn;

/// The MCP server name renderers show for the Code Mode tools.
const CODE_MODE_SERVER: &str = "centaur";
/// The server name of any other nanocode tool.
const NANOCODE_SERVER: &str = "nanocode";

/// One turn's translation state.
pub struct Turn {
    normalizer: CodexTurnNormalizer,
    agent_messages: HashSet<String>,
    /// Tool calls started and not yet completed, by call id.
    tool_calls: HashMap<String, ToolCall>,
    /// Tool calls already completed, so a replayed result is not repeated.
    tools_done: HashSet<String>,
    finished: bool,
}

struct ToolCall {
    tool: String,
    arguments: Value,
    started_at_ms: u64,
}

impl Turn {
    /// Start a turn, returning it with its opening lines (`turn/started`,
    /// preceded by `thread/started` when `announce_thread`).
    pub fn start(thread_id: &str, turn_id: &str, announce_thread: bool) -> (Self, Vec<String>) {
        let mut config = BridgeConfig::new(thread_id, turn_id);
        config.cli_version = "centaur-brain".to_owned();
        config.model_provider = "nanocode".to_owned();
        let mut normalizer = CodexTurnNormalizer::new(config);
        let opening = normalizer.start_notifications(announce_thread);
        let turn = Self {
            normalizer,
            agent_messages: HashSet::new(),
            tool_calls: HashMap::new(),
            tools_done: HashSet::new(),
            finished: false,
        };
        (turn, lines(opening.map_err(|error| error.to_string())))
    }

    /// Translate one nanocode agent event of this turn, given its wire type
    /// (`assistant.delta`, `run.completed`, ...), payload, and log time.
    pub fn event(&mut self, kind: &str, payload: &Value, at_ms: u64) -> Vec<String> {
        let text = |key: &str| payload[key].as_str().unwrap_or_default().to_owned();
        let mut out = Vec::new();
        match kind {
            "assistant.delta" => {
                let item_id = text("item_id");
                if self.agent_messages.insert(item_id.clone()) {
                    out.extend(self.process(&NormalizedEvent::AgentMessageStarted {
                        item_id: item_id.clone(),
                        stop_reason: stop_reason(payload),
                    }));
                }
                out.extend(self.process(&NormalizedEvent::AgentTextDelta {
                    item_id,
                    delta: text("text"),
                }));
            }
            "assistant.message" => {
                let item_id = text("item_id");
                self.agent_messages.insert(item_id.clone());
                out.extend(self.process(&NormalizedEvent::AssistantMessage {
                    partial: false,
                    stop_reason: stop_reason(payload),
                    content: vec![NormalizedContent::AgentText {
                        item_id,
                        text: text("text"),
                    }],
                }));
            }
            "reasoning.summary.delta" => {
                let item_id = payload["item_id"].as_str().map_or_else(
                    || format!("reasoning-{}", payload["model_call_index"]),
                    str::to_owned,
                );
                out.extend(self.process(&NormalizedEvent::ReasoningTextDelta {
                    item_id,
                    delta: text("text"),
                }));
            }
            "run.error" if payload["observational"] != true => {
                // Recorded for the turn's end; a nanocode run error is not
                // terminal on its own, so it is not an app-server `error`.
                let message = payload["message"]
                    .as_str()
                    .or_else(|| payload["error"].as_str())
                    .unwrap_or("run error")
                    .to_owned();
                out.extend(self.process(&NormalizedEvent::Result {
                    error: Some(message),
                }));
            }
            "tool.call" => out.extend(self.tool_started(payload, at_ms)),
            "tool.result" => out.extend(self.tool_completed(payload, at_ms)),
            "run.completed" => out.extend(self.finish(Ok(None))),
            "run.failed" => {
                let status = payload["status"].as_str().unwrap_or_default();
                if matches!(status, "cancelled" | "canceled") {
                    out.extend(self.finish(Err(())));
                } else {
                    let error = payload["error"]
                        .as_str()
                        .unwrap_or("turn failed")
                        .to_owned();
                    out.extend(self.finish(Ok(Some(error))));
                }
            }
            _ => {}
        }
        out
    }

    /// End the turn as completed, unless it already ended.
    pub fn complete(&mut self) -> Vec<String> {
        self.finish(Ok(None))
    }

    /// End the turn as failed, unless it already ended.
    pub fn fail(&mut self, error: &str) -> Vec<String> {
        self.finish(Ok(Some(error.to_owned())))
    }

    /// End the turn as interrupted, unless it already ended.
    pub fn interrupt(&mut self) -> Vec<String> {
        self.finish(Err(()))
    }

    /// `Ok(error)` completes (or fails, with an error); `Err(())` interrupts.
    fn finish(&mut self, outcome: Result<Option<String>, ()>) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        // A call the turn ended during never completes on its own.
        let mut out = Vec::new();
        let mut open = self.tool_calls.drain().collect::<Vec<_>>();
        open.sort_by_key(|(_, call)| call.started_at_ms);
        for (id, call) in open {
            let at_ms = call.started_at_ms;
            let item = self.tool_item(
                &id,
                &call,
                "failed",
                Value::Null,
                json!({"message": "the turn ended before the tool call finished"}),
                None,
            );
            out.push(self.item_notification("item/completed", item, at_ms));
        }
        let finished = match outcome {
            Ok(error) => self.normalizer.finish_turn(error),
            Err(()) => self.normalizer.finish_turn_interrupted(),
        };
        out.extend(lines(
            finished
                .map(|notification| notification.into_iter().collect())
                .map_err(|error| error.to_string()),
        ));
        out
    }

    /// `item/started` for a `tool.call` (`{call_id, tool, arguments}`).
    fn tool_started(&mut self, payload: &Value, at_ms: u64) -> Vec<String> {
        let Some(id) = payload["call_id"].as_str() else {
            return Vec::new();
        };
        if self.finished || self.tools_done.contains(id) || self.tool_calls.contains_key(id) {
            return Vec::new();
        }
        let call = ToolCall {
            tool: payload["tool"].as_str().unwrap_or("tool").to_owned(),
            arguments: arguments(&payload["arguments"]),
            started_at_ms: at_ms,
        };
        let item = self.tool_item(id, &call, "inProgress", Value::Null, Value::Null, None);
        self.tool_calls.insert(id.to_owned(), call);
        vec![self.item_notification("item/started", item, at_ms)]
    }

    /// `item/completed` for a `tool.result` (`{call_id, tool, status,
    /// duration_ns, result}`).
    fn tool_completed(&mut self, payload: &Value, at_ms: u64) -> Vec<String> {
        let Some(id) = payload["call_id"].as_str() else {
            return Vec::new();
        };
        if self.finished || !self.tools_done.insert(id.to_owned()) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let call = match self.tool_calls.remove(id) {
            Some(call) => call,
            None => {
                // A result whose call this projection never saw.
                let call = ToolCall {
                    tool: payload["tool"].as_str().unwrap_or("tool").to_owned(),
                    arguments: Value::Null,
                    started_at_ms: at_ms,
                };
                let item = self.tool_item(id, &call, "inProgress", Value::Null, Value::Null, None);
                out.push(self.item_notification("item/started", item, at_ms));
                call
            }
        };
        let text = result_text(&payload["result"]);
        let duration_ms = payload["duration_ns"]
            .as_u64()
            .map(|nanos| nanos / 1_000_000)
            .unwrap_or_else(|| at_ms.saturating_sub(call.started_at_ms));
        let item = if payload["status"] == "completed" {
            self.tool_item(
                id,
                &call,
                "completed",
                json!({"content": [{"type": "text", "text": text}], "structuredContent": null}),
                Value::Null,
                Some(duration_ms),
            )
        } else {
            let message = if text.is_empty() {
                format!(
                    "the tool call was {}",
                    payload["status"].as_str().unwrap_or("failed")
                )
            } else {
                text
            };
            self.tool_item(
                id,
                &call,
                "failed",
                Value::Null,
                json!({"message": message}),
                Some(duration_ms),
            )
        };
        out.push(self.item_notification("item/completed", item, at_ms));
        out
    }

    fn tool_item(
        &self,
        id: &str,
        call: &ToolCall,
        status: &str,
        result: Value,
        error: Value,
        duration_ms: Option<u64>,
    ) -> Value {
        let server = if call.tool.starts_with("centaur_") {
            CODE_MODE_SERVER
        } else {
            NANOCODE_SERVER
        };
        json!({
            "type": "mcpToolCall",
            "id": id,
            "server": server,
            "tool": call.tool,
            "status": status,
            "arguments": call.arguments,
            "result": result,
            "error": error,
            "durationMs": duration_ms,
        })
    }

    /// An `item/started` or `item/completed` notification line.
    fn item_notification(&self, method: &str, item: Value, at_ms: u64) -> String {
        let at = if method == "item/started" {
            "startedAtMs"
        } else {
            "completedAtMs"
        };
        json!({
            "method": method,
            "params": {
                "item": item,
                "threadId": self.normalizer.thread_id(),
                "turnId": self.normalizer.turn_id(),
                (at): at_ms,
            },
        })
        .to_string()
    }

    fn process(&mut self, event: &NormalizedEvent) -> Vec<String> {
        lines(
            self.normalizer
                .process_event(event)
                .map_err(|error| error.to_string()),
        )
    }
}

/// A tool call's arguments: the model's JSON object, also when nanocode
/// carries it as JSON text.
fn arguments(raw: &Value) -> Value {
    match raw {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| raw.clone()),
        other => other.clone(),
    }
}

/// The text of a nanocode tool result: plain text, or content items.
fn result_text(result: &Value) -> String {
    match result {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// nanocode marks agent messages by phase; the normalizer reads the phase from
/// a Claude-style stop reason.
fn stop_reason(payload: &Value) -> Option<String> {
    match payload["phase"].as_str() {
        Some("final_answer") => Some("end_turn".to_owned()),
        Some("commentary") => Some("tool_use".to_owned()),
        _ => None,
    }
}

fn lines(notifications: Result<Vec<ServerNotification>, String>) -> Vec<String> {
    match notifications {
        Ok(notifications) => notifications
            .iter()
            .filter_map(
                |notification| match notification_to_wire_value(notification) {
                    Ok(value) => Some(value.to_string()),
                    Err(error) => {
                        warn!(%error, "cannot encode a notification");
                        None
                    }
                },
            )
            .collect(),
        Err(error) => {
            warn!(%error, "cannot translate a nanocode event");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn methods(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|line| parse(line)["method"].as_str().unwrap().to_owned())
            .collect()
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn a_streamed_answer_becomes_an_app_server_turn() {
        let (mut turn, opening) = Turn::start("thread-1", "turn-1", true);
        assert_eq!(methods(&opening), ["thread/started", "turn/started"]);
        assert_eq!(parse(&opening[0])["params"]["thread"]["id"], "thread-1");

        let delta = |text: &str| json!({"item_id": "msg-1", "phase": "final_answer", "text": text});
        let first = turn.event("assistant.delta", &delta("Hi"), 0);
        assert_eq!(methods(&first), ["item/started", "item/agentMessage/delta"]);
        assert_eq!(parse(&first[1])["params"]["delta"], "Hi");
        assert_eq!(
            methods(&turn.event("assistant.delta", &delta(" there"), 0)),
            ["item/agentMessage/delta"]
        );

        let message = turn.event(
            "assistant.message",
            &json!({"item_id": "msg-1", "phase": "final_answer", "text": "Hi there"}),
            0,
        );
        let completed = message
            .iter()
            .map(|line| parse(line))
            .find(|value| value["method"] == "item/completed")
            .unwrap();
        assert_eq!(completed["params"]["item"]["text"], "Hi there");
        assert_eq!(completed["params"]["item"]["phase"], "final_answer");

        let end = turn.event("run.completed", &json!({"status": "completed"}), 0);
        assert_eq!(methods(&end), ["turn/completed"]);
        assert_eq!(parse(&end[0])["params"]["turn"]["status"], "completed");
        assert!(turn.fail("late").is_empty());
        assert!(turn.complete().is_empty());
    }

    #[test]
    fn failures_and_cancellations_end_the_turn_once() {
        let (mut failed, _) = Turn::start("t", "turn-1", false);
        assert!(
            failed
                .event("run.error", &json!({"message": "rate limited"}), 0)
                .is_empty()
        );
        let end = failed.event(
            "run.failed",
            &json!({"status": "failed", "error": "boom"}),
            0,
        );
        let turn = &parse(&end[0])["params"]["turn"];
        assert_eq!(turn["status"], "failed");
        assert_eq!(turn["error"]["message"], "boom");

        let (mut cancelled, _) = Turn::start("t", "turn-2", false);
        let end = cancelled.event("run.failed", &json!({"status": "cancelled"}), 0);
        assert_eq!(parse(&end[0])["params"]["turn"]["status"], "interrupted");
        assert!(cancelled.interrupt().is_empty());

        let (mut interrupted, _) = Turn::start("t", "turn-3", false);
        let end = interrupted.interrupt();
        assert_eq!(parse(&end[0])["params"]["turn"]["status"], "interrupted");
    }

    #[test]
    fn tool_calls_become_mcp_tool_call_items() {
        let (mut turn, _) = Turn::start("thread-1", "turn-1", false);
        let started = turn.event(
            "tool.call",
            &json!({
                "call_id": "call-1",
                "tool": "centaur_execute",
                "arguments": {"code": "return 1"},
                "model_call_index": 0,
            }),
            1_000,
        );
        assert_eq!(methods(&started), ["item/started"]);
        let params = &parse(&started[0])["params"];
        assert_eq!(params["threadId"], "thread-1");
        assert_eq!(params["turnId"], "turn-1");
        assert_eq!(params["startedAtMs"], 1_000);
        assert_eq!(
            params["item"],
            json!({
                "type": "mcpToolCall",
                "id": "call-1",
                "server": "centaur",
                "tool": "centaur_execute",
                "status": "inProgress",
                "arguments": {"code": "return 1"},
                "result": null,
                "error": null,
                "durationMs": null,
            })
        );
        // A replayed call is not announced twice.
        assert!(
            turn.event(
                "tool.call",
                &json!({"call_id": "call-1", "tool": "centaur_execute", "arguments": {}}),
                1_001
            )
            .is_empty()
        );

        let result = json!({
            "call_id": "call-1",
            "tool": "centaur_execute",
            "status": "completed",
            "duration_ns": 2_500_000_000_u64,
            "result": "1",
            "structured_result": "1",
        });
        let completed = turn.event("tool.result", &result, 3_500);
        assert_eq!(methods(&completed), ["item/completed"]);
        let params = &parse(&completed[0])["params"];
        assert_eq!(params["completedAtMs"], 3_500);
        let item = &params["item"];
        assert_eq!(item["status"], "completed");
        assert_eq!(item["durationMs"], 2_500);
        assert_eq!(item["arguments"], json!({"code": "return 1"}));
        assert_eq!(
            item["result"],
            json!({"content": [{"type": "text", "text": "1"}], "structuredContent": null})
        );
        assert_eq!(item["error"], Value::Null);
        assert!(turn.event("tool.result", &result, 3_600).is_empty());

        let failed = turn.event(
            "tool.call",
            &json!({"call_id": "call-2", "tool": "centaur_catalog_load", "arguments": "{\"tool\":\"x\"}"}),
            4_000,
        );
        assert_eq!(
            parse(&failed[0])["params"]["item"]["arguments"],
            json!({"tool": "x"})
        );
        let failed = turn.event(
            "tool.result",
            &json!({
                "call_id": "call-2",
                "tool": "centaur_catalog_load",
                "status": "failed",
                "duration_ns": 1_000_000,
                "result": [{"type": "input_text", "text": "Unknown or unavailable tool x."}],
            }),
            4_001,
        );
        let item = &parse(&failed[0])["params"]["item"];
        assert_eq!(item["status"], "failed");
        assert_eq!(item["result"], Value::Null);
        assert_eq!(item["error"]["message"], "Unknown or unavailable tool x.");
    }

    #[test]
    fn a_turn_ending_mid_call_fails_the_call_first() {
        let (mut turn, _) = Turn::start("t", "turn-1", false);
        turn.event(
            "tool.call",
            &json!({"call_id": "c", "tool": "centaur_catalog_search", "arguments": {"query": "x"}}),
            10,
        );
        let end = turn.interrupt();
        assert_eq!(methods(&end), ["item/completed", "turn/completed"]);
        let item = &parse(&end[0])["params"]["item"];
        assert_eq!(item["status"], "failed");
        assert_eq!(item["server"], "centaur");
        assert!(
            turn.event(
                "tool.result",
                &json!({"call_id": "c", "status": "completed"}),
                20
            )
            .is_empty()
        );
    }

    #[test]
    fn other_events_are_dropped() {
        let (mut turn, _) = Turn::start("t", "turn-1", false);
        assert!(turn.event("model.call.started", &json!({}), 0).is_empty());
        assert!(
            turn.event(
                "run.error",
                &json!({"observational": true, "error": "x"}),
                0
            )
            .is_empty()
        );
    }
}
