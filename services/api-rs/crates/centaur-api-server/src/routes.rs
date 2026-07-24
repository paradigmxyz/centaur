use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    convert::TryFrom,
    env,
    path::Path as FsPath,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use aws_config::BehaviorVersion;
use aws_sdk_s3::{
    Client as S3Client,
    config::{Builder as S3ConfigBuilder, Region},
    presigning::PresigningConfig,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose};
use centaur_session_core::{ChatDestination, ThreadKey};
use centaur_session_runtime::{
    ExecuteSessionInput, HarnessConflictPolicy, PersonaSummary, SandboxRuntime, SessionRuntime,
    thread_trace_id, thread_trace_parent_span_id,
};
use centaur_session_sqlx::PgSessionStore;
use centaur_telemetry::{
    PrometheusHandle, http_status_class, prometheus_handle, record_http_request_finished,
    record_http_request_started, set_span_parent_trace,
};
use centaur_workflows::{
    CreateWorkflowRunRequest, WebhookFilter, WorkflowRuntime, WorkflowWebhookAuth,
    WorkflowWebhookSpec, WorkflowWebhookTriggerKey,
};
use futures_util::{Stream, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration as TimeDuration, OffsetDateTime};
use tower_http::trace::TraceLayer;
use tracing::Span;
use uuid::Uuid;

use crate::{
    ApiError,
    api_jwt::{bearer_jwt_from_headers, decode_jwt_payload, verify_console_jwt},
    mcp::{mcp_get, mcp_post, mcp_protected_resource_metadata},
    slack_proxy::slack_proxy_router,
    types::{
        AppendMessagesRequest, AppendMessagesResponse, CreateSessionRequest, CreateSessionResponse,
        DiscordThreadContext, EmitWorkflowEventRequest, EventsQuery, ExecuteSessionRequest,
        ExecuteSessionResponse, GithubThreadContext, InterruptSessionExecutionRequest,
        InterruptSessionExecutionResponse, LinearThreadContext, ListWorkflowRunsQuery,
        OnHarnessConflict, SessionContextResponse, SessionSseEvent, SlackThreadContext,
        stream_error_sse,
    },
};

#[derive(Clone)]
pub struct AppState {
    initialized: Arc<RwLock<Option<AppRuntimeState>>>,
    metrics: PrometheusHandle,
}

#[derive(Clone)]
struct AppRuntimeState {
    runtime: SessionRuntime,
    workflows: Option<WorkflowRuntime>,
    pool: Option<PgPool>,
}

impl AppState {
    pub fn unready() -> Self {
        Self {
            initialized: Arc::new(RwLock::new(None)),
            metrics: prometheus_handle().expect("failed to initialize Prometheus metrics recorder"),
        }
    }

    pub fn ready(runtime: SessionRuntime, workflows: Option<WorkflowRuntime>) -> Self {
        Self::ready_with_pool(runtime, workflows, None)
    }

    pub fn ready_with_pool(
        runtime: SessionRuntime,
        workflows: Option<WorkflowRuntime>,
        pool: Option<PgPool>,
    ) -> Self {
        let state = Self::unready();
        state.mark_ready(runtime, workflows, pool);
        state
    }

    pub fn mark_ready(
        &self,
        runtime: SessionRuntime,
        workflows: Option<WorkflowRuntime>,
        pool: Option<PgPool>,
    ) {
        let mut initialized = self
            .initialized
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *initialized = Some(AppRuntimeState {
            runtime,
            workflows,
            pool,
        });
    }

    fn initialized(&self) -> Option<AppRuntimeState> {
        self.initialized
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn is_ready(&self) -> bool {
        self.initialized().is_some()
    }

    /// The session runtime, if initialization completed. Unlike the private
    /// request-path accessor this does not error while starting; the
    /// shutdown path uses it to skip the execution handoff when the runtime
    /// never came up.
    pub fn session_runtime(&self) -> Option<SessionRuntime> {
        self.initialized().map(|initialized| initialized.runtime)
    }

    pub(crate) fn runtime(&self) -> Result<SessionRuntime, ApiError> {
        self.initialized()
            .map(|initialized| initialized.runtime)
            .ok_or_else(|| ApiError::ServiceUnavailable("api-rs is still starting".to_owned()))
    }

    fn workflows(&self) -> Result<WorkflowRuntime, ApiError> {
        let initialized = self
            .initialized()
            .ok_or_else(|| ApiError::ServiceUnavailable("api-rs is still starting".to_owned()))?;
        initialized
            .workflows
            .ok_or_else(|| ApiError::BadRequest("workflow runtime is not enabled".to_owned()))
    }

    fn pool(&self) -> Result<PgPool, ApiError> {
        let initialized = self
            .initialized()
            .ok_or_else(|| ApiError::ServiceUnavailable("api-rs is still starting".to_owned()))?;
        initialized.pool.ok_or_else(|| {
            ApiError::BadRequest("database-backed admin routes are not enabled".to_owned())
        })
    }
}

const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;
const REDACTED_WEBHOOK_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-centaur-api-key",
    "x-hub-signature",
    "x-hub-signature-256",
    "x-slack-signature",
    "stripe-signature",
];

pub fn build_router_with_runtime(store: PgSessionStore, sandbox_runtime: SandboxRuntime) -> Router {
    let pool = store.pool().clone();
    build_router_with_app_state(AppState::ready_with_pool(
        SessionRuntime::new(store, sandbox_runtime),
        None,
        Some(pool),
    ))
}

pub fn build_router_with_session_runtime(runtime: SessionRuntime) -> Router {
    build_router_with_session_and_workflow_runtime(runtime, None)
}

pub fn build_router_with_session_and_workflow_runtime(
    runtime: SessionRuntime,
    workflows: Option<WorkflowRuntime>,
) -> Router {
    build_router_with_app_state(AppState::ready(runtime, workflows))
}

pub fn build_router_with_app_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/api/personas", get(list_personas))
        .route("/mcp", post(mcp_post).get(mcp_get))
        .route(
            "/.well-known/oauth-protected-resource",
            get(mcp_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(mcp_protected_resource_metadata),
        )
        .route(
            "/api/session/{thread_key}",
            post(create_or_get_session).get(get_session_context),
        )
        .route(
            "/api/session/{thread_key}/messages",
            post(append_messages).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/session/{thread_key}/execute",
            post(execute_session).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/session/{thread_key}/interrupt",
            post(interrupt_session_execution),
        )
        .route("/api/session/{thread_key}/events", get(stream_events))
        .route("/api/sandboxes/drain", post(drain_sandboxes))
        .merge(slack_proxy_router())
        .route("/api/workflows/schedules", get(list_workflow_schedules))
        .route(
            "/api/workflows/runs",
            post(create_workflow_run).get(list_workflow_runs),
        )
        .route("/api/workflows/runs/{run_id}", get(get_workflow_run))
        .route(
            "/api/workflows/runs/{run_id}/cancel",
            post(cancel_workflow_run),
        )
        .route("/api/workflows/events", post(emit_workflow_event))
        .route(
            "/api/admin/slack/archive-imports",
            get(list_slack_archive_imports).post(presign_slack_archive_import),
        )
        .route(
            "/api/admin/slack/archive-imports/presign",
            post(presign_slack_archive_import),
        )
        .route(
            "/api/admin/slack/archive-imports/{import_id}",
            get(get_slack_archive_import).delete(delete_slack_archive_import),
        )
        .route(
            "/api/admin/slack/archive-imports/{import_id}/upload-url",
            post(refresh_slack_archive_import_upload_url),
        )
        .route(
            "/api/admin/slack/archive-imports/{import_id}/download-url",
            post(create_slack_archive_import_download_url),
        )
        .route(
            "/api/admin/slack/archive-imports/{import_id}/start",
            post(start_slack_archive_import),
        )
        .route(
            "/api/admin/slack/archive-imports/{import_id}/retry",
            post(retry_slack_archive_import),
        )
        .route(
            "/api/admin/slack/dm-sync/checkpoints",
            get(list_slack_private_sync_checkpoints),
        )
        .route(
            "/api/admin/slack/dm-sync/batch",
            post(ingest_slack_dm_sync_batch).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/admin/google/docs-sync/checkpoint",
            get(get_google_docs_sync_checkpoint),
        )
        .route(
            "/api/admin/google/docs-sync/batch",
            post(ingest_google_docs_sync_batch).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/admin/granola/sync/checkpoint",
            get(get_granola_sync_checkpoint),
        )
        .route(
            "/api/admin/granola/sync/batch",
            post(ingest_granola_sync_batch).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/webhooks/{slug}", any(invoke_workflow_webhook))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<Body>| {
                    let route = matched_route(request);
                    let span = tracing::info_span!(
                        "centaur.api_rs.http_request",
                        "otel.kind" = "server",
                        "otel.status_code" = tracing::field::Empty,
                        "http.request.method" = request.method().as_str(),
                        "http.route" = route.as_str(),
                        "http.response.status_code" = tracing::field::Empty,
                        "centaur.thread_key" = tracing::field::Empty,
                        thread_key = tracing::field::Empty,
                    );
                    if let Some(thread_key) = session_thread_key_from_request(request) {
                        span.record("centaur.thread_key", thread_key.as_str());
                        span.record("thread_key", thread_key.as_str());
                        set_span_parent_trace(
                            &span,
                            &thread_trace_id(&thread_key),
                            &thread_trace_parent_span_id(&thread_key),
                        );
                    }
                    span
                })
                .on_request(())
                .on_response(|response: &Response, latency: Duration, span: &Span| {
                    let status = response.status();
                    span.record("http.response.status_code", status.as_u16());
                    span.record(
                        "otel.status_code",
                        if status.is_server_error() {
                            "ERROR"
                        } else {
                            "OK"
                        },
                    );

                    tracing::info!(
                        component = "api_server",
                        event = "http_request",
                        status = status.as_u16(),
                        status_class = http_status_class(status.as_u16()),
                        duration_ms = (latency.as_secs_f64() * 1000.0),
                        "http request completed"
                    );
                }),
        )
        .layer(middleware::from_fn(http_metrics))
        .with_state(state)
}

async fn healthz(headers: HeaderMap) -> Json<Value> {
    let mut body = json!({"ok": true});
    if let Some(token) = bearer_jwt_from_headers(&headers) {
        body["slack_client_jwt"] = match decode_jwt_payload(token) {
            Ok(claims) => {
                let mut jwt = json!({ "claims": claims });
                match verify_console_jwt::<Value>(token) {
                    Ok(_) => {
                        jwt["valid"] = json!(true);
                    }
                    Err(error) => {
                        jwt["valid"] = json!(false);
                        jwt["error"] = json!(error.to_string());
                    }
                }
                jwt
            }
            Err(error) => json!({
                "valid": false,
                "error": error,
            }),
        };
    }
    Json(body)
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.is_ready() {
        (StatusCode::OK, Json(json!({"ok": true, "ready": true})))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "ready": false, "error": "api-rs is still starting"})),
        )
    }
}

async fn metrics(State(state): State<AppState>) -> Response {
    (
        [("Content-Type", "text/plain; version=0.0.4; charset=utf-8")],
        Body::from(state.metrics.render()),
    )
        .into_response()
}

async fn http_metrics(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let route = matched_route(&req);

    if route == "/metrics" {
        return next.run(req).await;
    }

    let start = Instant::now();
    record_http_request_started();
    let response = next.run(req).await;
    let status = response.status();
    let duration = start.elapsed();
    record_http_request_finished(method.as_str(), route.as_str(), status.as_u16(), duration);

    response
}

fn matched_route<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| "__unmatched__".to_owned())
}

fn session_thread_key_from_request<B>(request: &Request<B>) -> Option<ThreadKey> {
    session_thread_key_from_path(request.uri().path())
}

fn session_thread_key_from_path(path: &str) -> Option<ThreadKey> {
    let rest = path.strip_prefix("/api/session/")?;
    let raw_thread_key = rest.split('/').next()?;
    if raw_thread_key.is_empty() {
        return None;
    }
    let decoded = urlencoding::decode(raw_thread_key).ok()?;
    ThreadKey::try_from(decoded.into_owned()).ok()
}

async fn create_or_get_session(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let on_harness_conflict = match request.on_harness_conflict {
        Some(OnHarnessConflict::Restart) => HarnessConflictPolicy::Restart,
        Some(OnHarnessConflict::Reject) | None => HarnessConflictPolicy::Reject,
    };
    let outcome = state
        .runtime()?
        .create_or_get_session(
            &thread_key,
            &request.harness_type,
            request.persona_id.as_deref(),
            request.metadata,
            on_harness_conflict,
        )
        .await?;
    Ok(Json(CreateSessionResponse {
        session: outcome.session,
        harness_switched: outcome.harness_switched,
    }))
}

async fn get_session_context(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
) -> Result<Json<SessionContextResponse>, ApiError> {
    let runtime = state.runtime()?;
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let destination = thread_key.chat_destination();
    let platform = destination
        .as_ref()
        .map(ChatDestination::platform)
        .unwrap_or("unknown")
        .to_owned();
    let (slack, discord, linear, github) = match destination {
        Some(ChatDestination::Slack {
            channel_id,
            thread_ts,
        }) => (
            Some(SlackThreadContext {
                channel_id,
                thread_ts,
            }),
            None,
            None,
            None,
        ),
        Some(ChatDestination::Discord {
            guild_id,
            channel_id,
            thread_id,
        }) => (
            None,
            Some(DiscordThreadContext {
                guild_id,
                channel_id,
                thread_id,
            }),
            None,
            None,
        ),
        Some(ChatDestination::Linear {
            issue_id,
            comment_id,
            agent_session_id,
        }) => (
            None,
            None,
            Some(LinearThreadContext {
                issue_id,
                comment_id,
                agent_session_id,
            }),
            None,
        ),
        Some(ChatDestination::Github {
            owner,
            repo,
            number,
            kind,
            review_comment_id,
        }) => (
            None,
            None,
            None,
            Some(GithubThreadContext {
                owner,
                repo,
                number,
                kind: kind.as_str().to_owned(),
                review_comment_id,
            }),
        ),
        None => (None, None, None, None),
    };
    let title = match runtime.session_title(&thread_key).await {
        Ok(title) => title,
        Err(error) => {
            tracing::warn!(
                thread_key = %thread_key,
                %error,
                "failed to load optional session title"
            );
            None
        }
    };
    Ok(Json(SessionContextResponse {
        title,
        thread_key,
        platform,
        slack,
        discord,
        linear,
        github,
    }))
}

async fn list_personas(
    State(state): State<AppState>,
) -> Result<Json<Vec<PersonaSummary>>, ApiError> {
    Ok(Json(state.runtime()?.personas()))
}

async fn append_messages(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<AppendMessagesRequest>,
) -> Result<Json<AppendMessagesResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let message_ids = state
        .runtime()?
        .append_messages(&thread_key, &request.messages)
        .await?;
    Ok(Json(AppendMessagesResponse {
        ok: true,
        message_ids,
    }))
}

