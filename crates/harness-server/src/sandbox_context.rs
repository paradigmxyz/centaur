//! The sandbox context file: which session, principal, and permissions this
//! sandbox serves.
//!
//! A warm sandbox boots before any session claims it, so its environment
//! cannot carry per-session values such as `CENTAUR_THREAD_KEY`. Each blocks
//! server writes this file once, before its first turn, when the claim has
//! already bound the session's iron-proxy principal. The thread key comes from
//! that turn's trace context. The principal, capabilities, and redacted grants
//! come from Console's `GET /api/v1/sandbox/permissions`, which authenticates
//! the sandbox-scoped token iron-proxy injects for it.
//!
//! The file is informational. The agent can overwrite it, so nothing may make
//! an authorization decision from it; iron-proxy and Console stay the
//! enforcement points.

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::otel::TraceContext;

/// Overrides where the file is written. Readers honor the same variable.
const CONTEXT_FILE_ENV: &str = "CENTAUR_SANDBOX_CONTEXT_FILE";
/// Relative to `$HOME` when `CENTAUR_SANDBOX_CONTEXT_FILE` is unset.
const DEFAULT_CONTEXT_FILE: &str = ".centaur/sandbox.json";
/// Set by the sandbox backend when the sandbox has an iron-proxy.
const CONSOLE_URL_ENV: &str = "CENTAUR_CONSOLE_URL";
/// Set on every sandbox api-rs creates.
const API_URL_ENV: &str = "CENTAUR_API_URL";
const PERSONA_ENV: &str = "CENTAUR_PERSONA_ID";
const PERMISSIONS_PATH: &str = "/api/v1/sandbox/permissions";
const CONTEXT_VERSION: u64 = 1;
/// The fetch delays the first reply, so it gets one short budget, retries
/// included. A miss still writes the file without Console's fields.
const FETCH_BUDGET: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_millis(250);

/// Writes the context file for the session this sandbox now serves. Failures
/// are logged and never fail the turn.
pub(crate) fn write_sandbox_context(trace: Option<&TraceContext>, harness: &str) {
    let Some(path) = context_path() else {
        return;
    };
    let started = Instant::now();
    let console = console_url().map(|url| fetch_permissions(&url, FETCH_BUDGET, true));
    let context = build_context(
        trace.and_then(|trace| trace.thread_key.as_deref()),
        harness,
        non_empty_env(PERSONA_ENV).as_deref(),
        console,
        unix_now(),
    );
    match write_atomically(&path, &context) {
        Ok(()) => eprintln!(
            "sandbox context written to {} in {}ms{}",
            path.display(),
            started.elapsed().as_millis(),
            context
                .get("sandbox_error")
                .and_then(Value::as_str)
                .map(|error| format!(" (without Console permissions: {error})"))
                .unwrap_or_default()
        ),
        Err(error) => eprintln!("sandbox context: writing {}: {error}", path.display()),
    }
}

/// Only API-created sandboxes (which always carry `CENTAUR_API_URL`) write
/// to the default path, so running harness-server elsewhere, as tests do,
/// leaves `$HOME` alone.
fn context_path() -> Option<PathBuf> {
    if let Some(path) = non_empty_env(CONTEXT_FILE_ENV) {
        return Some(PathBuf::from(path));
    }
    non_empty_env(API_URL_ENV)?;
    non_empty_env("HOME").map(|home| Path::new(&home).join(DEFAULT_CONTEXT_FILE))
}

fn console_url() -> Option<String> {
    let url = non_empty_env(CONSOLE_URL_ENV)?;
    let url = url.trim_end_matches('/');
    Some(
        if url.starts_with("http://") || url.starts_with("https://") {
            url.to_owned()
        } else {
            format!("http://{url}")
        },
    )
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `console` is `None` when the sandbox has no Console to ask, and the fetch
/// result otherwise.
fn build_context(
    thread_key: Option<&str>,
    harness: &str,
    persona: Option<&str>,
    console: Option<Result<Value, String>>,
    generated_at_unix: u64,
) -> Value {
    let mut context = Map::new();
    context.insert("version".to_owned(), json!(CONTEXT_VERSION));
    context.insert("generated_at_unix".to_owned(), json!(generated_at_unix));
    context.insert("harness".to_owned(), json!(harness));
    context.insert("thread_key".to_owned(), json!(thread_key));
    context.insert("persona".to_owned(), json!(persona));
    match console {
        Some(Ok(sandbox)) => {
            context.insert("sandbox".to_owned(), sandbox);
        }
        Some(Err(error)) => {
            context.insert("sandbox".to_owned(), Value::Null);
            context.insert("sandbox_error".to_owned(), json!(error));
        }
        None => {
            context.insert("sandbox".to_owned(), Value::Null);
        }
    }
    Value::Object(context)
}

/// Fetches Console's sandbox permissions `data` object, retrying transient
/// failures until `budget` runs out. A 401 is transient: right after a claim
/// the proxy may not have its new principal's token yet.
///
/// Runs on its own thread because the blocking client cannot run inside the
/// nanocodex server's async runtime.
fn fetch_permissions(
    base_url: &str,
    budget: Duration,
    use_env_proxy: bool,
) -> Result<Value, String> {
    let url = format!("{base_url}{PERMISSIONS_PATH}");
    thread::spawn(move || {
        let mut builder = reqwest::blocking::Client::builder().timeout(REQUEST_TIMEOUT);
        if !use_env_proxy {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|error| format!("building HTTP client: {error}"))?;
        let deadline = Instant::now() + budget;
        loop {
            let (error, retry) = match fetch_once(&client, &url) {
                Ok(data) => return Ok(data),
                Err(failure) => failure,
            };
            if !retry || Instant::now() + RETRY_DELAY >= deadline {
                return Err(error);
            }
            thread::sleep(RETRY_DELAY);
        }
    })
    .join()
    .unwrap_or_else(|_| Err("permissions fetch panicked".to_owned()))
}

/// One attempt. Errors carry whether a retry may succeed.
fn fetch_once(client: &reqwest::blocking::Client, url: &str) -> Result<Value, (String, bool)> {
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| (format!("GET {PERMISSIONS_PATH}: {error}"), true))?;
    let status = response.status();
    if !status.is_success() {
        let retry = status.as_u16() == 401 || status.as_u16() == 429 || status.is_server_error();
        return Err((format!("GET {PERMISSIONS_PATH}: HTTP {status}"), retry));
    }
    let body: Value = response
        .json()
        .map_err(|error| (format!("decoding {PERMISSIONS_PATH}: {error}"), false))?;
    match body.get("data") {
        Some(data) if data.is_object() => Ok(data.clone()),
        _ => Err((
            format!("{PERMISSIONS_PATH} response did not include a data object"),
            false,
        )),
    }
}

