//! The turn trace file: the trace context of the turn this sandbox is running.
//!
//! Programs the agent runs during a turn, such as `centaur-codemode exec`,
//! read it to parent their spans on the turn's execution span, beside the
//! harness's own tool spans. A harness process lives across turns, so its
//! environment cannot carry a per-turn `traceparent`; a file can.
//!
//! The file is written when a turn starts and removed when it ends. Like the
//! sandbox context file it is informational: the agent can overwrite it, and
//! a wrong value only misplaces spans.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::otel::TraceContext;

/// Overrides where the file is written. Readers honor the same variable.
const TRACE_FILE_ENV: &str = "CENTAUR_TURN_TRACE_FILE";
/// Relative to `$HOME` when `CENTAUR_TURN_TRACE_FILE` is unset.
const DEFAULT_TRACE_FILE: &str = ".centaur/turn-trace.json";
/// Set on every sandbox api-rs creates.
const API_URL_ENV: &str = "CENTAUR_API_URL";
const TRACE_FILE_VERSION: u64 = 1;

/// The file for one turn; dropping it removes the file unless a later turn
/// has replaced it.
#[derive(Debug)]
pub(crate) struct TurnTraceFile {
    path: PathBuf,
    traceparent: String,
}

impl TurnTraceFile {
    /// Writes the file for a turn with a valid `traceparent`. Failures are
    /// logged and never fail the turn.
    pub(crate) fn write(trace: &TraceContext) -> Option<Self> {
        Self::write_to(trace_file_path()?, trace)
    }

    fn write_to(path: PathBuf, trace: &TraceContext) -> Option<Self> {
        let traceparent = trace.effective_traceparent()?;
        let contents = json!({
            "version": TRACE_FILE_VERSION,
            "traceparent": traceparent,
            "thread_key": trace.thread_key,
            "execution_id": trace.metadata.get("execution_id").and_then(Value::as_str),
        });
        match crate::sandbox_context::write_atomically(&path, &contents) {
            Ok(()) => Some(Self { path, traceparent }),
            Err(error) => {
                eprintln!("turn trace file: writing {}: {error}", path.display());
                None
            }
        }
    }
}

impl Drop for TurnTraceFile {
    fn drop(&mut self) {
        if written_traceparent(&self.path).as_deref() == Some(self.traceparent.as_str()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn written_traceparent(path: &Path) -> Option<String> {
    let file: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    file.get("traceparent")?.as_str().map(str::to_owned)
}

/// Only API-created sandboxes (which always carry `CENTAUR_API_URL`) write
/// to the default path, so running harness-server elsewhere, as tests do,
/// leaves `$HOME` alone.
fn trace_file_path() -> Option<PathBuf> {
    let non_empty =
        |name: &str| env::var_os(name).filter(|value| !value.to_string_lossy().trim().is_empty());
    if let Some(path) = non_empty(TRACE_FILE_ENV) {
        return Some(PathBuf::from(path));
    }
    non_empty(API_URL_ENV)?;
    non_empty("HOME").map(|home| PathBuf::from(home).join(DEFAULT_TRACE_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path =
                env::temp_dir().join(format!("turn-trace-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const TRACEPARENT: &str = "00-0123456789abcdef0123456789abcdef-1111111111111111-01";

    fn trace(traceparent: &str) -> TraceContext {
        TraceContext {
            thread_key: Some("slack:C1:1.2".to_owned()),
            traceparent: Some(traceparent.to_owned()),
            metadata: BTreeMap::from([("execution_id".to_owned(), json!("exe_1"))]),
        }
    }

    #[test]
    fn writes_the_turn_context_and_removes_it_when_the_turn_ends() {
        let dir = TempDir::new();
        let path = dir.path().join(".centaur/turn-trace.json");

        let file = TurnTraceFile::write_to(path.clone(), &trace(TRACEPARENT)).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["traceparent"], TRACEPARENT);
        assert_eq!(written["thread_key"], "slack:C1:1.2");
        assert_eq!(written["execution_id"], "exe_1");
        assert_eq!(written["version"], 1);

        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn leaves_a_newer_turns_file_in_place() {
        let dir = TempDir::new();
        let path = dir.path().join("turn-trace.json");
        let newer = "00-0123456789abcdef0123456789abcdef-2222222222222222-01";

        let first = TurnTraceFile::write_to(path.clone(), &trace(TRACEPARENT)).unwrap();
        let _second = TurnTraceFile::write_to(path.clone(), &trace(newer)).unwrap();
        drop(first);

        assert_eq!(written_traceparent(&path).as_deref(), Some(newer));
    }

    #[test]
    fn writes_nothing_without_a_valid_traceparent() {
        let dir = TempDir::new();
        let path = dir.path().join("turn-trace.json");

        assert!(TurnTraceFile::write_to(path.clone(), &trace("invalid")).is_none());
        assert!(TurnTraceFile::write_to(path.clone(), &TraceContext::default()).is_none());
        assert!(!path.exists());
    }
}
