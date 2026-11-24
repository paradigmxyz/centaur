//! The input half of the blocks protocol that api-rs writes to a harness.

use serde_json::Value;

#[derive(Debug, PartialEq)]
pub enum Input {
    User(UserTurn),
    Interrupt { thread_key: Option<String> },
    Ignored { kind: String },
}

#[derive(Debug, PartialEq)]
pub struct UserTurn {
    pub thread_key: Option<String>,
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub text: String,
    pub steer: bool,
}

pub fn parse(line: &str) -> Result<Input, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| format!("input is not JSON: {error}"))?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or("input has no type")?;
    let thread_key = string(&value, "thread_key");
    match kind {
        "user" => Ok(Input::User(UserTurn {
            request_id: string(&value, "client_user_message_id").or_else(|| {
                value
                    .pointer("/trace_metadata/execution_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
            model: string(&value, "model"),
            reasoning: string(&value, "reasoning"),
            text: text(&value),
            steer: value
                .pointer("/trace_metadata/action")
                .and_then(Value::as_str)
                == Some("steer_active_execution"),
            thread_key,
        })),
        "interrupt" => Ok(Input::Interrupt { thread_key }),
        other => Ok(Input::Ignored {
            kind: other.to_owned(),
        }),
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn text(value: &Value) -> String {
    let parts = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut text = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                    text.push(chunk.to_owned());
                }
            }
            Some(other) => text.push(format!("[{other} input omitted: not supported yet]")),
            None => {}
        }
    }
    let text = text.join("\n");
    if text.trim().is_empty() {
        "continue".to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_user_line() {
        let line = r#"{"type":"user","thread_key":"slack:T1:C1:1.2","client_user_message_id":"m1","model":"luna","reasoning":"high","message":{"role":"user","content":[{"type":"text","text":"hi"},{"type":"text","text":"there"}]},"trace_metadata":{"execution_id":"e1"}}"#;
        assert_eq!(
            parse(line).unwrap(),
            Input::User(UserTurn {
                thread_key: Some("slack:T1:C1:1.2".into()),
                request_id: Some("m1".into()),
                model: Some("luna".into()),
                reasoning: Some("high".into()),
                text: "hi\nthere".into(),
                steer: false,
            })
        );
    }

    #[test]
    fn falls_back_to_the_execution_id_and_marks_steers() {
        let line = r#"{"type":"user","thread_key":"t","message":{"content":[]},"trace_metadata":{"execution_id":"e1","action":"steer_active_execution"}}"#;
        let Input::User(turn) = parse(line).unwrap() else {
            panic!("not a user turn");
        };
        assert_eq!(turn.request_id.as_deref(), Some("e1"));
        assert!(turn.steer);
        assert_eq!(turn.text, "continue");
    }

    #[test]
    fn notes_unsupported_parts() {
        let line = r#"{"type":"user","message":{"content":[{"type":"text","text":"look"},{"type":"image","url":"https://x"}]}}"#;
        let Input::User(turn) = parse(line).unwrap() else {
            panic!("not a user turn");
        };
        assert_eq!(turn.text, "look\n[image input omitted: not supported yet]");
    }

    #[test]
    fn parses_interrupts_and_ignores_other_kinds() {
        assert_eq!(
            parse(r#"{"type":"interrupt","thread_key":"t"}"#).unwrap(),
            Input::Interrupt {
                thread_key: Some("t".into())
            }
        );
        assert_eq!(
            parse(r#"{"type":"attachment.chunk"}"#).unwrap(),
            Input::Ignored {
                kind: "attachment.chunk".into()
            }
        );
        assert!(parse("nope").is_err());
        assert!(parse("{}").is_err());
    }
}
