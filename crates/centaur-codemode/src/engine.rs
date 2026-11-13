//! Runs one cell in a fresh QuickJS runtime.
//!
//! Every cell gets its own runtime and context, so cells share no JS state and
//! limits apply per cell. Connectors are ES modules served from memory and
//! imported lazily on first call. Host capabilities (HTTP, sleep, files, logs,
//! secrets) are Rust functions captured by the prelude and hidden from cells.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr};
use rquickjs::{
    ArrayBuffer, AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Ctx, Exception, Function,
    IntoJs, Object, Promise, TypedArray, Value,
    loader::{BuiltinLoader, BuiltinResolver},
    prelude::{Async, Opt},
};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;

const PRELUDE: &str = include_str!("prelude.js");

/// Hard-coded limits for the spike.
pub const CELL_TIMEOUT: Duration = Duration::from_secs(120);
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
pub const STACK_LIMIT: usize = 1024 * 1024;
pub const MAX_RESULT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_FETCH_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_FILE_BYTES: usize = 100 * 1024 * 1024;
const MAX_LOG_LINES: usize = 1000;
const MAX_LOG_LINE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Principal {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Live OAuth providers, e.g. `["slack", "google"]`.
    #[serde(default)]
    pub oauth: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecRequest {
    pub code: String,
    pub principal: Principal,
    /// Connectors this principal may use; `None` means every loaded connector.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ExecError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ExecError>,
    pub logs: Vec<String>,
    pub calls: Vec<serde_json::Value>,
    pub ms: u64,
}

#[derive(Clone)]
pub struct Engine {
    catalog: Arc<Catalog>,
    http: reqwest::Client,
    files: Files,
}

impl Engine {
    pub fn new(catalog: Arc<Catalog>, files_dir: PathBuf, ca_file: Option<&Path>) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(TOOL_TIMEOUT)
            .connect_timeout(Duration::from_secs(10));
        if let Some(path) = ca_file {
            let pem = std::fs::read(path)
                .wrap_err_with(|| format!("reading CA bundle {}", path.display()))?;
            for certificate in reqwest::Certificate::from_pem_bundle(&pem)? {
                builder = builder.add_root_certificate(certificate);
            }
        }
        std::fs::create_dir_all(&files_dir)
            .wrap_err_with(|| format!("creating files dir {}", files_dir.display()))?;
        Ok(Self {
            catalog,
            http: builder.build()?,
            files: Files::new(files_dir)?,
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn files(&self) -> &Files {
        &self.files
    }

    pub async fn exec(&self, request: ExecRequest) -> ExecResponse {
        let started = Instant::now();
        let logs = Arc::new(Mutex::new(Vec::new()));
        let outcome = match tokio::time::timeout(
            CELL_TIMEOUT + Duration::from_secs(1),
            self.run(&request, logs.clone()),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => Outcome::failed(format!("{error:#}")),
            Err(_) => Outcome::failed(format!("cell timed out after {}s", CELL_TIMEOUT.as_secs())),
        };
        let logs = std::mem::take(&mut *logs.lock().expect("log lock"));
        ExecResponse {
            ok: outcome.error.is_none(),
            result: outcome.result,
            error: outcome.error,
            logs,
            calls: outcome.calls,
            ms: started.elapsed().as_millis() as u64,
        }
    }

    async fn run(&self, request: &ExecRequest, logs: Arc<Mutex<Vec<String>>>) -> Result<Outcome> {
        let connectors: Vec<_> = self.catalog.visible(request.tools.as_deref()).collect();
        let spec = serde_json::json!({
            "principal": request.principal,
            "tool_timeout_ms": TOOL_TIMEOUT.as_millis() as u64,
            "connectors": connectors
                .iter()
                .map(|connector| {
                    (
                        connector.name.clone(),
                        connector
                            .functions
                            .iter()
                            .map(|function| function.name.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        })
        .to_string();

        let runtime = AsyncRuntime::new()?;
        runtime.set_memory_limit(MEMORY_LIMIT).await;
        runtime.set_max_stack_size(STACK_LIMIT).await;
        let deadline = Instant::now() + CELL_TIMEOUT;
        runtime
            .set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)))
            .await;
        let mut resolver = BuiltinResolver::default();
        let mut loader = BuiltinLoader::default();
        for connector in &connectors {
            let module = format!("connector:{}", connector.name);
            resolver.add_module(module.clone());
            loader.add_module(module, connector.js.clone());
        }
        runtime.set_loader(resolver, loader).await;
        let context = AsyncContext::full(&runtime).await?;

        let host = Host {
            http: self.http.clone(),
            files: self.files.clone(),
            logs,
        };
        // No newline after the opening brace, so error line numbers match the cell.
        let code = format!("(async () => {{{}\n}})()", request.code);
        let outcome = context
            .async_with(async |ctx| {
                let setup = (|| -> rquickjs::Result<Promise<'_>> {
                    ctx.globals().set("__host", host.install(&ctx)?)?;
                    ctx.globals().set("__spec", ctx.json_parse(spec.as_str())?)?;
                    ctx.eval::<(), _>(PRELUDE)?;
                    ctx.eval::<Promise, _>(code.as_str())
                })();
                let promise = match setup.catch(&ctx) {
                    Ok(promise) => promise,
                    Err(error) => return Outcome::caught(&ctx, error),
                };
                let value = promise.into_future::<Value>().await.catch(&ctx);
                let mut outcome = match value {
                    Ok(value) => match ctx.json_stringify(value) {
                        Ok(text) => {
                            let text = text
                                .map(|text| text.to_string())
                                .transpose()
                                .unwrap_or_default()
                                .unwrap_or_else(|| "null".to_owned());
                            if text.len() > MAX_RESULT_BYTES {
                                Outcome::failed(format!(
                                    "result is {} bytes, over the {MAX_RESULT_BYTES}-byte limit; write large data with files.write and return the path",
                                    text.len()
                                ))
                            } else {
                                match serde_json::from_str(&text) {
                                    Ok(result) => Outcome::ok(result),
                                    Err(error) => Outcome::failed(format!(
                                        "result is not JSON-serializable: {error}"
                                    )),
                                }
                            }
                        }
                        Err(error) => Outcome::caught(&ctx, CaughtError::from_error(&ctx, error)),
                    },
                    Err(error) => Outcome::caught(&ctx, error),
                };
                outcome.calls = calls(&ctx);
                outcome
            })
            .await;
        // Pending host futures (such as cancelled tool-timeout timers) are
        // dropped with the runtime; the cell's result is already settled.
        drop(context);
        drop(runtime);
        Ok(outcome)
    }
}

fn calls(ctx: &Ctx<'_>) -> Vec<serde_json::Value> {
    let Ok(value) = ctx.globals().get::<_, Value>("__calls") else {
        return Vec::new();
    };
    ctx.json_stringify(value)
        .ok()
        .flatten()
        .and_then(|text| text.to_string().ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

struct Outcome {
    result: Option<serde_json::Value>,
    error: Option<ExecError>,
    calls: Vec<serde_json::Value>,
}

impl Outcome {
    fn ok(result: serde_json::Value) -> Self {
        Self {
            result: Some(result),
            error: None,
            calls: Vec::new(),
        }
    }

    fn failed(message: String) -> Self {
        Self {
            result: None,
            error: Some(ExecError {
                message,
                stack: None,
                tool: None,
            }),
            calls: Vec::new(),
        }
    }

    fn caught<'js>(ctx: &Ctx<'js>, error: CaughtError<'js>) -> Self {
        let error = match error {
            CaughtError::Exception(exception) => ExecError {
                message: exception
                    .message()
                    .unwrap_or_else(|| "uncaught exception".to_owned()),
                stack: exception.stack().filter(|stack| !stack.is_empty()),
                tool: exception.get::<_, Option<String>>("tool").ok().flatten(),
            },
            CaughtError::Value(value) => ExecError {
                message: ctx
                    .json_stringify(value)
                    .ok()
                    .flatten()
                    .and_then(|text| text.to_string().ok())
                    .unwrap_or_else(|| "uncaught non-Error value".to_owned()),
                stack: None,
                tool: None,
            },
            CaughtError::Error(error) => ExecError {
                message: error.to_string(),
                stack: None,
                tool: None,
            },
        };
        Self {
            result: None,
            error: Some(error),
            calls: Vec::new(),
        }
    }
}

#[derive(Clone)]
struct Host {
    http: reqwest::Client,
    files: Files,
    logs: Arc<Mutex<Vec<String>>>,
}

struct FetchOut {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl<'js> IntoJs<'js> for FetchOut {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let object = Object::new(ctx.clone())?;
        object.set("status", self.status)?;
        object.set("headers", self.headers)?;
        object.set("body", ArrayBuffer::new(ctx.clone(), self.body)?)?;
        Ok(object.into_value())
    }
}

struct Bytes(Vec<u8>);

impl<'js> IntoJs<'js> for Bytes {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        Ok(ArrayBuffer::new(ctx.clone(), self.0)?.into_value())
    }
}

fn copy_bytes(array: &TypedArray<'_, u8>) -> Vec<u8> {
    // SAFETY: the slice is copied before any JavaScript runs.
    unsafe { array.as_bytes() }
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

fn reject<T>(ctx: &Ctx<'_>, message: impl AsRef<str>) -> rquickjs::Result<T> {
    Err(Exception::throw_message(ctx, message.as_ref()))
}

impl Host {
    fn install<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
        let host = Object::new(ctx.clone())?;

        let http = self.http.clone();
        host.set(
            "fetch",
            Function::new(
                ctx.clone(),
                Async(
                    move |ctx: Ctx<'js>,
                          url: String,
                          method: String,
                          headers: String,
                          body: Opt<TypedArray<'js, u8>>| {
                        let http = http.clone();
                        let body = body.0.as_ref().map(copy_bytes);
                        async move {
                            match fetch(&http, &url, &method, &headers, body).await {
                                Ok(out) => Ok(out),
                                Err(message) => reject(&ctx, message),
                            }
                        }
                    },
                ),
            )?,
        )?;

        host.set(
            "sleep",
            Function::new(
                ctx.clone(),
                Async(move |ms: f64| async move {
                    let ms = if ms.is_finite() { ms.max(0.0) } else { 0.0 };
                    let duration = Duration::from_millis(ms as u64).min(CELL_TIMEOUT);
                    tokio::time::sleep(duration).await;
                }),
            )?,
        )?;

        let logs = self.logs.clone();
        host.set(
            "log",
            Function::new(ctx.clone(), move |level: String, line: String| {
                let mut logs = logs.lock().expect("log lock");
                if logs.len() >= MAX_LOG_LINES {
                    return;
                }
                let mut line = if level == "log" {
                    line
                } else {
                    format!("[{level}] {line}")
                };
                if line.len() > MAX_LOG_LINE_BYTES {
                    let mut end = MAX_LOG_LINE_BYTES;
                    while !line.is_char_boundary(end) {
                        end -= 1;
                    }
                    line.truncate(end);
                    line.push_str("…[truncated]");
                }
                logs.push(line);
            })?,
        )?;

        host.set(
            "secret",
            Function::new(ctx.clone(), |name: String| -> Option<String> {
                let valid = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                valid
                    .then(|| std::env::var(&name).ok())
                    .flatten()
                    .filter(|value| !value.is_empty())
            })?,
        )?;

        let files = self.files.clone();
        host.set(
            "fileRead",
            Function::new(
                ctx.clone(),
                Async(move |ctx: Ctx<'js>, path: String| {
                    let files = files.clone();
                    async move {
                        match files.read(&path).await {
                            Ok(bytes) => Ok(Bytes(bytes)),
                            Err(error) => reject(&ctx, format!("files.read({path:?}): {error:#}")),
                        }
                    }
                }),
            )?,
        )?;

        let files = self.files.clone();
        host.set(
            "fileWrite",
            Function::new(
                ctx.clone(),
                Async(
                    move |ctx: Ctx<'js>, path: String, data: TypedArray<'js, u8>| {
                        let files = files.clone();
                        let data = copy_bytes(&data);
                        async move {
                            let len = data.len();
                            match files.write(&path, data).await {
                                Ok(()) => Ok(len as f64),
                                Err(error) => {
                                    reject(&ctx, format!("files.write({path:?}): {error:#}"))
                                }
                            }
                        }
                    },
                ),
            )?,
        )?;

        host.set(
            "utf8Decode",
            Function::new(ctx.clone(), |data: TypedArray<'js, u8>| {
                String::from_utf8_lossy(&copy_bytes(&data)).into_owned()
            })?,
        )?;
        host.set(
            "utf8Encode",
            Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
                TypedArray::<u8>::new(ctx, text.into_bytes())
            })?,
        )?;

        Ok(host)
    }
}