async fn execute_session(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<ExecuteSessionRequest>,
) -> Result<Json<ExecuteSessionResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let execution = state
        .runtime()?
        .execute_session(
            &thread_key,
            ExecuteSessionInput {
                idempotency_key: request.idempotency_key,
                metadata: request.metadata,
                input_lines: request.input_lines,
                idle_timeout_ms: request.idle_timeout_ms,
                max_duration_ms: request.max_duration_ms,
            },
        )
        .await?;
    Ok(Json(ExecuteSessionResponse {
        ok: true,
        execution_id: execution.execution_id,
        thread_key: execution.thread_key,
        status: execution.status.to_string(),
    }))
}

async fn interrupt_session_execution(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<InterruptSessionExecutionRequest>,
) -> Result<Json<InterruptSessionExecutionResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Interrupted from Slack");
    let outcome = state
        .runtime()?
        .interrupt_active_execution(&thread_key, reason)
        .await?;
    Ok(Json(InterruptSessionExecutionResponse {
        ok: true,
        interrupted: outcome.interrupted,
        execution_id: outcome.execution_id,
        thread_key,
    }))
}

async fn drain_sandboxes(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let report = state.runtime()?.drain().await?;
    let failed = report
        .failed
        .iter()
        .map(|failure| json!({ "sandbox_id": failure.sandbox_id, "error": failure.error }))
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "ok": report.failed.is_empty(),
        "stopped_count": report.stopped.len(),
        "stopped": report.stopped,
        "failed": failed,
    })))
}

