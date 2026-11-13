//! Runs cells and connector calls.
//!
//! Security invariants (see the README):
//!
//! 1. Rust is the only trust boundary. The call record, policy checks and
//!    limits live here; nothing a JS realm reports is trusted.
//! 2. Every host function a realm receives is safe for code in that realm to
//!    call with any arguments. Cells get `invoke`, `log`, `sleep` and UTF-8
//!    helpers; they get no `fetch` and no module loader.
//! 3. Cells and connectors never share a realm. Every tool call runs in a
//!    fresh QuickJS runtime; arguments and results cross as JSON text.
//! 4. A connector's `fetch` is bound to its call, so every upstream request
//!    is recorded against the call that made it, at the point of I/O.
//! 5. `invoke` checks the tool against the cell's allowlist.
//! 6. Time, memory, call count and call concurrency are bounded per cell,
//!    including the work connectors do.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr};
use rquickjs::{
    ArrayBuffer, AsyncContext, AsyncRuntime, CatchResultExt, CaughtError, Ctx, Exception, Function,
    IntoJs, Module, Object, Promise, TypedArray, Value,
    prelude::{Async, Opt},
};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tokio::sync::Semaphore;

use crate::catalog::Catalog;

const COMMON_PRELUDE: &str = include_str!("js/common.js");
const CELL_PRELUDE: &str = include_str!("js/cell.js");
const CONNECTOR_PRELUDE: &str = include_str!("js/connector.js");

/// Hard-coded limits for the spike.
pub const CELL_TIMEOUT: Duration = Duration::from_secs(120);
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// Heap per realm: the cell, and each connector call while it runs.
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
pub const STACK_LIMIT: usize = 1024 * 1024;
pub const MAX_CALLS_PER_CELL: usize = 1000;
pub const MAX_CONCURRENT_CALLS: usize = 8;
pub const MAX_RESULT_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_FETCH_BYTES: usize = 10 * 1024 * 1024;
const MAX_LOG_LINES: usize = 1000;
const MAX_LOG_LINE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct ExecRequest {
    pub code: String,
    /// Connectors the cell may use; `None` means every loaded connector.
    /// Credentials are not this runtime's concern: iron-proxy injects them.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
}

/// A failed cell or tool call.
#[derive(Debug, Serialize)]
pub struct ExecError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// The tool whose failure the cell did not catch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl ExecError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            stack: None,
            tool: None,
        }
    }
}

/// One tool call, recorded by the host.
#[derive(Clone, Debug, Serialize)]
pub struct CallRecord {
    pub tool: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ms: u64,
    pub requests: Vec<RequestRecord>,
}

/// One upstream HTTP request made by a connector call. The query string is
/// dropped, since some APIs carry keys there.
#[derive(Clone, Debug, Serialize)]
pub struct RequestRecord {
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ms: u64,
}

#[derive(Debug, Serialize)]
pub struct ExecResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ExecError>,
    pub logs: Vec<String>,
    pub calls: Vec<CallRecord>,
    pub ms: u64,
}

#[derive(Clone)]
pub struct Engine {
    catalog: Arc<Catalog>,
    http: reqwest::Client,
    context_file: Option<PathBuf>,
}

