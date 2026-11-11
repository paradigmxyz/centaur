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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::otel::{PromptHookRun, PromptHooksRun, TraceContext, record_first_turn_prompt_hooks};

/// Overlay repo roots that may run sandbox hooks, set by api-rs from the
/// configured overlay sources and scoped to the session's repo-cache access.
const OVERLAY_REPO_DIRS_ENV: &str = "CENTAUR_OVERLAY_REPO_DIRS";
const COMPOSE_BIN_ENV: &str = "CENTAUR_COMPOSE_SYSTEM_PROMPT_BIN";
const DEFAULT_COMPOSE_BIN: &str = "compose-system-prompt";
/// Relative to the workspace, which is the harness-server working directory.
const TARGET_PROMPT: &str = "AGENTS.md";

/// Runs the hooks once per process, before the first turn, and records their
/// timing on that turn's trace. A failure keeps the boot-time prompt and never
/// fails the turn.
/// `harness` names the harness in span names, as in `<harness>.session_task.turn`.
pub(crate) fn run_first_turn_prompt_hooks(trace: Option<&TraceContext>, harness: &str) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if env::var(OVERLAY_REPO_DIRS_ENV).map_or(true, |dirs| dirs.trim().is_empty()) {
            return;
        }
        let run = compose_with_hooks();
        let elapsed_ms = run
            .ended_at
            .duration_since(run.started_at)
            .unwrap_or_default()
            .as_millis();
        eprintln!(
            "first-turn prompt hooks {} in {elapsed_ms}ms ({} hooks)",
            run.outcome,
            run.hooks.len()
        );
        record_first_turn_prompt_hooks(trace, harness, &run);
    });
}

fn compose_with_hooks() -> PromptHooksRun {
    let bin = env::var(COMPOSE_BIN_ENV).unwrap_or_else(|_| DEFAULT_COMPOSE_BIN.to_owned());
    let started_at = SystemTime::now();
    // stdout is captured for the hook report, never inherited: the
    // harness-server stdout is the protocol channel.
    let output = ProcessCommand::new(&bin)
        .args([
            "--target-prompt",
            TARGET_PROMPT,
            "--first-turn-prompt-hooks",
            "--report-json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output();
    let ended_at = SystemTime::now();
    let (outcome, hooks) = match output {
        Err(error) => {
            eprintln!("first-turn prompt hooks did not run: {bin}: {error}");
            ("not_run", Vec::new())
        }
        Ok(output) => {
            let hooks = match serde_json::from_slice::<HooksReport>(&output.stdout) {
                Ok(report) => Some(report.hooks.into_iter().map(PromptHookRun::from).collect()),
                Err(error) => {
                    eprintln!("first-turn prompt hook report unreadable: {error}");
                    None
                }
            };
            if !output.status.success() {
                eprintln!("first-turn prompt hooks failed: {bin} {}", output.status);
            }
            match hooks {
                Some(hooks) if output.status.success() => ("composed", hooks),
                hooks => ("failed", hooks.unwrap_or_default()),
            }
        }
    };
    PromptHooksRun {
        started_at,
        ended_at,
        outcome,
        hooks,
    }
}

#[derive(Debug, Deserialize)]
struct HooksReport {
    hooks: Vec<HookReport>,
}

#[derive(Debug, Deserialize)]
struct HookReport {
    repo: String,
    hook: String,
    status: String,
    /// Unix seconds.
    started_at: f64,
    ended_at: f64,
    #[serde(default)]
    output_bytes: u64,
    exit_code: Option<i64>,
}

impl From<HookReport> for PromptHookRun {
    fn from(report: HookReport) -> Self {
        Self {
            repo: report.repo,
            hook: report.hook,
            status: report.status,
            started_at: unix_seconds(report.started_at),
            ended_at: unix_seconds(report.ended_at),
            output_bytes: report.output_bytes,
            exit_code: report.exit_code,
        }
    }
}

fn unix_seconds(seconds: f64) -> SystemTime {
    Duration::try_from_secs_f64(seconds).map_or(UNIX_EPOCH, |offset| UNIX_EPOCH + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_report_converts_unix_seconds_and_optional_fields() {
        let report: HooksReport = serde_json::from_str(
            r#"{"hooks":[
                {"repo":"/r/a","hook":"/r/a/h.sh","status":"ok","started_at":1.5,
                 "ended_at":2.25,"output_bytes":3,"exit_code":0},
                {"repo":"/r/b","hook":"/r/b/h.sh","status":"timeout","started_at":2.0,
                 "ended_at":12.0,"exit_code":null}
            ]}"#,
        )
        .expect("report parses");
        let runs: Vec<PromptHookRun> = report.hooks.into_iter().map(Into::into).collect();

        assert_eq!(
            runs[0].ended_at.duration_since(runs[0].started_at).unwrap(),
            Duration::from_millis(750)
        );
        assert_eq!(runs[0].output_bytes, 3);
        assert_eq!(runs[0].exit_code, Some(0));
        assert_eq!(runs[1].status, "timeout");
        assert_eq!(runs[1].output_bytes, 0);
        assert_eq!(runs[1].exit_code, None);
    }
}
