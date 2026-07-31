pub mod config;
mod model;
mod policy;
mod registry;
mod store;

use std::{collections::HashMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use centaur_telemetry::{render_metrics, set_span_parent_trace, traceparent_for_span};
use model::{
    AuthorizeRequest, AuthorizeResponse, BeginAttempt, CompleteRequest, CompleteResponse,
    CompletionOutcome, NewAttempt, PolicyRule,
};
use mpp::{
    ChargeRequest, PrivateKeySigner,
    client::{PaymentProvider as _, TempoProvider},
    format_authorization, parse_receipt, parse_www_authenticate_all,
};
use policy::Policy;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use tower_http::trace::TraceLayer;
use tracing::{Instrument as _, Span, info_span};
use uuid::Uuid;

pub use registry::Registry;
pub use store::PgSignerStore;
use store::SignerStore;

const AUTHORIZATIONS_TOTAL: &str = "centaur_mpp_authorizations_total";
const CHARGES_TOTAL: &str = "centaur_mpp_charges_total";
const SPEND_ATOMIC_TOTAL: &str = "centaur_mpp_spend_atomic_total";
const AUTHORIZATION_DURATION_SECONDS: &str = "centaur_mpp_authorization_duration_seconds";
const SIGNING_DURATION_SECONDS: &str = "centaur_mpp_signing_duration_seconds";
const REPLAY_DURATION_SECONDS: &str = "centaur_mpp_replay_duration_seconds";
const CHARGE_DURATION_SECONDS: &str = "centaur_mpp_charge_duration_seconds";
const COMPLETION_DURATION_SECONDS: &str = "centaur_mpp_completion_duration_seconds";
const ACTIVE_RESERVATIONS: &str = "centaur_mpp_active_budget_reservations";
const BUDGET_REJECTIONS_TOTAL: &str = "centaur_mpp_budget_rejections_total";
const LEASE_CHECKS_TOTAL: &str = "centaur_mpp_execution_lease_checks_total";
const ACTIVE_EXECUTION_LEASES: &str = "centaur_mpp_active_execution_leases";

#[async_trait]
pub trait ChargeSigner: Send + Sync {
    async fn authorization(&self, challenge: &mpp::PaymentChallenge) -> anyhow::Result<String>;
}

pub struct TempoChargeSigner {
    provider: TempoProvider,
}

impl TempoChargeSigner {
    pub fn new(signer: PrivateKeySigner, rpc_url: &str) -> anyhow::Result<Self> {
        let provider = TempoProvider::new(signer, rpc_url)?.with_client_id("centaur");
        Ok(Self { provider })
    }
}

#[async_trait]
impl ChargeSigner for TempoChargeSigner {
    async fn authorization(&self, challenge: &mpp::PaymentChallenge) -> anyhow::Result<String> {
        let credential = self.provider.pay(challenge).await?;
        Ok(format_authorization(&credential)?)
    }
}

#[derive(Clone)]
pub struct AppState {
    token: Arc<Vec<u8>>,
    store: Arc<dyn SignerStore>,
    registry: Arc<Registry>,
    policy: Arc<Policy>,
    signer: Arc<dyn ChargeSigner>,
    max_per_charge_atomic: Option<i64>,
    max_daily_atomic: Option<i64>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        token: String,
        store: Arc<dyn SignerStore>,
        registry: Arc<Registry>,
        default_methods: Vec<String>,
        policy_rules: Vec<PolicyRule>,
        signer: Arc<dyn ChargeSigner>,
        max_per_charge_atomic: Option<i64>,
        max_daily_atomic: Option<i64>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            token.len() >= 32,
            "MPP signer token must be at least 32 bytes"
        );
        Ok(Self {
            token: Arc::new(token.into_bytes()),
            store,
            registry,
            policy: Arc::new(Policy::new(default_methods, policy_rules)?),
            signer,
            max_per_charge_atomic,
            max_daily_atomic,
        })
    }

    pub fn budgets_disabled(&self) -> bool {
        self.max_per_charge_atomic.is_none() && self.max_daily_atomic.is_none()
    }

    fn reservations_enabled(&self) -> bool {
        self.max_daily_atomic.is_some()
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .route("/authorize", post(authorize))
        .route("/complete", post(complete))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn describe_metrics() {
    metrics::describe_counter!(
        AUTHORIZATIONS_TOTAL,
        "MPP authorization decisions by outcome, reason, intent, method, and service."
    );
    metrics::describe_counter!(
        CHARGES_TOTAL,
        "Completed MPP charge replays by outcome and service."
    );
    metrics::describe_counter!(
        SPEND_ATOMIC_TOTAL,
        "Settled MPP spend in atomic units by currency and service."
    );
    metrics::describe_histogram!(
        AUTHORIZATION_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "MPP authorization latency."
    );
    metrics::describe_histogram!(
        SIGNING_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "MPP credential signing latency."
    );
    metrics::describe_histogram!(
        COMPLETION_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "MPP replay completion processing latency."
    );
    metrics::describe_histogram!(
        REPLAY_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "MPP authorized upstream replay latency."
    );
    metrics::describe_histogram!(
        CHARGE_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "End-to-end MPP challenge authorization and replay latency."
    );
    metrics::describe_gauge!(
        ACTIVE_RESERVATIONS,
        "MPP budget reservations currently awaiting completion."
    );
    metrics::describe_counter!(
        BUDGET_REJECTIONS_TOTAL,
        "MPP charges rejected by configured software budgets."
    );
    metrics::describe_counter!(
        LEASE_CHECKS_TOTAL,
        "MPP active execution lease checks by outcome."
    );
    metrics::describe_gauge!(
        ACTIVE_EXECUTION_LEASES,
        "Active Centaur execution leases eligible to authorize MPP charges."
    );
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        database: None,
        registry: None,
        key: Some(true),
    })
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let (database, registry) = tokio::join!(state.store.ready(), state.registry.ready());
    let status = if database && registry {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(HealthResponse {
            ok: database && registry,
            database: Some(database),
            registry: Some(registry),
            key: Some(true),
        }),
    )
}