impl Engine {
    /// `context_file` is the sandbox context file (`~/.centaur/sandbox.json`),
    /// read once per cell and given to cells as `context` and to connectors
    /// as `ctx.context`.
    pub fn new(
        catalog: Arc<Catalog>,
        ca_file: Option<&Path>,
        context_file: Option<PathBuf>,
    ) -> Result<Self> {
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
        Ok(Self {
            catalog,
            http: builder.build()?,
            context_file,
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The calling context for one cell. Informational: written by the
    /// platform but readable and writable by agents in a sandbox, so it may
    /// choose a route, never authorize anything. `null` when absent.
    async fn calling_context(&self) -> String {
        let Some(path) = &self.context_file else {
            return "null".to_owned();
        };
        match tokio::fs::read(path).await {
            Ok(bytes) => match serde_json::from_slice::<Json>(&bytes) {
                Ok(context @ Json::Object(_)) => context.to_string(),
                _ => {
                    tracing::warn!(path = %path.display(), "sandbox context file is not a JSON object");
                    "null".to_owned()
                }
            },
            Err(_) => "null".to_owned(),
        }
    }

    pub async fn exec(&self, request: ExecRequest) -> ExecResponse {
        let started = Instant::now();
        let allowed: Vec<String> = self
            .catalog
            .visible(request.tools.as_deref())
            .map(|connector| connector.name.clone())
            .collect();
        let connectors: BTreeMap<&str, Vec<&str>> = self
            .catalog
            .visible(request.tools.as_deref())
            .map(|connector| {
                let functions = connector.functions.iter().map(|f| f.name.as_str());
                (connector.name.as_str(), functions.collect())
            })
            .collect();
        let cell = Arc::new(Cell {
            catalog: self.catalog.clone(),
            allowed,
            context: self.calling_context().await,
            http: self.http.clone(),
            logs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            deadline: started + CELL_TIMEOUT,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS)),
        });
        // No newline after the opening brace, so error line numbers match the cell.
        let code = format!("(async () => {{{}\n}})()", request.code);
        let realm = Realm {
            host: HostKind::Cell,
            prelude: cell_prelude(),
            globals: vec![(
                "__connectors",
                serde_json::to_string(&connectors).expect("connector list serializes"),
            )],
            entry: Entry::Script(&code),
            deadline: cell.deadline,
        };
        let outcome = tokio::time::timeout(
            CELL_TIMEOUT + Duration::from_secs(1),
            run_realm(&cell, realm),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ExecError::new(format!(
                "cell timed out after {}s",
                CELL_TIMEOUT.as_secs()
            )))
        })
        .and_then(|text| {
            serde_json::from_str(&text)
                .map_err(|error| ExecError::new(format!("result is not JSON: {error}")))
        });
        let (result, error) = match outcome {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error)),
        };
        ExecResponse {
            ok: error.is_none(),
            result,
            error,
            logs: std::mem::take(&mut *cell.logs.lock().expect("log lock")),
            calls: cell.finished_calls(),
            ms: started.elapsed().as_millis() as u64,
        }
    }
}

/// Per-cell state shared by the host functions of the cell and its calls.
struct Cell {
    catalog: Arc<Catalog>,
    allowed: Vec<String>,
    /// The sandbox context file as JSON text, or `null`.
    context: String,
    http: reqwest::Client,
    logs: Mutex<Vec<String>>,
    calls: Mutex<Vec<Call>>,
    deadline: Instant,
    permits: Arc<Semaphore>,
}

struct Call {
    tool: String,
    started: Instant,
    ended: Option<(Instant, Result<(), String>)>,
    requests: Vec<RequestRecord>,
}

impl Cell {
    fn log(&self, line: String) {
        let mut logs = self.logs.lock().expect("log lock");
        if logs.len() < MAX_LOG_LINES {
            logs.push(truncate(line, MAX_LOG_LINE_BYTES));
        }
    }

    fn finished_calls(&self) -> Vec<CallRecord> {
        let now = Instant::now();
        let calls = self.calls.lock().expect("calls lock");
        calls
            .iter()
            .map(|call| {
                let (ended, result) = call.ended.clone().unwrap_or_else(|| {
                    let unfinished = "unfinished: the cell ended before this call settled";
                    (now, Err(unfinished.to_owned()))
                });
                CallRecord {
                    tool: call.tool.clone(),
                    ok: result.is_ok(),
                    error: result.err(),
                    ms: ended.duration_since(call.started).as_millis() as u64,
                    requests: call.requests.clone(),
                }
            })
            .collect()
    }

    fn record_request(&self, call: usize, request: RequestRecord) {
        if let Some(call) = self.calls.lock().expect("calls lock").get_mut(call) {
            call.requests.push(request);
        }
    }

