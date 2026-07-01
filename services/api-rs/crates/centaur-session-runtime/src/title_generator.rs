use std::{env, sync::Arc, time::Duration};

use serde_json::{Value, json};
use thiserror::Error;

const SESSION_TITLE_MODEL: &str = "gpt-5.4-nano";
const SESSION_TITLE_MAX_SOURCE_CHARS: usize = 4_000;
const SESSION_TITLE_MAX_CHARS: usize = 80;
const SESSION_TITLE_REQUEST_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub(crate) struct OpenAiSessionTitleGenerator {
    api_key: Arc<str>,
    client: reqwest::Client,
}

impl OpenAiSessionTitleGenerator {
    pub(crate) fn from_env() -> Option<Self> {
        let api_key = env::var("OPENAI_API_KEY").ok()?;
        let api_key = api_key.trim();
        if api_key.is_empty() || api_key == "OPENAI_API_KEY" {
            return None;
        }
        let client = reqwest::Client::builder()
            .timeout(SESSION_TITLE_REQUEST_TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            api_key: Arc::from(api_key.to_owned()),
            client,
        })
    }

    pub(crate) async fn generate(
        &self,
        source: String,
    ) -> Result<String, SessionTitleGenerationError> {
        let body = json!({
            "model": SESSION_TITLE_MODEL,
            "instructions": "Generate a short session title for the user's request. Return only the title. Use commit-message style with an imperative verb first, such as Fix, Investigate, Add, Update, Debug, Review, Explain, or Analyze. Keep it to 5 words max; 6-7 words are okay only when needed for a product name. Do not include punctuation, quotes, emoji, markdown, or a trailing period.",
            "input": format!("User request:\n{}", source),
            "max_output_tokens": 24,
        });
        let response = self
            .client
            .post("https://api.openai.com/v1/responses")
            .bearer_auth(self.api_key.as_ref())
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(SessionTitleGenerationError::HttpStatus { status, body: text });
        }
        openai_response_output_text(&text).ok_or(SessionTitleGenerationError::MissingOutput)
    }
}