async fn metrics(State(state): State<AppState>) -> Result<String, ApiError> {
    let (leases, reservations) = tokio::try_join!(
        state.store.active_execution_lease_count(),
        state.store.active_reservation_count(),
    )
    .map_err(ApiError::internal)?;
    metrics::gauge!(ACTIVE_EXECUTION_LEASES).set(leases as f64);
    metrics::gauge!(ACTIVE_RESERVATIONS).set(reservations as f64);
    render_metrics().map_err(ApiError::internal)
}

async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthorizeRequest>,
) -> Result<Json<AuthorizeResponse>, ApiError> {
    authenticate(&state, &headers)?;
    let execution = state
        .store
        .active_execution(&request.sandbox_id)
        .await
        .map_err(ApiError::internal)?;
    let parent_traceparent = request
        .traceparent
        .clone()
        .filter(|value| valid_traceparent(value))
        .or_else(|| {
            execution
                .as_ref()
                .map(|value| thread_traceparent(&value.thread_key))
        });
    let started = Instant::now();
    let sandbox_ref = redacted_identifier(&request.sandbox_id);
    let span = info_span!(
        "mpp.charge",
        "mpp.sandbox_ref" = %sandbox_ref,
        "mpp.service_id" = tracing::field::Empty,
        "mpp.execution_ref" = tracing::field::Empty,
        "mpp.amount_atomic" = tracing::field::Empty,
        "mpp.currency" = tracing::field::Empty,
        "mpp.outcome" = tracing::field::Empty,
    );
    if let Some(traceparent) = parent_traceparent.as_deref() {
        apply_traceparent_to(&span, traceparent);
    }
    let charge_traceparent = traceparent_for_span(&span).or(parent_traceparent);
    let result = async {
        authorize_inner(&state, request, execution)
            .instrument(info_span!("mpp.authorize"))
            .await
    }
    .instrument(span)
    .await;
    metrics::histogram!(AUTHORIZATION_DURATION_SECONDS).record(started.elapsed().as_secs_f64());
    result.map(|mut response| {
        if response.retry {
            response.traceparent = charge_traceparent;
        }
        Json(response)
    })
}