    /// The `invoke` host function: record, check policy, run, record.
    async fn invoke(self: Arc<Self>, tool: String, args: String) -> Result<String, ExecError> {
        let id = {
            let mut calls = self.calls.lock().expect("calls lock");
            if calls.len() >= MAX_CALLS_PER_CELL {
                return Err(ExecError::new(format!(
                    "a cell may make at most {MAX_CALLS_PER_CELL} tool calls"
                )));
            }
            calls.push(Call {
                tool: tool.clone(),
                started: Instant::now(),
                ended: None,
                requests: Vec::new(),
            });
            calls.len() - 1
        };
        // Each call runs on its own task, so connector work from parallel
        // calls runs on separate threads instead of the cell's.
        let result = tokio::spawn(self.clone().call(tool.clone(), args, id))
            .await
            .unwrap_or_else(|error| Err(ExecError::new(format!("{tool} failed: {error}"))));
        if let Some(call) = self.calls.lock().expect("calls lock").get_mut(id) {
            let status = result.as_ref().map(|_| ()).map_err(|e| e.message.clone());
            call.ended = Some((Instant::now(), status));
        }
        result
    }

    /// Run one connector function in its own realm.
    async fn call(
        self: Arc<Self>,
        tool: String,
        args: String,
        id: usize,
    ) -> Result<String, ExecError> {
        let (name, function) = tool
            .split_once('.')
            .ok_or_else(|| ExecError::new(format!("unknown tool {tool:?}")))?;
        let connector = self
            .allowed
            .iter()
            .any(|allowed| allowed == name)
            .then(|| self.catalog.connectors.get(name))
            .flatten()
            .filter(|connector| connector.functions.iter().any(|f| f.name == function))
            .ok_or_else(|| ExecError::new(format!("unknown or unavailable tool {tool:?}")))?;
        let _permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ExecError::new("cell is shutting down"))?;
        let deadline = self.deadline.min(Instant::now() + TOOL_TIMEOUT);
        let budget = deadline.saturating_duration_since(Instant::now());
        let realm = Realm {
            host: HostKind::Connector {
                call: id,
                label: &tool,
            },
            prelude: connector_prelude(),
            globals: vec![("__env", environment().to_owned())],
            entry: Entry::Function {
                bytecode: &connector.bytecode,
                function,
                args: &args,
            },
            deadline,
        };
        tokio::time::timeout(budget, run_realm(&self, realm))
            .await
            .unwrap_or_else(|_| {
                Err(ExecError::new(format!(
                    "{tool} timed out after {}s",
                    budget.as_secs()
                )))
            })
    }
}

/// One fresh QuickJS runtime: a cell, or one connector call.
struct Realm<'a> {
    host: HostKind<'a>,
    /// Prelude module bytecode; a connector prelude's default export is `ctx`.
    prelude: &'static [u8],
    /// Extra JSON globals for the prelude to consume.
    globals: Vec<(&'static str, String)>,
    entry: Entry<'a>,
    deadline: Instant,
}

enum HostKind<'a> {
    Cell,
    Connector { call: usize, label: &'a str },
}

enum Entry<'a> {
    /// A script evaluating to a promise (the wrapped cell).
    Script(&'a str),
    /// `function(args, ctx)` exported by a connector module.
    Function {
        bytecode: &'a [u8],
        function: &'a str,
        args: &'a str,
    },
}