async fn fetch(
    http: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &str,
    body: Option<Vec<u8>>,
) -> std::result::Result<FetchOut, String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("unsupported URL scheme {:?}", parsed.scheme()));
    }
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| format!("invalid HTTP method {method:?}"))?;
    let headers: BTreeMap<String, String> =
        serde_json::from_str(headers).map_err(|error| format!("invalid headers: {error}"))?;
    let mut request = http.request(method, parsed);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    // Drop the URL: query-string keys would otherwise land in error text.
    let transport = |error: reqwest::Error| {
        let error = error.without_url();
        let mut message = format!("request failed: {error}");
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            message.push_str(&format!(": {cause}"));
            source = cause.source();
        }
        message
    };
    let mut response = request.send().await.map_err(transport)?;
    let status = response.status().as_u16();
    let mut out_headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in response.headers() {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        out_headers
            .entry(name.as_str().to_owned())
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(&value);
            })
            .or_insert(value);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if body.len() + chunk.len() > MAX_FETCH_BYTES {
            return Err(format!(
                "response body is over the {MAX_FETCH_BYTES}-byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchOut {
        status,
        headers: serde_json::to_string(&out_headers).unwrap_or_else(|_| "{}".to_owned()),
        body,
    })
}

/// The principal's scratch directory. Paths are relative and must stay inside
/// it after symlinks resolve.
#[derive(Clone, Debug)]
pub struct Files {
    root: Arc<PathBuf>,
}

