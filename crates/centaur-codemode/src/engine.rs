//! Runs cells and connector calls.
//!
//! Security invariants (see the README):
//!
//! 1. Rust is the only trust boundary. The call record, policy checks and
//!    limits live here; nothing a JS realm reports is trusted.
//! 2. Every host function a realm receives is safe for code in that realm to
//!    call with any arguments. Cells get `invoke`, `log`, `sleep` and UTF-8
//!    helpers; they get no `fetch` and no module loader. Connector realms can
//!    import only the catalog's shared `_lib` modules.
//! 3. Cells and connectors never share a realm. Every tool call runs in a
//!    fresh QuickJS runtime; arguments and results cross as JSON text.
//! 4. A connector's `fetch` is bound to its call, so every upstream request
//!    is recorded against the call that made it, at the point of I/O. So are
//!    its file operations, when the runner has the files capability; cells
//!    never get file access.
//! 5. `invoke` checks the tool against the cell's allowlist.
//! 6. Time, memory, call count and call concurrency are bounded per cell,
//!    including the work connectors do.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
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

use crate::{
    catalog::{Catalog, lib_name},
    files::{FileInfo, Files, MAX_FILE_BYTES},
};

const COMMON_PRELUDE: &str = include_str!("js/common.js");
const CELL_PRELUDE: &str = include_str!("js/cell.js");
const CONNECTOR_PRELUDE: &str = include_str!("js/connector.js");

/// Hard-coded limits for the spike.
pub const CELL_TIMEOUT: Duration = Duration::from_secs(120);
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest a connector may let one request run (`timeout_ms`), for large
/// downloads and uploads. A request that needs longer than its call or cell
/// has left pushes both deadlines back.
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Time after a long request for the connector to finish its call.
const REQUEST_GRACE: Duration = Duration::from_secs(10);
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
    /// Files the call read or wrote through the files capability.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileRecord>,
}

/// One file operation made by a connector call.
#[derive(Clone, Debug, Serialize)]
pub struct FileRecord {
    /// `read`, `write`, `download` or `upload` (sent as a request body).
    pub op: &'static str,
    /// The path as the connector gave it.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    files: Option<Files>,
}

impl Engine {
    /// `context_file` is the sandbox context file (`~/.centaur/sandbox.json`),
    /// read once per cell and given to cells as `context` and to connectors
    /// as `ctx.context`. `files_dir` turns on the files capability: connectors
    /// get `ctx.files`, rooted there.
    pub fn new(
        catalog: Arc<Catalog>,
        ca_file: Option<&Path>,
        context_file: Option<PathBuf>,
        files_dir: Option<&Path>,
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
            files: files_dir.map(Files::new).transpose()?,
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
            files: self.files.clone(),
            logs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            deadline: Deadline::new(started + CELL_TIMEOUT),
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
            deadline: cell.deadline.clone(),
            libs: None,
        };
        let expired = async {
            cell.deadline.passed().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        let outcome = tokio::select! {
            outcome = run_realm(&cell, realm) => outcome,
            () = expired => Err(ExecError::new(format!(
                "cell timed out after {}s",
                started.elapsed().as_secs()
            ))),
        }
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

/// A deadline that long requests may push back, shared by the timers and the
/// interrupt handler that enforce it.
#[derive(Clone)]
struct Deadline {
    base: Instant,
    /// Nanoseconds after `base`.
    at: Arc<AtomicU64>,
}

impl Deadline {
    fn new(at: Instant) -> Self {
        let base = Instant::now();
        Self {
            base,
            at: Arc::new(AtomicU64::new(
                at.saturating_duration_since(base).as_nanos() as u64,
            )),
        }
    }

    fn get(&self) -> Instant {
        self.base + Duration::from_nanos(self.at.load(Ordering::Relaxed))
    }

    /// Move the deadline to `at` if that is later.
    fn extend(&self, at: Instant) {
        let nanos = at.saturating_duration_since(self.base).as_nanos() as u64;
        self.at.fetch_max(nanos, Ordering::Relaxed);
    }

    /// Resolves once the deadline, as it stands then, has passed.
    async fn passed(&self) {
        loop {
            let at = self.get();
            if Instant::now() >= at {
                return;
            }
            tokio::time::sleep_until(at.into()).await;
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
    files: Option<Files>,
    logs: Mutex<Vec<String>>,
    calls: Mutex<Vec<Call>>,
    deadline: Deadline,
    permits: Arc<Semaphore>,
}

struct Call {
    tool: String,
    started: Instant,
    ended: Option<(Instant, Result<(), String>)>,
    requests: Vec<RequestRecord>,
    files: Vec<FileRecord>,
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
                    files: call.files.clone(),
                }
            })
            .collect()
    }

