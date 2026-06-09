use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

use crate::traits::HarnessKind;

pub type Result<T> = std::result::Result<T, HarnessServerError>;

#[derive(Debug, Error)]
pub enum HarnessServerError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex App Server protocol conversion failed: {0}")]
    Protocol(String),
    #[error("notification is not a Codex App Server V2 server notification: {message}")]
    InvalidServerNotification { message: String },
    #[error("invalid request params: {source}")]
    InvalidParams {
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid blocks-mode input: {message}")]
    InvalidBlocksInput { message: String },
    #[error("unknown threadId {thread_id}")]
    UnknownThread { thread_id: String },
    #[error("cwd must be absolute: {path}")]
    CwdMustBeAbsolute { path: PathBuf },
    #[error("path must be absolute: {path}")]
    PathMustBeAbsolute { path: PathBuf },
    #[error("failed to spawn harness command in {cwd}: {source}")]
    SpawnHarness {
        cwd: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("harness stdin was not piped")]
    HarnessStdinUnavailable,
    #[error("harness stdout was not piped")]
    HarnessStdoutUnavailable,
    #[error("harness stderr was not piped")]
    HarnessStderrUnavailable,
    #[error("{kind:?} exited with status {status}{stderr}")]
    HarnessExited {
        kind: HarnessKind,
        status: ExitStatus,
        stderr: String,
    },
    #[error("failed to spawn {bin} app-server: {source}")]
    SpawnCodex {
        bin: String,
        #[source]
        source: io::Error,
    },
    #[error("Codex stdin was not piped")]
    CodexStdinUnavailable,
    #[error("Codex stdout was not piped")]
    CodexStdoutUnavailable,
    #[error("Codex stderr was not piped")]
    CodexStderrUnavailable,
    #[error("codex app-server exited with status {status}")]
    CodexExited { status: ExitStatus },
}