async fn stream_events(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let events = state
        .runtime()?
        .stream_events(
            &thread_key,
            query.after_event_id.unwrap_or(0),
            query.execution_id.as_deref(),
        )
        .await?;
    let stream = events.map(move |result| {
        // Stream failures are server-side faults: log the details, send the
        // client an opaque stream-error event.
        let opaque = |error: &dyn std::error::Error| {
            tracing::error!(
                thread_key = %thread_key,
                error = %crate::error::error_chain(error),
                "session event stream failed"
            );
            stream_error_sse("event stream failed")
        };
        let sse = match result {
            Ok(event) => SessionSseEvent::try_from(event)
                .map(Event::from)
                .unwrap_or_else(|error| opaque(&error)),
            Err(error) => opaque(&error),
        };
        Ok(sse)
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[derive(Debug, Deserialize)]
struct GranolaSyncCheckpointQuery {
    scope_id: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct GranolaSyncCheckpointResponse {
    scope_id: String,
    #[serde(with = "time::serde::rfc3339::option")]
    watermark_time: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_success_at: Option<OffsetDateTime>,
    last_error: String,
}

#[derive(Debug, Deserialize)]
struct GranolaSyncBatchRequest {
    run: GranolaSyncRunPayload,
    #[serde(default)]
    notes: Vec<GranolaSyncNotePayload>,
    #[serde(default)]
    checkpoint: Option<GranolaSyncCheckpointPayload>,
}

#[derive(Debug, Deserialize)]
struct GranolaSyncRunPayload {
    run_id: String,
    #[serde(default = "default_granola_sync_mode")]
    mode: String,
    status: String,
    scope_id: String,
    broker_credential_id: String,
    source_user_email: String,
    #[serde(default)]
    notes_seen: i32,
    #[serde(default)]
    notes_upserted: i32,
    #[serde(default)]
    transcripts_seen: i32,
    #[serde(default)]
    transcripts_upserted: i32,
    #[serde(default)]
    error_text: String,
    #[serde(default)]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct GranolaSyncNotePayload {
    note_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    owner: Value,
    #[serde(default)]
    attendees: Value,
    #[serde(default)]
    calendar_event: Value,
    #[serde(default)]
    summary_markdown: String,
    #[serde(default)]
    summary_text: String,
    #[serde(default)]
    transcript: Value,
    #[serde(default)]
    url: String,
    #[serde(default)]
    source_created_at: Option<String>,
    #[serde(default)]
    source_updated_at: Option<String>,
    #[serde(default)]
    raw_payload: Value,
}

#[derive(Debug, Deserialize)]
struct GranolaSyncCheckpointPayload {
    scope_id: String,
    #[serde(default)]
    watermark_time: Option<String>,
}

async fn get_granola_sync_checkpoint(
    State(state): State<AppState>,
    Query(query): Query<GranolaSyncCheckpointQuery>,
) -> Result<Json<Value>, ApiError> {
    require_non_empty("scope_id", &query.scope_id)?;
    let checkpoint = sqlx::query_as::<_, GranolaSyncCheckpointResponse>(
        "SELECT scope_id, watermark_time, last_success_at, last_error \
         FROM granola_sync_checkpoints WHERE scope_id = $1",
    )
    .bind(&query.scope_id)
    .fetch_optional(&db_pool(&state)?)
    .await?;

    Ok(Json(json!({ "ok": true, "checkpoint": checkpoint })))
}

async fn ingest_granola_sync_batch(
    State(state): State<AppState>,
    Json(request): Json<GranolaSyncBatchRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_granola_sync_batch(&request)?;
    let mut tx = db_pool(&state)?.begin().await?;
    let run = &request.run;
    let scope = json!({
        "scope_id": run.scope_id,
        "broker_credential_id": run.broker_credential_id,
        "source_user_email": run.source_user_email,
    });
    let completed = run.status == "completed";
    let scopes_synced = if completed {
        json!([scope.clone()])
    } else {
        json!([])
    };
    let scopes_failed = if completed {
        json!([])
    } else {
        json!([{ "scope_id": run.scope_id, "reason": run.error_text }])
    };
    let finished_at = Some(OffsetDateTime::now_utc());
    let metadata = json!({
        "broker_credential_id": run.broker_credential_id,
        "source_user_email": run.source_user_email,
        "console_metadata": run.metadata.clone(),
    });

    sqlx::query(
        "INSERT INTO granola_sync_runs (\
         run_id, mode, status, scopes_requested, scopes_synced, scopes_failed, \
         notes_seen, notes_upserted, transcripts_seen, transcripts_upserted, \
         finished_at, error_text, metadata\
         ) VALUES (\
         $1, $2, $3, $4::jsonb, $5::jsonb, $6::jsonb, $7, $8, $9, $10, $11, $12, $13::jsonb\
         ) ON CONFLICT (run_id) DO UPDATE SET \
         mode = EXCLUDED.mode, status = EXCLUDED.status, \
         scopes_requested = EXCLUDED.scopes_requested, scopes_synced = EXCLUDED.scopes_synced, \
         scopes_failed = EXCLUDED.scopes_failed, notes_seen = EXCLUDED.notes_seen, \
         notes_upserted = EXCLUDED.notes_upserted, transcripts_seen = EXCLUDED.transcripts_seen, \
         transcripts_upserted = EXCLUDED.transcripts_upserted, \
         finished_at = COALESCE(EXCLUDED.finished_at, granola_sync_runs.finished_at), \
         error_text = EXCLUDED.error_text, metadata = EXCLUDED.metadata",
    )
    .bind(&run.run_id)
    .bind(&run.mode)
    .bind(&run.status)
    .bind(json!([scope.clone()]))
    .bind(scopes_synced)
    .bind(scopes_failed)
    .bind(run.notes_seen)
    .bind(run.notes_upserted)
    .bind(run.transcripts_seen)
    .bind(run.transcripts_upserted)
    .bind(finished_at)
    .bind(&run.error_text)
    .bind(metadata)
    .execute(&mut *tx)
    .await?;

    for note in &request.notes {
        let owner_id = json_text(&note.owner, "id").or_else(|| json_text(&note.owner, "user_id"));
        let owner_email = json_text(&note.owner, "email").unwrap_or_default();
        let owner_name = json_text(&note.owner, "name")
            .or_else(|| json_text(&note.owner, "display_name"))
            .unwrap_or_default();
        let attendees = ensure_json_array("note.attendees", &note.attendees)?;
        let transcript = ensure_json_array("note.transcript", &note.transcript)?;
        ensure_json_object("note.calendar_event", &note.calendar_event)?;
        let transcript_text = transcript
            .iter()
            .filter_map(|entry| json_text(entry, "text"))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let content_text = [
            note.title.trim(),
            note.summary_markdown.trim(),
            note.summary_text.trim(),
            transcript_text.trim(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        let access_emails = granola_access_emails(&run.source_user_email, &owner_email, attendees);
        let content_hash = hex::encode(Sha256::digest(content_text.as_bytes()));

        sqlx::query(
            "INSERT INTO granola_sync_notes (\
             note_id, title, owner_id, owner_email, owner_name, attendees, access_emails, \
             calendar_event, summary_markdown, summary_text, transcript_text, transcript_payload, \
             url, content_text, content_hash, source_created_at, source_updated_at, raw_payload, \
             source_run_id, last_seen_at, last_error, updated_at\
             ) VALUES (\
             $1, $2, $3, $4, $5, $6::jsonb, $7::text[], $8::jsonb, $9, $10, $11, $12::jsonb, \
             $13, $14, $15, $16::timestamptz, $17::timestamptz, $18::jsonb, $19, NOW(), '', NOW()\
             ) ON CONFLICT (note_id) DO UPDATE SET \
             title = EXCLUDED.title, owner_id = EXCLUDED.owner_id, owner_email = EXCLUDED.owner_email, \
             owner_name = EXCLUDED.owner_name, attendees = EXCLUDED.attendees, \
             access_emails = (SELECT COALESCE(array_agg(DISTINCT email ORDER BY email), ARRAY[]::text[]) \
                              FROM unnest(granola_sync_notes.access_emails || EXCLUDED.access_emails) AS emails(email) \
                              WHERE email <> ''), \
             calendar_event = EXCLUDED.calendar_event, summary_markdown = EXCLUDED.summary_markdown, \
             summary_text = EXCLUDED.summary_text, transcript_text = EXCLUDED.transcript_text, \
             transcript_payload = EXCLUDED.transcript_payload, url = EXCLUDED.url, \
             content_text = EXCLUDED.content_text, content_hash = EXCLUDED.content_hash, \
             source_created_at = COALESCE(EXCLUDED.source_created_at, granola_sync_notes.source_created_at), \
             source_updated_at = COALESCE(EXCLUDED.source_updated_at, granola_sync_notes.source_updated_at), \
             raw_payload = EXCLUDED.raw_payload, source_run_id = EXCLUDED.source_run_id, \
             last_seen_at = NOW(), last_error = '', updated_at = NOW()",
        )
        .bind(&note.note_id)
        .bind(&note.title)
        .bind(owner_id.unwrap_or_default())
        .bind(&owner_email)
        .bind(&owner_name)
        .bind(&note.attendees)
        .bind(access_emails)
        .bind(&note.calendar_event)
        .bind(&note.summary_markdown)
        .bind(&note.summary_text)
        .bind(transcript_text)
        .bind(&note.transcript)
        .bind(&note.url)
        .bind(content_text)
        .bind(content_hash)
        .bind(&note.source_created_at)
        .bind(&note.source_updated_at)
        .bind(&note.raw_payload)
        .bind(&run.run_id)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(checkpoint) = &request.checkpoint {
        let successful_at = completed.then(OffsetDateTime::now_utc);
        sqlx::query(
            "INSERT INTO granola_sync_checkpoints (\
             scope_id, watermark_time, last_run_id, last_success_at, last_error, updated_at\
             ) VALUES ($1, $2::timestamptz, $3, $4, $5, NOW()) \
             ON CONFLICT (scope_id) DO UPDATE SET \
             watermark_time = COALESCE(EXCLUDED.watermark_time, granola_sync_checkpoints.watermark_time), \
             last_run_id = EXCLUDED.last_run_id, \
             last_success_at = COALESCE(EXCLUDED.last_success_at, granola_sync_checkpoints.last_success_at), \
             last_error = EXCLUDED.last_error, updated_at = NOW()",
        )
        .bind(&checkpoint.scope_id)
        .bind(&checkpoint.watermark_time)
        .bind(&run.run_id)
        .bind(successful_at)
        .bind(&run.error_text)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Json(json!({
        "ok": true,
        "run_id": run.run_id,
        "notes_ingested": request.notes.len(),
    })))
}

fn default_granola_sync_mode() -> String {
    "incremental".to_owned()
}

fn validate_granola_sync_batch(request: &GranolaSyncBatchRequest) -> Result<(), ApiError> {
    let run = &request.run;
    require_non_empty("run.run_id", &run.run_id)?;
    require_non_empty("run.status", &run.status)?;
    require_non_empty("run.scope_id", &run.scope_id)?;
    require_non_empty("run.broker_credential_id", &run.broker_credential_id)?;
    require_non_empty("run.source_user_email", &run.source_user_email)?;
    if run.scope_id != format!("oauth:{}", run.broker_credential_id) {
        return Err(ApiError::BadRequest(
            "run.scope_id must be the OAuth credential scope".to_owned(),
        ));
    }
    if !matches!(run.status.as_str(), "completed" | "failed") {
        return Err(ApiError::BadRequest(
            "run.status must be completed or failed".to_owned(),
        ));
    }
    ensure_json_object("run.metadata", &run.metadata)?;
    for note in &request.notes {
        require_non_empty("note.note_id", &note.note_id)?;
        ensure_json_object("note.owner", &note.owner)?;
        ensure_json_array("note.attendees", &note.attendees)?;
        ensure_json_object("note.calendar_event", &note.calendar_event)?;
        ensure_json_array("note.transcript", &note.transcript)?;
        ensure_json_object("note.raw_payload", &note.raw_payload)?;
    }
    if let Some(checkpoint) = &request.checkpoint {
        require_non_empty("checkpoint.scope_id", &checkpoint.scope_id)?;
        if checkpoint.scope_id != run.scope_id {
            return Err(ApiError::BadRequest(
                "checkpoint.scope_id must match run.scope_id".to_owned(),
            ));
        }
    }
    Ok(())
}

fn ensure_json_object<'a>(field: &str, value: &'a Value) -> Result<&'a Value, ApiError> {
    if value.is_object() {
        Ok(value)
    } else {
        Err(ApiError::BadRequest(format!(
            "{field} must be a JSON object"
        )))
    }
}

fn ensure_json_array<'a>(field: &str, value: &'a Value) -> Result<&'a Vec<Value>, ApiError> {
    value
        .as_array()
        .ok_or_else(|| ApiError::BadRequest(format!("{field} must be a JSON array")))
}

fn json_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn granola_access_emails(
    source_user_email: &str,
    owner_email: &str,
    attendees: &[Value],
) -> Vec<String> {
    let mut emails = BTreeSet::new();
    for candidate in std::iter::once(source_user_email)
        .chain(std::iter::once(owner_email))
        .chain(
            attendees
                .iter()
                .filter_map(|attendee| attendee.get("email").and_then(Value::as_str)),
        )
    {
        let normalized = candidate.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            emails.insert(normalized);
        }
    }
    emails.into_iter().collect()
}

#[derive(Debug, Deserialize)]
struct PresignSlackArchiveImportRequest {
    filename: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct ListSlackArchiveImportsQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct SlackArchiveImportResponse {
    import_id: String,
    mode: String,
    archive_uri: String,
    object_bucket: String,
    object_key: String,
    original_filename: String,
    content_type: String,
    file_size_bytes: Option<i64>,
    sha256: Option<String>,
    status: String,
    workflow_run_id: Option<String>,
    workflow_task_id: Option<String>,
    channels_imported: i32,
    users_imported: i32,
    messages_imported: i32,
    error_text: String,
    created_by: String,
    #[serde(with = "time::serde::rfc3339::option")]
    uploaded_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    finished_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    upload_url_expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
    metadata: Value,
}

#[derive(Debug, sqlx::FromRow)]
struct SlackArchiveImportRow {
    import_id: String,
    mode: String,
    archive_uri: String,
    object_bucket: String,
    object_key: String,
    original_filename: String,
    content_type: String,
    file_size_bytes: Option<i64>,
    sha256: Option<String>,
    status: String,
    workflow_run_id: Option<String>,
    workflow_task_id: Option<String>,
    channels_imported: i32,
    users_imported: i32,
    messages_imported: i32,
    error_text: String,
    created_by: String,
    uploaded_at: Option<OffsetDateTime>,
    started_at: Option<OffsetDateTime>,
    finished_at: Option<OffsetDateTime>,
    upload_url_expires_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    metadata: Value,
}

const SLACK_ARCHIVE_IMPORT_COLUMNS: &str = "import_id, mode, archive_uri, \
object_bucket, object_key, original_filename, content_type, file_size_bytes, sha256, status, \
workflow_run_id, workflow_task_id, channels_imported, users_imported, messages_imported, \
error_text, created_by, uploaded_at, started_at, finished_at, upload_url_expires_at, created_at, \
updated_at, metadata";

#[derive(Debug, Deserialize)]
struct ListSlackDmSyncCheckpointsQuery {
    broker_credential_id: String,
    #[serde(default)]
    home_team_id: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct SlackDmSyncCheckpointResponse {
    broker_credential_id: String,
    home_team_id: String,
    conversation_id: String,
    watermark_ts: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncBatchRequest {
    #[serde(default)]
    run: Option<SlackDmSyncRunPayload>,
    #[serde(default)]
    replace_memberships: bool,
    #[serde(default)]
    conversations: Vec<SlackDmSyncConversationPayload>,
    #[serde(default)]
    members: Vec<SlackDmSyncMemberPayload>,
    #[serde(default)]
    messages: Vec<SlackDmSyncMessagePayload>,
    #[serde(default)]
    attachments: Vec<SlackDmSyncAttachmentPayload>,
    #[serde(default)]
    checkpoints: Vec<SlackDmSyncCheckpointPayload>,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncRunPayload {
    run_id: String,
    #[serde(default)]
    workflow_run_id: Option<String>,
    #[serde(default = "default_slack_dm_sync_mode")]
    mode: String,
    status: String,
    broker_credential_id: String,
    source_user_id: String,
    home_team_id: String,
    #[serde(default)]
    conversations_requested: i32,
    #[serde(default)]
    conversations_synced: i32,
    #[serde(default)]
    conversations_failed: i32,
    #[serde(default)]
    messages_fetched: i32,
    #[serde(default)]
    messages_upserted: i32,
    #[serde(default)]
    replies_fetched: i32,
    #[serde(default)]
    replies_upserted: i32,
    #[serde(default)]
    finished: bool,
    #[serde(default)]
    error_text: String,
    #[serde(default = "empty_object")]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncConversationPayload {
    home_team_id: String,
    conversation_id: String,
    conversation_type: String,
    #[serde(default)]
    is_archived: bool,
    #[serde(default)]
    is_ext_shared: bool,
    #[serde(default = "empty_object")]
    raw_payload: Value,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncMemberPayload {
    home_team_id: String,
    conversation_id: String,
    user_id: String,
    #[serde(default)]
    user_team_id: Option<String>,
    #[serde(default)]
    is_external: bool,
    #[serde(default = "default_true")]
    is_current_member: bool,
    #[serde(default = "empty_object")]
    raw_payload: Value,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncMessagePayload {
    home_team_id: String,
    conversation_id: String,
    message_ts: String,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    parent_message_ts: Option<String>,
    #[serde(default)]
    is_thread_root: bool,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    user_team_id: Option<String>,
    #[serde(default)]
    bot_id: String,
    #[serde(default = "default_slack_message_type")]
    message_type: String,
    #[serde(default)]
    message_subtype: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    permalink: String,
    #[serde(default)]
    reply_count: i32,
    #[serde(default = "empty_array")]
    reply_users: Value,
    #[serde(default)]
    latest_reply_ts: Option<String>,
    #[serde(default)]
    thread_refreshed: bool,
    #[serde(default = "empty_object")]
    raw_payload: Value,
    #[serde(default)]
    source_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncAttachmentPayload {
    home_team_id: String,
    conversation_id: String,
    message_ts: String,
    slack_file_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    mimetype: String,
    #[serde(default)]
    filetype: String,
    #[serde(default)]
    size_bytes: i64,
    #[serde(default)]
    url_private: String,
    #[serde(default)]
    permalink: String,
    #[serde(default = "default_attachment_download_status")]
    download_status: String,
    #[serde(default)]
    download_error: String,
    #[serde(default)]
    content_sha256: Option<String>,
    #[serde(default = "empty_object")]
    raw_payload: Value,
    #[serde(default)]
    source_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackDmSyncCheckpointPayload {
    broker_credential_id: String,
    home_team_id: String,
    conversation_id: String,
    #[serde(default)]
    watermark_ts: Option<String>,
    #[serde(default)]
    last_run_id: Option<String>,
    #[serde(default)]
    last_error: String,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncCheckpointQuery {
    broker_credential_id: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct GoogleDocsSyncCheckpointResponse {
    broker_credential_id: String,
    provider_subject: String,
    provider_email: String,
    start_page_token: String,
    changes_page_token: String,
    #[serde(with = "time::serde::rfc3339::option")]
    last_full_sync_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_incremental_sync_at: Option<OffsetDateTime>,
    last_run_id: Option<String>,
    last_error: String,
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncBatchRequest {
    #[serde(default)]
    run: Option<GoogleDocsSyncRunPayload>,
    #[serde(default)]
    files: Vec<GoogleDocsSyncFilePayload>,
    #[serde(default)]
    observations: Vec<GoogleDocsSyncObservationPayload>,
    #[serde(default)]
    contents: Vec<GoogleDocsSyncContentPayload>,
    #[serde(default)]
    context_documents: Vec<GoogleDocsContextDocumentPayload>,
    #[serde(default)]
    checkpoint: Option<GoogleDocsSyncCheckpointPayload>,
    #[serde(default = "default_true")]
    replace_context_documents: bool,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncRunPayload {
    run_id: String,
    #[serde(default)]
    workflow_run_id: Option<String>,
    #[serde(default = "default_google_docs_sync_mode")]
    mode: String,
    status: String,
    broker_credential_id: String,
    #[serde(default)]
    provider_subject: String,
    #[serde(default)]
    provider_email: String,
    #[serde(default)]
    files_seen: i32,
    #[serde(default)]
    files_upserted: i32,
    #[serde(default)]
    docs_fetched: i32,
    #[serde(default)]
    docs_upserted: i32,
    #[serde(default)]
    chunks_upserted: i32,
    #[serde(default)]
    finished: bool,
    #[serde(default)]
    error_text: String,
    #[serde(default = "empty_object")]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncFilePayload {
    file_id: String,
    #[serde(default)]
    drive_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    web_view_link: String,
    #[serde(default = "empty_array")]
    owners: Value,
    #[serde(default = "empty_object")]
    last_modifying_user: Value,
    #[serde(default = "empty_object")]
    capabilities: Value,
    #[serde(default = "empty_object")]
    labels: Value,
    #[serde(default)]
    trashed: bool,
    #[serde(default)]
    explicitly_trashed: bool,
    #[serde(default)]
    source_created_at: Option<String>,
    #[serde(default)]
    source_modified_at: Option<String>,
    #[serde(default)]
    source_version: String,
    #[serde(default = "empty_object")]
    raw_payload: Value,
    #[serde(default)]
    source_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncObservationPayload {
    broker_credential_id: String,
    observed_file_id: String,
    file_id: String,
    #[serde(default)]
    provider_subject: String,
    #[serde(default)]
    provider_email: String,
    #[serde(default)]
    observed_name: String,
    #[serde(default)]
    observed_mime_type: String,
    #[serde(default)]
    observed_web_view_link: String,
    #[serde(default)]
    shortcut_target_file_id: String,
    #[serde(default)]
    role_hint: String,
    #[serde(default = "empty_array")]
    permission_ids: Value,
    #[serde(default = "default_true")]
    active: bool,
    #[serde(default = "empty_object")]
    raw_payload: Value,
    #[serde(default)]
    source_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncContentPayload {
    file_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    text_content: String,
    #[serde(default)]
    text_hash: String,
    #[serde(default)]
    export_mime_type: String,
    #[serde(default)]
    exported_at: Option<String>,
    #[serde(default)]
    source_modified_at: Option<String>,
    #[serde(default)]
    source_version: String,
    #[serde(default)]
    source_run_id: Option<String>,
    #[serde(default)]
    last_error: String,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsContextDocumentPayload {
    document_id: String,
    file_id: String,
    #[serde(default)]
    chunk_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    provider_author_id: String,
    #[serde(default)]
    provider_author_name: String,
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    drive_id: String,
    #[serde(default)]
    source_created_at: Option<String>,
    #[serde(default)]
    source_modified_at: Option<String>,
    #[serde(default)]
    source_version: String,
    #[serde(default)]
    content_hash: String,
    #[serde(default = "empty_object")]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct GoogleDocsSyncCheckpointPayload {
    broker_credential_id: String,
    #[serde(default)]
    provider_subject: String,
    #[serde(default)]
    provider_email: String,
    #[serde(default)]
    start_page_token: String,
    #[serde(default)]
    changes_page_token: String,
    #[serde(default)]
    last_full_sync_at: Option<String>,
    #[serde(default)]
    last_incremental_sync_at: Option<String>,
    #[serde(default)]
    last_run_id: Option<String>,
    #[serde(default)]
    last_error: String,
    #[serde(default = "empty_object")]
    metadata: Value,
}

impl From<SlackArchiveImportRow> for SlackArchiveImportResponse {
    fn from(row: SlackArchiveImportRow) -> Self {
        Self {
            import_id: row.import_id,
            mode: row.mode,
            archive_uri: row.archive_uri,
            object_bucket: row.object_bucket,
            object_key: row.object_key,
            original_filename: row.original_filename,
            content_type: row.content_type,
            file_size_bytes: row.file_size_bytes,
            sha256: row.sha256,
            status: row.status,
            workflow_run_id: row.workflow_run_id,
            workflow_task_id: row.workflow_task_id,
            channels_imported: row.channels_imported,
            users_imported: row.users_imported,
            messages_imported: row.messages_imported,
            error_text: row.error_text,
            created_by: row.created_by,
            uploaded_at: row.uploaded_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
            upload_url_expires_at: row.upload_url_expires_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            metadata: row.metadata,
        }
    }
}

#[derive(Clone, Debug)]
struct SlackArchiveUploadConfig {
    bucket: String,
    prefix: String,
    region: Option<String>,
    endpoint: Option<String>,
    presign_ttl: Duration,
}

async fn list_slack_archive_imports(
    State(state): State<AppState>,
    Query(query): Query<ListSlackArchiveImportsQuery>,
) -> Result<Json<Value>, ApiError> {
    let pool = db_pool(&state)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let sql = format!(
        "SELECT {SLACK_ARCHIVE_IMPORT_COLUMNS} FROM slack_archive_imports \
         WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY created_at DESC LIMIT $2"
    );
    let rows = sqlx::query_as::<_, SlackArchiveImportRow>(&sql)
        .bind(query.status.as_deref().filter(|value| !value.is_empty()))
        .bind(limit)
        .fetch_all(&pool)
        .await?;
    let imports = rows
        .into_iter()
        .map(SlackArchiveImportResponse::from)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "ok": true, "imports": imports })))
}

async fn get_slack_archive_import(
    State(state): State<AppState>,
    Path(import_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let pool = db_pool(&state)?;
    let import = load_slack_archive_import(&pool, &import_id).await?;
    Ok(Json(
        json!({ "ok": true, "import": SlackArchiveImportResponse::from(import) }),
    ))
}

async fn presign_slack_archive_import(
    State(state): State<AppState>,
    Json(request): Json<PresignSlackArchiveImportRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pool = db_pool(&state)?;
    let config = slack_archive_upload_config()?;
    let filename = sanitize_filename(&request.filename)?;
    let content_type = request
        .content_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/zip")
        .to_owned();
    if !matches!(
        content_type.as_str(),
        "application/zip" | "application/x-zip-compressed"
    ) {
        return Err(ApiError::BadRequest(
            "content_type must be application/zip".to_owned(),
        ));
    }
    let import_id = prefixed_id("sai");
    let object_key = slack_archive_object_key(&config.prefix, &import_id, &filename);
    let archive_uri = format!("s3://{}/{}", config.bucket, object_key);
    let upload_url = presign_s3_put_url(&config, &object_key, &content_type).await?;
    let expires_at = OffsetDateTime::now_utc() + config.presign_ttl;
    let metadata = if request.metadata.is_object() {
        request.metadata
    } else {
        json!({})
    };

    let sql = format!(
        "INSERT INTO slack_archive_imports (\
         import_id, mode, archive_uri, object_bucket, object_key, \
         original_filename, content_type, status, created_by, upload_url_expires_at, metadata\
         ) VALUES ($1, 'public_channels', $2, $3, $4, $5, $6, \
         'upload_pending', $7, $8, $9::jsonb) \
         RETURNING {SLACK_ARCHIVE_IMPORT_COLUMNS}"
    );
    let row = sqlx::query_as::<_, SlackArchiveImportRow>(&sql)
        .bind(&import_id)
        .bind(&archive_uri)
        .bind(&config.bucket)
        .bind(&object_key)
        .bind(&filename)
        .bind(&content_type)
        .bind(request.created_by.as_deref().unwrap_or(""))
        .bind(expires_at)
        .bind(metadata)
        .fetch_one(&pool)
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(slack_archive_upload_response(row, upload_url, expires_at)),
    ))
}

async fn refresh_slack_archive_import_upload_url(
    State(state): State<AppState>,
    Path(import_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pool = db_pool(&state)?;
    let import = load_slack_archive_import(&pool, &import_id).await?;
    ensure_archive_import_status(
        &import.status,
        &["upload_pending"],
        "archive upload URL cannot be refreshed",
    )?;
    let config = slack_archive_upload_config()?;
    ensure_archive_import_bucket_matches_config(&import, &config)?;
    let upload_url = presign_s3_put_url(&config, &import.object_key, &import.content_type).await?;
    let expires_at = OffsetDateTime::now_utc() + config.presign_ttl;
    let sql = format!(
        "UPDATE slack_archive_imports SET upload_url_expires_at = $2, updated_at = NOW() \
         WHERE import_id = $1 RETURNING {SLACK_ARCHIVE_IMPORT_COLUMNS}"
    );
    let row = sqlx::query_as::<_, SlackArchiveImportRow>(&sql)
        .bind(&import.import_id)
        .bind(expires_at)
        .fetch_one(&pool)
        .await?;

    Ok((
        StatusCode::OK,
        Json(slack_archive_upload_response(row, upload_url, expires_at)),
    ))
}

async fn create_slack_archive_import_download_url(
    State(state): State<AppState>,
    Path(import_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let pool = db_pool(&state)?;
    let import = load_slack_archive_import(&pool, &import_id).await?;
    ensure_archive_import_status(
        &import.status,
        &["uploaded", "importing", "failed"],
        "archive download URL cannot be created",
    )?;
    let config = slack_archive_upload_config()?;
    ensure_archive_import_bucket_matches_config(&import, &config)?;
    let download_url = presign_s3_get_url(&config, &import.object_key).await?;
    let expires_at = OffsetDateTime::now_utc() + config.presign_ttl;
    Ok(Json(slack_archive_download_response(
        import,
        download_url,
        expires_at,
    )))
}

async fn delete_slack_archive_import(
    State(state): State<AppState>,
    Path(import_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let pool = db_pool(&state)?;
    let import = load_slack_archive_import(&pool, &import_id).await?;
    ensure_archive_import_status(
        &import.status,
        &["upload_pending", "uploaded", "failed", "cancelled"],
        "archive import cannot be deleted",
    )?;
    let mut object_delete = json!({"attempted": false, "deleted": false});
    if import.status != "cancelled" {
        let config = slack_archive_upload_config()?;
        ensure_archive_import_bucket_matches_config(&import, &config)?;
        delete_s3_object(&config, &import.object_key).await?;
        object_delete = json!({"attempted": true, "deleted": true});
    }
    let sql = format!(
        "UPDATE slack_archive_imports SET status = 'cancelled', \
         finished_at = COALESCE(finished_at, NOW()), error_text = '', updated_at = NOW() \
         WHERE import_id = $1 RETURNING {SLACK_ARCHIVE_IMPORT_COLUMNS}"
    );
    let row = sqlx::query_as::<_, SlackArchiveImportRow>(&sql)
        .bind(&import.import_id)
        .fetch_one(&pool)
        .await?;
    Ok(Json(json!({
        "ok": true,
        "import": SlackArchiveImportResponse::from(row),
        "archive_object": object_delete,
    })))
}

async fn start_slack_archive_import(
    State(state): State<AppState>,
    Path(import_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pool = db_pool(&state)?;
    let import = load_slack_archive_import(&pool, &import_id).await?;
    ensure_archive_import_status(
        &import.status,
        &["upload_pending"],
        "archive upload cannot be confirmed",
    )?;
    let config = slack_archive_upload_config()?;
    ensure_archive_import_bucket_matches_config(&import, &config)?;
    let head = head_s3_object(&config, &import.object_key).await?;
    let workflows = workflow_runtime(&state)?;
    let workflow = workflows
        .create_run(CreateWorkflowRunRequest {
            workflow_name: "slack_archive_import".to_owned(),
            input: json!({ "import_id": import.import_id }),
            idempotency_key: Some(format!("slack_archive_import:{}", import.import_id)),
            harness_type: None,
            max_attempts: Some(1),
        })
        .await?;
    let row =
        mark_slack_archive_import_queued(&pool, &import, head, &workflow.run_id, &workflow.task_id)
            .await?;

    Ok((
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "import": SlackArchiveImportResponse::from(row),
            "ingestion": {
                "status": workflow.status,
                "workflow_name": "slack_archive_import",
                "workflow_run_id": workflow.run_id,
                "workflow_task_id": workflow.task_id,
                "created": workflow.created
            }
        })),
    ))
}

async fn retry_slack_archive_import(
    State(state): State<AppState>,
    Path(import_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pool = db_pool(&state)?;
    let import = load_slack_archive_import(&pool, &import_id).await?;
    ensure_archive_import_status(
        &import.status,
        &["failed"],
        "archive import cannot be retried",
    )?;
    let config = slack_archive_upload_config()?;
    ensure_archive_import_bucket_matches_config(&import, &config)?;
    let head = head_s3_object(&config, &import.object_key).await?;
    let workflows = workflow_runtime(&state)?;
    let workflow = workflows
        .create_run(CreateWorkflowRunRequest {
            workflow_name: "slack_archive_import".to_owned(),
            input: json!({ "import_id": import.import_id }),
            idempotency_key: Some(format!(
                "slack_archive_import:{}:retry:{}",
                import.import_id,
                Uuid::new_v4().simple()
            )),
            harness_type: None,
            max_attempts: Some(1),
        })
        .await?;
    let row =
        mark_slack_archive_import_queued(&pool, &import, head, &workflow.run_id, &workflow.task_id)
            .await?;

    Ok((
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "import": SlackArchiveImportResponse::from(row),
            "ingestion": {
                "status": workflow.status,
                "workflow_name": "slack_archive_import",
                "workflow_run_id": workflow.run_id,
                "workflow_task_id": workflow.task_id,
                "created": workflow.created
            }
        })),
    ))
}

async fn list_slack_private_sync_checkpoints(
    State(state): State<AppState>,
    Query(query): Query<ListSlackDmSyncCheckpointsQuery>,
) -> Result<Json<Value>, ApiError> {
    let pool = db_pool(&state)?;
    require_non_empty("broker_credential_id", &query.broker_credential_id)?;
    let rows = sqlx::query_as::<_, SlackDmSyncCheckpointResponse>(
        "SELECT broker_credential_id, home_team_id, conversation_id, watermark_ts \
         FROM slack_private_sync_checkpoints \
         WHERE broker_credential_id = $1 \
         AND ($2::text IS NULL OR home_team_id = $2) \
         ORDER BY home_team_id, conversation_id",
    )
    .bind(&query.broker_credential_id)
    .bind(
        query
            .home_team_id
            .as_deref()
            .filter(|value| !value.is_empty()),
    )
    .fetch_all(&pool)
    .await?;

    Ok(Json(json!({ "ok": true, "checkpoints": rows })))
}

async fn ingest_slack_dm_sync_batch(
    State(state): State<AppState>,
    Json(request): Json<SlackDmSyncBatchRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_slack_dm_sync_batch(&request)?;
    let pool = db_pool(&state)?;
    let mut tx = pool.begin().await?;

    if let Some(run) = &request.run {
        upsert_slack_dm_sync_run(&mut tx, run).await?;
    }

    for conversation in &request.conversations {
        sqlx::query(
            "INSERT INTO slack_private_sync_conversations (\
             home_team_id, conversation_id, conversation_type, is_archived, is_ext_shared, \
             raw_payload, last_seen_at, updated_at\
             ) VALUES ($1, $2, $3, $4, $5, $6::jsonb, NOW(), NOW()) \
             ON CONFLICT (home_team_id, conversation_id) DO UPDATE SET \
             conversation_type = EXCLUDED.conversation_type, \
             is_archived = EXCLUDED.is_archived, \
             is_ext_shared = EXCLUDED.is_ext_shared, \
             raw_payload = EXCLUDED.raw_payload, \
             last_seen_at = NOW(), \
             updated_at = NOW()",
        )
        .bind(&conversation.home_team_id)
        .bind(&conversation.conversation_id)
        .bind(&conversation.conversation_type)
        .bind(conversation.is_archived)
        .bind(conversation.is_ext_shared)
        .bind(&conversation.raw_payload)
        .execute(&mut *tx)
        .await?;
    }

    if request.replace_memberships {
        let mut conversations = BTreeMap::new();
        for member in &request.members {
            conversations.insert(
                (member.home_team_id.clone(), member.conversation_id.clone()),
                true,
            );
        }
        for ((home_team_id, conversation_id), _) in conversations {
            sqlx::query(
                "UPDATE slack_private_sync_conversation_members \
                 SET is_current_member = false, updated_at = NOW() \
                 WHERE home_team_id = $1 AND conversation_id = $2",
            )
            .bind(home_team_id)
            .bind(conversation_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    for member in &request.members {
        sqlx::query(
            "INSERT INTO slack_private_sync_conversation_members (\
             home_team_id, conversation_id, user_id, user_team_id, is_external, \
             is_current_member, raw_payload, last_seen_at, updated_at\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, NOW(), NOW()) \
             ON CONFLICT (home_team_id, conversation_id, user_id) DO UPDATE SET \
             user_team_id = EXCLUDED.user_team_id, \
             is_external = EXCLUDED.is_external, \
             is_current_member = EXCLUDED.is_current_member, \
             raw_payload = EXCLUDED.raw_payload, \
             last_seen_at = NOW(), \
             updated_at = NOW()",
        )
        .bind(&member.home_team_id)
        .bind(&member.conversation_id)
        .bind(&member.user_id)
        .bind(&member.user_team_id)
        .bind(member.is_external)
        .bind(member.is_current_member)
        .bind(&member.raw_payload)
        .execute(&mut *tx)
        .await?;
    }

    for message in &request.messages {
        let occurred_at = slack_ts_to_datetime(Some(&message.message_ts))?;
        let thread_refreshed_at = if message.thread_refreshed {
            Some(OffsetDateTime::now_utc())
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO slack_private_sync_messages (\
             home_team_id, conversation_id, message_ts, occurred_at, thread_ts, \
             parent_message_ts, is_thread_root, user_id, user_team_id, bot_id, \
             message_type, message_subtype, text, permalink, reply_count, reply_users, \
             latest_reply_ts, thread_refreshed_at, raw_payload, source_run_id, \
             last_seen_at, updated_at\
             ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
             $11, $12, $13, $14, $15, $16::jsonb, $17, $18, $19::jsonb, $20, \
             NOW(), NOW()) \
             ON CONFLICT (home_team_id, conversation_id, message_ts) DO UPDATE SET \
             occurred_at = EXCLUDED.occurred_at, \
             thread_ts = EXCLUDED.thread_ts, \
             parent_message_ts = EXCLUDED.parent_message_ts, \
             is_thread_root = EXCLUDED.is_thread_root, \
             user_id = EXCLUDED.user_id, \
             user_team_id = EXCLUDED.user_team_id, \
             bot_id = EXCLUDED.bot_id, \
             message_type = EXCLUDED.message_type, \
             message_subtype = EXCLUDED.message_subtype, \
             text = EXCLUDED.text, \
             permalink = EXCLUDED.permalink, \
             reply_count = EXCLUDED.reply_count, \
             reply_users = EXCLUDED.reply_users, \
             latest_reply_ts = EXCLUDED.latest_reply_ts, \
             thread_refreshed_at = COALESCE(EXCLUDED.thread_refreshed_at, slack_private_sync_messages.thread_refreshed_at), \
             raw_payload = EXCLUDED.raw_payload, \
             source_run_id = COALESCE(EXCLUDED.source_run_id, slack_private_sync_messages.source_run_id), \
             last_seen_at = NOW(), \
             updated_at = NOW()",
        )
        .bind(&message.home_team_id)
        .bind(&message.conversation_id)
        .bind(&message.message_ts)
        .bind(occurred_at)
        .bind(empty_to_none(message.thread_ts.as_deref()))
        .bind(empty_to_none(message.parent_message_ts.as_deref()))
        .bind(message.is_thread_root)
        .bind(&message.user_id)
        .bind(&message.user_team_id)
        .bind(&message.bot_id)
        .bind(&message.message_type)
        .bind(&message.message_subtype)
        .bind(&message.text)
        .bind(&message.permalink)
        .bind(message.reply_count)
        .bind(&message.reply_users)
        .bind(empty_to_none(message.latest_reply_ts.as_deref()))
        .bind(thread_refreshed_at)
        .bind(&message.raw_payload)
        .bind(empty_to_none(message.source_run_id.as_deref()))
        .execute(&mut *tx)
        .await?;
    }

    for attachment in &request.attachments {
        sqlx::query(
            "INSERT INTO slack_private_sync_message_attachments (\
             home_team_id, conversation_id, message_ts, slack_file_id, name, title, \
             mimetype, filetype, size_bytes, url_private, permalink, download_status, \
             download_error, content_sha256, raw_payload, source_run_id, last_seen_at, updated_at\
             ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, \
             $13, $14, $15::jsonb, $16, NOW(), NOW()) \
             ON CONFLICT (home_team_id, conversation_id, message_ts, slack_file_id) DO UPDATE SET \
             name = EXCLUDED.name, \
             title = EXCLUDED.title, \
             mimetype = EXCLUDED.mimetype, \
             filetype = EXCLUDED.filetype, \
             size_bytes = EXCLUDED.size_bytes, \
             url_private = EXCLUDED.url_private, \
             permalink = EXCLUDED.permalink, \
             download_status = EXCLUDED.download_status, \
             download_error = EXCLUDED.download_error, \
             content_sha256 = EXCLUDED.content_sha256, \
             raw_payload = EXCLUDED.raw_payload, \
             source_run_id = COALESCE(EXCLUDED.source_run_id, slack_private_sync_message_attachments.source_run_id), \
             last_seen_at = NOW(), \
             updated_at = NOW()",
        )
        .bind(&attachment.home_team_id)
        .bind(&attachment.conversation_id)
        .bind(&attachment.message_ts)
        .bind(&attachment.slack_file_id)
        .bind(&attachment.name)
        .bind(&attachment.title)
        .bind(&attachment.mimetype)
        .bind(&attachment.filetype)
        .bind(attachment.size_bytes)
        .bind(&attachment.url_private)
        .bind(&attachment.permalink)
        .bind(&attachment.download_status)
        .bind(&attachment.download_error)
        .bind(&attachment.content_sha256)
        .bind(&attachment.raw_payload)
        .bind(empty_to_none(attachment.source_run_id.as_deref()))
        .execute(&mut *tx)
        .await?;
    }

    for checkpoint in &request.checkpoints {
        let last_success_at = if checkpoint.last_error.trim().is_empty() {
            Some(OffsetDateTime::now_utc())
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO slack_private_sync_checkpoints (\
             broker_credential_id, home_team_id, conversation_id, watermark_ts, \
             last_run_id, last_success_at, last_error, updated_at\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW()) \
             ON CONFLICT (broker_credential_id, home_team_id, conversation_id) DO UPDATE SET \
             watermark_ts = EXCLUDED.watermark_ts, \
             last_run_id = EXCLUDED.last_run_id, \
             last_success_at = COALESCE(EXCLUDED.last_success_at, slack_private_sync_checkpoints.last_success_at), \
             last_error = EXCLUDED.last_error, \
             updated_at = NOW()",
        )
        .bind(&checkpoint.broker_credential_id)
        .bind(&checkpoint.home_team_id)
        .bind(&checkpoint.conversation_id)
        .bind(empty_to_none(checkpoint.watermark_ts.as_deref()))
        .bind(empty_to_none(checkpoint.last_run_id.as_deref()))
        .bind(last_success_at)
        .bind(&checkpoint.last_error)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(Json(json!({
        "ok": true,
        "counts": {
            "conversations": request.conversations.len(),
            "members": request.members.len(),
            "messages": request.messages.len(),
            "attachments": request.attachments.len(),
            "checkpoints": request.checkpoints.len(),
        }
    })))
}

async fn get_google_docs_sync_checkpoint(
    State(state): State<AppState>,
    Query(query): Query<GoogleDocsSyncCheckpointQuery>,
) -> Result<Json<Value>, ApiError> {
    let pool = db_pool(&state)?;
    require_non_empty("broker_credential_id", &query.broker_credential_id)?;
    let checkpoint = sqlx::query_as::<_, GoogleDocsSyncCheckpointResponse>(
        "SELECT broker_credential_id, provider_subject, provider_email, \
         start_page_token, changes_page_token, last_full_sync_at, \
         last_incremental_sync_at, last_run_id, last_error, metadata \
         FROM google_docs_sync_checkpoints WHERE broker_credential_id = $1",
    )
    .bind(&query.broker_credential_id)
    .fetch_optional(&pool)
    .await?;

    Ok(Json(json!({ "ok": true, "checkpoint": checkpoint })))
}

async fn ingest_google_docs_sync_batch(
    State(state): State<AppState>,
    Json(request): Json<GoogleDocsSyncBatchRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_google_docs_sync_batch(&request)?;
    let pool = db_pool(&state)?;
    let mut tx = pool.begin().await?;

    if let Some(run) = &request.run {
        upsert_google_docs_sync_run(&mut tx, run).await?;
    }

    for file in &request.files {
        sqlx::query(
            "INSERT INTO google_docs_sync_files (\
             file_id, drive_id, name, mime_type, web_view_link, owners, \
             last_modifying_user, capabilities, labels, trashed, explicitly_trashed, \
             source_created_at, source_modified_at, source_version, raw_payload, \
             source_run_id, last_seen_at, updated_at\
             ) VALUES (\
             $1, $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8::jsonb, $9::jsonb, \
             $10, $11, $12, $13, $14, $15::jsonb, $16, NOW(), NOW()) \
             ON CONFLICT (file_id) DO UPDATE SET \
             drive_id = EXCLUDED.drive_id, \
             name = EXCLUDED.name, \
             mime_type = EXCLUDED.mime_type, \
             web_view_link = EXCLUDED.web_view_link, \
             owners = EXCLUDED.owners, \
             last_modifying_user = EXCLUDED.last_modifying_user, \
             capabilities = EXCLUDED.capabilities, \
             labels = EXCLUDED.labels, \
             trashed = EXCLUDED.trashed, \
             explicitly_trashed = EXCLUDED.explicitly_trashed, \
             source_created_at = EXCLUDED.source_created_at, \
             source_modified_at = EXCLUDED.source_modified_at, \
             source_version = EXCLUDED.source_version, \
             raw_payload = EXCLUDED.raw_payload, \
             source_run_id = COALESCE(EXCLUDED.source_run_id, google_docs_sync_files.source_run_id), \
             last_seen_at = NOW(), \
             updated_at = NOW()",
        )
        .bind(&file.file_id)
        .bind(&file.drive_id)
        .bind(&file.name)
        .bind(&file.mime_type)
        .bind(&file.web_view_link)
        .bind(&file.owners)
        .bind(&file.last_modifying_user)
        .bind(&file.capabilities)
        .bind(&file.labels)
        .bind(file.trashed)
        .bind(file.explicitly_trashed)
        .bind(parse_rfc3339_option("file.source_created_at", file.source_created_at.as_deref())?)
        .bind(parse_rfc3339_option("file.source_modified_at", file.source_modified_at.as_deref())?)
        .bind(&file.source_version)
        .bind(&file.raw_payload)
        .bind(empty_to_none(file.source_run_id.as_deref()))
        .execute(&mut *tx)
        .await?;
    }

    for observation in &request.observations {
        sqlx::query(
            "INSERT INTO google_docs_sync_file_observations (\
             broker_credential_id, observed_file_id, file_id, provider_subject, \
             provider_email, observed_name, observed_mime_type, observed_web_view_link, \
             shortcut_target_file_id, role_hint, permission_ids, active, raw_payload, \
             source_run_id, last_seen_at, updated_at\
             ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb, $12, $13::jsonb, \
             $14, NOW(), NOW()) \
             ON CONFLICT (broker_credential_id, observed_file_id) DO UPDATE SET \
             file_id = EXCLUDED.file_id, \
             provider_subject = EXCLUDED.provider_subject, \
             provider_email = EXCLUDED.provider_email, \
             observed_name = EXCLUDED.observed_name, \
             observed_mime_type = EXCLUDED.observed_mime_type, \
             observed_web_view_link = EXCLUDED.observed_web_view_link, \
             shortcut_target_file_id = EXCLUDED.shortcut_target_file_id, \
             role_hint = EXCLUDED.role_hint, \
             permission_ids = EXCLUDED.permission_ids, \
             active = EXCLUDED.active, \
             raw_payload = EXCLUDED.raw_payload, \
             source_run_id = COALESCE(EXCLUDED.source_run_id, google_docs_sync_file_observations.source_run_id), \
             last_seen_at = NOW(), \
             updated_at = NOW()",
        )
        .bind(&observation.broker_credential_id)
        .bind(&observation.observed_file_id)
        .bind(&observation.file_id)
        .bind(&observation.provider_subject)
        .bind(&observation.provider_email)
        .bind(&observation.observed_name)
        .bind(&observation.observed_mime_type)
        .bind(&observation.observed_web_view_link)
        .bind(&observation.shortcut_target_file_id)
        .bind(&observation.role_hint)
        .bind(&observation.permission_ids)
        .bind(observation.active)
        .bind(&observation.raw_payload)
        .bind(empty_to_none(observation.source_run_id.as_deref()))
        .execute(&mut *tx)
        .await?;
    }

    for content in &request.contents {
        sqlx::query(
            "INSERT INTO google_docs_sync_document_contents (\
             file_id, title, text_content, text_hash, export_mime_type, exported_at, \
             source_modified_at, source_version, source_run_id, last_error, updated_at\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW()) \
             ON CONFLICT (file_id) DO UPDATE SET \
             title = EXCLUDED.title, \
             text_content = EXCLUDED.text_content, \
             text_hash = EXCLUDED.text_hash, \
             export_mime_type = EXCLUDED.export_mime_type, \
             exported_at = EXCLUDED.exported_at, \
             source_modified_at = EXCLUDED.source_modified_at, \
             source_version = EXCLUDED.source_version, \
             source_run_id = COALESCE(EXCLUDED.source_run_id, google_docs_sync_document_contents.source_run_id), \
             last_error = EXCLUDED.last_error, \
             updated_at = NOW()",
        )
        .bind(&content.file_id)
        .bind(&content.title)
        .bind(&content.text_content)
        .bind(&content.text_hash)
        .bind(&content.export_mime_type)
        .bind(parse_rfc3339_option("content.exported_at", content.exported_at.as_deref())?)
        .bind(parse_rfc3339_option(
            "content.source_modified_at",
            content.source_modified_at.as_deref(),
        )?)
        .bind(&content.source_version)
        .bind(empty_to_none(content.source_run_id.as_deref()))
        .bind(&content.last_error)
        .execute(&mut *tx)
        .await?;
    }

    for document in &request.context_documents {
        sqlx::query(
            "INSERT INTO google_docs_context_documents (\
             document_id, file_id, chunk_id, title, body, url, provider_author_id, \
             provider_author_name, mime_type, drive_id, source_created_at, source_modified_at, \
             source_version, content_hash, metadata, updated_at\
             ) VALUES (\
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::jsonb, NOW()) \
             ON CONFLICT (document_id) DO UPDATE SET \
             file_id = EXCLUDED.file_id, \
             chunk_id = EXCLUDED.chunk_id, \
             title = EXCLUDED.title, \
             body = EXCLUDED.body, \
             url = EXCLUDED.url, \
             provider_author_id = EXCLUDED.provider_author_id, \
             provider_author_name = EXCLUDED.provider_author_name, \
             mime_type = EXCLUDED.mime_type, \
             drive_id = EXCLUDED.drive_id, \
             source_created_at = EXCLUDED.source_created_at, \
             source_modified_at = EXCLUDED.source_modified_at, \
             source_version = EXCLUDED.source_version, \
             content_hash = EXCLUDED.content_hash, \
             metadata = EXCLUDED.metadata, \
             updated_at = NOW()",
        )
        .bind(&document.document_id)
        .bind(&document.file_id)
        .bind(&document.chunk_id)
        .bind(&document.title)
        .bind(&document.body)
        .bind(&document.url)
        .bind(&document.provider_author_id)
        .bind(&document.provider_author_name)
        .bind(&document.mime_type)
        .bind(&document.drive_id)
        .bind(parse_rfc3339_option(
            "context_document.source_created_at",
            document.source_created_at.as_deref(),
        )?)
        .bind(parse_rfc3339_option(
            "context_document.source_modified_at",
            document.source_modified_at.as_deref(),
        )?)
        .bind(&document.source_version)
        .bind(&document.content_hash)
        .bind(&document.metadata)
        .execute(&mut *tx)
        .await?;
    }

    if request.replace_context_documents {
        let mut chunks_by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for document in &request.context_documents {
            chunks_by_file
                .entry(document.file_id.clone())
                .or_default()
                .push(document.chunk_id.clone());
        }
        for content in &request.contents {
            chunks_by_file.entry(content.file_id.clone()).or_default();
        }
        for (file_id, chunk_ids) in chunks_by_file {
            sqlx::query(
                "DELETE FROM google_docs_context_documents \
                 WHERE file_id = $1 AND NOT (chunk_id = ANY($2::text[]))",
            )
            .bind(file_id)
            .bind(chunk_ids)
            .execute(&mut *tx)
            .await?;
        }
    }

    if let Some(checkpoint) = &request.checkpoint {
        sqlx::query(
            "INSERT INTO google_docs_sync_checkpoints (\
             broker_credential_id, provider_subject, provider_email, start_page_token, \
             changes_page_token, last_full_sync_at, last_incremental_sync_at, last_run_id, \
             last_error, metadata, updated_at\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, NOW()) \
             ON CONFLICT (broker_credential_id) DO UPDATE SET \
             provider_subject = EXCLUDED.provider_subject, \
             provider_email = EXCLUDED.provider_email, \
             start_page_token = COALESCE(NULLIF(EXCLUDED.start_page_token, ''), google_docs_sync_checkpoints.start_page_token), \
             changes_page_token = COALESCE(NULLIF(EXCLUDED.changes_page_token, ''), google_docs_sync_checkpoints.changes_page_token), \
             last_full_sync_at = COALESCE(EXCLUDED.last_full_sync_at, google_docs_sync_checkpoints.last_full_sync_at), \
             last_incremental_sync_at = COALESCE(EXCLUDED.last_incremental_sync_at, google_docs_sync_checkpoints.last_incremental_sync_at), \
             last_run_id = EXCLUDED.last_run_id, \
             last_error = EXCLUDED.last_error, \
             metadata = EXCLUDED.metadata, \
             updated_at = NOW()",
        )
        .bind(&checkpoint.broker_credential_id)
        .bind(&checkpoint.provider_subject)
        .bind(&checkpoint.provider_email)
        .bind(&checkpoint.start_page_token)
        .bind(&checkpoint.changes_page_token)
        .bind(parse_rfc3339_option(
            "checkpoint.last_full_sync_at",
            checkpoint.last_full_sync_at.as_deref(),
        )?)
        .bind(parse_rfc3339_option(
            "checkpoint.last_incremental_sync_at",
            checkpoint.last_incremental_sync_at.as_deref(),
        )?)
        .bind(empty_to_none(checkpoint.last_run_id.as_deref()))
        .bind(&checkpoint.last_error)
        .bind(&checkpoint.metadata)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(Json(json!({
        "ok": true,
        "counts": {
            "files": request.files.len(),
            "observations": request.observations.len(),
            "contents": request.contents.len(),
            "context_documents": request.context_documents.len(),
            "checkpoint": request.checkpoint.is_some(),
        }
    })))
}

async fn create_workflow_run(
    State(state): State<AppState>,
    Json(request): Json<CreateWorkflowRunRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let run = workflows.create_run(request).await?;
    Ok(Json(serde_json::to_value(run)?))
}

async fn list_workflow_runs(
    State(state): State<AppState>,
    Query(query): Query<ListWorkflowRunsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let runs = workflows.list_runs(query.limit.unwrap_or(50)).await?;
    Ok(Json(json!({ "ok": true, "runs": runs })))
}

async fn list_workflow_schedules(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let schedules = workflows.list_schedules();
    Ok(Json(json!({ "ok": true, "schedules": schedules })))
}

async fn get_workflow_run(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let run = workflows.get_run(&run_id).await?;
    Ok(Json(json!({ "ok": true, "run": run })))
}

async fn cancel_workflow_run(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    workflows.cancel_run(&run_id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn emit_workflow_event(
    State(state): State<AppState>,
    Json(request): Json<EmitWorkflowEventRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let event_name = match (
        request.event_name.as_deref(),
        request.event_type.as_deref(),
        request.correlation_id.as_deref(),
    ) {
        (Some(event_name), None, None) if !event_name.trim().is_empty() => event_name.to_owned(),
        (None, Some(event_type), Some(correlation_id))
            if !event_type.trim().is_empty() && !correlation_id.trim().is_empty() =>
        {
            centaur_workflows::python_workflow_event_name(event_type, correlation_id)
        }
        _ => {
            return Err(ApiError::BadRequest(
                "provide either event_name or both event_type and correlation_id".to_owned(),
            ));
        }
    };
    workflows.emit_event(&event_name, request.payload).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn invoke_workflow_webhook(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let workflows = workflow_runtime(&state)?;
    let registered = workflows
        .get_webhook(&slug)
        .ok_or_else(|| ApiError::NotFound("webhook not found".to_owned()))?;
    let spec = &registered.spec;
    let method_name = method.as_str().to_ascii_uppercase();
    if !spec
        .allowed_methods
        .iter()
        .any(|allowed| allowed == &method_name)
    {
        return Err(ApiError::MethodNotAllowed(
            "method not allowed for webhook".to_owned(),
        ));
    }
    let content_type = content_type(&headers);
    if !content_type.is_empty()
        && !spec
            .allowed_content_types
            .iter()
            .any(|allowed| allowed == &content_type)
    {
        return Err(ApiError::BadRequest(
            "unsupported webhook content type".to_owned(),
        ));
    }
    if raw_body.len() > MAX_WEBHOOK_BODY_BYTES {
        return Err(ApiError::PayloadTooLarge(
            "webhook payload too large".to_owned(),
        ));
    }
    verify_webhook_auth(spec, &headers, &raw_body)?;

    let raw_body_sha256 = hex::encode(Sha256::digest(&raw_body));
    let body = parse_webhook_body(&headers, &raw_body)?;

    // Edge pre-filter: drop events no handler could match, in-process, before
    // spawning a sandbox-backed run. Keeps org-wide webhooks cheap.
    if let Some(filter) = &spec.filter
        && !webhook_filter_matches(filter, &headers, &body)
    {
        return Ok((
            StatusCode::OK,
            Json(json!({ "ok": true, "filtered": true })),
        ));
    }

    let trigger_key = webhook_trigger_key(&slug, &raw_body_sha256, spec, &headers);
    let request = CreateWorkflowRunRequest {
        workflow_name: registered.workflow_name.clone(),
        input: json!({
            "webhook": {
                "slug": spec.slug,
                "provider": spec.provider,
                "method": method_name,
                "path": uri.path(),
                "headers": safe_webhook_headers(&headers, spec),
                "query": parse_query(uri.query().unwrap_or("")),
                "body": body,
                "raw_body_sha256": raw_body_sha256,
            }
        }),
        idempotency_key: Some(trigger_key),
        harness_type: None,
        max_attempts: None,
    };
    let run = workflows.create_run(request).await?;
    let status = if run.created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(json!({
            "ok": true,
            "run_id": run.run_id,
            "task_id": run.task_id,
            "workflow_name": registered.workflow_name,
            "status": run.status,
            "idempotent": !run.created,
        })),
    ))
}

fn workflow_runtime(state: &AppState) -> Result<WorkflowRuntime, ApiError> {
    state.workflows()
}

fn db_pool(state: &AppState) -> Result<PgPool, ApiError> {
    state.pool()
}

async fn load_slack_archive_import(
    pool: &PgPool,
    import_id: &str,
) -> Result<SlackArchiveImportRow, ApiError> {
    let sql = format!(
        "SELECT {SLACK_ARCHIVE_IMPORT_COLUMNS} FROM slack_archive_imports WHERE import_id = $1"
    );
    sqlx::query_as::<_, SlackArchiveImportRow>(&sql)
        .bind(import_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::NotFound("archive import not found".to_owned()))
}

fn slack_archive_upload_response(
    row: SlackArchiveImportRow,
    upload_url: String,
    expires_at: OffsetDateTime,
) -> Value {
    let archive_uri = row.archive_uri.clone();
    json!({
        "ok": true,
        "import": SlackArchiveImportResponse::from(row),
        "upload": {
            "archive_uri": archive_uri,
            "upload_url": upload_url,
            "expires_at": expires_at,
        }
    })
}

fn slack_archive_download_response(
    row: SlackArchiveImportRow,
    download_url: String,
    expires_at: OffsetDateTime,
) -> Value {
    let archive_uri = row.archive_uri.clone();
    json!({
        "ok": true,
        "import": SlackArchiveImportResponse::from(row),
        "download": {
            "archive_uri": archive_uri,
            "download_url": download_url,
            "expires_at": expires_at,
        }
    })
}

fn ensure_archive_import_status(
    status: &str,
    allowed: &[&str],
    action: &str,
) -> Result<(), ApiError> {
    if allowed.contains(&status) {
        return Ok(());
    }
    Err(ApiError::BadRequest(format!(
        "{action} from status {status}"
    )))
}

fn ensure_archive_import_bucket_matches_config(
    import: &SlackArchiveImportRow,
    config: &SlackArchiveUploadConfig,
) -> Result<(), ApiError> {
    if import.object_bucket == config.bucket {
        return Ok(());
    }
    Err(ApiError::BadRequest(
        "archive import bucket no longer matches configured bucket".to_owned(),
    ))
}

async fn mark_slack_archive_import_queued(
    pool: &PgPool,
    import: &SlackArchiveImportRow,
    head: S3ObjectHead,
    workflow_run_id: &str,
    workflow_task_id: &str,
) -> Result<SlackArchiveImportRow, ApiError> {
    let sql = format!(
        "UPDATE slack_archive_imports SET \
         status = 'uploaded', \
         file_size_bytes = $2, \
         sha256 = COALESCE(sha256, $3), \
         uploaded_at = COALESCE(uploaded_at, NOW()), \
         started_at = NULL, \
         finished_at = NULL, \
         workflow_run_id = $4, \
         workflow_task_id = $5, \
         channels_imported = 0, \
         users_imported = 0, \
         messages_imported = 0, \
         error_text = '', \
         updated_at = NOW() \
         WHERE import_id = $1 \
         RETURNING {SLACK_ARCHIVE_IMPORT_COLUMNS}"
    );
    sqlx::query_as::<_, SlackArchiveImportRow>(&sql)
        .bind(&import.import_id)
        .bind(head.size_bytes)
        .bind(head.sha256)
        .bind(workflow_run_id)
        .bind(workflow_task_id)
        .fetch_one(pool)
        .await
        .map_err(ApiError::from)
}

async fn upsert_slack_dm_sync_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &SlackDmSyncRunPayload,
) -> Result<(), ApiError> {
    let finished_at = if run.finished {
        Some(OffsetDateTime::now_utc())
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO slack_private_sync_runs (\
         run_id, workflow_run_id, mode, status, broker_credential_id, source_user_id, \
         home_team_id, conversations_requested, conversations_synced, conversations_failed, \
         messages_fetched, messages_upserted, replies_fetched, replies_upserted, \
         finished_at, error_text, metadata\
         ) VALUES (\
         $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
         $11, $12, $13, $14, $15, $16, $17::jsonb) \
         ON CONFLICT (run_id) DO UPDATE SET \
         workflow_run_id = EXCLUDED.workflow_run_id, \
         mode = EXCLUDED.mode, \
         status = EXCLUDED.status, \
         broker_credential_id = EXCLUDED.broker_credential_id, \
         source_user_id = EXCLUDED.source_user_id, \
         home_team_id = EXCLUDED.home_team_id, \
         conversations_requested = EXCLUDED.conversations_requested, \
         conversations_synced = EXCLUDED.conversations_synced, \
         conversations_failed = EXCLUDED.conversations_failed, \
         messages_fetched = EXCLUDED.messages_fetched, \
         messages_upserted = EXCLUDED.messages_upserted, \
         replies_fetched = EXCLUDED.replies_fetched, \
         replies_upserted = EXCLUDED.replies_upserted, \
         finished_at = COALESCE(EXCLUDED.finished_at, slack_private_sync_runs.finished_at), \
         error_text = EXCLUDED.error_text, \
         metadata = EXCLUDED.metadata",
    )
    .bind(&run.run_id)
    .bind(&run.workflow_run_id)
    .bind(&run.mode)
    .bind(&run.status)
    .bind(&run.broker_credential_id)
    .bind(&run.source_user_id)
    .bind(&run.home_team_id)
    .bind(run.conversations_requested)
    .bind(run.conversations_synced)
    .bind(run.conversations_failed)
    .bind(run.messages_fetched)
    .bind(run.messages_upserted)
    .bind(run.replies_fetched)
    .bind(run.replies_upserted)
    .bind(finished_at)
    .bind(&run.error_text)
    .bind(&run.metadata)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn upsert_google_docs_sync_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &GoogleDocsSyncRunPayload,
) -> Result<(), ApiError> {
    let finished_at = if run.finished {
        Some(OffsetDateTime::now_utc())
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO google_docs_sync_runs (\
         run_id, workflow_run_id, mode, status, broker_credential_id, provider_subject, \
         provider_email, files_seen, files_upserted, docs_fetched, docs_upserted, \
         chunks_upserted, finished_at, error_text, metadata\
         ) VALUES (\
         $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::jsonb) \
         ON CONFLICT (run_id) DO UPDATE SET \
         workflow_run_id = EXCLUDED.workflow_run_id, \
         mode = EXCLUDED.mode, \
         status = EXCLUDED.status, \
         broker_credential_id = EXCLUDED.broker_credential_id, \
         provider_subject = EXCLUDED.provider_subject, \
         provider_email = EXCLUDED.provider_email, \
         files_seen = EXCLUDED.files_seen, \
         files_upserted = EXCLUDED.files_upserted, \
         docs_fetched = EXCLUDED.docs_fetched, \
         docs_upserted = EXCLUDED.docs_upserted, \
         chunks_upserted = EXCLUDED.chunks_upserted, \
         finished_at = COALESCE(EXCLUDED.finished_at, google_docs_sync_runs.finished_at), \
         error_text = EXCLUDED.error_text, \
         metadata = EXCLUDED.metadata",
    )
    .bind(&run.run_id)
    .bind(&run.workflow_run_id)
    .bind(&run.mode)
    .bind(&run.status)
    .bind(&run.broker_credential_id)
    .bind(&run.provider_subject)
    .bind(&run.provider_email)
    .bind(run.files_seen)
    .bind(run.files_upserted)
    .bind(run.docs_fetched)
    .bind(run.docs_upserted)
    .bind(run.chunks_upserted)
    .bind(finished_at)
    .bind(&run.error_text)
    .bind(&run.metadata)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn slack_archive_upload_config() -> Result<SlackArchiveUploadConfig, ApiError> {
    let bucket = env::var("SLACK_ARCHIVE_UPLOAD_BUCKET")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if bucket.is_empty() {
        return Err(ApiError::BadRequest(
            "SLACK_ARCHIVE_UPLOAD_BUCKET is not configured".to_owned(),
        ));
    }
    let prefix = env::var("SLACK_ARCHIVE_UPLOAD_PREFIX")
        .unwrap_or_else(|_| "slack-archives".to_owned())
        .trim_matches('/')
        .to_owned();
    Ok(SlackArchiveUploadConfig {
        bucket,
        prefix,
        region: non_empty_env("SLACK_ARCHIVE_UPLOAD_REGION"),
        endpoint: non_empty_env("SLACK_ARCHIVE_UPLOAD_ENDPOINT"),
        presign_ttl: Duration::from_secs(positive_env_u64(
            "SLACK_ARCHIVE_UPLOAD_PRESIGN_TTL_SECONDS",
            900,
        )),
    })
}

pub(crate) fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn positive_env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn prefixed_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn default_slack_dm_sync_mode() -> String {
    "incremental".to_owned()
}

fn default_google_docs_sync_mode() -> String {
    "incremental".to_owned()
}

fn default_slack_message_type() -> String {
    "message".to_owned()
}

fn default_attachment_download_status() -> String {
    "metadata_only".to_owned()
}

fn empty_object() -> Value {
    json!({})
}

fn empty_array() -> Value {
    json!([])
}

fn default_true() -> bool {
    true
}

fn empty_to_none(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn require_non_empty(field: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::BadRequest(format!("{field} must not be empty")));
    }
    Ok(())
}

fn slack_ts_to_datetime(value: Option<&str>) -> Result<Option<OffsetDateTime>, ApiError> {
    let Some(value) = empty_to_none(value) else {
        return Ok(None);
    };
    let (seconds, micros) = value.split_once('.').unwrap_or((value, "0"));
    let seconds = seconds
        .parse::<i64>()
        .map_err(|_| ApiError::BadRequest(format!("invalid Slack timestamp {value}")))?;
    let micros = format!("{micros:0<6}");
    let micros = micros[..micros.len().min(6)]
        .parse::<i64>()
        .map_err(|_| ApiError::BadRequest(format!("invalid Slack timestamp {value}")))?;
    let timestamp = OffsetDateTime::from_unix_timestamp(seconds)
        .map_err(|_| ApiError::BadRequest(format!("invalid Slack timestamp {value}")))?
        .checked_add(TimeDuration::microseconds(micros))
        .ok_or_else(|| ApiError::BadRequest(format!("invalid Slack timestamp {value}")))?;
    Ok(Some(timestamp))
}

fn parse_rfc3339_option(
    field: &str,
    value: Option<&str>,
) -> Result<Option<OffsetDateTime>, ApiError> {
    let Some(value) = empty_to_none(value) else {
        return Ok(None);
    };
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(Some)
        .map_err(|_| ApiError::BadRequest(format!("{field} must be RFC3339")))
}

fn validate_json_shape(field: &str, value: &Value, object: bool) -> Result<(), ApiError> {
    let valid = if object {
        value.is_object()
    } else {
        value.is_array()
    };
    if valid {
        return Ok(());
    }
    let expected = if object { "object" } else { "array" };
    Err(ApiError::BadRequest(format!(
        "{field} must be a JSON {expected}"
    )))
}

fn validate_slack_dm_sync_batch(request: &SlackDmSyncBatchRequest) -> Result<(), ApiError> {
    if let Some(run) = &request.run {
        require_non_empty("run.run_id", &run.run_id)?;
        require_non_empty("run.status", &run.status)?;
        require_non_empty("run.broker_credential_id", &run.broker_credential_id)?;
        require_non_empty("run.source_user_id", &run.source_user_id)?;
        require_non_empty("run.home_team_id", &run.home_team_id)?;
        validate_json_shape("run.metadata", &run.metadata, true)?;
    }
    for conversation in &request.conversations {
        require_non_empty("conversation.home_team_id", &conversation.home_team_id)?;
        require_non_empty(
            "conversation.conversation_id",
            &conversation.conversation_id,
        )?;
        if !matches!(
            conversation.conversation_type.as_str(),
            "im" | "mpim" | "private_channel"
        ) {
            return Err(ApiError::BadRequest(
                "conversation.conversation_type must be im, mpim, or private_channel".to_owned(),
            ));
        }
        validate_json_shape("conversation.raw_payload", &conversation.raw_payload, true)?;
    }
    for member in &request.members {
        require_non_empty("member.home_team_id", &member.home_team_id)?;
        require_non_empty("member.conversation_id", &member.conversation_id)?;
        require_non_empty("member.user_id", &member.user_id)?;
        validate_json_shape("member.raw_payload", &member.raw_payload, true)?;
    }
    for message in &request.messages {
        require_non_empty("message.home_team_id", &message.home_team_id)?;
        require_non_empty("message.conversation_id", &message.conversation_id)?;
        require_non_empty("message.message_ts", &message.message_ts)?;
        validate_json_shape("message.reply_users", &message.reply_users, false)?;
        validate_json_shape("message.raw_payload", &message.raw_payload, true)?;
        slack_ts_to_datetime(Some(&message.message_ts))?;
    }
    for attachment in &request.attachments {
        require_non_empty("attachment.home_team_id", &attachment.home_team_id)?;
        require_non_empty("attachment.conversation_id", &attachment.conversation_id)?;
        require_non_empty("attachment.message_ts", &attachment.message_ts)?;
        require_non_empty("attachment.slack_file_id", &attachment.slack_file_id)?;
        validate_json_shape("attachment.raw_payload", &attachment.raw_payload, true)?;
    }
    for checkpoint in &request.checkpoints {
        require_non_empty(
            "checkpoint.broker_credential_id",
            &checkpoint.broker_credential_id,
        )?;
        require_non_empty("checkpoint.home_team_id", &checkpoint.home_team_id)?;
        require_non_empty("checkpoint.conversation_id", &checkpoint.conversation_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod slack_user_sync_tests {
    use super::*;

    fn request_with_conversation_type(conversation_type: &str) -> SlackDmSyncBatchRequest {
        SlackDmSyncBatchRequest {
            run: None,
            replace_memberships: false,
            conversations: vec![SlackDmSyncConversationPayload {
                home_team_id: "T123".to_owned(),
                conversation_id: "G123".to_owned(),
                conversation_type: conversation_type.to_owned(),
                is_archived: false,
                is_ext_shared: false,
                raw_payload: json!({"name": "leadership"}),
            }],
            members: vec![],
            messages: vec![],
            attachments: vec![],
            checkpoints: vec![],
        }
    }

    #[test]
    fn accepts_private_channel_conversations() {
        validate_slack_dm_sync_batch(&request_with_conversation_type("private_channel")).unwrap();
    }

    #[test]
    fn rejects_public_channel_conversations() {
        let error = validate_slack_dm_sync_batch(&request_with_conversation_type("public_channel"))
            .unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }
}

fn validate_google_docs_sync_batch(request: &GoogleDocsSyncBatchRequest) -> Result<(), ApiError> {
    if let Some(run) = &request.run {
        require_non_empty("run.run_id", &run.run_id)?;
        require_non_empty("run.status", &run.status)?;
        require_non_empty("run.broker_credential_id", &run.broker_credential_id)?;
        validate_json_shape("run.metadata", &run.metadata, true)?;
    }
    for file in &request.files {
        require_non_empty("file.file_id", &file.file_id)?;
        validate_json_shape("file.owners", &file.owners, false)?;
        validate_json_shape("file.last_modifying_user", &file.last_modifying_user, true)?;
        validate_json_shape("file.capabilities", &file.capabilities, true)?;
        validate_json_shape("file.labels", &file.labels, true)?;
        validate_json_shape("file.raw_payload", &file.raw_payload, true)?;
        parse_rfc3339_option("file.source_created_at", file.source_created_at.as_deref())?;
        parse_rfc3339_option(
            "file.source_modified_at",
            file.source_modified_at.as_deref(),
        )?;
    }
    for observation in &request.observations {
        require_non_empty(
            "observation.broker_credential_id",
            &observation.broker_credential_id,
        )?;
        require_non_empty(
            "observation.observed_file_id",
            &observation.observed_file_id,
        )?;
        require_non_empty("observation.file_id", &observation.file_id)?;
        validate_json_shape(
            "observation.permission_ids",
            &observation.permission_ids,
            false,
        )?;
        validate_json_shape("observation.raw_payload", &observation.raw_payload, true)?;
    }
    for content in &request.contents {
        require_non_empty("content.file_id", &content.file_id)?;
        parse_rfc3339_option("content.exported_at", content.exported_at.as_deref())?;
        parse_rfc3339_option(
            "content.source_modified_at",
            content.source_modified_at.as_deref(),
        )?;
    }
    for document in &request.context_documents {
        require_non_empty("context_document.document_id", &document.document_id)?;
        require_non_empty("context_document.file_id", &document.file_id)?;
        require_non_empty("context_document.chunk_id", &document.chunk_id)?;
        validate_json_shape("context_document.metadata", &document.metadata, true)?;
        parse_rfc3339_option(
            "context_document.source_created_at",
            document.source_created_at.as_deref(),
        )?;
        parse_rfc3339_option(
            "context_document.source_modified_at",
            document.source_modified_at.as_deref(),
        )?;
    }
    if let Some(checkpoint) = &request.checkpoint {
        require_non_empty(
            "checkpoint.broker_credential_id",
            &checkpoint.broker_credential_id,
        )?;
        validate_json_shape("checkpoint.metadata", &checkpoint.metadata, true)?;
        parse_rfc3339_option(
            "checkpoint.last_full_sync_at",
            checkpoint.last_full_sync_at.as_deref(),
        )?;
        parse_rfc3339_option(
            "checkpoint.last_incremental_sync_at",
            checkpoint.last_incremental_sync_at.as_deref(),
        )?;
    }
    Ok(())
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .trim_matches('_')
        .to_owned()
}

fn sanitize_filename(value: &str) -> Result<String, ApiError> {
    let basename = FsPath::new(value.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let filename = sanitize_path_segment(basename);
    if filename.is_empty() {
        return Err(ApiError::BadRequest(
            "filename must not be empty".to_owned(),
        ));
    }
    if !filename.to_ascii_lowercase().ends_with(".zip") {
        return Err(ApiError::BadRequest("filename must end in .zip".to_owned()));
    }
    Ok(filename)
}

fn slack_archive_object_key(prefix: &str, import_id: &str, filename: &str) -> String {
    [prefix, import_id, filename]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

async fn s3_client(config: &SlackArchiveUploadConfig) -> S3Client {
    let mut loader = aws_config::defaults(BehaviorVersion::latest());
    if let Some(region) = &config.region {
        loader = loader.region(Region::new(region.clone()));
    }
    if let Some(endpoint) = &config.endpoint {
        loader = loader.endpoint_url(endpoint);
    }
    let shared_config = loader.load().await;
    let mut builder = S3ConfigBuilder::from(&shared_config);
    if config.endpoint.is_some() {
        builder = builder.force_path_style(true);
    }
    S3Client::from_conf(builder.build())
}

async fn presign_s3_put_url(
    config: &SlackArchiveUploadConfig,
    object_key: &str,
    content_type: &str,
) -> Result<String, ApiError> {
    let client = s3_client(config).await;
    let presigning = PresigningConfig::expires_in(config.presign_ttl)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let request = client
        .put_object()
        .bucket(&config.bucket)
        .key(object_key)
        .content_type(content_type)
        .presigned(presigning)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(request.uri().to_string())
}

async fn presign_s3_get_url(
    config: &SlackArchiveUploadConfig,
    object_key: &str,
) -> Result<String, ApiError> {
    let client = s3_client(config).await;
    let presigning = PresigningConfig::expires_in(config.presign_ttl)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let request = client
        .get_object()
        .bucket(&config.bucket)
        .key(object_key)
        .presigned(presigning)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(request.uri().to_string())
}

struct S3ObjectHead {
    size_bytes: Option<i64>,
    sha256: Option<String>,
}

async fn head_s3_object(
    config: &SlackArchiveUploadConfig,
    object_key: &str,
) -> Result<S3ObjectHead, ApiError> {
    let client = s3_client(config).await;
    let response = client
        .head_object()
        .bucket(&config.bucket)
        .key(object_key)
        .send()
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!("archive object is not readable: {error}"))
        })?;
    let sha256 = response
        .metadata()
        .and_then(|metadata| metadata.get("sha256").cloned());
    Ok(S3ObjectHead {
        size_bytes: response.content_length(),
        sha256,
    })
}

async fn delete_s3_object(
    config: &SlackArchiveUploadConfig,
    object_key: &str,
) -> Result<(), ApiError> {
    let client = s3_client(config).await;
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(object_key)
        .send()
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!("archive object could not be deleted: {error}"))
        })?;
    Ok(())
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split_once(';')
        .map(|(head, _)| head)
        .unwrap_or_else(|| {
            headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        })
        .trim()
        .to_ascii_lowercase()
}

fn parse_webhook_body(headers: &HeaderMap, raw_body: &[u8]) -> Result<Value, ApiError> {
    if raw_body.is_empty() {
        return Ok(json!({}));
    }
    match content_type(headers).as_str() {
        "application/json" => serde_json::from_slice(raw_body)
            .map_err(|_| ApiError::BadRequest("invalid JSON webhook body".to_owned())),
        "application/x-www-form-urlencoded" => {
            let form = parse_form(std::str::from_utf8(raw_body).unwrap_or_default());
            if let Some(Value::String(payload)) = form.get("payload")
                && let Ok(value) = serde_json::from_str(payload)
            {
                return Ok(value);
            }
            Ok(Value::Object(form.into_iter().collect()))
        }
        _ => Ok(Value::String(
            String::from_utf8_lossy(raw_body).into_owned(),
        )),
    }
}

fn parse_query(query: &str) -> Value {
    Value::Object(parse_form(query).into_iter().collect())
}

fn parse_form(input: &str) -> BTreeMap<String, Value> {
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pair in input.split('&').filter(|part| !part.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_form_component(key);
        let value = decode_form_component(value);
        values.entry(key).or_default().push(value);
    }
    values
        .into_iter()
        .map(|(key, mut values)| {
            let value = if values.len() == 1 {
                Value::String(values.pop().unwrap_or_default())
            } else {
                Value::Array(values.into_iter().map(Value::String).collect())
            };
            (key, value)
        })
        .collect()
}

fn decode_form_component(value: &str) -> String {
    let replaced = value.replace('+', " ");
    urlencoding::decode(&replaced)
        .map(|decoded| decoded.into_owned())
        .unwrap_or(replaced)
}

fn safe_webhook_headers(headers: &HeaderMap, spec: &WorkflowWebhookSpec) -> Value {
    let mut safe = serde_json::Map::new();
    let signature_header = signature_header_name(&spec.auth).map(|name| name.to_ascii_lowercase());
    for (name, value) in headers {
        let normalized = name.as_str().to_ascii_lowercase();
        if REDACTED_WEBHOOK_HEADERS.contains(&normalized.as_str())
            || signature_header.as_deref() == Some(normalized.as_str())
        {
            continue;
        }
        if let Ok(value) = value.to_str() {
            safe.insert(normalized, Value::String(value.to_owned()));
        }
    }
    Value::Object(safe)
}

fn webhook_trigger_key(
    slug: &str,
    raw_body_sha256: &str,
    spec: &WorkflowWebhookSpec,
    headers: &HeaderMap,
) -> String {
    match &spec.trigger_key {
        Some(WorkflowWebhookTriggerKey::Header { header }) => {
            if let Some(value) =
                header_value(headers, header).filter(|value| !value.trim().is_empty())
            {
                return format!(
                    "webhook:{slug}:{}:{}",
                    header.to_ascii_lowercase(),
                    value.trim()
                );
            }
        }
        Some(WorkflowWebhookTriggerKey::Static { value }) if !value.trim().is_empty() => {
            return format!("webhook:{slug}:{}", value.trim());
        }
        _ => {}
    }
    format!("webhook:{slug}:{raw_body_sha256}")
}

fn verify_webhook_auth(
    spec: &WorkflowWebhookSpec,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<(), ApiError> {
    match &spec.auth {
        WorkflowWebhookAuth::None => Ok(()),
        WorkflowWebhookAuth::Bearer { secret_ref } => {
            let expected = env::var(secret_ref).map_err(|_| {
                ApiError::Internal(format!(
                    "webhook auth secret {secret_ref} is not configured"
                ))
            })?;
            let Some(actual) = header_value(headers, "Authorization") else {
                return Err(ApiError::Unauthorized("missing bearer token".to_owned()));
            };
            let actual = actual
                .strip_prefix("Bearer ")
                .or_else(|| actual.strip_prefix("bearer "))
                .unwrap_or(actual.as_str())
                .trim();
            if constant_time_eq(actual.as_bytes(), expected.trim().as_bytes()) {
                Ok(())
            } else {
                Err(ApiError::Unauthorized("invalid bearer token".to_owned()))
            }
        }
        WorkflowWebhookAuth::Github { secret_ref } => verify_hmac_signature(
            "X-Hub-Signature-256",
            "sha256=",
            "hex",
            secret_ref,
            headers,
            raw_body,
        ),
        WorkflowWebhookAuth::Hmac {
            secret_ref,
            signature_header,
            signature_prefix,
            encoding,
            ..
        } => verify_hmac_signature(
            signature_header,
            signature_prefix,
            encoding,
            secret_ref,
            headers,
            raw_body,
        ),
    }
}

fn verify_hmac_signature(
    signature_header: &str,
    signature_prefix: &str,
    encoding: &str,
    secret_ref: &str,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<(), ApiError> {
    let Some(signature) = header_value(headers, signature_header) else {
        return Err(ApiError::Unauthorized(
            "missing webhook signature".to_owned(),
        ));
    };
    let secret = env::var(secret_ref).map_err(|_| {
        ApiError::Internal(format!(
            "webhook auth secret {secret_ref} is not configured"
        ))
    })?;
    let invalid = || ApiError::Unauthorized("invalid webhook signature".to_owned());
    let presented = signature
        .trim()
        .strip_prefix(signature_prefix)
        .ok_or_else(invalid)?;
    let presented = match encoding {
        "base64" => general_purpose::STANDARD
            .decode(presented)
            .map_err(|_| invalid())?,
        _ => hex::decode(presented).map_err(|_| invalid())?,
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| {
        ApiError::Internal(format!(
            "webhook auth secret {secret_ref} is not valid HMAC key material"
        ))
    })?;
    mac.update(raw_body);
    // `verify_slice` is a constant-time comparison.
    mac.verify_slice(&presented).map_err(|_| invalid())
}

/// Compare two byte strings in constant time (modulo length, which is not
/// secret here).
fn constant_time_eq(actual: &[u8], expected: &[u8]) -> bool {
    use subtle::ConstantTimeEq;

    actual.ct_eq(expected).into()
}

fn signature_header_name(auth: &WorkflowWebhookAuth) -> Option<&str> {
    match auth {
        WorkflowWebhookAuth::None | WorkflowWebhookAuth::Bearer { .. } => None,
        WorkflowWebhookAuth::Github { .. } => Some("X-Hub-Signature-256"),
        WorkflowWebhookAuth::Hmac {
            signature_header, ..
        } => Some(signature_header),
    }
}

pub(crate) fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

/// Read a dot-path (e.g. "repository.full_name") out of a JSON body.
fn webhook_body_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = body;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Evaluate a declarative webhook pre-filter against the event.
fn webhook_filter_matches(filter: &WebhookFilter, headers: &HeaderMap, body: &Value) -> bool {
    if let Some(any) = &filter.any {
        return any.iter().any(|f| webhook_filter_matches(f, headers, body));
    }
    if let Some(all) = &filter.all {
        return all.iter().all(|f| webhook_filter_matches(f, headers, body));
    }
    let actual = match filter.source.as_deref() {
        Some("header") => filter
            .key
            .as_deref()
            .and_then(|key| header_value(headers, key)),
        Some("body") => filter
            .key
            .as_deref()
            .and_then(|key| webhook_body_path(body, key))
            .and_then(|value| value.as_str().map(ToOwned::to_owned)),
        // Registry validation rejects empty nodes; fail closed if one reaches
        // the evaluator anyway.
        None => return false,
        Some(_) => return false,
    };
    let Some(actual) = actual else {
        return false;
    };
    match filter.op.as_deref() {
        Some("equals") => filter.value.as_deref() == Some(actual.as_str()),
        Some("in") => filter
            .values
            .as_ref()
            .is_some_and(|values| values.iter().any(|v| v == &actual)),
        Some("contains") => filter
            .value
            .as_deref()
            .is_some_and(|needle| actual.contains(needle)),
        Some("prefix") => filter
            .value
            .as_deref()
            .is_some_and(|prefix| actual.trim_start().starts_with(prefix)),
        _ => false,
    }
}

#[cfg(test)]
mod granola_sync_tests {
    use super::*;

    fn batch() -> GranolaSyncBatchRequest {
        GranolaSyncBatchRequest {
            run: GranolaSyncRunPayload {
                run_id: "granola_test".to_owned(),
                mode: "incremental".to_owned(),
                status: "completed".to_owned(),
                scope_id: "oauth:bcr_123".to_owned(),
                broker_credential_id: "bcr_123".to_owned(),
                source_user_email: "owner@example.com".to_owned(),
                notes_seen: 1,
                notes_upserted: 1,
                transcripts_seen: 1,
                transcripts_upserted: 1,
                error_text: String::new(),
                metadata: json!({"source": "console"}),
            },
            notes: vec![GranolaSyncNotePayload {
                note_id: "meeting-1".to_owned(),
                title: "Planning".to_owned(),
                owner: json!({"email": "ada@example.com"}),
                attendees: json!([
                    {"email": "bob@example.com"},
                    {"email": "OWNER@example.com"}
                ]),
                calendar_event: json!({}),
                summary_markdown: "Ship it".to_owned(),
                summary_text: "Ship it".to_owned(),
                transcript: json!([{ "text": "Ada: ship it" }]),
                url: String::new(),
                source_created_at: None,
                source_updated_at: None,
                raw_payload: json!({"source": "granola_mcp"}),
            }],
            checkpoint: Some(GranolaSyncCheckpointPayload {
                scope_id: "oauth:bcr_123".to_owned(),
                watermark_time: Some("2026-07-08T12:00:00Z".to_owned()),
            }),
        }
    }

    #[test]
    fn validates_a_credential_scoped_granola_batch() {
        validate_granola_sync_batch(&batch()).unwrap();
    }

    #[test]
    fn rejects_a_checkpoint_for_another_credential() {
        let mut request = batch();
        request.checkpoint.as_mut().unwrap().scope_id = "oauth:bcr_other".to_owned();
        let error = validate_granola_sync_batch(&request).unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    #[test]
    fn rejects_a_scope_that_does_not_belong_to_the_credential() {
        let mut request = batch();
        request.run.scope_id = "oauth:bcr_other".to_owned();
        request.checkpoint.as_mut().unwrap().scope_id = "oauth:bcr_other".to_owned();
        let error = validate_granola_sync_batch(&request).unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    #[test]
    fn access_is_granted_to_source_owner_and_attendees() {
        let emails = granola_access_emails(
            "Owner@example.com",
            "ada@example.com",
            batch().notes[0].attendees.as_array().unwrap(),
        );
        assert_eq!(
            emails,
            vec![
                "ada@example.com".to_owned(),
                "bob@example.com".to_owned(),
                "owner@example.com".to_owned(),
            ]
        );
    }
}

#[cfg(test)]
mod slack_archive_import_tests {
    use super::*;

    fn archive_row(status: &str) -> SlackArchiveImportRow {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        SlackArchiveImportRow {
            import_id: "sai_test".to_owned(),
            mode: "public_channels".to_owned(),
            archive_uri: "s3://bucket/prefix/sai_test/archive.zip".to_owned(),
            object_bucket: "bucket".to_owned(),
            object_key: "prefix/sai_test/archive.zip".to_owned(),
            original_filename: "archive.zip".to_owned(),
            content_type: "application/zip".to_owned(),
            file_size_bytes: None,
            sha256: None,
            status: status.to_owned(),
            workflow_run_id: None,
            workflow_task_id: None,
            channels_imported: 0,
            users_imported: 0,
            messages_imported: 0,
            error_text: String::new(),
            created_by: "tester".to_owned(),
            uploaded_at: None,
            started_at: None,
            finished_at: None,
            upload_url_expires_at: None,
            created_at: now,
            updated_at: now,
            metadata: json!({}),
        }
    }

    #[test]
    fn archive_import_status_gate_allows_only_requested_statuses() {
        assert!(
            ensure_archive_import_status(
                "upload_pending",
                &["upload_pending"],
                "archive upload URL cannot be refreshed",
            )
            .is_ok()
        );
        let error = ensure_archive_import_status(
            "failed",
            &["upload_pending"],
            "archive upload URL cannot be refreshed",
        )
        .unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    #[test]
    fn archive_import_delete_statuses_exclude_active_and_completed_imports() {
        for status in ["upload_pending", "uploaded", "failed", "cancelled"] {
            ensure_archive_import_status(
                status,
                &["upload_pending", "uploaded", "failed", "cancelled"],
                "archive import cannot be deleted",
            )
            .unwrap();
        }
        for status in ["importing", "completed"] {
            let error = ensure_archive_import_status(
                status,
                &["upload_pending", "uploaded", "failed", "cancelled"],
                "archive import cannot be deleted",
            )
            .unwrap_err();
            assert!(matches!(error, ApiError::BadRequest(_)));
        }
    }

    #[test]
    fn archive_import_download_url_statuses_exclude_unuploaded_or_terminal_imports() {
        for status in ["uploaded", "importing", "failed"] {
            ensure_archive_import_status(
                status,
                &["uploaded", "importing", "failed"],
                "archive download URL cannot be created",
            )
            .unwrap();
        }
        for status in ["upload_pending", "completed", "cancelled"] {
            let error = ensure_archive_import_status(
                status,
                &["uploaded", "importing", "failed"],
                "archive download URL cannot be created",
            )
            .unwrap_err();
            assert!(matches!(error, ApiError::BadRequest(_)));
        }
    }

    #[test]
    fn archive_import_bucket_must_match_current_upload_config() {
        let import = archive_row("upload_pending");
        let config = SlackArchiveUploadConfig {
            bucket: "bucket".to_owned(),
            prefix: "prefix".to_owned(),
            region: Some("us-east-1".to_owned()),
            endpoint: None,
            presign_ttl: Duration::from_secs(900),
        };
        ensure_archive_import_bucket_matches_config(&import, &config).unwrap();

        let config = SlackArchiveUploadConfig {
            bucket: "other-bucket".to_owned(),
            ..config
        };
        let error = ensure_archive_import_bucket_matches_config(&import, &config).unwrap_err();
        assert!(matches!(error, ApiError::BadRequest(_)));
    }

    #[test]
    fn archive_upload_response_includes_import_and_upload_contract() {
        let expires_at = OffsetDateTime::from_unix_timestamp(1_700_000_900).unwrap();
        let body = slack_archive_upload_response(
            archive_row("upload_pending"),
            "https://uploads.example/presigned".to_owned(),
            expires_at,
        );
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["import"]["import_id"], json!("sai_test"));
        assert!(body["import"].get("workspace_id").is_none());
        assert_eq!(
            body["upload"]["archive_uri"],
            json!("s3://bucket/prefix/sai_test/archive.zip")
        );
        assert_eq!(
            body["upload"]["upload_url"],
            json!("https://uploads.example/presigned")
        );
        assert!(body["upload"]["expires_at"].is_array());
    }

    #[test]
    fn archive_download_response_includes_import_and_download_contract() {
        let expires_at = OffsetDateTime::from_unix_timestamp(1_700_000_900).unwrap();
        let body = slack_archive_download_response(
            archive_row("uploaded"),
            "https://uploads.example/presigned-download".to_owned(),
            expires_at,
        );
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["import"]["import_id"], json!("sai_test"));
        assert_eq!(
            body["download"]["archive_uri"],
            json!("s3://bucket/prefix/sai_test/archive.zip")
        );
        assert_eq!(
            body["download"]["download_url"],
            json!("https://uploads.example/presigned-download")
        );
        assert!(body["download"]["expires_at"].is_array());
        assert!(body.get("upload").is_none());
    }
}

#[cfg(test)]
mod webhook_tests {
    use super::*;

    #[test]
    fn session_thread_key_from_path_decodes_session_routes() {
        assert_eq!(
            session_thread_key_from_path(
                "/api/session/slack%3AT092R71U6QY%3AC0B1NNXKE4F%3A1782217699.671539/execute"
            )
            .map(|thread_key| thread_key.to_string()),
            Some("slack:T092R71U6QY:C0B1NNXKE4F:1782217699.671539".to_owned())
        );
        assert!(session_thread_key_from_path("/api/workflows/runs").is_none());
        assert!(session_thread_key_from_path("/api/session//execute").is_none());
    }

    #[test]
    fn parses_form_payload_json() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        let body =
            parse_webhook_body(&headers, br#"payload=%7B%22hello%22%3A%22form%22%7D"#).unwrap();
        assert_eq!(body, json!({"hello": "form"}));
    }

    #[test]
    fn redacts_sensitive_and_signature_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret".parse().unwrap());
        headers.insert("cookie", "session=secret".parse().unwrap());
        headers.insert("x-test-signature", "sha256=secret".parse().unwrap());
        headers.insert("x-test-delivery", "delivery-1".parse().unwrap());
        let spec = WorkflowWebhookSpec {
            slug: "unit".to_owned(),
            provider: None,
            auth: WorkflowWebhookAuth::Hmac {
                secret_ref: "TEST_WEBHOOK_SECRET".to_owned(),
                signature_header: "X-Test-Signature".to_owned(),
                algorithm: "sha256".to_owned(),
                signature_prefix: "sha256=".to_owned(),
                encoding: "hex".to_owned(),
            },
            trigger_key: None,
            allowed_methods: vec!["POST".to_owned()],
            allowed_content_types: vec!["application/json".to_owned()],
            filter: None,
        };
        let safe = safe_webhook_headers(&headers, &spec);
        assert_eq!(safe, json!({"x-test-delivery": "delivery-1"}));
    }

    #[test]
    fn derives_header_trigger_key() {
        let mut headers = HeaderMap::new();
        headers.insert("x-test-delivery", "delivery-1".parse().unwrap());
        let spec = WorkflowWebhookSpec {
            slug: "unit".to_owned(),
            provider: None,
            auth: WorkflowWebhookAuth::None,
            trigger_key: Some(WorkflowWebhookTriggerKey::Header {
                header: "X-Test-Delivery".to_owned(),
            }),
            allowed_methods: vec!["POST".to_owned()],
            allowed_content_types: vec!["application/json".to_owned()],
            filter: None,
        };
        assert_eq!(
            webhook_trigger_key("unit", "abc", &spec, &headers),
            "webhook:unit:x-test-delivery:delivery-1"
        );
    }

    #[test]
    fn verifies_hmac_signature() {
        let raw_body = br#"{"hello":"signed"}"#;
        let secret_ref = "CENTRAUR_TEST_WEBHOOK_SECRET";
        unsafe {
            env::set_var(secret_ref, "test-webhook-secret");
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-webhook-secret").unwrap();
        mac.update(raw_body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        let mut headers = HeaderMap::new();
        headers.insert("x-test-signature", signature.parse().unwrap());
        verify_hmac_signature(
            "X-Test-Signature",
            "sha256=",
            "hex",
            secret_ref,
            &headers,
            raw_body,
        )
        .unwrap();
    }

    fn webhook_filter(value: Value) -> WebhookFilter {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn webhook_filter_evaluates() {
        let body = json!({
            "repository": {"full_name": "ethereum-optimism/ethereum-optimism.github.io"},
            "comment": {"body": "/review claude please"}
        });
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", "issue_comment".parse().unwrap());

        // union: a /review comment on any repo matches the command branch
        let filter = webhook_filter(json!({
            "any": [
                {"source": "header", "key": "x-github-event", "op": "equals", "value": "pull_request"},
                {"all": [
                    {"source": "header", "key": "x-github-event", "op": "equals", "value": "issue_comment"},
                    {"source": "body", "key": "comment.body", "op": "contains", "value": "/review"}
                ]}
            ]
        }));
        assert!(webhook_filter_matches(&filter, &headers, &body));

        // a plain comment on an unrelated repo doesn't match a repo-scoped filter
        let other = json!({
            "repository": {"full_name": "ethereum-optimism/k8s"},
            "comment": {"body": "lgtm"}
        });
        let repo_filter = webhook_filter(json!({
            "all": [
                {"source": "header", "key": "x-github-event", "op": "equals", "value": "issue_comment"},
                {"source": "body", "key": "repository.full_name", "op": "in",
                 "values": ["ethereum-optimism/ethereum-optimism.github.io"]}
            ]
        }));
        assert!(!webhook_filter_matches(&repo_filter, &headers, &other));

        // missing field -> no match.
        let missing = webhook_filter(json!({
            "source": "body",
            "key": "a.b",
            "op": "equals",
            "value": "x"
        }));
        assert!(!webhook_filter_matches(&missing, &headers, &body));

        let unknown_source = webhook_filter(json!({
            "source": "query",
            "key": "event",
            "op": "equals",
            "value": "issue_comment"
        }));
        assert!(!webhook_filter_matches(&unknown_source, &headers, &body));

        let unknown_op = webhook_filter(json!({
            "source": "body",
            "key": "comment.body",
            "op": "regex",
            "value": "/review"
        }));
        assert!(!webhook_filter_matches(&unknown_op, &headers, &body));

        let empty = webhook_filter(json!({}));
        assert!(!webhook_filter_matches(&empty, &headers, &body));
    }

    #[test]
    fn verifies_uppercase_hex_hmac_signature() {
        let raw_body = br#"{"hello":"signed"}"#;
        let secret_ref = "CENTRAUR_TEST_WEBHOOK_SECRET_UPPER";
        unsafe {
            env::set_var(secret_ref, "test-webhook-secret");
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-webhook-secret").unwrap();
        mac.update(raw_body);
        let signature = format!(
            "sha256={}",
            hex::encode(mac.finalize().into_bytes()).to_uppercase()
        );
        let mut headers = HeaderMap::new();
        headers.insert("x-test-signature", signature.parse().unwrap());
        verify_hmac_signature(
            "X-Test-Signature",
            "sha256=",
            "hex",
            secret_ref,
            &headers,
            raw_body,
        )
        .unwrap();
    }

    #[test]
    fn verifies_base64_hmac_signature() {
        let raw_body = br#"{"hello":"signed"}"#;
        let secret_ref = "CENTRAUR_TEST_WEBHOOK_SECRET_B64";
        unsafe {
            env::set_var(secret_ref, "test-webhook-secret");
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-webhook-secret").unwrap();
        mac.update(raw_body);
        let signature = general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        let mut headers = HeaderMap::new();
        headers.insert("x-test-signature", signature.parse().unwrap());
        verify_hmac_signature(
            "X-Test-Signature",
            "",
            "base64",
            secret_ref,
            &headers,
            raw_body,
        )
        .unwrap();
    }

    #[test]
    fn rejects_invalid_hmac_signature() {
        let secret_ref = "CENTRAUR_TEST_WEBHOOK_SECRET_REJECT";
        unsafe {
            env::set_var(secret_ref, "test-webhook-secret");
        }
        let mut headers = HeaderMap::new();
        for bad_signature in [
            // Valid hex, wrong digest.
            format!("sha256={}", hex::encode([0_u8; 32])),
            // Missing prefix.
            hex::encode([0_u8; 32]),
            // Not decodable.
            "sha256=not-hex".to_owned(),
        ] {
            headers.insert("x-test-signature", bad_signature.parse().unwrap());
            let error = verify_hmac_signature(
                "X-Test-Signature",
                "sha256=",
                "hex",
                secret_ref,
                &headers,
                br#"{"hello":"signed"}"#,
            )
            .unwrap_err();
            assert!(matches!(error, ApiError::Unauthorized(_)));
        }
    }

    #[test]
    fn missing_webhook_secret_is_internal_error() {
        let mut headers = HeaderMap::new();
        headers.insert("x-test-signature", "sha256=00".parse().unwrap());
        let error = verify_hmac_signature(
            "X-Test-Signature",
            "sha256=",
            "hex",
            "CENTRAUR_TEST_WEBHOOK_SECRET_UNSET",
            &headers,
            b"{}",
        )
        .unwrap_err();
        assert!(matches!(error, ApiError::Internal(_)));
    }
}
