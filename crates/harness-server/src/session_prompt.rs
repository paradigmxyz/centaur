//! Overlay `session-prompt.sh` hooks: prompt text an overlay generates for the
//! session a sandbox serves.
//!
//! Each blocks server starts its harness lazily on the first user message it
//! receives. By then the sandbox claim has bound the session's proxy
//! credentials, so overlay hooks run here rather than at boot (when a warm
//! sandbox belongs to no session). Their output is passed to `compose-system-prompt`, which rewrites
//! the workspace `AGENTS.md` that the harness reads as it starts.

use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::Once;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use uuid::Uuid;

use crate::otel::{PromptHookRun, PromptHooksRun, TraceContext, record_session_prompt_hooks};

/// Overlay repo roots that may run sandbox hooks, rendered by the chart from
/// the configured overlay sources. A public-only session mounts only public
/// repos, so a private overlay listed here has no hook on disk.
const OVERLAY_REPO_DIRS_ENV: &str = "CENTAUR_OVERLAY_REPO_DIRS";
const HOOK_PATH: &str = "services/sandbox/hooks/session-prompt.sh";
/// One time limit for every hook in the pass, since hooks delay the reply.
const HOOKS_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest output one hook may add to the prompt.
const HOOK_MAX_BYTES: usize = 64 * 1024;
const COMPOSE_BIN_ENV: &str = "CENTAUR_COMPOSE_SYSTEM_PROMPT_BIN";
const DEFAULT_COMPOSE_BIN: &str = "compose-system-prompt";
/// Relative to the workspace, which is the harness-server working directory.
const TARGET_PROMPT: &str = "AGENTS.md";
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Runs the hooks once per process, before the first turn it handles, and records
/// their timing on that turn's trace. A failure keeps the boot-time prompt and never
/// fails the turn.
///
/// The sandbox context file is written first, so hooks can read it.
///
/// `harness` names the harness in span names, as in `<harness>.session_task.turn`.
pub(crate) fn run_session_prompt_hooks(trace: Option<&TraceContext>, harness: &str) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        crate::sandbox_context::write_sandbox_context(trace, harness);
        let repos = overlay_repo_dirs();
        if repos.is_empty() {
            return;
        }
        let started_at = SystemTime::now();
        let hooks = run_hooks(&repos, HOOKS_TIMEOUT, HOOK_MAX_BYTES);
        let outcome = compose(&hooks);
        let run = PromptHooksRun {
            started_at,
            ended_at: SystemTime::now(),
            outcome,
            hooks: hooks.into_iter().map(|hook| hook.run).collect(),
        };
        let elapsed = run.ended_at.duration_since(started_at).unwrap_or_default();
        eprintln!(
            "session prompt hooks {outcome} in {}ms ({} hooks)",
            elapsed.as_millis(),
            run.hooks.len()
        );
        record_session_prompt_hooks(trace, harness, &run);
    });
}

struct HookOutcome {
    run: PromptHookRun,
    output: Option<Vec<u8>>,
}

