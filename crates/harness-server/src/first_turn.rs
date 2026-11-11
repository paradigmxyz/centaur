//! Overlay `first-turn-prompt.sh` hooks.
//!
//! Each blocks server starts its harness lazily on the first user message. By
//! then the sandbox claim has bound the session's proxy credentials, so overlay
//! hooks run here rather than at boot (when a warm sandbox belongs to no
//! session). `compose-system-prompt` reruns them and rewrites the workspace
//! `AGENTS.md`, which the harness reads as it starts.

use std::env;
use std::io::{self, Read};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::Once;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::otel::{PromptHookRun, PromptHooksRun, TraceContext, record_first_turn_prompt_hooks};

/// Overlay repo roots that may run sandbox hooks, rendered by the chart from
/// the configured overlay sources. A public-only session mounts only public
/// repos, so a private overlay listed here has no hook on disk.
const OVERLAY_REPO_DIRS_ENV: &str = "CENTAUR_OVERLAY_REPO_DIRS";
const COMPOSE_BIN_ENV: &str = "CENTAUR_COMPOSE_SYSTEM_PROMPT_BIN";
const DEFAULT_COMPOSE_BIN: &str = "compose-system-prompt";
/// Relative to the workspace, which is the harness-server working directory.
const TARGET_PROMPT: &str = "AGENTS.md";
/// The one time limit for every hook in the pass.
const HOOKS_TIMEOUT_ENV: &str = "CENTAUR_FIRST_TURN_PROMPT_HOOKS_TIMEOUT_SECONDS";
const DEFAULT_HOOKS_TIMEOUT: Duration = Duration::from_secs(10);
/// Extra time the composer gets past the hooks limit to start up and write the
/// prompt before it is stopped.
const COMPOSER_GRACE: Duration = Duration::from_secs(2);
/// How long the composer gets after SIGTERM to kill its running hook.
const COMPOSER_TERM_GRACE: Duration = Duration::from_secs(1);
const COMPOSER_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Runs the hooks once per process, before the first turn, and records their
/// timing on that turn's trace. A failure keeps the boot-time prompt and never
/// fails the turn.
///
/// `harness` names the harness in span names, as in `<harness>.session_task.turn`.
pub(crate) fn run_first_turn_prompt_hooks(trace: Option<&TraceContext>, harness: &str) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if env::var(OVERLAY_REPO_DIRS_ENV).map_or(true, |dirs| dirs.trim().is_empty()) {
            return;
        }
        let run = compose_with_hooks(hooks_timeout());
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

fn hooks_timeout() -> Duration {
    let Ok(raw) = env::var(HOOKS_TIMEOUT_ENV) else {
        return DEFAULT_HOOKS_TIMEOUT;
    };
    match raw.trim().parse::<f64>().ok().and_then(|seconds| {
        (seconds > 0.0)
            .then(|| Duration::try_from_secs_f64(seconds).ok())
            .flatten()
    }) {
        Some(timeout) => timeout,
        None => {
            eprintln!("ignoring invalid {HOOKS_TIMEOUT_ENV}={raw:?}");
            DEFAULT_HOOKS_TIMEOUT
        }
    }
}

fn compose_with_hooks(timeout: Duration) -> PromptHooksRun {
    let bin = env::var(COMPOSE_BIN_ENV).unwrap_or_else(|_| DEFAULT_COMPOSE_BIN.to_owned());
    let started_at = SystemTime::now();
    let finish = |outcome, hooks| PromptHooksRun {
        started_at,
        ended_at: SystemTime::now(),
        outcome,
        hooks,
    };
    // stdout is captured for the hook report, never inherited: the
    // harness-server stdout is the protocol channel.
    let spawned = ProcessCommand::new(&bin)
        .args([
            "--target-prompt",
            TARGET_PROMPT,
            "--first-turn-prompt-hooks",
            "--hooks-timeout-seconds",
            &timeout.as_secs_f64().to_string(),
            "--report-json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            eprintln!("first-turn prompt hooks did not run: {bin}: {error}");
            return finish("not_run", Vec::new());
        }
    };
    // Hooks never inherit the composer's stdout, so it closes when the
    // composer exits and this reader cannot outlive it.
    let reader = child.stdout.take().map(|mut stdout| {
        thread::spawn(move || {
            let mut report = Vec::new();
            let _ = stdout.read_to_end(&mut report);
            report
        })
    });
    let exit = wait_with_deadline(&mut child, Instant::now() + timeout + COMPOSER_GRACE);
    let report = reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let hooks = serde_json::from_slice::<HooksReport>(&report)
        .map(|report| report.hooks.into_iter().map(PromptHookRun::from).collect());
    match (exit, hooks) {
        (Ok(Some(status)), Ok(hooks)) if status.success() => finish("composed", hooks),
        (Ok(Some(status)), Err(error)) if status.success() => {
            eprintln!("first-turn prompt hook report unreadable: {error}");
            finish("report_unreadable", Vec::new())
        }
        (Ok(Some(status)), hooks) => {
            eprintln!("first-turn prompt hooks failed: {bin} {status}");
            finish("failed", hooks.unwrap_or_default())
        }
        (Ok(None), _) => {
            eprintln!("first-turn prompt hooks stopped: {bin} ran past the hooks time limit");
            finish("timeout", Vec::new())
        }
        (Err(error), _) => {
            eprintln!("first-turn prompt hooks failed: waiting on {bin}: {error}");
            finish("failed", Vec::new())
        }
    }
}

/// Waits for the composer until `deadline`, then stops it: SIGTERM first so it
/// can kill its running hook, then SIGKILL. `None` means it was stopped.
fn wait_with_deadline(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        thread::sleep(COMPOSER_POLL_INTERVAL);
    }
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill takes no pointers; the child is not yet reaped, so the
        // pid still names it.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    let kill_at = Instant::now() + COMPOSER_TERM_GRACE;
    while Instant::now() < kill_at {
        if child.try_wait()?.is_some() {
            return Ok(None);
        }
        thread::sleep(COMPOSER_POLL_INTERVAL);
    }
    let _ = child.kill();
    child.wait()?;
    Ok(None)
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
