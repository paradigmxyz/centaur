use std::collections::HashMap;
use std::io::{self, BufRead};

use codex_app_server_protocol::JSONRPCMessage;
use serde_json::Value;

use crate::wire::is_known_untyped_server_notification;
use crate::{HarnessServerError, Result};

#[derive(Debug, Default)]
struct AgentDeltaState {
    text_from_deltas: String,
    delta_count: usize,
    completed_text: Option<String>,
}

#[derive(Debug, Default)]
struct AgentDeltaReport {
    delta_items: usize,
    completed_agent_items: usize,
}

pub fn run_validate_agent_deltas() -> Result<()> {
    let stdin = io::stdin();
    let report = validate_agent_deltas(stdin.lock())?;
    eprintln!(
        "agent delta validation ok: delta_items={} completed_agent_items={}",
        report.delta_items, report.completed_agent_items
    );
    Ok(())
}

fn validate_agent_deltas<R: BufRead>(reader: R) -> Result<AgentDeltaReport> {
    let mut states: HashMap<String, AgentDeltaState> = HashMap::new();
    let mut completed_agent_items = 0usize;

    for raw in reader.lines() {
        let line = raw?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let message: JSONRPCMessage = serde_json::from_str(trimmed)?;
        if let JSONRPCMessage::Notification(notification) = message {
            let method = notification.method.clone();
            if !is_known_untyped_server_notification(&method) {
                let _typed = codex_app_server_protocol::ServerNotification::try_from(notification)
                    .map_err(|error| HarnessServerError::InvalidServerNotification {
                        message: error.to_string(),
                    })?;
            }
        }

        let value: Value = serde_json::from_str(trimmed)?;
        let method = value.get("method").and_then(Value::as_str);
        match method {
            Some("item/agentMessage/delta") => {
                let params = value.get("params").ok_or_else(|| {
                    HarnessServerError::Protocol(
                        "item/agentMessage/delta missing params".to_string(),
                    )
                })?;
                let item_id = required_str(params, "itemId", "item/agentMessage/delta")?;
                let delta = required_str(params, "delta", "item/agentMessage/delta")?;
                let state = states.entry(item_id.to_string()).or_default();
                state.text_from_deltas.push_str(delta);
                state.delta_count += 1;
            }
            Some("item/completed") => {
                let item = value
                    .get("params")
                    .and_then(|params| params.get("item"))
                    .ok_or_else(|| {
                        HarnessServerError::Protocol("item/completed missing item".to_string())
                    })?;
                if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                    completed_agent_items += 1;
                    let item_id = required_str(item, "id", "item/completed agentMessage")?;
                    let completed_text = required_str(item, "text", "item/completed agentMessage")?;
                    states
                        .entry(item_id.to_string())
                        .or_default()
                        .completed_text = Some(completed_text.to_string());
                }
            }
            _ => {}
        }
    }

    for (item_id, state) in &states {
        if state.delta_count == 0 {
            continue;
        }
        let Some(completed_text) = &state.completed_text else {
            return Err(HarnessServerError::Protocol(format!(
                "agentMessage item `{item_id}` emitted {} deltas but no completed item",
                state.delta_count
            )));
        };
        if completed_text != &state.text_from_deltas {
            return Err(HarnessServerError::Protocol(format!(
                "agentMessage item `{item_id}` delta text does not match completed text \
                 (delta bytes: {}, completed bytes: {})",
                state.text_from_deltas.len(),
                completed_text.len()
            )));
        }
    }

    Ok(AgentDeltaReport {
        delta_items: states
            .values()
            .filter(|state| state.delta_count > 0)
            .count(),
        completed_agent_items,
    })
}

fn required_str<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| HarnessServerError::Protocol(format!("{context} missing `{field}`")))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::validate_agent_deltas;

    #[test]
    fn validates_matching_agent_delta_reconstruction() {
        let input = concat!(
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"u","itemId":"msg","delta":"hello "}}"#,
            "\n",
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"u","itemId":"msg","delta":"world"}}"#,
            "\n",
            r#"{"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"agentMessage","id":"msg","text":"hello world","phase":null,"memoryCitation":null},"completedAtMs":1}}"#,
            "\n",
        );

        let report = validate_agent_deltas(Cursor::new(input)).expect("valid delta stream");

        assert_eq!(report.delta_items, 1);
        assert_eq!(report.completed_agent_items, 1);
    }

    #[test]
    fn rejects_mismatched_agent_delta_reconstruction() {
        let input = concat!(
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"u","itemId":"msg","delta":"hello"}}"#,
            "\n",
            r#"{"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"agentMessage","id":"msg","text":"goodbye","phase":null,"memoryCitation":null},"completedAtMs":1}}"#,
            "\n",
        );

        let error = validate_agent_deltas(Cursor::new(input))
            .expect_err("mismatched delta stream should fail")
            .to_string();

        assert!(error.contains("delta text does not match completed text"));
    }

    #[test]
    fn rejects_delta_without_completed_item() {
        let input = r#"{"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"u","itemId":"msg","delta":"hello"}}"#;

        let error = validate_agent_deltas(Cursor::new(input))
            .expect_err("unterminated delta stream should fail")
            .to_string();

        assert!(error.contains("but no completed item"));
    }
}