impl Files {
    fn new(root: PathBuf) -> Result<Self> {
        Ok(Self {
            root: Arc::new(root.canonicalize()?),
        })
    }

    fn relative(&self, path: &str) -> Result<PathBuf> {
        let relative = Path::new(path);
        if path.is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            eyre::bail!("path must be relative, without `.` or `..` segments");
        }
        Ok(self.root.join(relative))
    }

    fn contained(&self, path: &Path) -> Result<()> {
        if path.starts_with(self.root.as_path()) {
            Ok(())
        } else {
            eyre::bail!("path resolves outside the files directory")
        }
    }

    pub async fn read(&self, path: &str) -> Result<Vec<u8>> {
        let full = tokio::fs::canonicalize(self.relative(path)?).await?;
        self.contained(&full)?;
        let metadata = tokio::fs::metadata(&full).await?;
        if !metadata.is_file() {
            eyre::bail!("not a regular file");
        }
        if metadata.len() as usize > MAX_FILE_BYTES {
            eyre::bail!("file is over the {MAX_FILE_BYTES}-byte limit");
        }
        Ok(tokio::fs::read(&full).await?)
    }

    pub async fn write(&self, path: &str, data: Vec<u8>) -> Result<()> {
        if data.len() > MAX_FILE_BYTES {
            eyre::bail!("data is over the {MAX_FILE_BYTES}-byte limit");
        }
        let full = self.relative(path)?;
        let parent = full.parent().expect("joined path has a parent");
        tokio::fs::create_dir_all(parent).await?;
        self.contained(&tokio::fs::canonicalize(parent).await?)?;
        if let Ok(metadata) = tokio::fs::symlink_metadata(&full).await
            && metadata.file_type().is_symlink()
        {
            eyre::bail!("refusing to write through a symlink");
        }
        tokio::fs::write(&full, data).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTOR: &str = r#"
import type { Ctx } from "../centaur";

/** Echo the args with who called. */
export async function echo(args: { text: string }, ctx: Ctx): Promise<{ text: string; principal: string; oauth: readonly string[] }> {
  return { text: args.text, principal: ctx.principal.id, oauth: ctx.principal.oauth };
}

/** Save text to the scratch directory. */
export async function save(args: { path: string; text: string }, ctx: Ctx): Promise<{ path: string; bytes: number }> {
  return ctx.files.write(args.path, args.text);
}

/** Always fails. */
export async function boom(args: {}, ctx: Ctx): Promise<{}> {
  throw new Error("kaboom");
}
"#;

    fn engine() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let connector = dir.path().join("connectors").join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(connector.join("connector.toml"), "description = \"Demo\"\n").unwrap();
        std::fs::write(connector.join("index.ts"), CONNECTOR).unwrap();
        let catalog = Catalog::load(&[dir.path().join("connectors")]).unwrap();
        let engine = Engine::new(Arc::new(catalog), dir.path().join("files"), None).unwrap();
        (dir, engine)
    }

    async fn exec(engine: &Engine, code: &str, tools: Option<Vec<String>>) -> ExecResponse {
        engine
            .exec(ExecRequest {
                code: code.to_owned(),
                principal: Principal {
                    id: "prn_test".to_owned(),
                    oauth: vec!["slack".to_owned()],
                    ..Principal::default()
                },
                tools,
            })
            .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_connector_calls_in_parallel_and_records_them() {
        let (_dir, engine) = engine();
        let response = exec(
            &engine,
            r#"const [a, b] = await Promise.all([tools.demo.echo({ text: "a" }), tools.demo.echo({ text: "b" })]);
               console.log("done", a.text);
               return { a, b: b.text };"#,
            None,
        )
        .await;
        assert!(response.ok, "{response:?}");
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({ "a": { "text": "a", "principal": "prn_test", "oauth": ["slack"] }, "b": "b" })
        );
        assert_eq!(response.logs, vec!["done a"]);
        assert_eq!(response.calls.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_errors_name_the_tool() {
        let (_dir, engine) = engine();
        let response = exec(&engine, "return await tools.demo.boom({});", None).await;
        let error = response.error.unwrap();
        assert_eq!(error.message, "kaboom");
        assert_eq!(error.tool.as_deref(), Some("demo.boom"));
        assert_eq!(response.calls[0]["ok"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hides_host_bindings_and_filters_connectors() {
        let (_dir, engine) = engine();
        let response = exec(
            &engine,
            "return [typeof __host, typeof __spec, typeof fetch, Object.keys(tools)];",
            Some(vec!["other".to_owned()]),
        )
        .await;
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!(["undefined", "undefined", "undefined", []])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn files_stay_inside_the_scratch_directory() {
        let (dir, engine) = engine();
        let response = exec(
            &engine,
            r#"await tools.demo.save({ path: "a/b.txt", text: "hi" });
               return await files.readText("a/b.txt");"#,
            None,
        )
        .await;
        assert_eq!(response.result.unwrap(), "hi");

        for path in ["../x", "/etc/passwd", "a/../../x", ""] {
            let response = exec(
                &engine,
                &format!("return await files.read({path:?});"),
                None,
            )
            .await;
            assert!(!response.ok, "{path} should be rejected");
        }

        std::os::unix::fs::symlink("/etc", dir.path().join("files").join("etc")).unwrap();
        let response = exec(
            &engine,
            r#"return await files.readText("etc/hostname");"#,
            None,
        )
        .await;
        assert!(
            response
                .error
                .unwrap()
                .message
                .contains("outside the files directory"),
            "symlinks out of the scratch directory are refused"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_syntax_errors_and_non_json_results() {
        let (_dir, engine) = engine();
        assert!(!exec(&engine, "return {", None).await.ok);
        let response = exec(&engine, "return undefined;", None).await;
        assert_eq!(response.result.unwrap(), serde_json::Value::Null);
    }
}