pub(crate) fn session_title_source_from_parts(parts: &[Value]) -> Option<String> {
    let mut text_blocks = Vec::new();
    let mut slack_thread_source = None;
    let mut attachment_names = Vec::new();
    for part in parts {
        match part {
            Value::String(text) => {
                collect_title_source_text(text, &mut text_blocks, &mut slack_thread_source);
            }
            Value::Object(object) => {
                if let Some(text) = object.get("text").and_then(Value::as_str) {
                    collect_title_source_text(text, &mut text_blocks, &mut slack_thread_source);
                }
                for key in ["name", "title", "filename"] {
                    if let Some(name) = object.get(key).and_then(Value::as_str)
                        && let Some(name) = clean_nonempty(name)
                    {
                        attachment_names.push(name.to_owned());
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let source = slack_thread_source
        .or_else(|| text_blocks.first().cloned())
        .or_else(|| {
            attachment_names
                .first()
                .map(|name| format!("Analyze attachment {name}"))
        })?;
    Some(truncate_chars(&source, SESSION_TITLE_MAX_SOURCE_CHARS))
}

fn collect_title_source_text(
    raw_text: &str,
    text_blocks: &mut Vec<String>,
    slack_thread_source: &mut Option<String>,
) {
    if slack_thread_source.is_none()
        && let Some(text) = slack_thread_context_title_source(raw_text)
        && title_source_has_signal(&text)
    {
        *slack_thread_source = Some(text);
    }
    if is_session_context_text(raw_text) {
        return;
    }
    if let Some(text) = clean_title_source_text(raw_text)
        && title_source_has_signal(&text)
    {
        text_blocks.push(text);
    }
}

fn slack_thread_context_title_source(text: &str) -> Option<String> {
    if !text.trim_start().starts_with("# Slack Thread Context") {
        return None;
    }

    let mut in_first_message = false;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "# Current Request" {
            break;
        }
        if !in_first_message {
            if trimmed.starts_with("1. ") && trimmed.ends_with(':') {
                in_first_message = true;
            }
            continue;
        }
        if trimmed.starts_with("2. ") && trimmed.ends_with(':') {
            break;
        }
        if trimmed.is_empty() {
            if lines.is_empty() {
                continue;
            }
            break;
        }
        lines.push(trimmed);
    }

    let text = lines.join(" ");
    clean_title_source_text(&text)
}

fn title_source_has_signal(text: &str) -> bool {
    let normalized = normalize_low_signal_text(text);
    if normalized.is_empty() {
        return false;
    }
    if is_low_signal_phrase(&normalized) {
        return false;
    }

    let words = normalized.split_whitespace().collect::<Vec<_>>();
    if words.iter().all(|word| is_low_signal_word(word)) {
        return false;
    }

    true
}

fn normalize_low_signal_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let lowercase = text.to_lowercase();
    let mut chars = lowercase.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == ':' {
            let mut emoji_name = String::new();
            while let Some(next) = chars.peek().copied() {
                chars.next();
                if next == ':' {
                    break;
                }
                if next.is_ascii_alphanumeric() || matches!(next, '_' | '-' | '+') {
                    emoji_name.push(next);
                    continue;
                }
                output.push(' ');
                output.push_str(&emoji_name);
                output.push(next);
                emoji_name.clear();
                break;
            }
            continue;
        }
        if ch.is_alphanumeric() {
            output.push(ch);
        } else {
            output.push(' ');
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_low_signal_phrase(text: &str) -> bool {
    matches!(
        text,
        "hey"
            | "hi"
            | "hello"
            | "yo"
            | "hey bot"
            | "hi bot"
            | "hello bot"
            | "hey ai"
            | "hi ai"
            | "hello ai"
            | "thread"
            | "help"
            | "can you help"
            | "can you help me"
            | "please help"
            | "pls help"
    )
}

fn is_low_signal_word(word: &str) -> bool {
    matches!(
        word,
        "hey" | "hi" | "hello" | "yo" | "bot" | "ai" | "thread" | "please" | "pls"
    )
}

fn clean_title_source_text(text: &str) -> Option<String> {
    let text = strip_slack_user_mentions(text)
        .replace('\r', "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = clean_nonempty(&text)?.to_owned();
    if text.starts_with('@') {
        text = text
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(index, _)| text[index..].trim_start().to_owned())
            .unwrap_or_default();
    }
    clean_nonempty(&text).map(str::to_owned)
}

fn strip_slack_user_mentions(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<@") {
        output.push_str(&rest[..start]);
        let mention = &rest[start + 2..];
        let Some(end) = mention.find('>') else {
            output.push_str(&rest[start..]);
            return output;
        };
        rest = &mention[end + 1..];
    }
    output.push_str(rest);
    output
}

fn is_session_context_text(text: &str) -> bool {
    let text = text.trim_start();
    [
        "# Requester Context",
        "# Slack Session Context",
        "# Slack Thread Context",
        "Earlier Slack thread attachment",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

pub(crate) fn sanitize_session_title(title: &str) -> Option<String> {
    let title = title
        .trim()
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | '`' | '*' | '_' | '-' | ':' | ';' | ',' | '.'
            )
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title = clean_nonempty(&title)?;
    let words = title
        .split_whitespace()
        .take(7)
        .map(|word| {
            word.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | ',' | '.' | ':' | ';'))
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    Some(truncate_chars(&words.join(" "), SESSION_TITLE_MAX_CHARS))
}

fn openai_response_output_text(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    if let Some(text) = value.get("output_text").and_then(Value::as_str)
        && clean_nonempty(text).is_some()
    {
        return Some(text.to_owned());
    }
    for output in value.get("output").and_then(Value::as_array)? {
        let Some(content) = output.get("content").and_then(Value::as_array) else {
            continue;
        };
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && clean_nonempty(text).is_some()
            {
                return Some(text.to_owned());
            }
        }
    }
    None
}

fn clean_nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    if truncated.ends_with(char::is_whitespace) {
        truncated = truncated.trim_end().to_owned();
    }
    truncated
}

#[derive(Debug, Error)]
pub enum SessionTitleGenerationError {
    #[error("OpenAI title response did not include output text")]
    MissingOutput,
    #[error("OpenAI title request failed with status {status}: {body}")]
    HttpStatus {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_title_source_prefers_user_ask_over_slack_context() {
        let parts = vec![
            json!({
                "type": "text",
                "text": "# Requester Context\n\nThe Slack user who prompted this turn is Alice."
            }),
            json!({
                "type": "text",
                "text": "<@U123> please fix the memory leak in the worker"
            }),
        ];

        assert_eq!(
            session_title_source_from_parts(&parts),
            Some("please fix the memory leak in the worker".to_owned())
        );
    }

    #[test]
    fn session_title_source_uses_first_slack_thread_message() {
        let parts = vec![
            json!({
                "type": "text",
                "text": "# Slack Thread Context\n\nEarlier messages in this Slack thread, in chronological order:\n\n1. Alice:\n   Planning to replace the billing export job with a streaming worker because the nightly batch keeps timing out\n\n# Current Request\n\nThe user message follows in the next content block.\n---"
            }),
            json!({
                "type": "text",
                "text": "<@U123> investigate this"
            }),
        ];

        assert_eq!(
            session_title_source_from_parts(&parts),
            Some(
                "Planning to replace the billing export job with a streaming worker because the nightly batch keeps timing out"
                    .to_owned()
            )
        );
    }

    #[test]
    fn session_title_source_skips_low_signal_wakeups() {
        assert_eq!(
            session_title_source_from_parts(&[
                json!({"type": "text", "text": "<@U123> Hey"}),
                json!({"type": "text", "text": ":thread:"}),
            ]),
            None
        );

        assert_eq!(
            session_title_source_from_parts(&[
                json!({"type": "text", "text": "<@U123> Hey"}),
                json!({"type": "text", "text": "Can you investigate queue stalls?"}),
            ]),
            Some("Can you investigate queue stalls?".to_owned())
        );
    }

    #[test]
    fn sanitize_session_title_keeps_model_wording() {
        assert_eq!(
            sanitize_session_title("Memory leak in worker queue needs investigation immediately"),
            Some("Memory leak in worker queue needs investigation".to_owned())
        );
        assert_eq!(
            sanitize_session_title("\"Fix worker memory leak.\""),
            Some("Fix worker memory leak".to_owned())
        );
    }

    #[test]
    fn openai_response_output_text_reads_responses_api_shapes() {
        assert_eq!(
            openai_response_output_text(r#"{"output_text":"Fix worker memory leak"}"#),
            Some("Fix worker memory leak".to_owned())
        );
        assert_eq!(
            openai_response_output_text(
                r#"{"output":[{"content":[{"type":"output_text","text":"Add Tempo Explorer filter"}]}]}"#
            ),
            Some("Add Tempo Explorer filter".to_owned())
        );
    }
}
