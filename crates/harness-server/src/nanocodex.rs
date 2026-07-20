use std::collections::HashMap;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use nanocodex::{AgentEvent, AgentEvents, Nanocodex, Prompt, Tools, Turn, UserInput};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::nanocodex_subagents::{ChildAgents, with_subagents};
use crate::{HarnessServerError, Result};

/// Runs the Centaur blocks adapter while preserving Nanocodex's native event
/// protocol on stdout. This path intentionally does not import or construct a
/// Codex App Server protocol value.
pub fn run_nanocodex_blocks_server() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

async fn run() -> Result<()> {
    let api_key =
        env::var("OPENAI_API_KEY").map_err(|_| HarnessServerError::MissingEnvironment {
            name: "OPENAI_API_KEY",
        })?;
    let cwd = env::current_dir()?;
    let session_id = format!("nanocodex-{}", Uuid::new_v4().simple());
    let child_agents = Arc::new(ChildAgents::default());
    let tools_agents = Arc::downgrade(&child_agents);
    let tools = Tools::default();
    let (agent, mut events) = Nanocodex::builder(api_key)
        .workspace(&cwd)
        .session_id(session_id)
        .tools_factory(move |agent| with_subagents(tools.clone(), agent, tools_agents.clone()))
        .build()
        .map_err(nanocodex_error)?;

    let (sender, mut receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let mut stdout = io::stdout().lock();
    let mut staged = HashMap::new();
    while let Some(line) = receiver.recv().await {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match parse_blocks_line(&line, &mut staged)? {
            BlocksCommand::User(prompt) => {
                let turn = agent.prompt(prompt).await.map_err(nanocodex_error)?;
                run_turn(&mut events, turn, &mut receiver, &mut staged, &mut stdout).await?;
            }
            BlocksCommand::AttachmentChunk => {}
            BlocksCommand::Interrupt => {
                eprintln!("nanocodex interrupt ignored: no cancellation API is exposed");
            }
        }
    }
    child_agents.shutdown().await;
    Ok(())
}

async fn run_turn(
    events: &mut AgentEvents,
    turn: Turn,
    receiver: &mut mpsc::UnboundedReceiver<io::Result<String>>,
    staged: &mut HashMap<String, PathBuf>,
    stdout: &mut impl Write,
) -> Result<()> {
    let mut input_open = true;
    loop {
        tokio::select! {
            event = events.recv() => {
                let event = event.ok_or_else(|| HarnessServerError::Nanocodex(
                    "event stream closed before the turn completed".to_owned()
                ))?;
                let terminal = event.kind.is_terminal();
                write_event(stdout, &event)?;
                if terminal {
                    break;
                }
            }
            line = receiver.recv(), if input_open => {
                let Some(line) = line else {
                    input_open = false;
                    continue;
                };
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                match parse_blocks_line(&line, staged)? {
                    BlocksCommand::User(prompt) => {
                        turn.steer(prompt).await.map_err(nanocodex_error)?;
                    }
                    BlocksCommand::AttachmentChunk => {}
                    BlocksCommand::Interrupt => {
                        turn.cancel().await.map_err(nanocodex_error)?;
                        input_open = false;
                    }
                }
            }
        }
    }

    if let Err(error) = turn.result().await {
        eprintln!("nanocodex turn failed: {error:#}");
    }
    Ok(())
}

fn write_event(output: &mut impl Write, event: &AgentEvent) -> Result<()> {
    serde_json::to_writer(&mut *output, event)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn nanocodex_error(error: nanocodex::NanocodexError) -> HarnessServerError {
    HarnessServerError::Nanocodex(error.to_string())
}

enum BlocksCommand {
    User(Prompt),
    AttachmentChunk,
    Interrupt,
}

#[derive(Deserialize)]
struct BlocksLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    message: Option<BlocksMessage>,
    #[serde(rename = "attachmentId", default)]
    attachment_id: Option<String>,
    #[serde(rename = "localPath", alias = "path", default)]
    local_path: Option<PathBuf>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "mimeType", default)]
    mime_type: Option<String>,
    #[serde(rename = "dataBase64", default)]
    data_base64: Option<String>,
}