/// Writes beside the target and renames into place, so a reader never sees a
/// partial file.
fn write_atomically(path: &Path, context: &Value) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .map_or_else(|| "sandbox.json".into(), |name| name.to_string_lossy());
    let temp = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        serde_json::to_writer_pretty(&mut file, context)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// Serves one canned response per connection, in order, and records each
    /// request line.
    fn serve(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            for (status, body) in responses {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                seen.lock().unwrap().push(request_line.trim().to_owned());
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                }
                let mut discard = vec![0; content_length];
                reader.read_exact(&mut discard).unwrap();
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (url, requests)
    }

    #[test]
    fn retries_until_the_claimed_principal_is_applied() {
        let (url, requests) = serve(vec![
            (401, r#"{"error":"invalid sandbox token"}"#),
            (503, "{}"),
            (
                200,
                r#"{"data":{"sandbox_id":"sbx-1","principal_id":"prn_a"}}"#,
            ),
        ]);

        let data = fetch_permissions(&url, Duration::from_secs(5), false).unwrap();

        assert_eq!(data["principal_id"], "prn_a");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|line| line.starts_with(&format!("GET {PERMISSIONS_PATH} ")))
        );
    }

    #[test]
    fn does_not_retry_a_forbidden_request() {
        let (url, requests) = serve(vec![(403, "{}"), (200, r#"{"data":{}}"#)]);

        let error = fetch_permissions(&url, Duration::from_secs(5), false).unwrap_err();

        assert!(error.contains("403"), "{error}");
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn gives_up_when_the_budget_runs_out() {
        // Nothing listens on a port the OS just released.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let started = Instant::now();
        let error = fetch_permissions(
            &format!("http://127.0.0.1:{port}"),
            Duration::from_millis(600),
            false,
        )
        .unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            error.starts_with(&format!("GET {PERMISSIONS_PATH}")),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_response_without_data() {
        let (url, _) = serve(vec![(200, r#"{"data":[]}"#)]);

        let error = fetch_permissions(&url, Duration::from_secs(5), false).unwrap_err();

        assert!(error.contains("data object"), "{error}");
    }

    #[test]
    fn context_records_console_data_or_why_it_is_missing() {
        let with_console = build_context(
            Some("slack:T1:C1:1.0"),
            "codex",
            Some("eng"),
            Some(Ok(json!({ "principal_id": "prn_a" }))),
            1_700_000_000,
        );
        assert_eq!(
            with_console,
            json!({
                "version": 1,
                "generated_at_unix": 1_700_000_000,
                "harness": "codex",
                "thread_key": "slack:T1:C1:1.0",
                "persona": "eng",
                "sandbox": { "principal_id": "prn_a" },
            })
        );

        let failed = build_context(None, "pi", None, Some(Err("HTTP 401".to_owned())), 1);
        assert_eq!(failed["thread_key"], Value::Null);
        assert_eq!(failed["sandbox"], Value::Null);
        assert_eq!(failed["sandbox_error"], "HTTP 401");

        let no_console = build_context(Some("cli:t"), "pi", None, None, 1);
        assert_eq!(no_console["sandbox"], Value::Null);
        assert!(no_console.get("sandbox_error").is_none());
    }

    #[test]
    fn writes_the_file_atomically_and_replaces_it() {
        let root = env::temp_dir().join(format!("sandbox-context-{}", Uuid::new_v4().simple()));
        let path = root.join("nested/sandbox.json");

        write_atomically(&path, &json!({ "thread_key": "a" })).unwrap();
        write_atomically(&path, &json!({ "thread_key": "b" })).unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["thread_key"], "b");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["sandbox.json"]);
        let _ = fs::remove_dir_all(root);
    }
}
