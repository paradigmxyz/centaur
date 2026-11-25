//! The multi-tenant executor: runs Code Mode for agent turns outside any
//! sandbox (the brain's `nanocode` turns).
//!
//! Every request carries the turn token api-rs minted, as
//! `Authorization: Bearer <token>`. The executor verifies it before doing
//! anything, so a cell only boots for a valid turn, and takes the turn's tool
//! filter and thread from it. Cells and connectors run in this process exactly
//! as in a sandbox, but the executor holds no credentials: every connector
//! request goes through the tenancy iron-proxy, with the turn token as the
//! tunnel's `Proxy-Authorization`, and iron-proxy applies that turn's
//! principal's credentials. Each token gets its own HTTP clients, so a tunnel
//! opened for one turn is never reused for another.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Extension, Json, Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use centaur_codemode_protocol::{
    token::{TurnClaims, TurnTokenVerifier, bearer},
    wire::{
        EXEC_PATH, ExecRequest as WireExec, LOAD_PATH, LoadRequest, SEARCH_PATH, SearchRequest,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::{
    catalog::{Policy, connector_brief},
    engine::{Egress, Engine, ExecRequest, permissions_data, turn_context},
    search,
    telemetry::TraceParent,
};

/// Concurrent cells per executor process.
pub const MAX_CELLS: usize = 64;
/// How long a principal's Console permissions are reused for `context`.
const CONTEXT_TTL: Duration = Duration::from_secs(60);
/// Time for one Console permissions fetch.
const CONTEXT_TIMEOUT: Duration = Duration::from_secs(5);
const PERMISSIONS_PATH: &str = "/api/v1/sandbox/permissions";
/// Per-token clients unused this long are dropped.
const EGRESS_IDLE: Duration = Duration::from_secs(10 * 60);

pub struct Config {
    pub verifier: TurnTokenVerifier,
    /// The tenancy iron-proxy every connector request goes through.
    pub proxy_url: String,
    /// Console's base URL, for the calling context; `None` leaves the
    /// context's `sandbox` empty.
    pub console_url: Option<String>,
    pub max_cells: usize,
}

/// The verified turn token, as raw text and claims.
#[derive(Clone)]
struct Turn {
    raw: Arc<str>,
    claims: TurnClaims,
}

type Contexts = HashMap<String, (Instant, Result<Value, String>)>;

#[derive(Clone)]
struct AppState {
    engine: Engine,
    verifier: Arc<TurnTokenVerifier>,
    proxy_url: Arc<str>,
    console_url: Option<Arc<str>>,
    egress: Arc<Mutex<HashMap<String, (Egress, Instant)>>>,
    contexts: Arc<Mutex<Contexts>>,
    permits: Arc<Semaphore>,
}

pub fn router(engine: Engine, config: Config) -> Router {
    let state = AppState {
        engine,
        verifier: Arc::new(config.verifier),
        proxy_url: config.proxy_url.into(),
        console_url: config.console_url.map(Into::into),
        egress: Arc::default(),
        contexts: Arc::default(),
        permits: Arc::new(Semaphore::new(config.max_cells.max(1))),
    };
    Router::new()
        .route(EXEC_PATH, post(exec))
        .route(SEARCH_PATH, post(search_tools))
        .route(LOAD_PATH, post(load))
        // Before any body is read: nothing runs without a valid turn token.
        .route_layer(middleware::from_fn_with_state(state.clone(), require_turn))
        .route("/healthz", get(healthz))
        .with_state(state)
}

/// Serve until ctrl-c.
pub async fn serve(
    listener: tokio::net::TcpListener,
    engine: Engine,
    config: Config,
) -> std::io::Result<()> {
    axum::serve(listener, router(engine, config))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

async fn require_turn(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let Some(raw) = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer)
        .map(str::to_owned)
    else {
        return refused("missing turn token");
    };
    match state.verifier.verify(&raw) {
        Ok(claims) => {
            request.extensions_mut().insert(Turn {
                raw: raw.into(),
                claims,
            });
            next.run(request).await
        }
        Err(error) => {
            tracing::info!(%error, "refused a turn token");
            refused("invalid or expired turn token")
        }
    }
}

fn refused(message: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": message }))).into_response()
}

fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "busy", "retryable": true })),
    )
        .into_response()
}