fn overlay_repo_dirs() -> Vec<PathBuf> {
    env::var(OVERLAY_REPO_DIRS_ENV)
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !dir.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Runs each overlay's hook in order under one shared deadline. The hook running
/// when it expires is killed; hooks still left are skipped.
fn run_hooks(repos: &[PathBuf], timeout: Duration, max_bytes: usize) -> Vec<HookOutcome> {
    let deadline = Instant::now() + timeout;
    repos
        .iter()
        .map(|repo| (repo, repo.join(HOOK_PATH)))
        .filter(|(_, hook)| hook.is_file())
        .map(|(repo, hook)| run_hook(repo, &hook, deadline, max_bytes))
        .collect()
}

fn run_hook(repo: &Path, hook: &Path, deadline: Instant, max_bytes: usize) -> HookOutcome {
    let started_at = SystemTime::now();
    let finish = |status: &str, output: Option<Vec<u8>>, exit_code: Option<i32>| {
        if !matches!(status, "ok" | "empty") {
            eprintln!("session prompt hook {}: {status}", hook.display());
        }
        HookOutcome {
            run: PromptHookRun {
                repo: repo.display().to_string(),
                hook: hook.display().to_string(),
                status: status.to_owned(),
                started_at,
                ended_at: SystemTime::now(),
                output_bytes: output.as_ref().map_or(0, |output| output.len() as u64),
                exit_code: exit_code.map(i64::from),
            },
            output: output.filter(|_| status == "ok"),
        }
    };

    // A hook must live inside its overlay repo, not behind a symlink to an
    // arbitrary (possibly agent-writable) path.
    let inside_repo = match (hook.canonicalize(), repo.canonicalize()) {
        (Ok(hook), Ok(repo)) => hook.starts_with(repo),
        _ => false,
    };
    if !inside_repo {
        return finish("outside_repo", None, None);
    }
    if Instant::now() >= deadline {
        return finish("skipped", None, None);
    }
    // Its own process group, so a timeout kills everything the hook started.
    // stdout is captured, never inherited: the harness-server stdout is the
    // protocol channel.
    let mut child = match ProcessCommand::new("bash")
        .arg(hook)
        .current_dir(repo)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "session prompt hook {} failed to start: {error}",
                hook.display()
            );
            return finish("start_failed", None, None);
        }
    };
    let (output_tx, output_rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        // Read one byte past the cap, so an oversized hook is caught without
        // buffering everything it prints. Dropping stdout then closes the pipe.
        let limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        thread::spawn(move || {
            let mut output = Vec::new();
            let _ = stdout.take(limit).read_to_end(&mut output);
            let _ = output_tx.send(output);
        });
    }
    // A child that left the group (setsid) can hold stdout open past the hook,
    // so the output read is bounded by the deadline too.
    let output = match output_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(output) => output,
        Err(_) => {
            kill_hook(&mut child);
            return finish("timeout", None, None);
        }
    };
    if output.len() > max_bytes {
        kill_hook(&mut child);
        return finish("oversized", Some(output), None);
    }
    let Some(status) = wait_until(&mut child, deadline) else {
        kill_hook(&mut child);
        return finish("timeout", None, None);
    };
    let exit_code = status.code();
    if !status.success() {
        return finish("failed", Some(output), exit_code);
    }
    // Reject invalid text here, per hook, so it cannot fail the composer and
    // drop every other hook's output with it.
    match std::str::from_utf8(&output) {
        Err(_) => finish("invalid_utf8", Some(output), exit_code),
        Ok(text) if text.trim().is_empty() => finish("empty", Some(output), exit_code),
        Ok(_) => finish("ok", Some(output), exit_code),
    }
}

fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Kills the hook's process group and the hook itself, which may have left the
/// group. Background processes that left it are the hook author's to clean up.
fn kill_hook(child: &mut Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg takes no pointers. The hook is not yet reaped, so its
        // pid still names its process group.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    // SIGKILL to the hook itself, so the wait below returns promptly.
    let _ = child.kill();
    let _ = child.wait();
}

/// Writes each `ok` hook's output to a temp file and recomposes the workspace
/// prompt with them. Returns the pass outcome for the span.
fn compose(hooks: &[HookOutcome]) -> &'static str {
    let outputs: Vec<&[u8]> = hooks
        .iter()
        .filter_map(|hook| hook.output.as_deref())
        .collect();
    if outputs.is_empty() {
        return "no_output";
    }
    let pass = Uuid::new_v4().simple();
    let mut files = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        let path = env::temp_dir().join(format!("centaur-session-prompt-{pass}-{index}.md"));
        if let Err(error) = fs::write(&path, output) {
            eprintln!("session prompt hooks: writing {}: {error}", path.display());
            remove_all(&files);
            return "compose_failed";
        }
        files.push(path);
    }
    let bin = env::var(COMPOSE_BIN_ENV).unwrap_or_else(|_| DEFAULT_COMPOSE_BIN.to_owned());
    let mut command = ProcessCommand::new(&bin);
    command.args(["--target-prompt", TARGET_PROMPT]);
    for file in &files {
        command.arg("--session-prompt").arg(file);
    }
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();
    remove_all(&files);
    match status {
        Ok(status) if status.success() => "composed",
        Ok(status) => {
            eprintln!("session prompt hooks: {bin} {status}");
            "compose_failed"
        }
        Err(error) => {
            eprintln!("session prompt hooks: {bin}: {error}");
            "compose_failed"
        }
    }
}

