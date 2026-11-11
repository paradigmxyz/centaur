//! Overlay `first-turn-prompt.sh` hooks.
//!
//! Each blocks server starts its harness lazily on the first user message. By
//! then the sandbox claim has bound the session's proxy credentials, so overlay
//! hooks run here rather than at boot (when a warm sandbox belongs to no
//! session). `compose-system-prompt` reruns them and rewrites the workspace
//! `AGENTS.md`, which the harness reads as it starts.

use std::env;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Once;
use std::time::Instant;

/// Overlay repo roots that may run sandbox hooks, set by api-rs from the
/// configured overlay sources and scoped to the session's repo-cache access.
const OVERLAY_REPO_DIRS_ENV: &str = "CENTAUR_OVERLAY_REPO_DIRS";
const COMPOSE_BIN_ENV: &str = "CENTAUR_COMPOSE_SYSTEM_PROMPT_BIN";
const DEFAULT_COMPOSE_BIN: &str = "compose-system-prompt";
/// Relative to the workspace, which is the harness-server working directory.
const TARGET_PROMPT: &str = "AGENTS.md";

/// Runs the hooks once per process, before the first turn. A failure keeps the
/// boot-time prompt and never fails the turn.
pub(crate) fn run_first_turn_prompt_hooks() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if env::var(OVERLAY_REPO_DIRS_ENV).map_or(true, |dirs| dirs.trim().is_empty()) {
            return;
        }
        let bin = env::var(COMPOSE_BIN_ENV).unwrap_or_else(|_| DEFAULT_COMPOSE_BIN.to_owned());
        let started = Instant::now();
        // stdout is the harness-server protocol channel; the composer and its
        // hooks must never write to it.
        let status = ProcessCommand::new(&bin)
            .args([
                "--target-prompt",
                TARGET_PROMPT,
                "--first-turn-prompt-hooks",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status();
        let elapsed_ms = started.elapsed().as_millis();
        match status {
            Ok(status) if status.success() => {
                eprintln!("first-turn prompt hooks composed in {elapsed_ms}ms");
            }
            Ok(status) => {
                eprintln!("first-turn prompt hooks failed after {elapsed_ms}ms: {bin} {status}");
            }
            Err(error) => {
                eprintln!("first-turn prompt hooks did not run: {bin}: {error}");
            }
        }
    });
}
