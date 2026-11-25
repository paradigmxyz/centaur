//! Projects a nanocode session log into the session event stream Centaur
//! clients read: `session.execution_started`, `session.output.line` (Codex
//! app-server notifications), `session.first_token`, and one terminal event
//! per turn. The log is the only state, so any replica can serve any cursor.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::translate::Turn;

/// Events derived from one log entry share its seq; the low bits order them.
const EVENTS_PER_ENTRY: u64 = 64;

/// One event on the Centaur session stream.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamEvent {
    pub id: u64,
    pub event_type: &'static str,
    /// The raw line for `session.output.line`; one-line JSON otherwise.
    pub data: String,
    pub execution_id: String,
    pub terminal: bool,
}

/// One entry of the nanocode session log, already expanded.
pub struct Entry<'a> {
    pub seq: u64,
    pub request_id: Option<&'a str>,
    pub created_at_ms: u64,
    pub body: &'a Value,
}

pub struct Projector {
    session_id: String,
    thread_key: String,
    thread_announced: bool,
    turns: HashMap<String, Running>,
}

struct Running {
    turn: Turn,
    started_at_ms: u64,
    first_token: bool,
}

impl Projector {
    pub fn new(session_id: &str, thread_key: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            thread_key: thread_key.to_owned(),
            thread_announced: false,
            turns: HashMap::new(),
        }
    }

    /// The stream events one log entry yields, in order.
    pub fn project(&mut self, entry: &Entry<'_>) -> Vec<StreamEvent> {
        let Some(request_id) = entry
            .request_id
            .or_else(|| entry.body["request_id"].as_str())
        else {
            return Vec::new();
        };
        let mut out = Out::new(entry.seq, request_id);
        match entry.body["type"].as_str() {
            Some("running") => self.start(&mut out, request_id, entry.created_at_ms),
            Some("result") => self.finish(&mut out, request_id, entry.body),
            _ if entry.body["source"] == "agent" => {
                self.agent(&mut out, request_id, entry);
            }
            _ => {}
        }
        out.events
    }

    fn start(&mut self, out: &mut Out, request_id: &str, at_ms: u64) {
        if self.turns.contains_key(request_id) {
            // A resumed turn logs `running` again; it is the same execution.
            return;
        }
        out.push(
            "session.execution_started",
            json!({"execution_id": request_id, "thread_key": self.thread_key}).to_string(),
            false,
        );
        let (turn, opening) = Turn::start(&self.session_id, request_id, !self.thread_announced);
        self.thread_announced = true;
        out.lines(opening);
        self.turns.insert(
            request_id.to_owned(),
            Running {
                turn,
                started_at_ms: at_ms,
                first_token: false,
            },
        );
    }

    fn agent(&mut self, out: &mut Out, request_id: &str, entry: &Entry<'_>) {
        let Some(running) = self.turns.get_mut(request_id) else {
            return;
        };
        let event = &entry.body["event"];
        let kind = event["type"].as_str().unwrap_or_default();
        let payload = &event["payload"];
        out.lines(running.turn.event(kind, payload, entry.created_at_ms));
        if !running.first_token
            && kind == "assistant.delta"
            // Providers that do not mark phases stream only answer text.
            && payload["phase"] != "commentary"
            && payload["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        {
            running.first_token = true;
            out.push(
                "session.first_token",
                json!({
                    "execution_id": request_id,
                    "thread_key": self.thread_key,
                    "harness_type": "nanocode",
                    "latency_ms": entry.created_at_ms.saturating_sub(running.started_at_ms),
                })
                .to_string(),
                false,
            );
        }
    }

    fn finish(&mut self, out: &mut Out, request_id: &str, body: &Value) {
        let status = body["status"].as_str().unwrap_or_default();
        if status == "cancelled" && body["steered"] == true {
            // Folded into the running turn: there is no separate execution.
            return;
        }
        let mut running = self.turns.remove(request_id);
        let base = json!({"execution_id": request_id, "thread_key": self.thread_key});
        match status {
            "completed" => {
                if let Some(running) = &mut running {
                    out.lines(running.turn.complete());
                }
                let mut payload = base;
                payload["completion_reason"] = json!("turn_completed");
                if let Some(text) = body.pointer("/ok/message").and_then(Value::as_str) {
                    payload["result_text"] = json!(text);
                }
                out.push("session.execution_completed", payload.to_string(), true);
            }
            "cancelled" => {
                if let Some(running) = &mut running {
                    out.lines(running.turn.interrupt());
                }
                let mut payload = base;
                payload["reason"] = json!("turn_interrupted");
                out.push("session.execution_cancelled", payload.to_string(), true);
            }
            _ => {
                let error = body["error"]
                    .as_str()
                    .map_or_else(|| format!("turn ended as {status}"), str::to_owned);
                if let Some(running) = &mut running {
                    out.lines(running.turn.fail(&error));
                }
                let mut payload = base;
                payload["error"] = json!(error);
                out.push("session.execution_failed", payload.to_string(), true);
            }
        }
    }
}

struct Out {
    seq: u64,
    request_id: String,
    events: Vec<StreamEvent>,
}

impl Out {
    fn new(seq: u64, request_id: &str) -> Self {
        Self {
            seq,
            request_id: request_id.to_owned(),
            events: Vec::new(),
        }
    }

