//! `harness-server serve`: boot without a session, then bind to one.
//!
//! A generic sandbox (for example a warm-pool sandbox) runs `harness-server
//! serve`, which blocks until the control plane writes a `sandbox.bind` line on
//! stdin. The line names the harness and carries the session env and files.
//! `serve` applies them, runs the image's bind hook, and then serves the named
//! harness on the rest of stdin.
//!
//! The control plane sends the bind line on every attach, because it cannot
//! tell a fresh process from one that is already bound. Stdin is therefore
//! filtered for the life of the process: repeated bind lines are dropped before
//! the harness server reads its protocol.

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use serde::Deserialize;

use crate::{HarnessServerError, Result};

const BIND_LINE_TYPE: &str = "sandbox.bind";
/// Image hook that redoes the env-dependent part of sandbox startup.
const BIND_HOOK: &str = "centaur-bind-sandbox";
/// Overrides [`BIND_HOOK`]; used by tests that run outside the sandbox image.
const BIND_HOOK_ENV: &str = "CENTAUR_SANDBOX_BIND_HOOK";

#[derive(Debug, Deserialize)]
struct SandboxBind {
    #[serde(rename = "type")]
    kind: String,
    harness: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    unset: Vec<String>,
    #[serde(default)]
    files: Vec<BindFile>,
}

#[derive(Debug, Deserialize)]
struct BindFile {
    target_path: String,
    contents: String,
}

/// Waits for the `sandbox.bind` line, applies it, and leaves stdin filtered.
/// Returns the harness subcommand the line names.
pub fn accept_sandbox_bind() -> Result<String> {
    // Read through a duplicate so fd 0 can be replaced by the filter pipe.
    let mut stdin = BufReader::new(File::from(dup_cloexec(io::stdin().as_fd().as_raw_fd())?));
    let line = read_bind_line(&mut stdin)?;
    let bind = parse_bind_line(&line)?;
    let hook = env::var(BIND_HOOK_ENV).unwrap_or_else(|_| BIND_HOOK.to_owned());
    apply_bind(&bind, &hook)?;
    install_stdin_filter(stdin, line)?;
    Ok(bind.harness)
}

fn read_bind_line(reader: &mut impl BufRead) -> Result<String> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid_bind("stdin closed before a sandbox.bind line"));
        }
        if !line.trim().is_empty() {
            return Ok(line.trim().to_owned());
        }
    }
}

fn parse_bind_line(line: &str) -> Result<SandboxBind> {
    let bind: SandboxBind = serde_json::from_str(line).map_err(|error| {
        invalid_bind(format!("first stdin line is not a sandbox.bind: {error}"))
    })?;
    if bind.kind != BIND_LINE_TYPE {
        return Err(invalid_bind(format!(
            "first stdin line has type {:?}, expected {BIND_LINE_TYPE:?}",
            bind.kind
        )));
    }
    Ok(bind)
}

fn apply_bind(bind: &SandboxBind, hook: &str) -> Result<()> {
    let mut changed = Vec::new();
    for (name, value) in &bind.env {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(invalid_bind(format!("invalid env entry {name:?}")));
        }
        if env::var(name).ok().as_deref() != Some(value.as_str()) {
            changed.push(name.as_str());
        }
    }
    for name in &bind.unset {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(invalid_bind(format!("invalid env name {name:?}")));
        }
        if env::var_os(name).is_some() {
            changed.push(name.as_str());
        }
    }
    // SAFETY: this runs before harness-server starts serving. The only other
    // thread is the SIGTERM watcher, which is parked in the signal handler and
    // does not read the environment.
    unsafe {
        for (name, value) in &bind.env {
            env::set_var(name, value);
        }
        for name in &bind.unset {
            env::remove_var(name);
        }
    }

    for file in &bind.files {
        let path = Path::new(&file.target_path);
        if !path.is_absolute() {
            return Err(invalid_bind(format!(
                "file target {:?} is not absolute",
                file.target_path
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &file.contents)?;
    }

    // stdout carries the harness protocol, so hook output goes to stderr.
    let stdout = io::stderr().as_fd().try_clone_to_owned()?;
    let status = Command::new(hook)
        .env("CENTAUR_BIND_HARNESS", &bind.harness)
        .env("CENTAUR_BIND_CHANGED_ENV", changed.join(","))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .status()
        .map_err(|error| invalid_bind(format!("failed to run bind hook {hook:?}: {error}")))?;
    if !status.success() {
        return Err(invalid_bind(format!(
            "bind hook {hook:?} exited with {status}"
        )));
    }
    Ok(())
}

/// Replace fd 0 with a pipe fed from the real stdin minus bind lines, so every
/// harness server keeps reading `io::stdin()` unchanged.
fn install_stdin_filter(reader: BufReader<File>, bind_line: String) -> Result<()> {
    let (read_end, write_end) = pipe_cloexec()?;
    // dup2 clears close-on-exec on fd 0, so harness children still inherit it.
    // SAFETY: both descriptors are open and owned by this process.
    if unsafe { libc::dup2(read_end.as_raw_fd(), libc::STDIN_FILENO) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    drop(read_end);
    thread::spawn(move || forward_stdin(reader, File::from(write_end), &bind_line));
    Ok(())
}

fn forward_stdin(mut reader: impl BufRead, mut writer: impl Write, bind_line: &str) {
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if let Some(text) = std::str::from_utf8(&line)
            .ok()
            .map(str::trim)
            .filter(|text| is_bind_line(text))
        {
            if text != bind_line {
                eprintln!("harness-server: ignoring a different sandbox.bind; already bound");
            }
            continue;
        }
        if writer
            .write_all(&line)
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

/// Only lines that mention the bind type are parsed, so protocol lines (which
/// can be large attachment chunks) pass through without a second JSON parse.
fn is_bind_line(line: &str) -> bool {
    #[derive(Deserialize)]
    struct Kind<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<&'a str>,
    }
    line.contains(BIND_LINE_TYPE)
        && serde_json::from_str::<Kind<'_>>(line)
            .is_ok_and(|line| line.kind == Some(BIND_LINE_TYPE))
}

fn dup_cloexec(fd: i32) -> Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC returns a new descriptor or -1.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `duplicate` is a fresh descriptor owned by nothing else.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn pipe_cloexec() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: pipe2 succeeded, so both descriptors are fresh and owned here.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn invalid_bind(message: impl Into<String>) -> HarnessServerError {
    HarnessServerError::InvalidBind {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_stdin_drops_bind_lines_and_keeps_protocol_lines() {
        let bind = r#"{"type":"sandbox.bind","harness":"codex"}"#;
        let user = r#"{"type":"user","text":"what is a sandbox.bind line?"}"#;
        let input = format!(
            "{bind}\n{user}\n  {bind}\n{{\"harness\":\"amp\",\"type\":\"sandbox.bind\"}}\n{{\"type\":\"interrupt\"}}"
        );
        let mut output = Vec::new();

        forward_stdin(input.as_bytes(), &mut output, bind);

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("{user}\n{{\"type\":\"interrupt\"}}")
        );
    }

    #[test]
    fn parse_bind_line_rejects_protocol_lines() {
        let error = parse_bind_line(r#"{"type":"user","text":"hi"}"#).unwrap_err();
        assert!(error.to_string().contains("not a sandbox.bind"), "{error}");
    }
}