/// Run a realm to completion and return its result as JSON text. Pending host
/// futures (unsettled calls, cancelled timers) drop with the runtime.
async fn run_realm(cell: &Arc<Cell>, realm: Realm<'_>) -> Result<String, ExecError> {
    let internal = |error: rquickjs::Error| ExecError::new(format!("runtime error: {error}"));
    let runtime = AsyncRuntime::new().map_err(internal)?;
    runtime.set_memory_limit(MEMORY_LIMIT).await;
    runtime.set_max_stack_size(STACK_LIMIT).await;
    let deadline = realm.deadline;
    runtime
        .set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)))
        .await;
    // No module loader is set, so code in the realm cannot import modules.
    let context = AsyncContext::full(&runtime).await.map_err(internal)?;
    context
        .async_with(async |ctx| {
            let caught = |error| caught(&ctx, error);
            let host = match realm.host {
                HostKind::Cell => cell_host(&ctx, cell.clone()),
                HostKind::Connector { call, label } => {
                    connector_host(&ctx, cell.clone(), call, label)
                }
            };
            let setup = (|| -> rquickjs::Result<_> {
                let globals = ctx.globals();
                globals.set("__host", host?)?;
                globals.set("__context", ctx.json_parse(cell.context.as_str())?)?;
                for (name, json) in &realm.globals {
                    globals.set(*name, ctx.json_parse(json.as_str())?)?;
                }
                // SAFETY: prelude and connector bytecode is written by this
                // process from the same QuickJS build, never read from input.
                unsafe { Module::load(ctx.clone(), realm.prelude)? }.eval()
            })();
            let (prelude, done) = setup.catch(&ctx).map_err(caught)?;
            done.into_future::<()>().await.catch(&ctx).map_err(caught)?;

            let promise: Promise = match realm.entry {
                Entry::Script(code) => ctx.eval(code).catch(&ctx).map_err(caught)?,
                Entry::Function {
                    bytecode,
                    function,
                    args,
                } => {
                    let module = unsafe { Module::load(ctx.clone(), bytecode) }
                        .and_then(|module| module.eval())
                        .catch(&ctx)
                        .map_err(caught)?;
                    module
                        .1
                        .into_future::<()>()
                        .await
                        .catch(&ctx)
                        .map_err(caught)?;
                    (|| {
                        let entry: Function = module.0.get(function)?;
                        let ctx_arg: Object = prelude.get("default")?;
                        entry.call((ctx.json_parse(args)?, ctx_arg))
                    })()
                    .catch(&ctx)
                    .map_err(caught)?
                }
            };
            let value: Value = promise.into_future().await.catch(&ctx).map_err(caught)?;
            let text = ctx
                .json_stringify(value)
                .catch(&ctx)
                .map_err(caught)?
                .map(|text| text.to_string())
                .transpose()
                .map_err(internal)?
                .unwrap_or_else(|| "null".to_owned());
            if text.len() > MAX_RESULT_BYTES {
                return Err(ExecError::new(format!(
                    "result is {} bytes, over the {MAX_RESULT_BYTES}-byte limit",
                    text.len()
                )));
            }
            Ok(text)
        })
        .await
}

fn caught<'js>(ctx: &Ctx<'js>, error: CaughtError<'js>) -> ExecError {
    match error {
        CaughtError::Exception(exception) => ExecError {
            message: exception
                .message()
                .unwrap_or_else(|| "uncaught exception".to_owned()),
            stack: exception.stack().filter(|stack| !stack.is_empty()),
            tool: exception.get::<_, Option<String>>("tool").ok().flatten(),
        },
        CaughtError::Value(value) => ExecError::new(
            ctx.json_stringify(value)
                .ok()
                .flatten()
                .and_then(|text| text.to_string().ok())
                .unwrap_or_else(|| "uncaught non-Error value".to_owned()),
        ),
        CaughtError::Error(error) => ExecError::new(error.to_string()),
    }
}

/// The preludes as ES modules, compiled to bytecode once per process: parsing
/// them from source on every call costs more than the rest of a call.
fn cell_prelude() -> &'static [u8] {
    static BYTECODE: OnceLock<Vec<u8>> = OnceLock::new();
    BYTECODE.get_or_init(|| {
        compile_prelude("prelude:cell", format!("{COMMON_PRELUDE}\n{CELL_PRELUDE}"))
    })
}