fn policy(claims: &TurnClaims) -> Policy {
    Policy {
        allowlist: claims.tools.allowlist.clone(),
        blocklist: claims.tools.blocklist.clone(),
    }
}

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
    Extension(turn): Extension<Turn>,
    headers: HeaderMap,
    Json(request): Json<WireExec>,
) -> Response {
    let started = Instant::now();
    let Ok(_permit) = state.permits.clone().try_acquire_owned() else {
        tracing::warn!(route = EXEC_PATH, "busy");
        return busy();
    };
    let egress = match state.egress_for(&turn) {
        Ok(egress) => egress,
        Err(error) => {
            tracing::error!(%error, "building turn egress clients");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response();
        }
    };
    let context = state.context(&turn, &egress).await;
    let trace = headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(TraceParent::parse);
    let response = state
        .engine
        .exec(ExecRequest {
            code: request.code,
            tools: None,
            policy: Some(policy(&turn.claims)),
            trace,
            egress: Some(egress),
            context: Some(context.to_string()),
        })
        .await;
    // Cell code, arguments, results and logs are never logged.
    tracing::info!(
        route = EXEC_PATH,
        principal = %turn.claims.sub,
        execution_id = %turn.claims.execution_id,
        cell_id = %request.cell_id,
        ms = started.elapsed().as_millis() as u64,
        ok = response.ok,
        calls = response.calls.len(),
        failed_calls = response.calls.iter().filter(|call| !call.ok).count(),
        "request"
    );
    Json(response).into_response()
}

async fn search_tools(
    State(state): State<AppState>,
    Extension(turn): Extension<Turn>,
    Json(request): Json<SearchRequest>,
) -> Response {
    let policy = policy(&turn.claims);
    let catalog = state.engine.catalog();
    let limit = search::limit(request.limit.map(|limit| limit as usize));
    let results = catalog.search(&request.query, limit, Some(&policy));
    tracing::info!(route = SEARCH_PATH, principal = %turn.claims.sub, limit, results = results.len(), "request");
    Json(json!({ "results": results, "catalog_revision": catalog.revision })).into_response()
}

async fn load(
    State(state): State<AppState>,
    Extension(turn): Extension<Turn>,
    Json(request): Json<LoadRequest>,
) -> Response {
    let policy = policy(&turn.claims);
    let catalog = state.engine.catalog();
    let connector = catalog.find(&request.tool, Some(&policy));
    tracing::info!(route = LOAD_PATH, principal = %turn.claims.sub, tool = %request.tool, found = connector.is_some(), "request");
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

impl AppState {
    /// The turn's egress clients, built once per token.
    fn egress_for(&self, turn: &Turn) -> eyre::Result<Egress> {
        let key = hex::encode(Sha256::digest(turn.raw.as_bytes()));
        let now = Instant::now();
        let mut clients = self.egress.lock().expect("egress lock");
        clients.retain(|_, (_, used)| now.duration_since(*used) < EGRESS_IDLE);
        if let Some((egress, used)) = clients.get_mut(&key) {
            *used = now;
            return Ok(egress.clone());
        }
        let authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", turn.raw))?;
        let egress = self.engine.egress_via(&self.proxy_url, authorization)?;
        clients.insert(key, (egress.clone(), now));
        Ok(egress)
    }

    /// The calling context: the turn's thread and the principal's Console
    /// permissions, fetched through iron-proxy (which applies the principal's
    /// credentials for Console) and reused for [`CONTEXT_TTL`]. A failed
    /// refresh keeps the last good permissions.
    async fn context(&self, turn: &Turn, egress: &Egress) -> Value {
        let principal = &turn.claims.sub;
        let cached = self
            .contexts
            .lock()
            .expect("context lock")
            .get(principal)
            .cloned();
        let sandbox = match cached {
            Some((fetched, sandbox)) if fetched.elapsed() < CONTEXT_TTL => sandbox,
            stale => {
                let fresh = self.fetch_permissions(egress).await;
                let sandbox = match (fresh, stale) {
                    (Ok(sandbox), _) => Ok(sandbox),
                    (Err(_), Some((_, Ok(previous)))) => Ok(previous),
                    (Err(error), _) => Err(error),
                };
                let mut contexts = self.contexts.lock().expect("context lock");
                contexts.retain(|_, (fetched, _)| fetched.elapsed() < CONTEXT_TTL * 10);
                contexts.insert(principal.clone(), (Instant::now(), sandbox.clone()));
                sandbox
            }
        };
        turn_context(sandbox, &turn.claims.thread_key)
    }

    async fn fetch_permissions(&self, egress: &Egress) -> Result<Value, String> {
        let Some(console_url) = &self.console_url else {
            return Err("Console is not configured".to_owned());
        };
        let response = egress
            .http
            .get(format!(
                "{}{PERMISSIONS_PATH}",
                console_url.trim_end_matches('/')
            ))
            .timeout(CONTEXT_TIMEOUT)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| {
                format!(
                    "Console permissions request failed: {}",
                    error.without_url()
                )
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "Console permissions returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("decoding Console permissions: {}", error.without_url()))?;
        permissions_data(&body)
    }
}
