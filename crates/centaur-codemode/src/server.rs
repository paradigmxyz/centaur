//! HTTP API for api-rs. In a deployment only api pods reach this port
//! (NetworkPolicy); the API has no authentication of its own. Which principal
//! a runner serves is set by where it runs and which iron-proxy it sits
//! behind, not by anything in a request.
//!
//! Every request to a route other than `/healthz` counts as use, for idle
//! exit. `/exec` runs at most [`MAX_CELLS`] cells at a time; over the cap a
//! request gets `503` at once instead of queueing.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Semaphore, watch};

use crate::{
    catalog::{Policy, connector_brief},
    engine::{CallRecord, Engine, ExecRequest},
    search,
    telemetry::TraceParent,
};

/// Concurrent `/exec` requests per process.
pub const MAX_CELLS: usize = 32;
/// Exit after this long without a request to any route but `/healthz`.
pub const IDLE_EXIT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How often to check the connector roots for changes.
pub const RELOAD_INTERVAL: Duration = Duration::from_secs(10);
/// Query and error text in a request log line is cut to this many characters.
const MAX_LOG_TEXT_CHARS: usize = 200;
/// The per-call summary in an `/exec` log line is cut to this many characters.
const MAX_CONNECTORS_CHARS: usize = 1024;

/// What a request can observe about the process.
#[derive(Clone, Debug)]
pub struct Config {
    /// Concurrent `/exec` requests per process.
    pub max_cells: usize,
    /// Exit after this long without a request to any route but `/healthz`.
    pub idle_exit: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_cells: MAX_CELLS,
            idle_exit: IDLE_EXIT,
        }
    }
}

#[derive(Clone)]
struct AppState {
    engine: Engine,
    config: Arc<Config>,
    /// One permit per running cell.
    permits: Arc<Semaphore>,
    /// When a request to any route but `/healthz` last arrived.
    last_use: Arc<Mutex<Instant>>,
}

impl AppState {
    fn new(engine: Engine, config: Config) -> Self {
        let config = Config {
            max_cells: config.max_cells.max(1),
            ..config
        };
        Self {
            engine,
            permits: Arc::new(Semaphore::new(config.max_cells)),
            config: Arc::new(config),
            last_use: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// A request arrived, so the process is in use.
    fn used(&self) {
        *self.last_use.lock().expect("last use lock") = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_use.lock().expect("last use lock").elapsed()
    }

    /// No cell is running right now.
    fn idle(&self) -> bool {
        self.permits.available_permits() == self.config.max_cells
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(
            Router::new()
                .route("/exec", post(exec))
                .route("/search", post(search_tools))
                .route("/load", post(load))
                .route("/catalog", get(catalog))
                .route_layer(middleware::from_fn_with_state(state.clone(), mark_used)),
        )
        .with_state(state)
}

/// Serve until ctrl-c, or until nothing has used the process for the idle
/// timeout and no cell is running. Both shut down gracefully and return
/// `Ok(())`, so the process exits 0.
pub async fn serve(
    listener: tokio::net::TcpListener,
    engine: Engine,
    config: Config,
) -> std::io::Result<()> {
    let state = AppState::new(engine, config);
    let app = router(state.clone());
    let (shutdown, mut receiver) = watch::channel(false);
    let idle = tokio::spawn(watch_idle(state, shutdown));
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    let _ = result;
                }
                _ = receiver.changed() => {}
            }
        })
        .await;
    idle.abort();
    result
}

/// Ask the server to shut down once it has gone `idle_exit` without a request
/// and no cell is running.
async fn watch_idle(state: AppState, shutdown: watch::Sender<bool>) {
    let idle = state.config.idle_exit;
    loop {
        let check = idle
            .min(Duration::from_secs(60))
            .max(Duration::from_millis(20));
        tokio::time::sleep(check).await;
        if state.idle_for() >= idle && state.idle() {
            tracing::warn!(
                idle_exit_secs = idle.as_secs(),
                "no requests for the idle timeout; exiting"
            );
            let _ = shutdown.send(true);
            return;
        }
    }
}

/// Count a request to any route but `/healthz` as use, so a busy runner is
/// not retired.
async fn mark_used(State(state): State<AppState>, request: Request, next: Next) -> Response {
    state.used();
    next.run(request).await
}

/// Cut text to `max_chars` characters, never splitting a UTF-8 character.
fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// One compact entry per call for the `/exec` log line:
/// `connector.function:ok|err:<ms>ms[:<last status>]`. URLs are never logged.
fn connector_summary(calls: &[CallRecord]) -> String {
    let entries = calls
        .iter()
        .map(|call| {
            let outcome = if call.ok { "ok" } else { "err" };
            match call.requests.last().and_then(|request| request.status) {
                Some(status) => format!("{}:{outcome}:{}ms:{status}", call.tool, call.ms),
                None => format!("{}:{outcome}:{}ms", call.tool, call.ms),
            }
        })
        .collect::<Vec<_>>();
    truncate(&entries.join(","), MAX_CONNECTORS_CHARS)
}

fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "busy", "retryable": true })),
    )
        .into_response()
}

/// Liveness and readiness. Cheap, unauthenticated, and not counted as use: a
/// readiness probe must never keep an idle runner alive.
async fn healthz(State(state): State<AppState>) -> Response {
    let catalog = state.engine.catalog();
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "catalog_revision": catalog.revision,
        "connectors": catalog.connectors.len(),
    }))
    .into_response()
}

async fn exec(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<ExecRequest>,
) -> Response {
    let started = Instant::now();
    // api-rs sends the MCP tool call's trace context; the cell's spans join it.
    request.trace = headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(TraceParent::parse);
    let Ok(_permit) = state.permits.clone().try_acquire_owned() else {
        tracing::warn!(route = "/exec", "busy");
        return busy();
    };
    let response = state.engine.exec(request).await;
    // Cell code, arguments, results and logs are never logged.
    tracing::info!(
        route = "/exec",
        ms = started.elapsed().as_millis() as u64,
        ok = response.ok,
        calls = response.calls.len(),
        failed_calls = response.calls.iter().filter(|call| !call.ok).count(),
        error = response
            .error
            .as_ref()
            .map(|error| truncate(&error.message, MAX_LOG_TEXT_CHARS)),
        connectors = %connector_summary(&response.calls),
        "request"
    );
    Json(response).into_response()
}

#[derive(Deserialize)]
struct SearchRequest {
    #[serde(default)]
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    policy: Option<Policy>,
}

