use std::collections::BTreeMap;
use std::io::Write;

use codex_utils_stream_parser::{InlineHiddenTagParser, InlineTagSpec, StreamTextParser};
use serde_json::{Value, json};

use crate::Result;
use crate::util::write_value;

#[derive(Default)]
pub(super) struct CodexCitationFilter {
    parsers: BTreeMap<(String, String, String), InlineHiddenTagParser<()>>,
}

impl CodexCitationFilter {
    pub(super) fn write_value<W: Write>(&mut self, stdout: &mut W, value: &Value) -> Result<()> {
        for value in self.normalize(value.clone()) {
            write_value(stdout, &value)?;
        }
        Ok(())
    }

    pub(super) fn finish<W: Write>(&mut self, stdout: &mut W) -> Result<()> {
        for (key, mut parser) in std::mem::take(&mut self.parsers) {
            let text = parser.finish().visible_text;
            if !text.is_empty() {
                write_value(stdout, &delta_notification(key, text))?;
            }
        }
        Ok(())
    }

    fn normalize(&mut self, mut value: Value) -> Vec<Value> {
        let mut output = Vec::new();
        match value.get("method").and_then(Value::as_str) {
            Some("item/agentMessage/delta") => {
                if let Some(item) = value["params"]["itemId"].as_str()
                    && let Some(key) = message_key(&value["params"], item)
                    && let Some(delta) = value["params"]["delta"].as_str()
                {
                    let parser = self.parsers.entry(key).or_insert_with(citation_parser);
                    value["params"]["delta"] = parser.push_str(delta).visible_text.into();
                }
            }
            Some("item/started" | "item/completed") => {
                if value["params"]["item"]["type"] == "agentMessage" {
                    if value["method"] == "item/completed"
                        && let Some(item_id) = value["params"]["item"]["id"].as_str()
                        && let Some(key) = message_key(&value["params"], item_id)
                        && let Some(mut parser) = self.parsers.remove(&key)
                    {
                        let text = parser.finish().visible_text;
                        if !text.is_empty() {
                            output.push(delta_notification(key, text));
                        }
                    }
                    normalize_item(&mut value["params"]["item"]);
                }
            }
            Some("turn/started" | "turn/completed") => {
                if value["method"] == "turn/completed"
                    && let Some(thread) = value["params"]["threadId"].as_str()
                    && let Some(turn) = value["params"]["turn"]["id"].as_str()
                {
                    let keys = self
                        .parsers
                        .keys()
                        .filter(|(t, u, _)| t == thread && u == turn)
                        .cloned()
                        .collect::<Vec<_>>();
                    for key in keys {
                        if let Some(mut parser) = self.parsers.remove(&key) {
                            let text = parser.finish().visible_text;
                            if !text.is_empty() {
                                output.push(delta_notification(key, text));
                            }
                        }
                    }
                }
                if let Some(turn) = value.pointer_mut("/params/turn") {
                    normalize_turn(turn);
                }
            }
            Some("thread/started") => {
                if let Some(thread) = value.pointer_mut("/params/thread") {
                    normalize_thread(thread);
                }
            }
            None => {
                if let Some(result) = value.get_mut("result") {
                    if let Some(turn) = result.get_mut("turn") {
                        normalize_turn(turn);
                    }
                    if let Some(thread) = result.get_mut("thread") {
                        normalize_thread(thread);
                    }
                }
            }
            _ => {}
        }
        output.push(value);
        output
    }
}

fn citation_parser() -> InlineHiddenTagParser<()> {
    InlineHiddenTagParser::new(vec![
        InlineTagSpec {
            tag: (),
            open: "cite",
            close: "",
        },
        InlineTagSpec {
            tag: (),
            open: "cite:ship:",
            close: ":walking:",
        },
    ])
}

fn message_key(params: &Value, item_id: &str) -> Option<(String, String, String)> {
    Some((
        params.get("threadId")?.as_str()?.to_owned(),
        params.get("turnId")?.as_str()?.to_owned(),
        item_id.to_owned(),
    ))
}