fn connector_prelude() -> &'static [u8] {
    static BYTECODE: OnceLock<Vec<u8>> = OnceLock::new();
    BYTECODE.get_or_init(|| {
        compile_prelude(
            "prelude:connector",
            format!("{COMMON_PRELUDE}\nexport default {CONNECTOR_PRELUDE}"),
        )
    })
}

fn compile_prelude(name: &str, source: String) -> Vec<u8> {
    let runtime = rquickjs::Runtime::new().expect("QuickJS runtime");
    let context = rquickjs::Context::full(&runtime).expect("QuickJS context");
    context.with(|ctx| {
        Module::declare(ctx, name, source)
            .and_then(|module| module.write(rquickjs::module::WriteOptions::default()))
            .expect("built-in prelude compiles")
    })
}

/// `ctx.env`: the process environment as JSON, read once. Sandboxes and
/// runners hold no real credentials, only configuration and placeholders.
fn environment() -> &'static str {
    static ENV: OnceLock<String> = OnceLock::new();
    ENV.get_or_init(|| {
        let vars: BTreeMap<String, String> = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        serde_json::to_string(&vars).expect("environment serializes")
    })
}

fn truncate(mut line: String, max: usize) -> String {
    if line.len() > max {
        let mut end = max;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push_str("…[truncated]");
    }
    line
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

fn copy_bytes(array: &TypedArray<'_, u8>) -> Vec<u8> {
    // SAFETY: the slice is copied before any JavaScript runs.
    unsafe { array.as_bytes() }
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// Host functions every realm gets: logs, sleep, UTF-8.
fn common_host<'js>(
    ctx: &Ctx<'js>,
    cell: Arc<Cell>,
    log_prefix: String,
) -> rquickjs::Result<Object<'js>> {
    let host = Object::new(ctx.clone())?;
    host.set(
        "sleep",
        Function::new(
            ctx.clone(),
            Async(move |ms: f64| async move {
                let ms = if ms.is_finite() { ms.max(0.0) } else { 0.0 };
                tokio::time::sleep(Duration::from_millis(ms as u64).min(CELL_TIMEOUT)).await;
            }),
        )?,
    )?;
    host.set(
        "log",
        Function::new(ctx.clone(), move |level: String, line: String| {
            let level = if level == "log" {
                String::new()
            } else {
                format!("[{level}] ")
            };
            cell.log(format!("{log_prefix}{level}{line}"));
        })?,
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

/// The cell's host: the common functions plus `invoke`.
fn cell_host<'js>(ctx: &Ctx<'js>, cell: Arc<Cell>) -> rquickjs::Result<Object<'js>> {
    let host = common_host(ctx, cell.clone(), String::new())?;
    host.set(
        "invoke",
        Function::new(
            ctx.clone(),
            Async(move |ctx: Ctx<'js>, tool: String, args: String| {
                let cell = cell.clone();
                async move {
                    let failure = match cell.invoke(tool.clone(), args).await {
                        Ok(json) => return Ok(json),
                        Err(failure) => failure,
                    };
                    let exception = Exception::from_message(ctx.clone(), &failure.message)?;
                    exception.as_object().set("tool", tool)?;
                    if let Some(stack) = failure.stack {
                        exception.as_object().set("connector_stack", stack)?;
                    }
                    Err(ctx.throw(exception.into_value()))
                }
            }),
        )?,
    )?;
    Ok(host)
}

/// A connector call's host: the common functions plus `fetch` bound to this
/// call.
fn connector_host<'js>(
    ctx: &Ctx<'js>,
    cell: Arc<Cell>,
    call: usize,
    label: &str,
) -> rquickjs::Result<Object<'js>> {
    let host = common_host(ctx, cell.clone(), format!("[{label}] "))?;
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
                    let cell = cell.clone();
                    let body = body.0.as_ref().map(copy_bytes);
                    async move {
                        let started = Instant::now();
                        let result = fetch(&cell.http, &url, &method, &headers, body).await;
                        cell.record_request(
                            call,
                            RequestRecord {
                                method: method.to_ascii_uppercase(),
                                url: redact_url(&url),
                                status: result.as_ref().ok().map(|out| out.status),
                                error: result.as_ref().err().cloned(),
                                ms: started.elapsed().as_millis() as u64,
                            },
                        );
                        result.map_err(|message| Exception::throw_message(&ctx, &message))
                    }
                },
            ),
        )?,
    )?;
    Ok(host)
}

fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => "<invalid url>".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTOR: &str = r#"
import type { Ctx } from "../centaur";

/** Echo the args with the calling context's thread key. */
export async function echo(args: { text: string }, ctx: Ctx): Promise<{ text: string; thread_key: string | null }> {
  return { text: args.text, thread_key: ctx.context?.thread_key ?? null };
}

/** Always fails. */
export async function boom(args: {}, ctx: Ctx): Promise<{}> {
  throw new Error("kaboom");
}

/** GET a URL and return the status. */
export async function get(args: { url: string }, ctx: Ctx): Promise<{ status: number; body: string }> {
  const res = await ctx.fetch(args.url);
  return { status: res.status, body: await res.text() };
}

/** Read an environment variable. */
export async function env(args: { name: string }, ctx: Ctx): Promise<{ value: string | null; frozen: boolean }> {
  return { value: ctx.env[args.name] ?? null, frozen: Object.isFrozen(ctx.env) };
}

/** What the connector realm sees of globals a cell may have touched. */
export async function realm(args: {}, ctx: Ctx): Promise<{ marker: string; json: string }> {
  return { marker: typeof (globalThis as any).marker, json: JSON.stringify({ a: 1 }) };
}
"#;

    fn engine() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let connector = dir.path().join("connectors").join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(connector.join("connector.toml"), "description = \"Demo\"\n").unwrap();
        std::fs::write(connector.join("index.ts"), CONNECTOR).unwrap();
        let catalog = Catalog::load(&[dir.path().join("connectors")]).unwrap();
        let context_file = dir.path().join("sandbox.json");
        std::fs::write(
            &context_file,
            r#"{"thread_key":"slack:T1:C1:1.2","sandbox":null}"#,
        )
        .unwrap();
        let engine = Engine::new(Arc::new(catalog), None, Some(context_file)).unwrap();
        (dir, engine)
    }

    async fn exec(engine: &Engine, code: &str, tools: Option<Vec<String>>) -> ExecResponse {
        engine
            .exec(ExecRequest {
                code: code.to_owned(),
                tools,
            })
            .await
    }

    /// A one-route HTTP server for fetch tests.
    async fn upstream() -> String {
        let app = axum::Router::new().route("/hello", axum::routing::get(|| async { "hi there" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
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
            serde_json::json!({ "a": { "text": "a", "thread_key": "slack:T1:C1:1.2" }, "b": "b" })
        );
        assert_eq!(response.logs, vec!["done a"]);
        assert_eq!(response.calls.len(), 2);
        assert!(
            response
                .calls
                .iter()
                .all(|call| call.ok && call.tool == "demo.echo")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn records_upstream_requests_against_their_call() {
        let (_dir, engine) = engine();
        let base = upstream().await;
        let response = exec(
            &engine,
            &format!(r#"return await tools.demo.get({{ url: "{base}/hello?key=secret" }});"#),
            None,
        )
        .await;
        assert!(response.ok, "{response:?}");
        assert_eq!(response.result.unwrap()["body"], "hi there");
        let requests = &response.calls[0].requests;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].status, Some(200));
        assert_eq!(
            requests[0].url,
            format!("{base}/hello"),
            "query string is dropped"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cells_cannot_forge_or_bypass_the_call_record() {
        let (_dir, engine) = engine();
        let response = exec(
            &engine,
            r#"
            const attempts = {};
            // No record to write to.
            attempts.calls = typeof __calls;
            globalThis.__calls = [{ tool: "demo.fake", ok: true }];
            // No module loader: connector modules cannot be imported directly.
            try { await import("connector:demo"); attempts.import = "loaded"; }
            catch (e) { attempts.import = "blocked"; }
            // Tampering with built-ins stays in the cell's realm.
            globalThis.marker = "cell";
            JSON.stringify = () => "tampered";
            Promise.prototype.then = function () { throw new Error("tampered"); };
            return attempts;
            "#,
            None,
        )
        .await;
        // The cell broke its own Promise, so its own result handling may fail;
        // what matters is the record and the connector realm.
        assert!(response.calls.is_empty(), "{:?}", response.calls);
        let _ = response.result;

        let response = exec(
            &engine,
            r#"
            globalThis.marker = "cell";
            const realm = await tools.demo.realm({});
            let imported;
            try { await import("connector:demo"); imported = true; } catch { imported = false; }
            return { realm, imported, calls: typeof __calls };
            "#,
            None,
        )
        .await;
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({
                "realm": { "marker": "undefined", "json": "{\"a\":1}" },
                "imported": false,
                "calls": "undefined",
            })
        );
        assert_eq!(response.calls.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invoke_enforces_the_allowlist_and_records_refusals() {
        let (_dir, engine) = engine();
        // The cell sees no host bindings and no tools outside its allowlist.
        let response = exec(
            &engine,
            r#"return { host: Object.getOwnPropertyNames(globalThis).filter((k) => k.startsWith("__")), tools: Object.keys(tools) };"#,
            Some(vec!["other".to_owned()]),
        )
        .await;
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({ "host": [], "tools": [] })
        );

        // `invoke` itself refuses, whatever JS calls it with.
        let cell = Arc::new(Cell {
            catalog: engine.catalog.clone(),
            allowed: vec!["other".to_owned()],
            context: "null".to_owned(),
            http: engine.http.clone(),
            logs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            deadline: Instant::now() + CELL_TIMEOUT,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_CALLS)),
        });
        for tool in ["demo.echo", "demo.nope", "nope", "other.echo"] {
            let result = cell.clone().invoke(tool.to_owned(), "{}".to_owned()).await;
            assert!(result.is_err(), "{tool} must be refused");
        }
        let calls = cell.finished_calls();
        assert_eq!(calls.len(), 4, "refusals are recorded");
        assert!(
            calls
                .iter()
                .all(|call| !call.ok && call.requests.is_empty())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connectors_read_the_environment_and_everyone_reads_the_context() {
        let (_dir, engine) = engine();
        let response = exec(
            &engine,
            r#"return {
                connector: await tools.demo.env({ name: "PATH" }),
                cell: typeof env,
                thread_key: context.thread_key,
                frozen: Object.isFrozen(context),
            };"#,
            None,
        )
        .await;
        let result = response.result.unwrap();
        assert_eq!(result["connector"]["value"], std::env::var("PATH").unwrap());
        assert_eq!(result["connector"]["frozen"], true);
        assert_eq!(result["cell"], "undefined");
        assert_eq!(
            result["thread_key"], "slack:T1:C1:1.2",
            "cells see the calling context"
        );
        assert_eq!(result["frozen"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_errors_name_the_tool() {
        let (_dir, engine) = engine();
        let response = exec(&engine, "return await tools.demo.boom({});", None).await;
        let error = response.error.unwrap();
        assert_eq!(error.message, "kaboom");
        assert_eq!(error.tool.as_deref(), Some("demo.boom"));
        assert!(!response.calls[0].ok);
        assert_eq!(response.calls[0].error.as_deref(), Some("kaboom"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_syntax_errors_and_non_json_results() {
        let (_dir, engine) = engine();
        assert!(!exec(&engine, "return {", None).await.ok);
        let response = exec(&engine, "return undefined;", None).await;
        assert_eq!(response.result.unwrap(), serde_json::Value::Null);
    }
}