async fn authorize_inner(
    state: &AppState,
    request: AuthorizeRequest,
    execution: Option<model::ActiveExecution>,
) -> Result<AuthorizeResponse, ApiError> {
    if request.status != StatusCode::PAYMENT_REQUIRED.as_u16() {
        return Ok(decline(
            "status_not_payment_required",
            "unknown",
            &request.method,
        ));
    }
    if !request.replayable {
        return Ok(decline(
            "request_not_replayable",
            "unknown",
            &request.method,
        ));
    }

    let Some(execution) = execution else {
        metrics::counter!(LEASE_CHECKS_TOTAL, "outcome" => "rejected").increment(1);
        return Ok(decline("no_active_execution", "unknown", &request.method));
    };
    metrics::counter!(LEASE_CHECKS_TOTAL, "outcome" => "allowed").increment(1);
    let execution_ref = redacted_identifier(&execution.execution_id);
    Span::current().record("mpp.execution_ref", execution_ref.as_str());

    let challenge_headers = header_values(&request.response_headers, "www-authenticate");
    let mut selected = None;
    for parsed in parse_www_authenticate_all(challenge_headers.iter().map(String::as_str)) {
        let Ok(challenge) = parsed else {
            continue;
        };
        if challenge.method.eq_ignore_ascii_case("tempo") && challenge.intent.is_charge() {
            selected = Some(challenge);
            break;
        }
    }
    let Some(challenge) = selected else {
        return Ok(decline(
            "unsupported_or_invalid_challenge",
            "unknown",
            &request.method,
        ));
    };
    if mpp::expires::assert(challenge.expires.as_deref(), Some(&challenge.id)).is_err() {
        return Ok(decline("expired_challenge", "unknown", &request.method));
    }

    let route = match state
        .registry
        .route(&request.host, &request.method, &request.path)
        .await
    {
        Ok(route) => route,
        Err(error) => {
            tracing::warn!(error = %error, "MPP registry rejected request route");
            return Ok(decline("registry_denied", "unknown", &request.method));
        }
    };
    let service_id = route.service.id.as_str();
    Span::current().record("mpp.service_id", service_id);
    if route.endpoint.payment.as_ref().is_none_or(|payment| {
        payment
            .intent
            .as_deref()
            .is_none_or(|intent| !intent.eq_ignore_ascii_case("charge"))
            || payment
                .method
                .as_deref()
                .is_none_or(|method| !method.eq_ignore_ascii_case("tempo"))
    }) {
        return Ok(decline(
            "unsupported_registry_payment",
            service_id,
            &request.method,
        ));
    }
    let expected_realm = route.service.realm.clone().or_else(|| {
        route
            .service
            .base_url()
            .and_then(|url| reqwest::Url::parse(url).ok())
            .and_then(|url| url.host_str().map(str::to_owned))
    });
    if expected_realm
        .as_deref()
        .is_none_or(|realm| !challenge.realm.eq_ignore_ascii_case(realm))
    {
        return Ok(decline("realm_mismatch", service_id, &request.method));
    }
    let decision = state.policy.decide(&route.service, &route.endpoint);
    if !decision.allowed {
        return Ok(decline(decision.reason, service_id, &request.method));
    }

    let charge = match challenge.request.decode::<ChargeRequest>() {
        Ok(charge) => charge,
        Err(_) => {
            return Ok(decline(
                "invalid_charge_request",
                service_id,
                &request.method,
            ));
        }
    };
    let amount = match charge
        .parse_amount()
        .ok()
        .and_then(|amount| i64::try_from(amount).ok())
    {
        Some(amount) => amount,
        None => {
            return Ok(decline(
                "invalid_charge_amount",
                service_id,
                &request.method,
            ));
        }
    };
    if charge.currency.is_empty() {
        return Ok(decline(
            "invalid_charge_currency",
            service_id,
            &request.method,
        ));
    }
    Span::current().record("mpp.amount_atomic", amount);
    Span::current().record("mpp.currency", charge.currency.as_str());

    let challenge_hash = sha256_hex(&serde_json::to_vec(&challenge).map_err(ApiError::internal)?);
    let attempt_id = Uuid::new_v4();
    let attempt = NewAttempt {
        attempt_id,
        challenge_hash,
        service_id: route.service.id.clone(),
        method: route.endpoint.method.to_ascii_uppercase(),
        path_template: route.endpoint.path.clone(),
        amount_atomic: amount,
        currency: charge.currency.clone(),
        sandbox_id: execution.sandbox_id,
        execution_id: execution.execution_id,
    };
    match state
        .store
        .begin_attempt(
            &attempt,
            state.max_per_charge_atomic,
            state.max_daily_atomic,
        )
        .await
        .map_err(ApiError::internal)?
    {
        BeginAttempt::Created => {
            if state.reservations_enabled() {
                metrics::gauge!(ACTIVE_RESERVATIONS).increment(1.0);
            }
        }
        BeginAttempt::Duplicate {
            attempt_id: _,
            sandbox_id,
            execution_id,
        } => {
            if sandbox_id != attempt.sandbox_id || execution_id != attempt.execution_id {
                return Ok(decline(
                    "duplicate_challenge_identity_mismatch",
                    service_id,
                    &request.method,
                ));
            }
            return Ok(decline("duplicate_challenge", service_id, &request.method));
        }
        BeginAttempt::BudgetDenied { reason } => {
            metrics::counter!(BUDGET_REJECTIONS_TOTAL, "reason" => reason).increment(1);
            return Ok(decline(reason, service_id, &request.method));
        }
    }

    let sign_started = Instant::now();
    let authorization = match state
        .signer
        .authorization(&challenge)
        .instrument(info_span!("mpp.sign"))
        .await
    {
        Ok(authorization) => authorization,
        Err(error) => {
            state
                .store
                .mark_sign_failed(attempt_id, "signing_failed")
                .await
                .map_err(ApiError::internal)?;
            if state.reservations_enabled() {
                metrics::gauge!(ACTIVE_RESERVATIONS).decrement(1.0);
            }
            tracing::warn!(
                error = %error,
                service_id,
                execution_ref = %redacted_identifier(&attempt.execution_id),
                "MPP charge signing failed"
            );
            return Ok(decline("signing_failed", service_id, &request.method));
        }
    };
    metrics::histogram!(SIGNING_DURATION_SECONDS).record(sign_started.elapsed().as_secs_f64());
    tracing::info!(
        service_id,
        execution_ref = %redacted_identifier(&attempt.execution_id),
        amount_atomic = amount,
        currency = %charge.currency,
        "signed MPP charge credential"
    );
    state
        .store
        .mark_authorized(attempt_id)
        .await
        .map_err(ApiError::internal)?;
    Span::current().record("mpp.outcome", "authorized");
    record_authorization("retry", "authorized", service_id, &request.method);
    tracing::info!(
        service_id,
        execution_ref = %redacted_identifier(&attempt.execution_id),
        amount_atomic = amount,
        currency = %charge.currency,
        "authorized MPP charge replay"
    );
    Ok(AuthorizeResponse::retry(attempt_id, authorization))
}