    fn record_request(
        &self,
        call: usize,
        method: &str,
        url: &str,
        started: Instant,
        status: std::result::Result<u16, &String>,
    ) {
        let request = RequestRecord {
            method: method.to_ascii_uppercase(),
            url: redact_url(url),
            status: status.ok(),
            error: status.err().cloned(),
            ms: started.elapsed().as_millis() as u64,
        };
        if let Some(call) = self.calls.lock().expect("calls lock").get_mut(call) {
            call.requests.push(request);
        }
    }

    fn record_file(
        &self,
        call: usize,
        op: &'static str,
        path: &str,
        result: std::result::Result<&FileInfo, &eyre::Report>,
    ) {
        let record = FileRecord {
            op,
            path: truncate(path.to_owned(), 1024),
            bytes: result.as_ref().ok().map(|info| info.bytes),
            error: result.err().map(|error| format!("{error:#}")),
        };
        if let Some(call) = self.calls.lock().expect("calls lock").get_mut(call) {
            call.files.push(record);
        }
    }

    /// Join body parts, reading file parts through the files capability.
    async fn request_body(
        &self,
        call: usize,
        parts: Option<Vec<BodyPart>>,
    ) -> std::result::Result<Option<Vec<u8>>, String> {
        let Some(parts) = parts else {
            return Ok(None);
        };
        let mut body = Vec::new();
        for part in parts {
            match part {
                BodyPart::Bytes(bytes) => body.extend_from_slice(&bytes),
                BodyPart::File(path) => {
                    let files = self.files.as_ref().ok_or_else(|| {
                        "a { file } body part needs the files capability (--files-dir)".to_owned()
                    })?;
                    let result = files.read(&path).await;
                    self.record_file(call, "upload", &path, result.as_ref().map(|(info, _)| info));
                    let (_, data) =
                        result.map_err(|error| format!("files.read({path:?}): {error:#}"))?;
                    body.extend_from_slice(&data);
                }
            }
            if body.len() > MAX_FILE_BYTES {
                return Err(format!(
                    "request body is over the {MAX_FILE_BYTES}-byte limit"
                ));
            }
        }
        Ok(Some(body))
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
                files: Vec::new(),
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
        let started = Instant::now();
        let deadline = Deadline::new(self.deadline.get().min(started + TOOL_TIMEOUT));
        let realm = Realm {
            host: HostKind::Connector {
                call: id,
                label: &tool,
                deadline: deadline.clone(),
            },
            prelude: connector_prelude(),
            globals: vec![("__env", environment().to_owned())],
            entry: Entry::Function {
                bytecode: &connector.bytecode,
                function,
                args: &args,
            },
            deadline: deadline.clone(),
            libs: Some(self.catalog.libs.clone()),
        };
        tokio::select! {
            result = run_realm(&self, realm) => result,
            () = deadline.passed() => Err(ExecError::new(format!(
                "{tool} timed out after {}s",
                started.elapsed().as_secs()
            ))),
        }
    }