fn remove_all(files: &[PathBuf]) {
    for file in files {
        let _ = fs::remove_file(file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay(root: &Path, name: &str, script: &str) -> PathBuf {
        let repo = root.join(name);
        let hook = repo.join(HOOK_PATH);
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        fs::write(&hook, script).unwrap();
        repo
    }

    fn temp_root() -> PathBuf {
        let root =
            env::temp_dir().join(format!("session-prompt-hooks-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn statuses(hooks: &[HookOutcome]) -> Vec<&str> {
        hooks.iter().map(|hook| hook.run.status.as_str()).collect()
    }

    #[test]
    fn runs_hooks_in_order_and_reports_each_outcome() {
        let root = temp_root();
        let repos = vec![
            overlay(&root, "ok", "echo \"from $(basename \"$PWD\")\"\n"),
            root.join("not-mounted"),
            overlay(&root, "empty", "echo '   '\n"),
            overlay(&root, "failing", "echo partial\nexit 3\n"),
            overlay(&root, "large", "head -c 100 /dev/zero | tr '\\0' x\n"),
            overlay(&root, "binary", "printf 'bad \\377\\n'\n"),
            overlay(&root, "after", "echo after\n"),
        ];

        let hooks = run_hooks(&repos, Duration::from_secs(10), 50);

        assert_eq!(
            statuses(&hooks),
            ["ok", "empty", "failed", "oversized", "invalid_utf8", "ok"]
        );
        assert_eq!(hooks[5].output.as_deref(), Some(&b"after\n"[..]));
        assert_eq!(hooks[0].output.as_deref(), Some(&b"from ok\n"[..]));
        assert_eq!(hooks[0].run.exit_code, Some(0));
        assert_eq!(hooks[2].run.exit_code, Some(3));
        assert!(hooks[1..5].iter().all(|hook| hook.output.is_none()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn one_time_limit_covers_every_hook() {
        let root = temp_root();
        let repos = vec![
            overlay(&root, "a", "sleep 0.3\necho a\n"),
            overlay(&root, "b", "sleep 30\n"),
            overlay(&root, "c", "echo c\n"),
        ];

        let started = Instant::now();
        let hooks = run_hooks(&repos, Duration::from_millis(800), HOOK_MAX_BYTES);

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(statuses(&hooks), ["ok", "timeout", "skipped"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn timeout_does_not_wait_for_children_that_left_the_group() {
        let root = temp_root();
        let pid_file = root.join("escaped.pid");
        let repos = vec![overlay(
            &root,
            "escape",
            &format!(
                "setsid sleep 30 & echo $! > {}\nexit 0\n",
                pid_file.display()
            ),
        )];

        let started = Instant::now();
        let hooks = run_hooks(&repos, Duration::from_millis(300), HOOK_MAX_BYTES);
        let elapsed = started.elapsed();
        if let Ok(pid) = fs::read_to_string(&pid_file) {
            // SAFETY: kill takes no pointers.
            unsafe {
                libc::kill(pid.trim().parse().unwrap(), libc::SIGKILL);
            }
        }

        assert!(elapsed < Duration::from_secs(3), "waited {elapsed:?}");
        assert_eq!(statuses(&hooks), ["timeout"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stops_reading_an_oversized_hook_at_the_cap() {
        let root = temp_root();
        let repos = vec![overlay(&root, "noisy", "yes\n")];

        let started = Instant::now();
        let hooks = run_hooks(&repos, Duration::from_secs(10), 1024);

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(statuses(&hooks), ["oversized"]);
        assert_eq!(hooks[0].run.output_bytes, 1025);
        assert!(hooks[0].output.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn timeout_kills_a_hook_that_left_its_process_group() {
        let root = temp_root();
        let repos = vec![overlay(
            &root,
            "leader",
            "exec python3 -c 'import os, time; os.setpgid(0, os.getpgid(os.getppid())); time.sleep(30)'\n",
        )];

        let started = Instant::now();
        let hooks = run_hooks(&repos, Duration::from_millis(300), HOOK_MAX_BYTES);

        assert!(
            started.elapsed() < Duration::from_secs(3),
            "waited {:?}",
            started.elapsed()
        );
        assert_eq!(statuses(&hooks), ["timeout"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_a_hook_that_resolves_outside_its_repo() {
        let root = temp_root();
        let outside = root.join("outside.sh");
        fs::write(&outside, "echo escaped\n").unwrap();
        let repo = root.join("overlay");
        let hook = repo.join(HOOK_PATH);
        fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, &hook).unwrap();

        let hooks = run_hooks(&[repo], Duration::from_secs(10), HOOK_MAX_BYTES);

        assert_eq!(statuses(&hooks), ["outside_repo"]);
        assert!(hooks[0].output.is_none());
        let _ = fs::remove_dir_all(root);
    }
}