async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CompleteRequest>,
) -> Result<Json<CompleteResponse>, ApiError> {
    authenticate(&state, &headers)?;
    let complete_span = info_span!(
        "mpp.complete",
        "mpp.attempt_ref" = %redacted_identifier(&request.attempt_id.to_string()),
    );
    if let Some(traceparent) = request.traceparent.as_deref() {
        apply_traceparent_to(&complete_span, traceparent);
        let replay_span = info_span!(
            "mpp.replay",
            "http.response.status_code" = request.replay_status,
            "mpp.replay_duration_ms" = request.replay_duration_ms,
        );
        apply_traceparent_to(&replay_span, traceparent);
        let _entered = replay_span.enter();
    }
    let _entered = complete_span.enter();
    let started = Instant::now();
    let receipt_header = header_values(&request.response_headers, "payment-receipt")
        .into_iter()
        .next();
    let receipt_hash = receipt_header
        .as_deref()
        .map(|receipt| sha256_hex(receipt.as_bytes()));
    let (outcome, error_code) = completion_outcome(
        request.replay_status,
        request.transport_error.as_deref(),
        receipt_header.as_deref(),
    );

    let completed = state
        .store
        .complete_attempt(
            request.attempt_id,
            outcome.clone(),
            request.replay_status,
            receipt_hash.as_deref(),
            error_code,
        )
        .await
        .map_err(ApiError::internal)?;
    if let Some(ref completed) = completed {
        if state.reservations_enabled() {
            metrics::gauge!(ACTIVE_RESERVATIONS).decrement(1.0);
        }
        metrics::counter!(
            CHARGES_TOTAL,
            "outcome" => completed.outcome.as_str(),
            "reason" => completed.reason.clone(),
            "intent" => "charge",
            "method" => metric_method(&completed.method),
            "service" => completed.service_id.clone(),
        )
        .increment(1);
        if completed.outcome == CompletionOutcome::Settled {
            metrics::counter!(
                SPEND_ATOMIC_TOTAL,
                "currency" => completed.currency.clone(),
                "service" => completed.service_id.clone(),
            )
            .increment(completed.amount_atomic as u64);
        }
        tracing::info!(
            service_id = %completed.service_id,
            amount_atomic = completed.amount_atomic,
            currency = %completed.currency,
            outcome = completed.outcome.as_str(),
            replay_status = request.replay_status,
            "completed MPP charge replay"
        );
    }
    if let Some(duration_ms) = request.replay_duration_ms {
        metrics::histogram!(REPLAY_DURATION_SECONDS).record(duration_ms as f64 / 1_000.0);
    }
    if let Some(duration_ms) = request.charge_duration_ms {
        metrics::histogram!(CHARGE_DURATION_SECONDS).record(duration_ms as f64 / 1_000.0);
    }
    metrics::histogram!(COMPLETION_DURATION_SECONDS).record(started.elapsed().as_secs_f64());
    Ok(Json(CompleteResponse {
        ok: completed.is_some(),
        outcome: outcome.as_str(),
    }))
}

