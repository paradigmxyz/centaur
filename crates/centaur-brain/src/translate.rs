//! Translates nanocode agent events into the Codex app-server notifications
//! every Centaur harness speaks, through harness-server's turn normalizer.

use harness_server::{
    BridgeConfig, CodexTurnNormalizer, NormalizedContent, NormalizedEvent,
    notification_to_wire_value, protocol::ServerNotification,
};
use serde_json::Value;
use tracing::warn;

/// One turn's translation state.
pub struct Turn {
    normalizer: CodexTurnNormalizer,
    agent_messages: std::collections::HashSet<String>,
    finished: bool,
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
            agent_messages: std::collections::HashSet::new(),
            finished: false,
        };
        (turn, lines(opening.map_err(|error| error.to_string())))
    }

    /// Translate one nanocode agent event of this turn, given its wire type
    /// (`assistant.delta`, `run.completed`, ...) and payload.
    pub fn event(&mut self, kind: &str, payload: &Value) -> Vec<String> {
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
        let finished = match outcome {
            Ok(error) => self.normalizer.finish_turn(error),
            Err(()) => self.normalizer.finish_turn_interrupted(),
        };
        lines(
            finished
                .map(|notification| notification.into_iter().collect())
                .map_err(|error| error.to_string()),
        )
    }

    fn process(&mut self, event: &NormalizedEvent) -> Vec<String> {
        lines(
            self.normalizer
                .process_event(event)
                .map_err(|error| error.to_string()),
        )
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
        let first = turn.event("assistant.delta", &delta("Hi"));
        assert_eq!(methods(&first), ["item/started", "item/agentMessage/delta"]);
        assert_eq!(parse(&first[1])["params"]["delta"], "Hi");
        assert_eq!(
            methods(&turn.event("assistant.delta", &delta(" there"))),
            ["item/agentMessage/delta"]
        );

        let message = turn.event(
            "assistant.message",
            &json!({"item_id": "msg-1", "phase": "final_answer", "text": "Hi there"}),
        );
        let completed = message
            .iter()
            .map(|line| parse(line))
            .find(|value| value["method"] == "item/completed")
            .unwrap();
        assert_eq!(completed["params"]["item"]["text"], "Hi there");
        assert_eq!(completed["params"]["item"]["phase"], "final_answer");

        let end = turn.event("run.completed", &json!({"status": "completed"}));
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
                .event("run.error", &json!({"message": "rate limited"}))
                .is_empty()
        );
        let end = failed.event("run.failed", &json!({"status": "failed", "error": "boom"}));
        let turn = &parse(&end[0])["params"]["turn"];
        assert_eq!(turn["status"], "failed");
        assert_eq!(turn["error"]["message"], "boom");

        let (mut cancelled, _) = Turn::start("t", "turn-2", false);
        let end = cancelled.event("run.failed", &json!({"status": "cancelled"}));
        assert_eq!(parse(&end[0])["params"]["turn"]["status"], "interrupted");
        assert!(cancelled.interrupt().is_empty());

        let (mut interrupted, _) = Turn::start("t", "turn-3", false);
        let end = interrupted.interrupt();
        assert_eq!(parse(&end[0])["params"]["turn"]["status"], "interrupted");
    }

    #[test]
    fn other_events_are_dropped() {
        let (mut turn, _) = Turn::start("t", "turn-1", false);
        assert!(turn.event("model.call.started", &json!({})).is_empty());
        assert!(
            turn.event("run.error", &json!({"observational": true, "error": "x"}))
                .is_empty()
        );
    }
}