async fn search_tools(
    State(state): State<AppState>,
    Json(request): Json<SearchRequest>,
) -> Response {
    let catalog = state.engine.catalog();
    let limit = search::limit(request.limit);
    let results = catalog.search(&request.query, limit, request.policy.as_ref());
    tracing::info!(
        route = "/search",
        query = %truncate(&request.query, MAX_LOG_TEXT_CHARS),
        limit,
        results = results.len(),
        "request"
    );
    Json(json!({
        "results": results,
        "catalog_revision": catalog.revision,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LoadRequest {
    tool: String,
    #[serde(default)]
    policy: Option<Policy>,
}

async fn load(State(state): State<AppState>, Json(request): Json<LoadRequest>) -> Response {
    let catalog = state.engine.catalog();
    let connector = catalog.find(&request.tool, request.policy.as_ref());
    tracing::info!(route = "/load", tool = %request.tool, found = connector.is_some(), "request");
    let Some(connector) = connector else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("unknown tool {}", request.tool) })),
        )
            .into_response();
    };
    Json(json!({
        "tool": connector.name,
        "description": connector.description,
        "brief": connector_brief(connector).trim_end(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct ToolsQuery {
    tools: Option<String>,
}

fn allowlist(raw: Option<&str>) -> Option<Vec<String>> {
    raw.map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

async fn catalog(State(state): State<AppState>, Query(query): Query<ToolsQuery>) -> Response {
    let policy = allowlist(query.tools.as_deref()).map(Policy::from_allowlist);
    let catalog = state.engine.catalog();
    Json(json!({
        "connectors": catalog.visible(policy.as_ref()).collect::<Vec<_>>(),
        "brief": catalog.brief(policy.as_ref()),
        "catalog_revision": catalog.revision,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request as HttpRequest, header},
    };
    use tower::ServiceExt as _;

    const CONNECTOR: &str = r#"
import type { Ctx } from "../centaur";

/** Echo the args back. */
export async function echo(args: { text: string }, ctx: Ctx): Promise<{ text: string }> {
  return { text: args.text };
}

/** Wait, then answer. */
export async function wait(args: { ms: number }, ctx: Ctx): Promise<{ waited: number }> {
  await ctx.sleep(args.ms);
  return { waited: args.ms };
}
"#;

    fn engine() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let connector = dir.path().join("connectors").join("demo");
        std::fs::create_dir_all(&connector).unwrap();
        std::fs::write(
            connector.join("connector.toml"),
            "description = \"Demo connector\"\n",
        )
        .unwrap();
        std::fs::write(connector.join("index.ts"), CONNECTOR).unwrap();
        let catalog =
            crate::catalog::Catalog::load(&[dir.path().join("connectors")], false).unwrap();
        let engine = Engine::new(Arc::new(catalog), None, None, None).unwrap();
        (dir, engine)
    }

    fn app(engine: Engine) -> Router {
        router(AppState::new(engine, Config::default()))
    }

    fn post(path: &str, body: serde_json::Value) -> HttpRequest<Body> {
        HttpRequest::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn every_route_answers_and_honors_the_policy() {
        let (_dir, engine) = engine();
        let app = app(engine);

        let response = app
            .clone()
            .oneshot(HttpRequest::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["connectors"], 1);
        assert!(
            body["catalog_revision"]
                .as_str()
                .is_some_and(|revision| revision.len() == 16),
            "{body}"
        );

        let response = app
            .clone()
            .oneshot(post("/search", json!({ "query": "echo args back" })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["results"][0]["tool"], "demo", "{body}");
        assert_eq!(body["results"][0]["function"], "echo");
        assert_eq!(
            body["results"][0]["signature"],
            "echo(args: { text: string }): Promise<{ text: string }>"
        );
        let blocked = json!({ "query": "echo", "policy": { "blocklist": ["demo"] } });
        let body = json_body(app.clone().oneshot(post("/search", blocked)).await.unwrap()).await;
        assert_eq!(body["results"], json!([]), "{body}");

        let response = app
            .clone()
            .oneshot(HttpRequest::get("/catalog").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(post("/load", json!({ "tool": "demo" })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["tool"], "demo");
        assert_eq!(body["description"], "Demo connector");
        assert!(
            body["brief"]
                .as_str()
                .is_some_and(|brief| brief.contains("echo")),
            "{body}"
        );
        let hidden = json!({ "tool": "demo", "policy": { "allowlist": ["other"] } });
        let response = app.oneshot(post("/load", hidden)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn over_the_cell_cap_answers_503_immediately() {
        let (_dir, engine) = engine();
        let app = router(AppState::new(
            engine,
            Config {
                max_cells: 1,
                ..Config::default()
            },
        ));
        let slow = tokio::spawn(app.clone().oneshot(post(
            "/exec",
            json!({ "code": "await tools.demo.wait({ ms: 1500 }); return 1" }),
        )));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let response = app
            .oneshot(post("/exec", json!({ "code": "return 1" })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json_body(response).await,
            json!({ "error": "busy", "retryable": true })
        );
        assert_eq!(slow.await.unwrap().unwrap().status(), StatusCode::OK);
    }

    fn call(tool: &str, ok: bool, ms: u64, statuses: &[Option<u16>]) -> crate::engine::CallRecord {
        crate::engine::CallRecord {
            tool: tool.to_owned(),
            ok,
            error: (!ok).then(|| "failed".to_owned()),
            ms,
            requests: statuses
                .iter()
                .map(|status| crate::engine::RequestRecord {
                    method: "GET".to_owned(),
                    url: "https://example.invalid/path".to_owned(),
                    status: *status,
                    error: None,
                    ms: 1,
                })
                .collect(),
            files: Vec::new(),
        }
    }

    #[test]
    fn connector_summary_is_compact_and_truncated() {
        let calls = [
            call("slack.search", true, 12, &[Some(200)]),
            call("demo.boom", false, 3, &[Some(200), Some(500)]),
            call("demo.local", true, 0, &[]),
        ];
        assert_eq!(
            connector_summary(&calls),
            "slack.search:ok:12ms:200,demo.boom:err:3ms:500,demo.local:ok:0ms"
        );

        let long = call(&"😀".repeat(2000), true, 1, &[Some(200)]);
        let summary = connector_summary(&[long]);
        assert_eq!(summary.chars().count(), MAX_CONNECTORS_CHARS);
        assert!(summary.chars().all(|c| c == '😀' || c == ':'));
        assert_eq!(truncate("aé😀b", 2), "aé");
        assert_eq!(truncate("aé😀b", 3), "aé😀");
        assert_eq!(truncate("short", 200), "short");
    }

    #[tokio::test]
    async fn idle_exit_runs_after_non_healthz_use() {
        let (_dir, engine) = engine();
        let state = AppState::new(
            engine.clone(),
            Config {
                idle_exit: Duration::from_millis(150),
                ..Config::default()
            },
        );
        let app = router(state.clone());
        tokio::time::sleep(Duration::from_millis(40)).await;
        let idle = state.idle_for();
        let response = app
            .clone()
            .oneshot(HttpRequest::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.idle_for() >= idle, "healthz does not count as use");

        tokio::time::sleep(Duration::from_millis(40)).await;
        let response = app
            .oneshot(post("/search", json!({ "query": "echo" })))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            state.idle_for() < Duration::from_millis(20),
            "a route request counts as use"
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = Config {
            idle_exit: Duration::from_millis(150),
            ..Config::default()
        };
        assert!(
            tokio::time::timeout(Duration::from_secs(5), serve(listener, engine, config))
                .await
                .is_ok(),
            "the server exits when idle"
        );
    }
}