#[derive(Deserialize)]
struct BlocksMessage {
    #[serde(default)]
    content: Option<Value>,
}

fn parse_blocks_line(line: &str, staged: &mut HashMap<String, PathBuf>) -> Result<BlocksCommand> {
    let parsed: BlocksLine =
        serde_json::from_str(line).map_err(|source| HarnessServerError::InvalidBlocksInput {
            message: source.to_string(),
        })?;
    match parsed.kind.as_str() {
        "user" => {
            let content = parsed
                .message
                .as_ref()
                .and_then(|message| message.content.as_ref())
                .or(parsed.content.as_ref());
            let mut inputs = content
                .map(|content| parse_content(content, staged))
                .transpose()?
                .unwrap_or_default();
            if inputs.is_empty()
                && let Some(text) = parsed.text
            {
                inputs.push(UserInput::Text { text });
            }
            if inputs.is_empty() {
                inputs.push(UserInput::Text {
                    text: "continue".to_owned(),
                });
            }
            Ok(BlocksCommand::User(Prompt::content(inputs)))
        }
        "attachment.chunk" => {
            let id = required_string(parsed.attachment_id, "attachmentId")?;
            if let Some(path) = parsed.local_path {
                staged.insert(id, path);
                return Ok(BlocksCommand::AttachmentChunk);
            }
            let path = if let Some(path) = staged.get(&id) {
                path.clone()
            } else {
                let path = temporary_attachment_path(parsed.name.as_deref());
                staged.insert(id.clone(), path.clone());
                path
            };
            if let Some(data) = parsed.data_base64.filter(|data| !data.is_empty()) {
                let bytes = BASE64_STANDARD.decode(data).map_err(|source| {
                    HarnessServerError::InvalidBlocksInput {
                        message: format!("invalid attachment chunk for {id}: {source}"),
                    }
                })?;
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?
                    .write_all(&bytes)?;
            }
            let _mime_type = parsed.mime_type;
            Ok(BlocksCommand::AttachmentChunk)
        }
        "interrupt" => Ok(BlocksCommand::Interrupt),
        kind => Err(HarnessServerError::InvalidBlocksInput {
            message: format!("unsupported blocks input type `{kind}`"),
        }),
    }
}

fn parse_content(value: &Value, staged: &HashMap<String, PathBuf>) -> Result<Vec<UserInput>> {
    if let Some(text) = value.as_str() {
        return Ok(vec![UserInput::Text {
            text: text.to_owned(),
        }]);
    }
    let items = value
        .as_array()
        .ok_or_else(|| HarnessServerError::InvalidBlocksInput {
            message: "user content must be a string or array".to_owned(),
        })?;
    items.iter().map(|item| parse_input(item, staged)).collect()
}

fn parse_input(value: &Value, staged: &HashMap<String, PathBuf>) -> Result<UserInput> {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("text");
    match kind {
        "text" | "input_text" => Ok(UserInput::Text {
            text: required_value_string(value, "text")?,
        }),
        "image" | "input_image" => Ok(UserInput::Image {
            image_url: required_value_string_alias(value, "image_url", "url")?,
            detail: None,
        }),
        "local_image" | "localImage" => Ok(UserInput::LocalImage {
            path: PathBuf::from(required_value_string_alias(value, "path", "localPath")?),
            detail: None,
        }),
        "audio" | "input_audio" => Ok(UserInput::Audio {
            audio_url: required_value_string_alias(value, "audio_url", "url")?,
        }),
        "local_audio" | "localAudio" => Ok(UserInput::LocalAudio {
            path: PathBuf::from(required_value_string_alias(value, "path", "localPath")?),
        }),
        "attachment" => attachment_input(value, staged),
        "attachment_ref" => Ok(UserInput::Text {
            text: "[Attachment reference was not provided to this sandbox]".to_owned(),
        }),
        other => Err(HarnessServerError::InvalidBlocksInput {
            message: format!("unsupported Nanocodex input type `{other}`"),
        }),
    }
}