fn completion_outcome(
    replay_status: Option<u16>,
    transport_error: Option<&str>,
    receipt: Option<&str>,
) -> (CompletionOutcome, Option<&'static str>) {
    let receipt_is_valid = receipt
        .map(parse_receipt)
        .transpose()
        .is_ok_and(|receipt| receipt.is_none_or(|receipt| receipt.is_success()));
    if receipt.is_some() && receipt_is_valid {
        return (CompletionOutcome::Settled, None);
    }
    if transport_error.is_some() {
        return (CompletionOutcome::Unknown, Some("transport_error"));
    }
    if replay_status.is_some_and(|status| (200..300).contains(&status)) {
        return (
            CompletionOutcome::Settled,
            (!receipt_is_valid).then_some("invalid_receipt"),
        );
    }
    if replay_status == Some(StatusCode::PAYMENT_REQUIRED.as_u16()) {
        return (CompletionOutcome::Released, Some("credential_rejected"));
    }
    if replay_status.is_some() {
        return (CompletionOutcome::Released, Some("replay_failed"));
    }
    (CompletionOutcome::Unknown, Some("missing_replay_outcome"))
}

fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::as_bytes)
        .unwrap_or_default();
    let valid = supplied.len() == state.token.len()
        && supplied.ct_eq(state.token.as_slice()).unwrap_u8() == 1;
    if valid {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            message: "unauthorized",
        })
    }
}

fn decline(reason: &'static str, service: &str, method: &str) -> AuthorizeResponse {
    record_authorization("declined", reason, service, method);
    Span::current().record("mpp.outcome", reason);
    tracing::info!(service_id = service, reason, "declined MPP charge replay");
    AuthorizeResponse::decline(reason)
}

fn record_authorization(outcome: &'static str, reason: &'static str, service: &str, method: &str) {
    metrics::counter!(
        AUTHORIZATIONS_TOTAL,
        "outcome" => outcome,
        "reason" => reason,
        "intent" => "charge",
        "method" => metric_method(method),
        "service" => service.to_owned(),
    )
    .increment(1);
}

fn header_values(headers: &HashMap<String, Vec<String>>, name: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .flat_map(|(_, values)| values.iter().cloned())
        .collect()
}

fn sha256_hex(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn redacted_identifier(value: &str) -> String {
    format!("sha256:{}", &sha256_hex(value.as_bytes())[..16])
}

fn thread_traceparent(thread_key: &str) -> String {
    let trace_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("centaur:thread:{thread_key}").as_bytes(),
    )
    .simple()
    .to_string();
    let digest = Sha256::digest(format!("centaur:thread-parent:{thread_key}"));
    let mut span_id = [0_u8; 8];
    span_id.copy_from_slice(&digest[..8]);
    if span_id.iter().all(|byte| *byte == 0) {
        span_id[7] = 1;
    }
    format!("00-{trace_id}-{}-01", hex::encode(span_id))
}

fn metric_method(method: &str) -> String {
    let method = method.to_ascii_uppercase();
    match method.as_str() {
        "DELETE" | "GET" | "HEAD" | "OPTIONS" | "PATCH" | "POST" | "PUT" => method,
        _ => "OTHER".to_owned(),
    }
}

fn apply_traceparent_to(span: &Span, traceparent: &str) {
    if !valid_traceparent(traceparent) {
        return;
    }
    let parts = traceparent.split('-').collect::<Vec<_>>();
    let _ = set_span_parent_trace(span, parts[1], parts[2]);
}

fn valid_traceparent(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0] == "00"
        && parts[1].len() == 32
        && parts[2].len() == 16
        && parts[3].len() == 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && parts[1].bytes().any(|byte| byte != b'0')
        && parts[2].bytes().any(|byte| byte != b'0')
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<bool>,
}

struct ApiError {
    status: StatusCode,
    message: &'static str,
}

