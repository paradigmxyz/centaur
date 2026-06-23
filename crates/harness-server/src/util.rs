use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use codex_app_server_protocol::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{HarnessServerError, Result};

pub(crate) fn command_from_override(env_key: &str) -> Option<ProcessCommand> {
    let raw = env::var(env_key).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let mut command = ProcessCommand::new("sh");
    command.arg("-lc").arg(raw);
    Some(command)
}

pub(crate) fn user_input_to_anthropic_content(input: &[UserInput]) -> Vec<Value> {
    input
        .iter()
        .map(|item| match item {
            UserInput::Text { text, .. } => json!({"type": "text", "text": text}),
            UserInput::Image { url, .. } => json!({
                "type": "text",
                "text": format!("[image: {url}]"),
            }),
            UserInput::LocalImage { path, .. } => json!({
                "type": "text",
                "text": format!("[local image: {}]", path.display()),
            }),
            UserInput::Skill { name, path } => json!({
                "type": "text",
                "text": format!("[skill: {name} at {}]", path.display()),
            }),
            UserInput::Mention { name, path } => json!({
                "type": "text",
                "text": format!("[mention: {name} at {path}]"),
            }),
        })
        .collect()
}

pub(crate) fn write_value<W: Write>(stdout: &mut W, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn absolute_path(path: PathBuf) -> Result<AbsolutePathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    };
    AbsolutePathBuf::from_absolute_path(&path)
        .map_err(|_| HarnessServerError::PathMustBeAbsolute { path })
}

pub(crate) fn default_codex_home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".codex")
}

pub(crate) fn suffix_delta(previous: &str, current: &str) -> String {
    current
        .strip_prefix(previous)
        .unwrap_or(current)
        .to_string()
}

pub(crate) fn stable_id(raw: &str, prefix: &str) -> String {
    let clean = raw.trim();
    if clean.is_empty() {
        format!("{prefix}-{}", Uuid::new_v4().simple())
    } else {
        clean.replace(['/', ' ', ':'], "_")
    }
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