fn attachment_input(value: &Value, staged: &HashMap<String, PathBuf>) -> Result<UserInput> {
    let path = value
        .get("localPath")
        .or_else(|| value.get("path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| {
            value
                .get("stagedAttachmentId")
                .and_then(Value::as_str)
                .and_then(|id| staged.get(id).cloned())
        });
    let path = match path {
        Some(path) => path,
        None => inline_attachment_path(value)?.ok_or_else(|| {
            HarnessServerError::InvalidBlocksInput {
                message:
                    "Nanocodex attachment requires localPath, stagedAttachmentId, or dataBase64"
                        .to_owned(),
            }
        })?,
    };
    let mime = value
        .get("mimeType")
        .or_else(|| value.get("mime_type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if mime.starts_with("image/") {
        Ok(UserInput::LocalImage { path, detail: None })
    } else if mime.starts_with("audio/") {
        Ok(UserInput::LocalAudio { path })
    } else {
        Ok(UserInput::Text {
            text: format!("[Attached file saved to {}]", path.display()),
        })
    }
}

fn inline_attachment_path(value: &Value) -> Result<Option<PathBuf>> {
    let Some(data) = value
        .get("dataBase64")
        .and_then(Value::as_str)
        .filter(|data| !data.is_empty())
    else {
        return Ok(None);
    };
    let bytes =
        BASE64_STANDARD
            .decode(data)
            .map_err(|source| HarnessServerError::InvalidBlocksInput {
                message: format!("invalid attachment dataBase64: {source}"),
            })?;
    let path = temporary_attachment_path(value.get("name").and_then(Value::as_str));
    std::fs::write(&path, bytes)?;
    Ok(Some(path))
}

fn temporary_attachment_path(name: Option<&str>) -> PathBuf {
    let suffix = PathBuf::from(name.unwrap_or("attachment"))
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    env::temp_dir().join(format!(
        "centaur-nanocodex-{}{}",
        Uuid::new_v4().simple(),
        suffix
    ))
}

fn required_string(value: Option<String>, name: &str) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| HarnessServerError::InvalidBlocksInput {
            message: format!("missing {name}"),
        })
}

fn required_value_string(value: &Value, name: &str) -> Result<String> {
    required_value_string_alias(value, name, name)
}

fn required_value_string_alias(value: &Value, name: &str, alias: &str) -> Result<String> {
    required_string(
        value
            .get(name)
            .or_else(|| value.get(alias))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_without_codex_protocol_types() {
        let command = parse_blocks_line(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hello"}]}}"#,
            &mut HashMap::new(),
        )
        .unwrap();
        let BlocksCommand::User(prompt) = command else {
            panic!("expected user prompt");
        };
        assert_eq!(
            serde_json::to_value(prompt).unwrap()["instruction"][0]["text"],
            "hello"
        );
    }

    #[test]
    fn materializes_inline_attachment_without_codex_protocol_types() {
        let command = parse_blocks_line(
            r#"{"type":"user","message":{"content":[{"type":"attachment","attachment_type":"document","dataBase64":"aGVsbG8=","name":"notes.txt","mimeType":"text/plain"}]}}"#,
            &mut HashMap::new(),
        )
        .unwrap();
        let BlocksCommand::User(prompt) = command else {
            panic!("expected user prompt");
        };
        let text = serde_json::to_value(prompt).unwrap()["instruction"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let path = text
            .strip_prefix("[Attached file saved to ")
            .and_then(|text| text.strip_suffix(']'))
            .map(PathBuf::from)
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        std::fs::remove_file(path).unwrap();
    }
}