    /// Give a request of up to `timeout_ms` (bounded by `MAX_REQUEST_TIMEOUT`)
    /// room to finish by pushing back the call's and the cell's deadlines.
    /// Returns the timeout to set on the request, or `None` for the default.
    fn request_timeout(&self, call_deadline: &Deadline, timeout_ms: f64) -> Option<Duration> {
        if !timeout_ms.is_finite() || timeout_ms <= 0.0 {
            return None;
        }
        let timeout = Duration::from_millis(timeout_ms as u64).min(MAX_REQUEST_TIMEOUT);
        let until = Instant::now() + timeout + REQUEST_GRACE;
        call_deadline.extend(until);
        self.deadline.extend(until);
        Some(timeout)
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
    deadline: Deadline,
    /// Shared modules the realm may import; `None` means no module loader.
    libs: Option<Libs>,
}

type Libs = Arc<BTreeMap<String, Arc<[u8]>>>;

/// Resolves `../_lib/<name>` (from a connector) and `./<name>` (from a shared
/// module) to `lib:<name>`, and nothing else.
struct LibResolver(Libs);

impl rquickjs::loader::Resolver for LibResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        _base: &str,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        match lib_name(name) {
            Some(lib) if self.0.contains_key(lib) => Ok(format!("lib:{lib}")),
            _ => Err(rquickjs::Error::new_resolving(_base, name)),
        }
    }
}

struct LibLoader(Libs);

impl rquickjs::loader::Loader for LibLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, rquickjs::module::Declared>> {
        let bytecode = name
            .strip_prefix("lib:")
            .and_then(|lib| self.0.get(lib))
            .ok_or_else(|| rquickjs::Error::new_loading(name))?;
        // SAFETY: written by Catalog::load in this process, never read from input.
        unsafe { Module::load(ctx.clone(), bytecode) }
    }
}

enum HostKind<'a> {
    Cell,
    Connector {
        call: usize,
        label: &'a str,
        deadline: Deadline,
    },
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
    let deadline = realm.deadline.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline.get())))
        .await;
    // Without a loader, code in the realm cannot import modules at all.
    if let Some(libs) = realm.libs.clone() {
        runtime
            .set_loader(LibResolver(libs.clone()), LibLoader(libs))
            .await;
    }
    let context = AsyncContext::full(&runtime).await.map_err(internal)?;
    context
        .async_with(async |ctx| {
            let caught = |error| caught(&ctx, error);
            let host = match realm.host {
                HostKind::Cell => cell_host(&ctx, cell.clone()),
                HostKind::Connector {
                    call,
                    label,
                    ref deadline,
                } => connector_host(&ctx, cell.clone(), call, label, deadline.clone()),
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
    host.set(
        "base64Encode",
        Function::new(ctx.clone(), |data: TypedArray<'js, u8>, url: Opt<bool>| {
            base64_encode(&copy_bytes(&data), url.0.unwrap_or(false))
        })?,
    )?;
    host.set(
        "base64Decode",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
            let bytes =
                base64_decode(&text).map_err(|message| Exception::throw_type(&ctx, &message))?;
            TypedArray::<u8>::new(ctx, bytes)
        })?,
    )?;
    host.set(
        "gzip",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, data: TypedArray<'js, u8>| {
            let bytes = gzip(&copy_bytes(&data))
                .map_err(|message| Exception::throw_message(&ctx, &message))?;
            TypedArray::<u8>::new(ctx, bytes)
        })?,
    )?;
    host.set(
        "gunzip",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, data: TypedArray<'js, u8>| {
            let bytes = gunzip(&copy_bytes(&data))
                .map_err(|message| Exception::throw_message(&ctx, &message))?;
            TypedArray::<u8>::new(ctx, bytes)
        })?,
    )?;
    Ok(host)
}

fn base64_encode(data: &[u8], url: bool) -> String {
    use base64::Engine as _;
    if url {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    } else {
        base64::engine::general_purpose::STANDARD.encode(data)
    }
}

/// Standard or URL-safe alphabet, padding optional, whitespace ignored.
fn base64_decode(text: &str) -> std::result::Result<Vec<u8>, String> {
    use base64::Engine as _;
    let normalized: String = text
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '=')
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(normalized)
        .map_err(|error| format!("invalid base64: {error}"))
}