    fn push(&mut self, event_type: &'static str, data: String, terminal: bool) {
        let index = self.events.len() as u64;
        assert!(
            index < EVENTS_PER_ENTRY,
            "one log entry derived more than {EVENTS_PER_ENTRY} events"
        );
        self.events.push(StreamEvent {
            id: self.seq * EVENTS_PER_ENTRY + index,
            event_type,
            data,
            execution_id: self.request_id.clone(),
            terminal,
        });
    }

    fn lines(&mut self, lines: Vec<String>) {
        for line in lines {
            self.push("session.output.line", line, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(seq: u64, request_id: &'a str, at: u64, body: &'a Value) -> Entry<'a> {
        Entry {
            seq,
            request_id: Some(request_id),
            created_at_ms: at,
            body,
        }
    }

    fn agent(kind: &str, payload: Value) -> Value {
        json!({"source": "agent", "event": {"seq": 1, "type": kind, "payload": payload}})
    }

    fn kinds(events: &[StreamEvent]) -> Vec<String> {
        events
            .iter()
            .map(|event| {
                if event.event_type == "session.output.line" {
                    serde_json::from_str::<Value>(&event.data).unwrap()["method"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                } else {
                    event.event_type.to_owned()
                }
            })
            .collect()
    }

    #[test]
    fn a_completed_turn_projects_the_client_stream() {
        let mut projector = Projector::new("sess", "slack:T:C:1");
        let queued = json!({"type": "queued", "request_id": "exe_1"});
        assert!(
            projector
                .project(&entry(1, "exe_1", 1_000, &queued))
                .is_empty()
        );

        let running = json!({"type": "running", "request_id": "exe_1"});
        let started = projector.project(&entry(2, "exe_1", 1_000, &running));
        assert_eq!(
            kinds(&started),
            [
                "session.execution_started",
                "thread/started",
                "turn/started"
            ]
        );
        assert_eq!(
            started.iter().map(|event| event.id).collect::<Vec<_>>(),
            [128, 129, 130]
        );

        let delta = agent(
            "assistant.delta",
            json!({"item_id": "m", "phase": "final_answer", "text": "Hi"}),
        );
        let streamed = projector.project(&entry(3, "exe_1", 1_250, &delta));
        assert_eq!(
            kinds(&streamed),
            [
                "item/started",
                "item/agentMessage/delta",
                "session.first_token"
            ]
        );
        let first_token: Value = serde_json::from_str(&streamed[2].data).unwrap();
        assert_eq!(first_token["latency_ms"], 250);
        let commentary = agent(
            "assistant.delta",
            json!({"item_id": "c", "phase": "commentary", "text": "Looking"}),
        );
        let mut thinking = Projector::new("sess", "t");
        thinking.project(&entry(1, "exe_9", 0, &running));
        assert!(
            thinking
                .project(&entry(2, "exe_9", 10, &commentary))
                .iter()
                .all(|event| event.event_type != "session.first_token")
        );
        let unmarked = agent("assistant.delta", json!({"item_id": "u", "text": "Hi"}));
        assert!(
            thinking
                .project(&entry(3, "exe_9", 20, &unmarked))
                .iter()
                .any(|event| event.event_type == "session.first_token")
        );
        assert!(
            projector
                .project(&entry(4, "exe_1", 1_300, &delta))
                .iter()
                .all(|event| event.event_type != "session.first_token")
        );

        let result = json!({"type": "result", "request_id": "exe_1", "status": "completed", "ok": {"message": "Hi"}});
        let end = projector.project(&entry(5, "exe_1", 1_400, &result));
        assert_eq!(
            kinds(&end),
            ["turn/completed", "session.execution_completed"]
        );
        assert!(end[1].terminal);
        let completed: Value = serde_json::from_str(&end[1].data).unwrap();
        assert_eq!(completed["result_text"], "Hi");
        assert_eq!(completed["execution_id"], "exe_1");

        let second = json!({"type": "running", "request_id": "exe_2"});
        let next = projector.project(&entry(6, "exe_2", 2_000, &second));
        assert_eq!(kinds(&next), ["session.execution_started", "turn/started"]);
    }

    #[test]
    fn failures_cancellations_and_steers() {
        let mut projector = Projector::new("sess", "t");
        let running = json!({"type": "running", "request_id": "a"});
        projector.project(&entry(1, "a", 0, &running));
        let failed =
            json!({"type": "result", "request_id": "a", "status": "failed", "error": "boom"});
        let end = projector.project(&entry(2, "a", 0, &failed));
        assert_eq!(kinds(&end), ["turn/completed", "session.execution_failed"]);
        let payload: Value = serde_json::from_str(&end[1].data).unwrap();
        assert_eq!(payload["error"], "boom");

        let queued_then_cancelled =
            json!({"type": "result", "request_id": "b", "status": "cancelled", "cancelled": true});
        let end = projector.project(&entry(3, "b", 0, &queued_then_cancelled));
        assert_eq!(kinds(&end), ["session.execution_cancelled"]);

        let steered = json!({"type": "result", "request_id": "c", "status": "cancelled", "cancelled": true, "steered": true});
        assert!(projector.project(&entry(4, "c", 0, &steered)).is_empty());
    }
}
