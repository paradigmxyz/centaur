use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use codex_app_server_protocol::UserInput;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::server::{BlocksCommand, BlocksState, parse_blocks_line_with_state, write_blocks_error};
use crate::util::{user_input_to_anthropic_content, write_value};
use crate::{AppServerRuntime, HarnessServerError, Result};

/// Runs Flue as a Centaur harness.
///
/// Centaur already owns durable sessions and workflow orchestration. This
/// adapter therefore treats Flue as a finite per-turn executor: every Centaur
/// user turn becomes one `flue run` invocation, and the JSON result is projected
/// back onto the existing Centaur app-server event stream.
#[derive(Debug, Default)]
pub struct FlueHarnessServer;

impl AppServerRuntime for FlueHarnessServer {
    fn run_stdio(&self) -> Result<()> {
        run_flue_blocks_server()
    }
}

pub fn run_flue_blocks_server() -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut blocks_state = BlocksState::default();
    let thread_id = env::var("CENTAUR_THREAD_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("flue-{}", Uuid::new_v4().simple()));

    write_value(
        &mut stdout,
        &json!({"type": "thread.started", "thread_id": thread_id}),
    )?;

    for raw in stdin.lock().lines() {
        let line = raw?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match parse_blocks_line_with_state(trimmed, &mut blocks_state) {
            Ok(BlocksCommand::User {
                input,
                client_user_message_id: _,
                model: _,
                provider: _,
                reasoning: _,
                trace_context: _,
            }) => {
                let turn_id = format!("turn-{}", Uuid::new_v4().simple());
                match run_flue_turn(&input, &thread_id, &turn_id) {
                    Ok(result_text) => {
                        write_flue_success(&mut stdout, &thread_id, &turn_id, result_text)?
                    }
                    Err(error) => {
                        eprintln!("Flue turn failed: {error:#}");
                        write_blocks_error(&mut stdout, &thread_id, &turn_id, error.to_string())?;
                    }
                }
            }
            Ok(BlocksCommand::Interrupt) => {
                eprintln!(
                    "Flue blocks interrupt ignored: flue run is per-turn and non-interactive"
                );
            }
            Ok(BlocksCommand::AttachmentChunk) => {}
            Err(error) => {
                eprintln!("invalid Flue blocks input: {error:#}");
                write_blocks_error(&mut stdout, &thread_id, "input", error.to_string())?;
            }
        }
    }

    Ok(())
}

fn run_flue_turn(input: &[UserInput], thread_id: &str, turn_id: &str) -> Result<String> {
    let workflow = env::var("CENTAUR_FLUE_WORKFLOW")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "agent-turn".to_string());
    let target = env::var("CENTAUR_FLUE_TARGET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "node".to_string());
    let input_json = flue_input_json(input, thread_id, turn_id)?;
    let project_dir = env::var_os("CENTAUR_FLUE_PROJECT_DIR").map(PathBuf::from);

    let mut command = ProcessCommand::new(flue_bin());
    command.args([
        "run",
        &workflow,
        "--target",
        &target,
        "--input",
        &input_json,
    ]);
    if let Some(project_dir) = project_dir {
        command.current_dir(project_dir);
    }

    let output = command
        .output()
        .map_err(|source| HarnessServerError::SpawnHarness {
            cwd: env::current_dir().unwrap_or_else(|_| "/".into()),
            source,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = [stderr, stdout]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(HarnessServerError::InvalidBlocksInput {
            message: if detail.is_empty() {
                format!("flue run exited with status {}", output.status)
            } else {
                format!("flue run exited with status {}: {detail}", output.status)
            },
        });
    }

    Ok(extract_flue_result_text(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn flue_input_json(input: &[UserInput], thread_id: &str, turn_id: &str) -> Result<String> {
    let text = input
        .iter()
        .map(user_input_text)
        .collect::<Vec<_>>()
        .join("\n");
    let value = json!({
        "text": text,
        "thread_key": thread_id,
        "turn_id": turn_id,
        "content": user_input_to_anthropic_content(input),
    });
    Ok(serde_json::to_string(&value)?)
}

/// Preserve Centaur's canonical input surface while giving Flue a simple text
/// field for ordinary workflows and the original Anthropic-style content blocks
/// for richer workflows that want attachment/image metadata.
fn user_input_text(input: &UserInput) -> String {
    match input {
        UserInput::Text { text, .. } => text.clone(),
        UserInput::Image { url, .. } => format!("[image: {url}]"),
        UserInput::LocalImage { path, .. } => format!("[local image: {}]", path.display()),
        UserInput::Skill { name, path } => format!("[skill: {name} at {}]", path.display()),
        UserInput::Mention { name, path } => format!("[mention: {name} at {path}]"),
    }
}

fn extract_flue_result_text(stdout: &str) -> String {
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(text) = value.pointer("/result/text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(text) = value.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            if let Some(result) = value.get("result") {
                return result
                    .as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| result.to_string());
            }
            return value.to_string();
        }
    }
    stdout.trim().to_string()
}

fn write_flue_success<W: Write>(
    stdout: &mut W,
    thread_id: &str,
    turn_id: &str,
    result_text: String,
) -> Result<()> {
    let item_id = format!("msg-{}", Uuid::new_v4().simple());
    // Session runtime already understands these event shapes from the other
    // harnesses. Emitting them here avoids adding a Flue-specific event parser
    // in Centaur's durable execution layer.
    write_value(
        stdout,
        &json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": result_text}]
            },
            "thread_id": thread_id,
            "turn_id": turn_id
        }),
    )?;
    write_value(
        stdout,
        &json!({
            "type": "item.completed",
            "item": {
                "id": item_id,
                "type": "agentMessage",
                "phase": "final_answer",
                "text": result_text
            },
            "thread_id": thread_id,
            "turn_id": turn_id
        }),
    )?;
    write_value(
        stdout,
        &json!({"type": "turn.done", "thread_id": thread_id, "turn_id": turn_id, "result": result_text}),
    )
}

fn flue_bin() -> String {
    env::var("FLUE_BIN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "flue".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flue_input_includes_text_and_anthropic_content() {
        let input = vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }];
        let value: Value =
            serde_json::from_str(&flue_input_json(&input, "thread:1", "turn-1").unwrap()).unwrap();

        assert_eq!(value["text"], "hello");
        assert_eq!(value["thread_key"], "thread:1");
        assert_eq!(value["turn_id"], "turn-1");
        assert_eq!(value["content"][0]["text"], "hello");
    }

    #[test]
    fn extracts_last_json_result_text() {
        let stdout = "event line\n{\"result\":{\"text\":\"done\"}}\n";
        assert_eq!(extract_flue_result_text(stdout), "done");
    }
}