fn gzip(data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .map_err(|error| format!("gzip failed: {error}"))
}

/// Decompresses gzip (all members) or zlib data, bounded by `MAX_FETCH_BYTES`
/// so a small bomb cannot exhaust host memory.
fn gunzip(data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    use std::io::Read as _;
    let reader: Box<dyn std::io::Read + '_> = if data.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::MultiGzDecoder::new(data))
    } else {
        Box::new(flate2::read::ZlibDecoder::new(data))
    };
    let mut out = Vec::new();
    reader
        .take(MAX_FETCH_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|error| format!("gunzip failed: {error}"))?;
    if out.len() > MAX_FETCH_BYTES {
        return Err(format!("gunzip output exceeds {MAX_FETCH_BYTES} bytes"));
    }
    Ok(out)
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
/// call, and the files functions when the runner has the files capability.
fn connector_host<'js>(
    ctx: &Ctx<'js>,
    cell: Arc<Cell>,
    call: usize,
    label: &str,
    deadline: Deadline,
) -> rquickjs::Result<Object<'js>> {
    let host = common_host(ctx, cell.clone(), format!("[{label}] "))?;
    let fetch_cell = cell.clone();
    let fetch_deadline = deadline.clone();
    host.set(
        "fetch",
        Function::new(
            ctx.clone(),
            Async(
                move |ctx: Ctx<'js>,
                      url: String,
                      method: String,
                      headers: String,
                      timeout_ms: f64,
                      body: Opt<Value<'js>>| {
                    let cell = fetch_cell.clone();
                    let timeout = cell.request_timeout(&fetch_deadline, timeout_ms);
                    let parts = body_parts(body.0);
                    async move {
                        let parts =
                            parts.map_err(|message| Exception::throw_type(&ctx, &message))?;
                        let body = cell
                            .request_body(call, parts)
                            .await
                            .map_err(|message| Exception::throw_message(&ctx, &message))?;
                        let started = Instant::now();
                        let result =
                            match send(&cell.http, &url, &method, &headers, timeout, body).await {
                                Ok(response) => collect(response).await,
                                Err(error) => Err(error),
                            };
                        cell.record_request(
                            call,
                            &method,
                            &url,
                            started,
                            result.as_ref().map(|out| out.status),
                        );
                        result.map_err(|message| Exception::throw_message(&ctx, &message))
                    }
                },
            ),
        )?,
    )?;
    let Some(files) = cell.files.clone() else {
        return Ok(host);
    };
    let read_cell = cell.clone();
    let read_files = files.clone();
    host.set(
        "filesRead",
        Function::new(
            ctx.clone(),
            Async(move |ctx: Ctx<'js>, path: String| {
                let (cell, files) = (read_cell.clone(), read_files.clone());
                async move {
                    let result = files.read(&path).await;
                    cell.record_file(call, "read", &path, result.as_ref().map(|(info, _)| info));
                    let (info, data) = result.map_err(|error| {
                        Exception::throw_message(&ctx, &format!("files.read({path:?}): {error:#}"))
                    })?;
                    let object = Object::new(ctx.clone())?;
                    object.set("path", info.path.display().to_string())?;
                    object.set("data", TypedArray::<u8>::new(ctx.clone(), data)?)?;
                    Ok::<_, rquickjs::Error>(object)
                }
            }),
        )?,
    )?;
    let write_cell = cell.clone();
    let write_files = files.clone();
    host.set(
        "filesWrite",
        Function::new(
            ctx.clone(),
            Async(
                move |ctx: Ctx<'js>, path: String, data: TypedArray<'js, u8>| {
                    let (cell, files) = (write_cell.clone(), write_files.clone());
                    let data = copy_bytes(&data);
                    async move {
                        let result = files.write(&path, &data).await;
                        cell.record_file(call, "write", &path, result.as_ref());
                        let info = result.map_err(|error| {
                            Exception::throw_message(
                                &ctx,
                                &format!("files.write({path:?}): {error:#}"),
                            )
                        })?;
                        file_info(&ctx, &info)
                    }
                },
            ),
        )?,
    )?;
    let stat_files = files.clone();
    host.set(
        "filesStat",
        Function::new(
            ctx.clone(),
            Async(move |ctx: Ctx<'js>, path: String| {
                let files = stat_files.clone();
                async move {
                    match files.stat(&path).await {
                        Ok(Some(info)) => Ok(file_info(&ctx, &info)?.into_value()),
                        Ok(None) => Ok(Value::new_null(ctx.clone())),
                        Err(error) => Err(Exception::throw_message(
                            &ctx,
                            &format!("files.stat({path:?}): {error:#}"),
                        )),
                    }
                }
            }),
        )?,
    )?;
    let download_cell = cell.clone();
    host.set(
        "filesDownload",
        Function::new(
            ctx.clone(),
            Async(
                move |ctx: Ctx<'js>,
                      url: String,
                      path: String,
                      method: String,
                      headers: String,
                      timeout_ms: f64,
                      body: Opt<Value<'js>>| {
                    let (cell, files) = (download_cell.clone(), files.clone());
                    let timeout = cell.request_timeout(&deadline, timeout_ms);
                    let parts = body_parts(body.0);
                    async move {
                        let parts =
                            parts.map_err(|message| Exception::throw_type(&ctx, &message))?;
                        let body = cell
                            .request_body(call, parts)
                            .await
                            .map_err(|message| Exception::throw_message(&ctx, &message))?;
                        let started = Instant::now();
                        let result =
                            match send(&cell.http, &url, &method, &headers, timeout, body).await {
                                Ok(response) => download(&files, &path, response).await,
                                Err(error) => Err(error),
                            };
                        cell.record_request(
                            call,
                            &method,
                            &url,
                            started,
                            result.as_ref().map(|out| out.status),
                        );
                        if let Ok(out) = &result
                            && let Some(info) = &out.file
                        {
                            cell.record_file(call, "download", &path, Ok(info));
                        }
                        let out =
                            result.map_err(|message| Exception::throw_message(&ctx, &message))?;
                        let object = Object::new(ctx.clone())?;
                        object.set("status", out.status)?;
                        object.set("headers", out.headers)?;
                        if let Some(info) = &out.file {
                            object.set("path", info.path.display().to_string())?;
                            object.set("bytes", info.bytes as f64)?;
                        }
                        if let Some(text) = out.error_body {
                            object.set("body", text)?;
                        }
                        Ok::<_, rquickjs::Error>(object)
                    }
                },
            ),
        )?,
    )?;
    Ok(host)
}

