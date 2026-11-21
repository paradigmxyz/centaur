use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    convert::TryFrom,
    env,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use axum::{
    Extension, Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{any, delete, get, post},
};
use base64::{Engine as _, engine::general_purpose};
use centaur_session_core::{ChatDestination, ThreadKey};
use centaur_session_runtime::{
    ExecuteSessionInput, HarnessConflictPolicy, SandboxRuntime, SessionPrincipalRegistrar,
    SessionRuntime, SessionRuntimeError,
};
use centaur_session_sqlx::{PgSessionStore, SessionStoreError};
use centaur_telemetry::{
    PrometheusHandle, http_status_class, prometheus_handle, record_api_authentication,
    record_http_request_finished, record_http_request_started,
};
use centaur_workflows::{
    CreateWorkflowRunRequest, WebhookFilter, WorkflowRuntime, WorkflowWebhookAuth,
    WorkflowWebhookSpec, WorkflowWebhookTriggerKey,
};
use futures_util::{Stream, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::{
    ApiError,
    api_clients::{
        ApiClientContext, api_client_metadata, api_client_read_access_ended, end_events_when,
        issue_api_client_token, refresh_api_client, select_principal,
    },
    api_jwt::{bearer_jwt_from_headers, decode_jwt_payload, verify_console_jwt},
    auth::{ApiAuthConfig, AuthenticatedCaller, CallerClass, Capability},
    mcp::{mcp_enabled, mcp_get, mcp_post, mcp_protected_resource_metadata},
    slack_external_teams::{get_slack_external_team_ids, replace_slack_external_team_ids},
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
    auth: ApiAuthConfig,
    /// Console's database, read for API client authentication.
    console_db: Option<PgPool>,
}

#[derive(Clone)]
struct AppRuntimeState {
    runtime: SessionRuntime,
    workflows: Option<WorkflowRuntime>,
    pool: Option<PgPool>,
}

impl AppState {
    pub fn unready(auth: ApiAuthConfig) -> Self {
        Self {
            initialized: Arc::new(RwLock::new(None)),
            metrics: prometheus_handle().expect("failed to initialize Prometheus metrics recorder"),
            auth,
            console_db: None,
        }
    }

    /// Enables API clients, which Console stores in its database.
    pub fn with_console_db(mut self, pool: PgPool) -> Self {
        self.console_db = Some(pool);
        self
    }

    pub(crate) fn auth(&self) -> &ApiAuthConfig {
        &self.auth
    }

    pub(crate) fn console_db(&self) -> Result<PgPool, ApiError> {
        self.console_db.clone().ok_or_else(|| {
            ApiError::ServiceUnavailable("API clients are not configured".to_owned())
        })
    }

    pub fn ready(
        runtime: SessionRuntime,
        workflows: Option<WorkflowRuntime>,
        auth: ApiAuthConfig,
    ) -> Self {
        Self::ready_with_pool(runtime, workflows, None, auth)
    }

    pub fn ready_with_pool(
        runtime: SessionRuntime,
        workflows: Option<WorkflowRuntime>,
        pool: Option<PgPool>,
        auth: ApiAuthConfig,
    ) -> Self {
        let state = Self::unready(auth);
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

    pub(crate) fn workflows(&self) -> Result<WorkflowRuntime, ApiError> {
        let initialized = self
            .initialized()
            .ok_or_else(|| ApiError::ServiceUnavailable("api-rs is still starting".to_owned()))?;
        initialized
            .workflows
            .ok_or_else(|| ApiError::BadRequest("workflow runtime is not enabled".to_owned()))
    }

    pub(crate) fn pool(&self) -> Result<PgPool, ApiError> {
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
    "webhook-signature",
    "stripe-signature",
];

pub fn build_router_with_runtime(
    store: PgSessionStore,
    sandbox_runtime: SandboxRuntime,
    iron_control: impl SessionPrincipalRegistrar + 'static,
    auth: ApiAuthConfig,
) -> Router {
    let pool = store.pool().clone();
    build_router_with_app_state(AppState::ready_with_pool(
        SessionRuntime::new(store, sandbox_runtime, iron_control),
        None,
        Some(pool),
        auth,
    ))
}

pub fn build_router_with_session_runtime(runtime: SessionRuntime, auth: ApiAuthConfig) -> Router {
    build_router_with_session_and_workflow_runtime(runtime, None, auth)
}

pub fn build_router_with_session_and_workflow_runtime(
    runtime: SessionRuntime,
    workflows: Option<WorkflowRuntime>,
    auth: ApiAuthConfig,
) -> Router {
    build_router_with_app_state(AppState::ready(runtime, workflows, auth))
}

pub fn build_router_with_app_state(state: AppState) -> Router {
    let protected = Router::new()
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
        .route(
            "/api/session/{thread_key}/pause",
            post(pause_session_sandbox),
        )
        .route(
            "/api/session/{thread_key}/sandbox",
            delete(release_session_sandbox),
        )
        .route("/api/sandboxes/drain", post(drain_sandboxes))
        .route(
            "/api/slack/external-team-ids",
            get(get_slack_external_team_ids).put(replace_slack_external_team_ids),
        )
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
            "/api/scheduled-tasks/{task_id}/runs",
            post(crate::scheduled_tasks::run_scheduled_task),
        )
        .route(
            "/api/workflows/actions/invoke",
            post(invoke_workflow_button),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            authorize_api_request,
        ));

    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/api/webhooks/{slug}", any(invoke_workflow_webhook))
        .merge(protected);
    if mcp_enabled() {
        app = app
            .route("/mcp", post(mcp_post).get(mcp_get))
            .route(
                "/.well-known/oauth-protected-resource",
                get(mcp_protected_resource_metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                get(mcp_protected_resource_metadata),
            );
    }
    let app = if state.auth().api_clients_enabled() {
        app.route("/oauth/token", post(issue_api_client_token))
    } else {
        app
    };

    app.layer(
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

#[derive(Clone, Copy)]
enum RouteAccess {
    Capability(Capability),
    PrincipalOnly,
    /// Session write access, held by an API client (confined to its own
    /// namespace) or an operator; ingress keys are excluded.
    ApiClientOrOperatorSessionWrite,
}

async fn authorize_api_request(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let route = matched_route(&request);
    let method = request.method().clone();
    let mut caller = match state.auth.authenticate(request.headers()) {
        Ok(caller) => caller,
        Err(error) => {
            record_api_authentication("unknown", "unauthorized");
            return error.into_response();
        }
    };
    let api_client = if caller.class() == CallerClass::ApiClient {
        let refreshed = match state.console_db() {
            Ok(pool) => refresh_api_client(&pool, &mut caller).await,
            Err(error) => Err(error),
        };
        match refreshed {
            Ok(client) => Some(client),
            Err(error) => {
                record_api_authentication(caller.class().as_str(), "unauthorized");
                return error.into_response();
            }
        }
    } else {
        None
    };
    let Some(access) = route_access(&method, &route) else {
        record_api_authentication(caller.class().as_str(), "forbidden");
        tracing::warn!(
            caller_class = caller.class().as_str(),
            caller_identity = if caller.class() == CallerClass::Principal {
                "principal"
            } else {
                caller.identity()
            },
            http_method = method.as_str(),
            http_route = route,
            "authenticated caller denied because route has no authorization policy"
        );
        return ApiError::Forbidden("caller is not authorized for this route".to_owned())
            .into_response();
    };

    let allowed = match access {
        RouteAccess::Capability(capability) => caller.has_capability(capability),
        RouteAccess::PrincipalOnly => caller.class() == CallerClass::Principal,
        RouteAccess::ApiClientOrOperatorSessionWrite => {
            caller.has_capability(Capability::SessionsWrite)
                && matches!(
                    caller.class(),
                    CallerClass::ApiClient | CallerClass::Admin | CallerClass::Console
                )
        }
    };
    if !allowed {
        record_api_authentication(caller.class().as_str(), "forbidden");
        tracing::warn!(
            caller_class = caller.class().as_str(),
            caller_identity = if caller.class() == CallerClass::Principal {
                "principal"
            } else {
                caller.identity()
            },
            http_method = method.as_str(),
            http_route = route,
            "authenticated caller lacks route capability"
        );
        return ApiError::Forbidden("caller is not authorized for this route".to_owned())
            .into_response();
    }

    if let Some(prefixes) = caller.platform_prefixes()
        && route.starts_with("/api/session/")
        && let Some(thread_key) = session_thread_key_from_request(&request)
        && !thread_key_matches_platform(prefixes, thread_key.as_str())
    {
        record_api_authentication(caller.class().as_str(), "forbidden");
        tracing::warn!(
            caller_class = caller.class().as_str(),
            caller_identity = if caller.class() == CallerClass::Principal {
                "principal"
            } else {
                caller.identity()
            },
            expected_thread_prefixes = ?prefixes,
            "ingress caller denied for another platform's session"
        );
        return ApiError::Forbidden("caller is not authorized for this session".to_owned())
            .into_response();
    }

    if let Some(client) = &api_client
        && !api_client_may_touch(client, &route, session_thread_key_from_request(&request))
    {
        record_api_authentication(caller.class().as_str(), "forbidden");
        tracing::warn!(
            caller_class = caller.class().as_str(),
            caller_identity = caller.identity(),
            http_route = route,
            "API client denied outside its session namespace"
        );
        return ApiError::Forbidden("caller is not authorized for this session".to_owned())
            .into_response();
    }

    record_api_authentication(caller.class().as_str(), "authorized");
    request.extensions_mut().insert(caller);
    if let Some(client) = api_client {
        request.extensions_mut().insert(client);
    }
    next.run(request).await
}

/// API clients reach only session routes, and only for thread keys in their
/// own `api:<client id>:` namespace.
pub(crate) fn api_client_may_touch(
    client: &ApiClientContext,
    route: &str,
    thread_key: Option<ThreadKey>,
) -> bool {
    route.starts_with("/api/session/")
        && thread_key.is_some_and(|thread_key| client.owns_thread_key(&thread_key))
}

fn route_access(method: &Method, route: &str) -> Option<RouteAccess> {
    let capability = |capability| Some(RouteAccess::Capability(capability));
    match (method, route) {
        (&Method::GET, "/api/session/{thread_key}")
        | (&Method::GET, "/api/session/{thread_key}/events") => {
            capability(Capability::SessionsRead)
        }
        (&Method::POST, "/api/session/{thread_key}")
        | (&Method::POST, "/api/session/{thread_key}/messages")
        | (&Method::POST, "/api/session/{thread_key}/execute")
        | (&Method::POST, "/api/session/{thread_key}/interrupt") => {
            capability(Capability::SessionsWrite)
        }
        (&Method::DELETE, "/api/session/{thread_key}/sandbox") => {
            Some(RouteAccess::ApiClientOrOperatorSessionWrite)
        }
        (&Method::POST, "/api/sandboxes/drain")
        | (&Method::POST, "/api/session/{thread_key}/pause") => {
            capability(Capability::SandboxesDrain)
        }
        (&Method::GET, "/api/workflows/schedules")
        | (&Method::GET, "/api/workflows/runs")
        | (&Method::GET, "/api/workflows/runs/{run_id}") => capability(Capability::WorkflowsRead),
        (&Method::POST, "/api/workflows/runs")
        | (&Method::POST, "/api/workflows/runs/{run_id}/cancel")
        | (&Method::POST, "/api/scheduled-tasks/{task_id}/runs") => {
            capability(Capability::WorkflowsWrite)
        }
        (&Method::POST, "/api/workflows/events") => capability(Capability::WorkflowsEvents),
        (&Method::POST, "/api/workflows/actions/invoke") => {
            capability(Capability::WorkflowsActions)
        }
        (&Method::GET | &Method::HEAD, "/api/slack/external-team-ids") => {
            capability(Capability::SlackExternalTeamsRead)
        }
        (&Method::PUT, "/api/slack/external-team-ids") => {
            capability(Capability::SlackExternalTeamsWrite)
        }
        // Keep other methods out of the principal-only `/api/slack/` family.
        (_, "/api/slack/external-team-ids") => None,
        (_, route) if route.starts_with("/api/slack/") => Some(RouteAccess::PrincipalOnly),
        _ => None,
    }
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

/// Whether an ingress caller scoped to `prefixes` may touch this session.
/// A bot can mint several thread-key families (githubbot: `github:`,
/// `github-manage:`, `github-review:`), so the caller carries them all.
pub(crate) fn thread_key_matches_platform(prefixes: &[&str], thread_key: &str) -> bool {
    prefixes.iter().any(|prefix| thread_key.starts_with(prefix))
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
    api_client: Option<Extension<ApiClientContext>>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let harness_type = request.harness_type;
    let runtime = state.runtime()?;
    let on_harness_conflict = match request.on_harness_conflict {
        Some(OnHarnessConflict::Restart) => HarnessConflictPolicy::Restart,
        Some(OnHarnessConflict::Reject) | None => HarnessConflictPolicy::Reject,
    };
    let outcome = match api_client {
        Some(Extension(client)) => {
            let bound = match runtime.session(&thread_key).await {
                Ok(session) => session.iron_control_principal,
                Err(SessionRuntimeError::Store(SessionStoreError::NotFound { .. })) => None,
                Err(error) => return Err(error.into()),
            };
            let attached = client.principals(&state.console_db()?).await?;
            let principal =
                select_principal(&attached, request.principal.as_deref(), bound.as_deref())?;
            runtime
                .create_or_get_session_with_principal(
                    &thread_key,
                    &harness_type,
                    request.persona_id.as_deref(),
                    Some(api_client_metadata(client.client_id(), request.metadata)),
                    on_harness_conflict,
                    Some(&principal),
                )
                .await?
        }
        None if request.principal.is_some() => {
            return Err(ApiError::BadRequest(
                "principal is accepted only from API clients".to_owned(),
            ));
        }
        None => {
            runtime
                .create_or_get_admitted_session(
                    &thread_key,
                    &harness_type,
                    request.persona_id.as_deref(),
                    request.metadata,
                    on_harness_conflict,
                )
                .await?
        }
    };
    let persona_model = runtime.persona_model(&outcome.session).map(str::to_owned);
    Ok(Json(CreateSessionResponse {
        session: outcome.session,
        harness_switched: outcome.harness_switched,
        unavailable_requested_persona_id: outcome.unavailable_requested_persona_id,
        persona_model,
    }))
}

async fn get_session_context(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedCaller>,
    Path(raw_thread_key): Path<String>,
) -> Result<Json<SessionContextResponse>, ApiError> {
    let runtime = state.runtime()?;
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    authorize_principal_session_read(&runtime, &caller, &thread_key).await?;
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

async fn append_messages(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<AppendMessagesRequest>,
) -> Result<Json<AppendMessagesResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let outcome = state
        .runtime()?
        .append_messages_with_steering(&thread_key, &request.messages)
        .await?;
    Ok(Json(AppendMessagesResponse {
        ok: true,
        message_ids: outcome.message_ids,
        steering: outcome.steering,
    }))
}

async fn execute_session(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedCaller>,
    api_client: Option<Extension<ApiClientContext>>,
    Path(raw_thread_key): Path<String>,
    Json(request): Json<ExecuteSessionRequest>,
) -> Result<Json<ExecuteSessionResponse>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let runtime = state.runtime()?;
    let metadata = match api_client {
        Some(Extension(client)) => {
            admit_api_client_execution(&state, &runtime, &client, &thread_key).await?;
            Some(api_client_metadata(client.client_id(), request.metadata))
        }
        None => sanitize_execute_metadata(caller.class(), request.metadata),
    };
    let execution = runtime
        .enqueue_session_execution(
            &thread_key,
            ExecuteSessionInput {
                idempotency_key: request.idempotency_key,
                metadata,
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

/// An API client may start a turn only while the session's principal is still
/// attached to it.
async fn admit_api_client_execution(
    state: &AppState,
    runtime: &SessionRuntime,
    client: &ApiClientContext,
    thread_key: &ThreadKey,
) -> Result<(), ApiError> {
    let session = runtime.session(thread_key).await?;
    let Some(bound) = session.iron_control_principal.as_deref() else {
        return Err(ApiError::Forbidden(
            "the session has no principal attached to this API client".to_owned(),
        ));
    };
    let attached = client.principals(&state.console_db()?).await?;
    select_principal(&attached, None, Some(bound))?;
    Ok(())
}

/// `requester_principal_foreign_id` is an identity assertion made by the
/// authenticated Console service, not ordinary caller-controlled metadata.
/// Strip it from every other caller class before the execution is persisted so
/// the runtime can safely honor Console requesters on any thread namespace.
fn sanitize_execute_metadata(
    caller_class: CallerClass,
    mut metadata: Option<Value>,
) -> Option<Value> {
    if caller_class != CallerClass::Console
        && let Some(Value::Object(fields)) = metadata.as_mut()
    {
        fields.remove("requester_principal_foreign_id");
    }
    metadata
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

#[derive(Debug, Default, serde::Deserialize)]
struct DrainQuery {
    /// When true, stop every non-terminal sandbox. Defaults to false, which
    /// stops only sandboxes durably known to be idle.
    #[serde(default)]
    force: bool,
}

async fn drain_sandboxes(
    State(state): State<AppState>,
    Query(query): Query<DrainQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = state.runtime()?.drain(query.force).await?;
    let failed = report
        .failed
        .iter()
        .map(|failure| json!({ "sandbox_id": failure.sandbox_id, "error": failure.error }))
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "ok": report.failed.is_empty(),
        "stopped_count": report.stopped.len(),
        "stopped": report.stopped,
        "busy_count": report.busy.len(),
        "busy": report.busy,
        "failed": failed,
    })))
}

/// Pauses a session's sandbox now, as its idle timeout would; the next turn
/// resumes it. An operator action, so it needs the sandbox-control capability.
async fn pause_session_sandbox(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let paused = state.runtime()?.pause_idle_session(&thread_key).await?;
    Ok(Json(json!({ "ok": true, "paused": paused })))
}

/// Stops and deletes a session's sandbox for a caller that will not use it
/// again, such as an eval run; a later turn gets a fresh sandbox. Refused
/// while a turn is queued or running, or for a workflow-owned session. API
/// clients reach only sessions in their own namespace, so they release only
/// sandboxes they created.
async fn release_session_sandbox(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let sandbox_id = state
        .runtime()?
        .release_session_sandbox(&thread_key)
        .await?;
    Ok(Json(json!({
        "ok": true,
        "released": sandbox_id.is_some(),
        "sandbox_id": sandbox_id,
    })))
}

async fn stream_events(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedCaller>,
    Path(raw_thread_key): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let runtime = state.runtime()?;
    authorize_principal_session_read(&runtime, &caller, &thread_key).await?;
    let events = runtime
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
    // An API client's access is checked per request; an open stream re-checks
    // it so revocation and token expiry also end long-lived streams.
    let stream = if caller.class() == CallerClass::ApiClient {
        let ended = api_client_read_access_ended(state.console_db()?, caller.clone());
        end_events_when(stream, ended).boxed()
    } else {
        stream.boxed()
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn authorize_principal_session_read(
    runtime: &SessionRuntime,
    caller: &AuthenticatedCaller,
    thread_key: &ThreadKey,
) -> Result<(), ApiError> {
    let Some(subject) = caller.principal_subject() else {
        return Ok(());
    };
    let session = runtime.session(thread_key).await?;
    if principal_subject_owns_session(Some(subject), session.iron_control_principal.as_deref()) {
        return Ok(());
    }
    Err(ApiError::Forbidden(
        "caller is not authorized for this session".to_owned(),
    ))
}

fn principal_subject_owns_session(subject: Option<&str>, session_principal: Option<&str>) -> bool {
    subject.is_none_or(|subject| session_principal == Some(subject))
}

/// A workflow that declares `WORKFLOW_STARTERS` may only be started through
/// the API by those caller classes; undeclared workflows accept any caller
/// with workflow write access.
pub(crate) fn caller_may_start_workflow(
    starters: Option<&BTreeSet<String>>,
    caller: CallerClass,
) -> bool {
    starters.is_none_or(|starters| starters.contains(caller.as_str()))
}

#[cfg(test)]
mod session_authorization_tests {
    use std::collections::BTreeSet;

    use super::{
        CallerClass, caller_may_start_workflow, principal_subject_owns_session,
        sanitize_execute_metadata,
    };
    use serde_json::json;

    #[test]
    fn declared_workflow_starters_limit_which_callers_may_start_a_run() {
        let console_only = BTreeSet::from(["console".to_owned()]);
        assert!(caller_may_start_workflow(
            Some(&console_only),
            CallerClass::Console
        ));
        for caller_class in [
            CallerClass::Admin,
            CallerClass::Ingress,
            CallerClass::Principal,
        ] {
            assert!(!caller_may_start_workflow(
                Some(&console_only),
                caller_class
            ));
        }
        assert!(!caller_may_start_workflow(
            Some(&BTreeSet::new()),
            CallerClass::Console
        ));
        assert!(caller_may_start_workflow(None, CallerClass::Principal));
    }

    #[test]
    fn principal_session_reads_require_exact_persisted_owner() {
        assert!(principal_subject_owns_session(None, Some("prn_owner")));
        assert!(principal_subject_owns_session(
            Some("prn_owner"),
            Some("prn_owner")
        ));
        assert!(!principal_subject_owns_session(
            Some("prn_other"),
            Some("prn_owner")
        ));
        assert!(!principal_subject_owns_session(Some("prn_owner"), None));
    }

    #[test]
    fn only_console_callers_may_assert_a_requester_principal_foreign_id() {
        let metadata = json!({
            "source": "console",
            "requester_principal_foreign_id": "console-user-ada"
        });

        assert_eq!(
            sanitize_execute_metadata(CallerClass::Console, Some(metadata.clone())),
            Some(metadata.clone())
        );
        for caller_class in [
            CallerClass::Admin,
            CallerClass::Ingress,
            CallerClass::Principal,
        ] {
            assert_eq!(
                sanitize_execute_metadata(caller_class, Some(metadata.clone())),
                Some(json!({ "source": "console" }))
            );
        }
    }
}

async fn invoke_workflow_button(
    State(state): State<AppState>,
    Json(request): Json<centaur_workflows::slack_buttons::Invocation>,
) -> Result<Json<Value>, ApiError> {
    let feedback =
        centaur_workflows::slack_button_feedback::ButtonFeedback::from_invocation(&request);
    let request = state.auth.verify_workflow_button(request)?;
    let run = workflow_runtime(&state)?
        .create_button_run(request, feedback)
        .await?;
    Ok(Json(serde_json::to_value(run)?))
}

async fn create_workflow_run(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedCaller>,
    Json(request): Json<CreateWorkflowRunRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let starters = workflows.workflow_starters(&request.workflow_name);
    if !caller_may_start_workflow(starters.as_ref(), caller.class()) {
        return Err(ApiError::Forbidden(format!(
            "caller is not allowed to start workflow {:?}",
            request.workflow_name.trim()
        )));
    }
    let run = workflows.create_run(request).await?;
    Ok(Json(serde_json::to_value(run)?))
}

async fn list_workflow_runs(
    State(state): State<AppState>,
    Query(query): Query<ListWorkflowRunsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workflows = workflow_runtime(&state)?;
    let runs = workflows
        .list_runs(query.limit.unwrap_or(50), query.workflow_name.as_deref())
        .await?;
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
        WorkflowWebhookAuth::StandardWebhooks { secret_ref } => {
            verify_standard_webhook_signature(secret_ref, headers, raw_body)
        }
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

fn verify_standard_webhook_signature(
    secret_ref: &str,
    headers: &HeaderMap,
    raw_body: &[u8],
) -> Result<(), ApiError> {
    let secret = env::var(secret_ref).map_err(|_| {
        ApiError::Internal(format!(
            "webhook auth secret {secret_ref} is not configured"
        ))
    })?;
    let secret = secret.trim();
    let encoded_secret = secret.strip_prefix("whsec_").unwrap_or(secret);
    if encoded_secret.is_empty() {
        return Err(ApiError::Internal(format!(
            "webhook auth secret {secret_ref} is not valid Standard Webhooks key material"
        )));
    }
    let webhook = standardwebhooks::Webhook::new(secret).map_err(|_| {
        ApiError::Internal(format!(
            "webhook auth secret {secret_ref} is not valid Standard Webhooks key material"
        ))
    })?;
    webhook
        .verify(raw_body, headers)
        .map_err(|_| ApiError::Unauthorized("invalid webhook signature".to_owned()))
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
        WorkflowWebhookAuth::StandardWebhooks { .. } => Some("webhook-signature"),
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
mod webhook_tests {
    use super::*;
    use time::OffsetDateTime;

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
    fn redacts_standard_webhooks_signature_header() {
        let mut headers = HeaderMap::new();
        headers.insert("webhook-signature", "v1,c2lnbmF0dXJl".parse().unwrap());
        headers.insert("webhook-id", "msg_test".parse().unwrap());
        headers.insert("webhook-timestamp", "1700000000".parse().unwrap());
        let spec = WorkflowWebhookSpec {
            slug: "unit".to_owned(),
            provider: None,
            auth: WorkflowWebhookAuth::StandardWebhooks {
                secret_ref: "TEST_WEBHOOK_SECRET".to_owned(),
            },
            trigger_key: None,
            allowed_methods: vec!["POST".to_owned()],
            allowed_content_types: vec!["application/json".to_owned()],
            filter: None,
        };

        let safe = safe_webhook_headers(&headers, &spec);

        assert_eq!(
            safe,
            json!({
                "webhook-id": "msg_test",
                "webhook-timestamp": "1700000000"
            })
        );
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

    fn standard_webhook_headers(
        secret: &str,
        message_id: &str,
        timestamp: i64,
        raw_body: &[u8],
    ) -> HeaderMap {
        let encoded_secret = secret.strip_prefix("whsec_").unwrap_or(secret);
        let key = general_purpose::STANDARD.decode(encoded_secret).unwrap();
        let mut signed_content = Vec::new();
        signed_content.extend_from_slice(message_id.as_bytes());
        signed_content.push(b'.');
        signed_content.extend_from_slice(timestamp.to_string().as_bytes());
        signed_content.push(b'.');
        signed_content.extend_from_slice(raw_body);
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
        mac.update(&signed_content);
        let signature = general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("webhook-id", message_id.parse().unwrap());
        headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
        headers.insert(
            "webhook-signature",
            format!("v2,ignored v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= v1,{signature}")
                .parse()
                .unwrap(),
        );
        headers
    }

    #[test]
    fn verifies_standard_webhooks_signature() {
        let raw_body = br#"{"hello":"signed"}"#;
        let secret_ref = "CENTRAUR_TEST_STANDARD_WEBHOOK_SECRET";
        let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
        let timestamp = OffsetDateTime::now_utc().unix_timestamp();
        unsafe {
            env::set_var(secret_ref, secret);
        }
        let headers = standard_webhook_headers(secret, "msg_test", timestamp, raw_body);

        verify_standard_webhook_signature(secret_ref, &headers, raw_body).unwrap();
    }

    #[test]
    fn rejects_invalid_or_stale_standard_webhooks_signature() {
        let raw_body = br#"{"hello":"signed"}"#;
        let secret_ref = "CENTRAUR_TEST_STANDARD_WEBHOOK_SECRET_REJECT";
        let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
        unsafe {
            env::set_var(secret_ref, secret);
        }

        let timestamp = OffsetDateTime::now_utc().unix_timestamp();
        let headers = standard_webhook_headers(secret, "msg_test", timestamp, raw_body);
        let error =
            verify_standard_webhook_signature(secret_ref, &headers, br#"{"hello":"tampered"}"#)
                .unwrap_err();
        assert!(matches!(error, ApiError::Unauthorized(_)));

        let stale_headers = standard_webhook_headers(secret, "msg_test", timestamp - 301, raw_body);
        let error =
            verify_standard_webhook_signature(secret_ref, &stale_headers, raw_body).unwrap_err();
        assert!(matches!(error, ApiError::Unauthorized(_)));
    }

    #[test]
    fn malformed_or_empty_standard_webhooks_secret_is_internal_error() {
        let secret_ref = "CENTRAUR_TEST_STANDARD_WEBHOOK_SECRET_INVALID";
        unsafe {
            env::set_var(secret_ref, "whsec_not-base64");
        }
        let headers = standard_webhook_headers(
            "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
            "msg_test",
            OffsetDateTime::now_utc().unix_timestamp(),
            b"{}",
        );

        let error = verify_standard_webhook_signature(secret_ref, &headers, b"{}").unwrap_err();

        assert!(matches!(error, ApiError::Internal(_)));

        unsafe {
            env::set_var(secret_ref, "whsec_");
        }
        let error = verify_standard_webhook_signature(secret_ref, &headers, b"{}").unwrap_err();

        assert!(matches!(error, ApiError::Internal(_)));
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