impl ApiError {
    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "MPP signer request failed");
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "temporarily unavailable",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use mpp::{Base64UrlJson, PaymentChallenge, format_www_authenticate};
    use serde_json::{Value, json};
    use time::{Duration, OffsetDateTime};
    use tower::ServiceExt as _;
    use uuid::Uuid;

    use super::{
        AppState, ChargeSigner, Registry, build_router,
        model::{
            ActiveExecution, BeginAttempt, Catalog, CompletedAttempt, CompletionOutcome, Endpoint,
            NewAttempt, PaymentOffer, RegistrySnapshot, Service,
        },
        store::SignerStore,
    };

    const TOKEN: &str = "test-signer-token-that-is-at-least-32-bytes";

    struct FakeStore {
        snapshot: RegistrySnapshot,
        execution: Option<ActiveExecution>,
        attempts: AtomicUsize,
        begin_result: Mutex<BeginAttempt>,
    }

    #[async_trait]
    impl SignerStore for FakeStore {
        async fn load_registry_cache(&self) -> anyhow::Result<Option<RegistrySnapshot>> {
            Ok(Some(self.snapshot.clone()))
        }

        async fn save_registry_cache(&self, _snapshot: &RegistrySnapshot) -> anyhow::Result<()> {
            Ok(())
        }

        async fn active_execution(
            &self,
            _sandbox_id: &str,
        ) -> anyhow::Result<Option<ActiveExecution>> {
            Ok(self.execution.clone())
        }

        async fn active_execution_lease_count(&self) -> anyhow::Result<i64> {
            Ok(i64::from(self.execution.is_some()))
        }

        async fn active_reservation_count(&self) -> anyhow::Result<i64> {
            Ok(0)
        }

        async fn begin_attempt(
            &self,
            _attempt: &NewAttempt,
            _max_per_charge_atomic: Option<i64>,
            _max_daily_atomic: Option<i64>,
        ) -> anyhow::Result<BeginAttempt> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Ok(self.begin_result.lock().expect("begin result").clone())
        }

        async fn mark_authorized(&self, _attempt_id: Uuid) -> anyhow::Result<()> {
            Ok(())
        }

        async fn mark_sign_failed(
            &self,
            _attempt_id: Uuid,
            _error_code: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn complete_attempt(
            &self,
            _attempt_id: Uuid,
            _outcome: CompletionOutcome,
            _replay_status: Option<u16>,
            _receipt_hash: Option<&str>,
            _error_code: Option<&str>,
        ) -> anyhow::Result<Option<CompletedAttempt>> {
            Ok(None)
        }

        async fn ready(&self) -> bool {
            true
        }
    }

    struct FakeSigner;

    #[async_trait]
    impl ChargeSigner for FakeSigner {
        async fn authorization(&self, _challenge: &PaymentChallenge) -> anyhow::Result<String> {
            Ok("Payment credential".to_owned())
        }
    }

    fn store(with_execution: bool) -> Arc<FakeStore> {
        Arc::new(FakeStore {
            snapshot: RegistrySnapshot {
                catalog: Catalog {
                    services: vec![Service {
                        id: "test-service".to_owned(),
                        name: Some("Test Service".to_owned()),
                        description: None,
                        service_url: Some("https://service.example".to_owned()),
                        url: None,
                        realm: Some("payments.example".to_owned()),
                        categories: vec!["test".to_owned()],
                        tags: vec![],
                        status: Some("active".to_owned()),
                        endpoints: vec![
                            Endpoint {
                                method: "GET".to_owned(),
                                path: "/paid".to_owned(),
                                description: None,
                                payment: Some(PaymentOffer {
                                    intent: Some("charge".to_owned()),
                                    method: Some("tempo".to_owned()),
                                    amount: Some("1000".to_owned()),
                                    currency: Some(
                                        "0x20c000000000000000000000b9537d11c60e8b50".to_owned(),
                                    ),
                                    extra: Default::default(),
                                }),
                                extra: Default::default(),
                            },
                            Endpoint {
                                method: "POST".to_owned(),
                                path: "/paid".to_owned(),
                                description: None,
                                payment: Some(PaymentOffer {
                                    intent: Some("charge".to_owned()),
                                    method: Some("tempo".to_owned()),
                                    amount: Some("1000".to_owned()),
                                    currency: Some(
                                        "0x20c000000000000000000000b9537d11c60e8b50".to_owned(),
                                    ),
                                    extra: Default::default(),
                                }),
                                extra: Default::default(),
                            },
                        ],
                        extra: Default::default(),
                    }],
                    extra: Default::default(),
                },
                fetched_at: OffsetDateTime::now_utc(),
                etag: Some("\"v1\"".to_owned()),
                last_modified: None,
            },
            execution: with_execution.then(|| ActiveExecution {
                execution_id: "exe-test".to_owned(),
                sandbox_id: "sandbox-test".to_owned(),
                thread_key: "cli:test".to_owned(),
            }),
            attempts: AtomicUsize::new(0),
            begin_result: Mutex::new(BeginAttempt::Created),
        })
    }

    fn challenge_for(realm: &str, intent: &str, expires: &str) -> String {
        let request = Base64UrlJson::from_value(&json!({
            "amount": "1000",
            "currency": "0x20c000000000000000000000b9537d11c60e8b50",
            "recipient": "0x0000000000000000000000000000000000000001"
        }))
        .expect("charge request");
        format_www_authenticate(
            &PaymentChallenge::new(
                "challenge-with-at-least-128-bits-of-entropy",
                realm,
                "tempo",
                intent,
                request,
            )
            .with_expires(expires),
        )
        .expect("challenge header")
    }

    fn challenge() -> String {
        challenge_for("payments.example", "charge", &mpp::expires::minutes(5))
    }

    async fn state(with_execution: bool) -> (AppState, Arc<FakeStore>) {
        let store = store(with_execution);
        let registry = Arc::new(
            Registry::new(
                store.clone(),
                "https://registry.example/services",
                Duration::minutes(15),
                Duration::hours(24),
            )
            .expect("registry"),
        );
        let state = AppState::new(
            TOKEN.to_owned(),
            store.clone(),
            registry,
            vec!["GET".to_owned()],
            vec![],
            Arc::new(FakeSigner),
            None,
            None,
        )
        .expect("state");
        (state, store)
    }

    async fn authorize_request(state: AppState, method: &str, host: &str) -> (StatusCode, Value) {
        authorize_request_with(
            state,
            json!({
                "host": host,
                "method": method,
                "path": "/paid?query=kept",
                "status": 402,
                "response_headers": {"WWW-Authenticate": [challenge()]},
                "replayable": true,
                "sandbox_id": "sandbox-test",
                "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            }),
        )
        .await
    }

    async fn authorize_request_with(state: AppState, payload: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/authorize")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request");
        let response = build_router(state)
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let body = to_bytes(response.into_body(), 64 << 10)
            .await
            .expect("response body");
        (
            status,
            serde_json::from_slice(&body).expect("response JSON"),
        )
    }

    #[tokio::test]
    async fn signer_endpoints_require_proxy_authentication() {
        let (state, store) = state(true).await;
        for (uri, payload) in [
            (
                "/authorize",
                json!({
                    "host": "service.example",
                    "method": "GET",
                    "path": "/paid",
                    "status": 402,
                    "response_headers": {"WWW-Authenticate": [challenge()]},
                    "replayable": true,
                    "sandbox_id": "sandbox-test"
                }),
            ),
            (
                "/complete",
                json!({
                    "attempt_id": Uuid::new_v4(),
                    "replay_status": 200,
                    "response_headers": {},
                    "transport_error": null,
                    "traceparent": null,
                    "replay_duration_ms": 1,
                    "charge_duration_ms": 2
                }),
            ),
        ] {
            for authorization in [None, Some("Bearer wrong-token")] {
                let mut request = Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json");
                if let Some(authorization) = authorization {
                    request = request.header("authorization", authorization);
                }
                let response = build_router(state.clone())
                    .oneshot(
                        request
                            .body(Body::from(payload.to_string()))
                            .expect("request"),
                    )
                    .await
                    .expect("response");
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                let body = to_bytes(response.into_body(), 64 << 10)
                    .await
                    .expect("response body");
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).expect("response JSON"),
                    json!({"error": "unauthorized"})
                );
            }
        }
        assert_eq!(store.attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authorize_signs_registered_get_with_active_lease() {
        let (state, store) = state(true).await;

        let (status, response) = authorize_request(state, "GET", "service.example").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["retry"], true);
        assert_eq!(response["headers"]["Authorization"], "Payment credential");
        assert!(response["attempt_id"].is_string());
        assert_eq!(
            response["traceparent"],
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
        assert_eq!(store.attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn authorize_links_to_thread_trace_when_request_has_no_traceparent() {
        let (state, _) = state(true).await;
        let (status, response) = authorize_request_with(
            state,
            json!({
                "host": "service.example",
                "method": "GET",
                "path": "/paid",
                "status": 402,
                "response_headers": {"WWW-Authenticate": [challenge()]},
                "replayable": true,
                "sandbox_id": "sandbox-test"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["retry"], true);
        assert_eq!(
            response["traceparent"],
            super::thread_traceparent("cli:test")
        );
    }

    #[tokio::test]
    async fn authorize_declines_without_active_execution_lease() {
        let (state, store) = state(false).await;

        let (status, response) = authorize_request(state, "GET", "service.example").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["retry"], false);
        assert_eq!(response["reason"], "no_active_execution");
        assert_eq!(store.attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authorize_declines_unregistered_host_and_default_post() {
        let (state, store) = state(true).await;
        let (status, response) = authorize_request(state.clone(), "GET", "other.example").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["reason"], "registry_denied");

        let (status, response) = authorize_request(state, "POST", "service.example").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["reason"], "method_not_allowed");
        assert_eq!(store.attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authorize_declines_invalid_challenge_variants_before_attempt() {
        let cases = [
            (
                "malformed",
                "not-a-payment-challenge".to_owned(),
                "unsupported_or_invalid_challenge",
            ),
            (
                "expired",
                challenge_for("service.example", "charge", "2020-01-01T00:00:00Z"),
                "expired_challenge",
            ),
            (
                "unsupported session",
                challenge_for("service.example", "session", &mpp::expires::minutes(5)),
                "unsupported_or_invalid_challenge",
            ),
            (
                "mismatched realm",
                challenge_for("other.example", "charge", &mpp::expires::minutes(5)),
                "realm_mismatch",
            ),
        ];
        for (name, challenge, expected) in cases {
            let (state, store) = state(true).await;
            let (_, response) = authorize_request_with(
                state,
                json!({
                    "host": "service.example",
                    "method": "GET",
                    "path": "/paid",
                    "status": 402,
                    "response_headers": {"WWW-Authenticate": [challenge]},
                    "replayable": true,
                    "sandbox_id": "sandbox-test"
                }),
            )
            .await;
            assert_eq!(response["reason"], expected, "{name}");
            assert_eq!(store.attempts.load(Ordering::SeqCst), 0, "{name}");
        }
    }

    #[tokio::test]
    async fn authorize_declines_non_402_and_non_replayable_requests() {
        for (status, replayable, expected) in [
            (200, true, "status_not_payment_required"),
            (402, false, "request_not_replayable"),
        ] {
            let (state, store) = state(true).await;
            let (_, response) = authorize_request_with(
                state,
                json!({
                    "host": "service.example",
                    "method": "GET",
                    "path": "/paid",
                    "status": status,
                    "response_headers": {"WWW-Authenticate": [challenge()]},
                    "replayable": replayable,
                    "sandbox_id": "sandbox-test"
                }),
            )
            .await;
            assert_eq!(response["reason"], expected);
            assert_eq!(store.attempts.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn authorize_declines_duplicate_and_budget_rejected_attempts() {
        for (begin_result, expected) in [
            (
                BeginAttempt::Duplicate {
                    attempt_id: Uuid::new_v4(),
                    sandbox_id: "sandbox-test".to_owned(),
                    execution_id: "exe-test".to_owned(),
                },
                "duplicate_challenge",
            ),
            (
                BeginAttempt::BudgetDenied {
                    reason: "daily_budget_exceeded",
                },
                "daily_budget_exceeded",
            ),
            (
                BeginAttempt::Duplicate {
                    attempt_id: Uuid::new_v4(),
                    sandbox_id: "other-sandbox".to_owned(),
                    execution_id: "other-execution".to_owned(),
                },
                "duplicate_challenge_identity_mismatch",
            ),
        ] {
            let (state, store) = state(true).await;
            *store.begin_result.lock().expect("begin result") = begin_result;
            let (_, response) = authorize_request(state, "GET", "service.example").await;
            assert_eq!(response["reason"], expected);
            assert_eq!(store.attempts.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn observability_identifiers_are_redacted_and_method_labels_are_bounded() {
        let raw = "execution-sensitive-identifier";
        let redacted = super::redacted_identifier(raw);
        assert_eq!(redacted.len(), "sha256:".len() + 16);
        assert!(!redacted.contains(raw));
        assert_eq!(super::metric_method("get"), "GET");
        assert_eq!(super::metric_method("ATTACKER-CONTROLLED-METHOD"), "OTHER");
    }

    #[tokio::test]
    async fn budget_caps_and_reservations_are_distinct() {
        let (mut state, _) = state(false).await;
        assert!(state.budgets_disabled());
        assert!(!state.reservations_enabled());

        state.max_per_charge_atomic = Some(100);
        assert!(!state.budgets_disabled());
        assert!(!state.reservations_enabled());

        state.max_daily_atomic = Some(1_000);
        assert!(state.reservations_enabled());
    }

    #[test]
    fn completion_outcomes_settle_optional_receipts_and_release_failed_replays() {
        for (status, transport_error, receipt, expected_outcome, expected_reason) in [
            (Some(200), None, None, CompletionOutcome::Settled, None),
            (
                Some(204),
                None,
                Some("invalid"),
                CompletionOutcome::Settled,
                Some("invalid_receipt"),
            ),
            (
                Some(402),
                None,
                None,
                CompletionOutcome::Released,
                Some("credential_rejected"),
            ),
            (
                Some(500),
                None,
                None,
                CompletionOutcome::Released,
                Some("replay_failed"),
            ),
            (
                None,
                Some("upstream_transport_error"),
                None,
                CompletionOutcome::Unknown,
                Some("transport_error"),
            ),
            (
                None,
                None,
                None,
                CompletionOutcome::Unknown,
                Some("missing_replay_outcome"),
            ),
        ] {
            assert_eq!(
                super::completion_outcome(status, transport_error, receipt),
                (expected_outcome, expected_reason)
            );
        }
    }
}