fn delta_notification((thread, turn, item): (String, String, String), text: String) -> Value {
    json!({
        "method": "item/agentMessage/delta",
        "params": {"threadId": thread, "turnId": turn, "itemId": item, "delta": text},
    })
}

fn normalize_item(item: &mut Value) {
    if item["type"] == "agentMessage"
        && let Some(text) = item["text"].as_str()
    {
        let mut parser = citation_parser();
        let mut visible = parser.push_str(text).visible_text;
        visible.push_str(&parser.finish().visible_text);
        item["text"] = visible.into();
    }
}

fn normalize_turn(turn: &mut Value) {
    if let Some(items) = turn.get_mut("items").and_then(Value::as_array_mut) {
        for item in items {
            normalize_item(item);
        }
    }
}

fn normalize_thread(thread: &mut Value) {
    if let Some(turns) = thread.get_mut("turns").and_then(Value::as_array_mut) {
        for turn in turns {
            normalize_turn(turn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(thread: &str, turn: &str, item: &str, text: &str) -> Value {
        delta_notification((thread.into(), turn.into(), item.into()), text.into())
    }

    fn visible_deltas(values: &[Value]) -> String {
        values
            .iter()
            .filter(|value| value["method"] == "item/agentMessage/delta")
            .filter_map(|value| value["params"]["delta"].as_str())
            .collect()
    }

    #[test]
    fn strips_citations_at_every_utf8_split_without_leaking_partial_tokens() {
        for marker in [
            "citeturn0search0turn1search2",
            "cite:ship:turn0search0:walking:",
        ] {
            let input = format!("é [source](https://example.com){marker} end");
            let expected = "é [source](https://example.com) end";
            for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
                let mut filter = CodexCitationFilter::default();
                let first = filter.normalize(delta("thread", "turn", "item", &input[..split]));
                let mut text = visible_deltas(&first);
                assert!(expected.starts_with(&text));
                text.push_str(&visible_deltas(&filter.normalize(delta(
                    "thread",
                    "turn",
                    "item",
                    &input[split..],
                ))));
                assert_eq!(text, expected, "split {split}");
            }
            let mut filter = CodexCitationFilter::default();
            let mut text = String::new();
            for character in input.chars() {
                text.push_str(&visible_deltas(&filter.normalize(delta(
                    "thread",
                    "turn",
                    "item",
                    &character.to_string(),
                ))));
                assert!(expected.starts_with(&text));
            }
            assert_eq!(text, expected);
        }
    }

    #[test]
    fn isolates_interleaved_items_threads_and_turns() {
        let mut filter = CodexCitationFilter::default();
        assert_eq!(
            visible_deltas(&filter.normalize(delta("a", "1", "x", "Aci"))),
            "A"
        );
        assert_eq!(
            visible_deltas(&filter.normalize(delta("a", "1", "y", "B"))),
            "B"
        );
        assert_eq!(
            visible_deltas(&filter.normalize(delta("a", "2", "x", "C"))),
            "C"
        );
        assert_eq!(
            visible_deltas(&filter.normalize(delta("b", "1", "x", "D"))),
            "D"
        );
        assert_eq!(
            visible_deltas(&filter.normalize(delta("a", "1", "x", "teturn0E"))),
            "E"
        );
    }

    #[test]
    fn normalizes_canonical_snapshots_and_history_without_changing_other_fields() {
        let raw = json!({
            "type": "agentMessage", "id": "item", "text": "answerciteturn0",
            "phase": "final_answer", "memoryCitation": null,
        });
        let mut clean = raw.clone();
        clean["text"] = "answer".into();
        let untouched = json!({"type": "userMessage", "id": "user", "content": [{
            "type": "text", "text": "citeturn0", "text_elements": [],
        }]});
        for method in ["item/started", "item/completed"] {
            let input = json!({"method": method, "params": {
                "threadId": "thread", "turnId": "turn", "item": raw,
            }});
            let mut expected = input.clone();
            expected["params"]["item"] = clean.clone();
            assert_eq!(
                CodexCitationFilter::default().normalize(input),
                vec![expected]
            );
        }
        let turn = json!({"id": "turn", "items": [raw, untouched], "status": "completed"});
        let mut clean_turn = turn.clone();
        clean_turn["items"][0] = clean;
        for method in ["turn/started", "turn/completed"] {
            let input = json!({"method": method, "params": {"threadId": "thread", "turn": turn}});
            let mut expected = input.clone();
            expected["params"]["turn"] = clean_turn.clone();
            assert_eq!(
                CodexCitationFilter::default().normalize(input),
                vec![expected]
            );
        }
        for (input, expected) in [
            (
                json!({"id": 1, "result": {"turn": turn}}),
                json!({"id": 1, "result": {"turn": clean_turn}}),
            ),
            (
                json!({"id": 2, "result": {"thread": {"turns": [turn]}}}),
                json!({"id": 2, "result": {"thread": {"turns": [clean_turn]}}}),
            ),
            (
                json!({"method": "thread/started", "params": {"thread": {"turns": [turn]}}}),
                json!({"method": "thread/started", "params": {"thread": {"turns": [clean_turn]}}}),
            ),
        ] {
            assert_eq!(
                CodexCitationFilter::default().normalize(input),
                vec![expected]
            );
        }
    }

    #[test]
    fn completion_uses_canonical_text_and_flushes_non_citation_prefixes() {
        let mut filter = CodexCitationFilter::default();
        filter.normalize(delta("thread", "turn", "item", "stale "));
        let input = json!({"method": "item/completed", "params": {
            "threadId": "thread", "turnId": "turn", "item": {
                "type": "agentMessage", "id": "item", "text": "canonicalciteturn0",
            },
        }});
        let mut expected = input.clone();
        expected["params"]["item"]["text"] = "canonical".into();
        assert_eq!(
            filter.normalize(input),
            vec![delta("thread", "turn", "item", ""), expected]
        );
        assert!(filter.parsers.is_empty());
    }

    #[test]
    fn terminal_events_discard_unfinished_citations_and_reset_only_the_finished_turn() {
        for status in ["completed", "interrupted", "failed"] {
            let mut filter = CodexCitationFilter::default();
            assert_eq!(
                visible_deltas(&filter.normalize(delta(
                    "thread",
                    "turn",
                    "item",
                    "answerciteturn0"
                ))),
                "answer"
            );
            filter.normalize(delta("thread", "other", "item", "ci"));
            let terminal = json!({"method": "turn/completed", "params": {
                "threadId": "thread", "turn": {"id": "turn", "items": [], "status": status},
            }});
            assert_eq!(filter.normalize(terminal.clone()), vec![terminal]);
            assert_eq!(filter.parsers.len(), 1);
            assert_eq!(
                visible_deltas(&filter.normalize(delta("thread", "turn", "item", "next"))),
                "next"
            );
            assert_eq!(
                visible_deltas(&filter.normalize(delta(
                    "thread",
                    "other",
                    "item",
                    "teturn0rest"
                ))),
                "rest"
            );
        }
    }

    #[test]
    fn end_of_stream_preserves_partial_literals_and_hides_open_citations() {
        let mut filter = CodexCitationFilter::default();
        filter.normalize(delta("thread", "turn", "a", "answerciteturn0"));
        filter.normalize(delta("thread", "turn", "b", "literal ci"));
        let mut output = Vec::new();
        filter.finish(&mut output).unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value, delta("thread", "turn", "b", "ci"));
        assert!(filter.parsers.is_empty());
    }

    #[test]
    fn leaves_tool_output_reasoning_and_unknown_events_untouched() {
        for input in [
            json!({"method": "item/commandExecution/outputDelta", "params": {"delta": "citeturn0"}}),
            json!({"method": "item/reasoning/textDelta", "params": {"delta": "citeturn0"}}),
            json!({"method": "item/completed", "params": {"item": {"type": "commandExecution", "text": "citeturn0"}}}),
            json!({"method": "unknown", "params": {"text": "citeturn0"}}),
        ] {
            assert_eq!(
                CodexCitationFilter::default().normalize(input.clone()),
                vec![input]
            );
        }
    }
}
