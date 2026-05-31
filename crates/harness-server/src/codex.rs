use std::env;
use std::io::{self, Write};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;

use crate::{AppServerRuntime, HarnessServerError, Result};

#[derive(Debug, Default)]
pub struct CodexHarnessServer;

impl AppServerRuntime for CodexHarnessServer {
    fn run_stdio(&self) -> Result<()> {
        let bin = codex_bin();
        let mut child = ProcessCommand::new(&bin)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| HarnessServerError::SpawnCodex {
                bin: bin.clone(),
                source,
            })?;

        let mut child_stdin = child
            .stdin
            .take()
            .ok_or(HarnessServerError::CodexStdinUnavailable)?;
        let _stdin_thread = thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            io::copy(&mut stdin, &mut child_stdin)
        });

        let mut child_stderr = child
            .stderr
            .take()
            .ok_or(HarnessServerError::CodexStderrUnavailable)?;
        let stderr_thread = thread::spawn(move || {
            let mut stderr = io::stderr().lock();
            io::copy(&mut child_stderr, &mut stderr)
        });

        let mut child_stdout = child
            .stdout
            .take()
            .ok_or(HarnessServerError::CodexStdoutUnavailable)?;
        {
            let mut stdout = io::stdout().lock();
            io::copy(&mut child_stdout, &mut stdout)?;
            stdout.flush()?;
        }

        let status = child.wait()?;
        let _ = stderr_thread.join();
        if !status.success() {
            return Err(HarnessServerError::CodexExited { status });
        }
        Ok(())
    }
}

fn codex_bin() -> String {
    if let Ok(bin) = env::var("CODEX_BIN") {
        return bin;
    }

    let candidates = ["codex", "/Applications/Codex.app/Contents/Resources/codex"];
    candidates
        .iter()
        .find(|bin| codex_supports_stdio_listen(bin))
        .copied()
        .unwrap_or("codex")
        .to_string()
}

fn codex_supports_stdio_listen(bin: &str) -> bool {
    let Ok(output) = ProcessCommand::new(bin)
        .args(["app-server", "--help"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout.contains("--listen") || stderr.contains("--listen")
}
