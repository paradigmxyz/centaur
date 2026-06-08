use std::{
    convert::Infallible,
    convert::TryFrom,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{MatchedPath, Path, Query, Request, State},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use centaur_session_core::{Session, ThreadKey};
use centaur_session_runtime::{ExecuteSessionInput, SandboxRuntime, SessionRuntime};
use centaur_session_sqlx::PgSessionStore;
use centaur_telemetry::{
    PrometheusHandle, http_status_class, prometheus_handle, record_http_request_finished,
    record_http_request_started,
};
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::{
    ApiError,
    types::{
        AppendMessagesRequest, AppendMessagesResponse, CreateSessionRequest, EventsQuery,
        ExecuteSessionRequest, ExecuteSessionResponse, SessionSseEvent, stream_error_sse,
    },
};

#[derive(Clone)]
pub struct AppState {
    runtime: SessionRuntime,
    metrics: PrometheusHandle,
}

pub fn build_router_with_runtime(store: PgSessionStore, sandbox_runtime: SandboxRuntime) -> Router {
    build_router_with_session_runtime(SessionRuntime::new(store, sandbox_runtime))
}

pub fn build_router_with_session_runtime(runtime: SessionRuntime) -> Router {
    let metrics_handle =
        prometheus_handle().expect("failed to initialize Prometheus metrics recorder");
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/api/session/{thread_key}", post(create_or_get_session))
        .route("/api/session/{thread_key}/messages", post(append_messages))
        .route("/api/session/{thread_key}/execute", post(execute_session))
        .route("/api/session/{thread_key}/events", get(stream_events))
        .route("/api/sandboxes/drain", post(drain_sandboxes))
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
        .with_state(AppState {
            runtime,
            metrics: metrics_handle,
        })
}

async fn healthz() -> Json<Value> {
    Json(json!({"ok": true}))
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
) -> Result<Json<Session>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let session = state
        .runtime
        .create_or_get_session(
            &thread_key,
            &request.harness_type,
            request.persona_id.as_deref(),
            request.metadata,
        )
        .await?;
    Ok(Json(session))
}

async fn append_messages(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<AppendMessagesRequest>,
) -> Result<Json<AppendMessagesResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let message_ids = state
        .runtime
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
        .runtime
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
    let report = state.runtime.drain().await?;
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
        .runtime
        .stream_events(
            &thread_key,
            query.after_event_id.unwrap_or(0),
            query.execution_id.as_deref(),
        )
        .await?;
    let stream = events.map(|result| {
        let sse = match result {
            Ok(event) => SessionSseEvent::try_from(event)
                .map(Event::from)
                .unwrap_or_else(|error| stream_error_sse(error.to_string())),
            Err(error) => stream_error_sse(error.to_string()),
        };
        Ok(sse)
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
