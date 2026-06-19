use std::{
    collections::BTreeMap,
    convert::Infallible,
    convert::TryFrom,
    env,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
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
use centaur_session_core::ThreadKey;
use centaur_session_runtime::{
    ExecuteSessionInput, HarnessConflictPolicy, SandboxRuntime, SessionRuntime,
};
use centaur_session_sqlx::PgSessionStore;
use centaur_telemetry::{
    PrometheusHandle, http_status_class, prometheus_handle, record_http_request_finished,
    record_http_request_started,
};
use centaur_workflows::{
    CreateWorkflowRunRequest, WorkflowRuntime, WorkflowWebhookAuth, WorkflowWebhookSpec,
    WorkflowWebhookTriggerKey,
};
use futures_util::{Stream, StreamExt};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::{
    ApiError,
    types::{
        AppendMessagesRequest, AppendMessagesResponse, CreateSessionRequest, CreateSessionResponse,
        EmitWorkflowEventRequest, EventsQuery, ExecuteSessionRequest, ExecuteSessionResponse,
        ListWorkflowRunsQuery, OnHarnessConflict, SessionContextResponse, SessionSseEvent,
        SlackThreadContext, stream_error_sse,
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
}

impl AppState {
    pub fn unready() -> Self {
        Self {
            initialized: Arc::new(RwLock::new(None)),
            metrics: prometheus_handle().expect("failed to initialize Prometheus metrics recorder"),
        }
    }

    pub fn ready(runtime: SessionRuntime, workflows: Option<WorkflowRuntime>) -> Self {
        let state = Self::unready();
        state.mark_ready(runtime, workflows);
        state
    }

    pub fn mark_ready(&self, runtime: SessionRuntime, workflows: Option<WorkflowRuntime>) {
        let mut initialized = self
            .initialized
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *initialized = Some(AppRuntimeState { runtime, workflows });
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

    fn runtime(&self) -> Result<SessionRuntime, ApiError> {
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
    build_router_with_session_runtime(SessionRuntime::new(store, sandbox_runtime))
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
        .route("/api/session/{thread_key}/events", get(stream_events))
        .route("/api/sandboxes/drain", post(drain_sandboxes))
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
        .route("/api/webhooks/{slug}", any(invoke_workflow_webhook))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<Body>| {
                    let route = matched_route(request);
                    tracing::info_span!(
                        "centaur.api_rs.http_request",
                        "otel.kind" = "server",
                        "otel.status_code" = tracing::field::Empty,
                        "http.request.method" = request.method().as_str(),
                        "http.route" = route.as_str(),
                        "http.response.status_code" = tracing::field::Empty,
                    )
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

async fn healthz() -> Json<Value> {
    Json(json!({"ok": true}))
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
    let _runtime = state.runtime()?;
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    Ok(Json(SessionContextResponse {
        slack: slack_thread_context(&thread_key),
        thread_key,
    }))
}

fn slack_thread_context(thread_key: &ThreadKey) -> Option<SlackThreadContext> {
    let parts = thread_key.as_str().split(':').collect::<Vec<_>>();
    let (channel_id, thread_ts) = match parts.as_slice() {
        ["slack", channel_id, thread_ts] => (*channel_id, *thread_ts),
        ["slack", _team_id, channel_id, thread_ts] => (*channel_id, *thread_ts),
        [channel_id, thread_ts] if is_slack_conversation_id(channel_id) => {
            (*channel_id, *thread_ts)
        }
        _ => return None,
    };
    if channel_id.is_empty() || thread_ts.is_empty() {
        return None;
    }
    Some(SlackThreadContext {
        channel_id: channel_id.to_owned(),
        thread_ts: thread_ts.to_owned(),
    })
}

fn is_slack_conversation_id(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'C' | b'D' | b'G'))
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
    workflows
        .emit_event(&request.event_name, request.payload)
        .await?;
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

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod webhook_tests {
    use super::*;

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