fn file_info<'js>(ctx: &Ctx<'js>, info: &FileInfo) -> rquickjs::Result<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("path", info.path.display().to_string())?;
    object.set("bytes", info.bytes as f64)?;
    Ok(object)
}

/// One piece of a request body: bytes, or a file under the files root.
enum BodyPart {
    Bytes(Vec<u8>),
    File(String),
}

/// A request body from the connector prelude: absent, one `Uint8Array`, or
/// an array of `Uint8Array` and `{ file }` parts, sent in order.
fn body_parts<'js>(body: Option<Value<'js>>) -> std::result::Result<Option<Vec<BodyPart>>, String> {
    let Some(body) = body.filter(|body| !body.is_undefined() && !body.is_null()) else {
        return Ok(None);
    };
    let part = |value: Value<'js>| -> std::result::Result<BodyPart, String> {
        if let Ok(array) = TypedArray::<u8>::from_value(value.clone()) {
            return Ok(BodyPart::Bytes(copy_bytes(&array)));
        }
        let path = value
            .as_object()
            .and_then(|object| object.get::<_, Option<String>>("file").ok().flatten());
        path.map(BodyPart::File)
            .ok_or_else(|| "body parts must be strings, bytes or { file: path }".to_owned())
    };
    if let Some(array) = body.as_array() {
        let mut parts = Vec::with_capacity(array.len());
        for value in array.iter::<Value>() {
            parts.push(part(value.map_err(|error| error.to_string())?)?);
        }
        return Ok(Some(parts));
    }
    Ok(Some(vec![part(body)?]))
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

/// Build and send one request; the caller reads the body.
async fn send(
    http: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &str,
    timeout: Option<Duration>,
    body: Option<Vec<u8>>,
) -> std::result::Result<reqwest::Response, String> {
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
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    request.send().await.map_err(transport)
}

/// Transport errors without the URL: query-string keys would otherwise land
/// in error text.
fn transport(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut message = format!("request failed: {error}");
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    message
}

fn response_headers(response: &reqwest::Response) -> String {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in response.headers() {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        out.entry(name.as_str().to_owned())
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(&value);
            })
            .or_insert(value);
    }
    serde_json::to_string(&out).unwrap_or_else(|_| "{}".to_owned())
}

async fn collect(mut response: reqwest::Response) -> std::result::Result<FetchOut, String> {
    let status = response.status().as_u16();
    let headers = response_headers(&response);
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if body.len() + chunk.len() > MAX_FETCH_BYTES {
            return Err(format!(
                "response body is over the {MAX_FETCH_BYTES}-byte limit; use ctx.files.download for large files"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchOut {
        status,
        headers,
        body,
    })
}

struct DownloadOut {
    status: u16,
    headers: String,
    /// The saved file, for a 2xx response.
    file: Option<FileInfo>,
    /// The start of the body of a non-2xx response, which is not saved.
    error_body: Option<String>,
}

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Stream a 2xx response body to `path`; keep the start of any other body
/// for the connector's error message.
async fn download(
    files: &Files,
    path: &str,
    mut response: reqwest::Response,
) -> std::result::Result<DownloadOut, String> {
    let status = response.status().as_u16();
    let headers = response_headers(&response);
    if !response.status().is_success() {
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_ERROR_BODY_BYTES {
                body.truncate(MAX_ERROR_BODY_BYTES);
                break;
            }
        }
        return Ok(DownloadOut {
            status,
            headers,
            file: None,
            error_body: Some(String::from_utf8_lossy(&body).into_owned()),
        });
    }
    let failed = |error: eyre::Report| format!("files.download({path:?}): {error:#}");
    let mut writer = files.create(path).await.map_err(failed)?;
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        writer.write(&chunk).await.map_err(failed)?;
    }
    let info = writer.finish().await.map_err(failed)?;
    Ok(DownloadOut {
        status,
        headers,
        file: Some(info),
        error_body: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTOR: &str = r#"
import type { Ctx } from "../centaur";
import { loud } from "../_lib/text";

/** Echo the args with the calling context's thread key. */
export async function echo(args: { text: string }, ctx: Ctx): Promise<{ text: string; thread_key: string | null }> {
  return { text: args.text, thread_key: ctx.context?.thread_key ?? null };
}

/** Uses a shared module. */
export async function shout(args: { text: string }, ctx: Ctx): Promise<{ text: string }> {
  return { text: loud(args.text) };
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

/** Round-trips text through base64 and gzip. */
export async function codec(args: { text: string }, ctx: Ctx): Promise<{ b64: string; url: string; back: string; btoa: string; zipped: string }> {
  const zipped = ctx.gzip(args.text);
  return {
    b64: ctx.base64.encode(args.text),
    url: ctx.base64.encode(new Uint8Array([251, 255]), { url: true }),
    back: ctx.base64.decodeText(ctx.base64.encode(args.text, { url: true })),
    btoa: atob(btoa("latin")),
    zipped: new TextDecoder().decode(ctx.gunzip(ctx.base64.decode(ctx.base64.encode(zipped)))),
  };
}

/** GET with a request timeout. */
export async function slow(args: { url: string; timeout_ms: number }, ctx: Ctx): Promise<{ status: number }> {
  const res = await ctx.fetch(args.url, { timeout_ms: args.timeout_ms });
  return { status: res.status };
}

/** Exercise the files capability. */
export async function files(args: { base: string }, ctx: Ctx): Promise<unknown> {
  if (!ctx.files) return { capability: false };
  const written = await ctx.files.write("out/a.txt", "héllo");
  const text = await ctx.files.readText("out/a.txt");
  const missing = await ctx.files.stat("out/none");
  const saved = await ctx.files.download(args.base + "/blob", "dl/blob.bin");
  const failed = await ctx.files.download(args.base + "/missing", "dl/missing.bin");
  const echoed = await ctx.fetch(args.base + "/echo", { method: "POST", body: ["<", { file: "out/a.txt" }, ">"] });
  let escape = "";
  try { await ctx.files.write("../x", "no"); } catch (e) { escape = String(e); }
  return {
    capability: true,
    written: written.path.endsWith("/out/a.txt") && written.bytes === 6,
    text,
    missing,
    saved: { ok: saved.ok, bytes: saved.bytes, type: saved.headers["content-type"], found: (await ctx.files.stat("dl/blob.bin"))?.bytes },
    failed: { ok: failed.ok, status: failed.status, body: failed.body, path: failed.path ?? null, found: await ctx.files.stat("dl/missing.bin") },
    echoed: await echoed.text(),
    escape: escape.includes("files.write"),
  };
}

/** What the connector realm sees of globals a cell may have touched. */
export async function realm(args: {}, ctx: Ctx): Promise<{ marker: string; json: string }> {
  return { marker: typeof (globalThis as any).marker, json: JSON.stringify({ a: 1 }) };
}
"#;

    fn engine() -> (tempfile::TempDir, Engine) {
        engine_with_files(false)
    }

    fn engine_with_files(files: bool) -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let connector = dir.path().join("connectors").join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(connector.join("connector.toml"), "description = \"Demo\"\n").unwrap();
        std::fs::write(connector.join("index.ts"), CONNECTOR).unwrap();
        let lib = dir.path().join("connectors").join("_lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            lib.join("text.ts"),
            "export const loud = (text: string): string => text.toUpperCase() + \"!\";",
        )
        .unwrap();
        let catalog = Catalog::load(&[dir.path().join("connectors")]).unwrap();
        let context_file = dir.path().join("sandbox.json");
        std::fs::write(
            &context_file,
            r#"{"thread_key":"slack:T1:C1:1.2","sandbox":null}"#,
        )
        .unwrap();
        let engine = Engine::new(
            Arc::new(catalog),
            None,
            Some(context_file),
            files.then(|| dir.path().join("files")).as_deref(),
        )
        .unwrap();
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
        let app = axum::Router::new()
            .route("/hello", axum::routing::get(|| async { "hi there" }))
            .route(
                "/blob",
                axum::routing::get(|| async {
                    (
                        [("content-type", "application/octet-stream")],
                        vec![7u8; 20 * 1024 * 1024],
                    )
                }),
            )
            .route(
                "/missing",
                axum::routing::get(|| async {
                    (axum::http::StatusCode::NOT_FOUND, "no such blob")
                }),
            )
            .route(
                "/echo",
                axum::routing::post(|body: String| async move { body }),
            )
            .route(
                "/slow",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    "finally"
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connectors_get_base64_and_gzip() {
        let (_dir, engine) = engine();
        let response = exec(
            &engine,
            r#"return await tools.demo.codec({ text: "héllo?" });"#,
            None,
        )
        .await;
        assert!(response.ok, "{response:?}");
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({ "b64": "aMOpbGxvPw==", "url": "-_8", "back": "héllo?", "btoa": "latin", "zipped": "héllo?" })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connectors_use_the_files_capability() {
        let base = upstream().await;
        let (_dir, engine) = engine();
        let code = format!("return await tools.demo.files({{ base: {base:?} }});");
        let response = exec(&engine, &code, None).await;
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({ "capability": false })
        );

        let (dir, engine) = engine_with_files(true);
        let response = exec(&engine, &code, None).await;
        assert!(response.ok, "{response:?}");
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({
                "capability": true,
                "written": true,
                "text": "héllo",
                "missing": null,
                "saved": { "ok": true, "bytes": 20971520, "type": "application/octet-stream", "found": 20971520 },
                "failed": { "ok": false, "status": 404, "body": "no such blob", "path": null, "found": null },
                "echoed": "<héllo>",
                "escape": true,
            })
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("files/out/a.txt")).unwrap(),
            "héllo"
        );
        let ops: Vec<(&str, String, Option<u64>)> = response.calls[0]
            .files
            .iter()
            .map(|file| (file.op, file.path.clone(), file.bytes))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("write", "out/a.txt".to_owned(), Some(6)),
                ("read", "out/a.txt".to_owned(), Some(6)),
                ("download", "dl/blob.bin".to_owned(), Some(20971520)),
                ("upload", "out/a.txt".to_owned(), Some(6)),
                ("write", "../x".to_owned(), None),
            ]
        );
        assert_eq!(response.calls[0].requests.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_take_a_timeout() {
        let base = upstream().await;
        let (_dir, engine) = engine();
        let short = exec(
            &engine,
            &format!(
                "return await tools.demo.slow({{ url: {:?}, timeout_ms: 200 }});",
                format!("{base}/slow")
            ),
            None,
        )
        .await;
        assert!(!short.ok, "{short:?}");
        let message = short.error.unwrap().message;
        assert!(message.contains("timed out"), "{message}");
        let long = exec(
            &engine,
            &format!(
                "return await tools.demo.slow({{ url: {:?}, timeout_ms: 5000 }});",
                format!("{base}/slow")
            ),
            None,
        )
        .await;
        assert_eq!(long.result.unwrap(), serde_json::json!({ "status": 200 }));
    }

    #[tokio::test]
    async fn deadlines_only_move_later() {
        let start = Instant::now();
        let deadline = Deadline::new(start + Duration::from_millis(50));
        deadline.extend(start + Duration::from_millis(10));
        assert!(deadline.get() >= start + Duration::from_millis(50));
        deadline.extend(start + Duration::from_millis(150));
        deadline.passed().await;
        assert!(start.elapsed() >= Duration::from_millis(150));

        // A long request pushes back both the call's and the cell's deadlines,
        // bounded by MAX_REQUEST_TIMEOUT.
        let (_dir, engine) = engine();
        let cell = Cell {
            catalog: engine.catalog.clone(),
            allowed: Vec::new(),
            context: "null".to_owned(),
            http: engine.http.clone(),
            files: None,
            logs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            deadline: Deadline::new(Instant::now() + CELL_TIMEOUT),
            permits: Arc::new(Semaphore::new(1)),
        };
        let call = Deadline::new(Instant::now() + TOOL_TIMEOUT);
        assert_eq!(cell.request_timeout(&call, 0.0), None);
        assert_eq!(cell.request_timeout(&call, f64::NAN), None);
        assert_eq!(cell.request_timeout(&call, 1e12), Some(MAX_REQUEST_TIMEOUT));
        let floor = Instant::now() + MAX_REQUEST_TIMEOUT;
        assert!(call.get() >= floor && cell.deadline.get() >= floor);
    }

    #[test]
    fn gunzip_is_bounded() {
        let bomb = gzip(&vec![0u8; MAX_FETCH_BYTES + 1]).unwrap();
        assert!(gunzip(&bomb).unwrap_err().contains("exceeds"));
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
            files: None,
            logs: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            deadline: Deadline::new(Instant::now() + CELL_TIMEOUT),
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
    async fn connectors_import_shared_modules_and_cells_cannot() {
        let (_dir, engine) = engine();
        let response = exec(
            &engine,
            r#"const { text } = await tools.demo.shout({ text: "hi" });
               let imported;
               try { await import("../_lib/text"); imported = true; } catch { imported = false; }
               return { text, imported };"#,
            None,
        )
        .await;
        assert_eq!(
            response.result.unwrap(),
            serde_json::json!({ "text": "HI!", "imported": false })
        );
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
