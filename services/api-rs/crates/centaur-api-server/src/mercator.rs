use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{process::Command, time::timeout};

use crate::ApiError;

const DEFAULT_JOBS_URL: &str = "https://mercator.tempoxyz.dev/v1/jobs";
const DEFAULT_TIMEOUT_SECONDS: u64 = 45;
const MAX_CLI_OUTPUT_BYTES: usize = 1024 * 1024;
const AMOUNT_SCALE: u128 = 1_000_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitMercatorRequest {
    /// Set after the user has accepted a quote above the automatic threshold.
    #[serde(default)]
    approved: bool,
    handoff: MercatorHandoff,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MercatorHandoff {
    client: MercatorClientHandoff,
    #[serde(rename = "maxSpend")]
    max_spend: String,
    #[serde(rename = "nextAction")]
    next_action: String,
    rest: MercatorRestHandoff,
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MercatorClientHandoff {
    arguments: Vec<String>,
    executable: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MercatorRestHandoff {
    body: Value,
    method: String,
    url: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SubmitMercatorResponse {
    ok: bool,
    approval: ApprovalMode,
    result: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ApprovalMode {
    Automatic,
    Explicit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EndpointRule {
    service_id: String,
    method: String,
    path: String,
}

#[derive(Debug)]
struct MercatorPolicy {
    auto_approve_max: u128,
    max_spend_per_job: u128,
    allowed_services: Vec<String>,
    denied_services: Vec<String>,
    allowed_endpoints: Vec<EndpointRule>,
    denied_endpoints: Vec<EndpointRule>,
}

impl MercatorPolicy {
    fn from_env() -> Result<Self, ApiError> {
        let auto_approve_max = policy_amount("CENTAUR_MERCATOR_AUTO_APPROVE_MAX")?;
        let max_spend_per_job = policy_amount("CENTAUR_MERCATOR_MAX_SPEND_PER_JOB")?;
        if auto_approve_max > max_spend_per_job {
            return Err(ApiError::Internal(
                "Mercator automatic approval threshold exceeds the per-job limit".to_owned(),
            ));
        }
        Ok(Self {
            auto_approve_max,
            max_spend_per_job,
            allowed_services: policy_json("CENTAUR_MERCATOR_ALLOWED_SERVICES")?,
            denied_services: policy_json("CENTAUR_MERCATOR_DENIED_SERVICES")?,
            allowed_endpoints: policy_json("CENTAUR_MERCATOR_ALLOWED_ENDPOINTS")?,
            denied_endpoints: policy_json("CENTAUR_MERCATOR_DENIED_ENDPOINTS")?,
        })
    }
}

pub(crate) async fn submit_mercator_job(
    Json(request): Json<SubmitMercatorRequest>,
) -> Result<Json<SubmitMercatorResponse>, ApiError> {
    let policy = MercatorPolicy::from_env()?;
    let configured_jobs_url = env::var("CENTAUR_MERCATOR_JOBS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_JOBS_URL.to_owned());
    let approval = validate_handoff(&request, &policy, &configured_jobs_url)?;

    let wallet_path = required_env("MERCATOR_WALLET_PATH")?;
    let wallet = TemporaryWallet::copy_from(Path::new(&wallet_path))?;

    let executable = env::var("CENTAUR_MERCATOR_CLI")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/bin/mercator".to_owned());
    let result = execute_handoff(&executable, wallet.path(), &request).await?;
    Ok(Json(SubmitMercatorResponse {
        ok: true,
        approval,
        result,
    }))
}

fn validate_handoff(
    request: &SubmitMercatorRequest,
    policy: &MercatorPolicy,
    configured_jobs_url: &str,
) -> Result<ApprovalMode, ApiError> {
    let handoff = &request.handoff;
    if handoff.status != "payment_required" || handoff.next_action != "run_rest_request" {
        return Err(ApiError::BadRequest(
            "expected a Mercator payment-required REST handoff".to_owned(),
        ));
    }
    if handoff.client.executable != "mercator"
        || handoff.client.arguments.first().map(String::as_str) != Some("submit")
    {
        return Err(ApiError::BadRequest(
            "expected a canonical Mercator CLI handoff".to_owned(),
        ));
    }
    if handoff.rest.method != "POST" {
        return Err(ApiError::BadRequest(
            "Mercator handoff method must be POST".to_owned(),
        ));
    }

    if handoff.rest.url != configured_jobs_url {
        return Err(ApiError::BadRequest(
            "Mercator handoff URL is not allowed".to_owned(),
        ));
    }
    let max_spend = amount_units(&handoff.max_spend).ok_or_else(|| {
        ApiError::BadRequest(
            "Mercator maxSpend must be a non-negative decimal with at most 6 decimals".to_owned(),
        )
    })?;
    let approval = if max_spend <= policy.auto_approve_max {
        ApprovalMode::Automatic
    } else if request.approved {
        ApprovalMode::Explicit
    } else {
        return Err(ApiError::BadRequest(
            "explicit user approval is required above the automatic payment threshold".to_owned(),
        ));
    };
    if max_spend > policy.max_spend_per_job {
        return Err(ApiError::Forbidden(
            "Mercator maxSpend exceeds the configured per-job limit".to_owned(),
        ));
    }

    let body = handoff.rest.body.as_object().ok_or_else(|| {
        ApiError::BadRequest("Mercator handoff body must be an object".to_owned())
    })?;
    let idempotency_key = body
        .get("idempotencyKey")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiError::BadRequest("Mercator handoff is missing idempotencyKey".to_owned())
        })?;
    if !(8..=200).contains(&idempotency_key.len()) {
        return Err(ApiError::BadRequest(
            "Mercator idempotencyKey must contain 8 to 200 characters".to_owned(),
        ));
    }
    let nodes = body
        .get("plan")
        .and_then(Value::as_object)
        .and_then(|plan| plan.get("nodes"))
        .and_then(Value::as_array)
        .filter(|nodes| !nodes.is_empty())
        .ok_or_else(|| ApiError::BadRequest("Mercator plan has no nodes".to_owned()))?;
    for node in nodes {
        validate_node(node, policy)?;
    }
    Ok(approval)
}

fn validate_node(node: &Value, policy: &MercatorPolicy) -> Result<(), ApiError> {
    let node = node
        .as_object()
        .ok_or_else(|| ApiError::BadRequest("Mercator plan node must be an object".to_owned()))?;
    let field = |name: &str| {
        node.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ApiError::BadRequest(format!("Mercator plan node is missing {name}")))
    };
    let service_id = field("serviceId")?;
    let method = field("method")?.to_ascii_uppercase();
    let path = field("path")?;
    let endpoint = EndpointRule {
        service_id: service_id.to_owned(),
        method,
        path: path.to_owned(),
    };

    if policy
        .denied_services
        .iter()
        .any(|value| value == service_id)
        || policy
            .denied_endpoints
            .iter()
            .any(|rule| endpoint_matches(rule, &endpoint))
    {
        return Err(ApiError::Forbidden(
            "Mercator plan contains a denied service or endpoint".to_owned(),
        ));
    }
    if !policy.allowed_services.is_empty()
        && !policy
            .allowed_services
            .iter()
            .any(|value| value == service_id)
    {
        return Err(ApiError::Forbidden(
            "Mercator plan contains a service outside the allowlist".to_owned(),
        ));
    }
    if !policy.allowed_endpoints.is_empty()
        && !policy
            .allowed_endpoints
            .iter()
            .any(|rule| endpoint_matches(rule, &endpoint))
    {
        return Err(ApiError::Forbidden(
            "Mercator plan contains an endpoint outside the allowlist".to_owned(),
        ));
    }
    Ok(())
}

fn endpoint_matches(rule: &EndpointRule, endpoint: &EndpointRule) -> bool {
    rule.service_id == endpoint.service_id
        && rule.method.eq_ignore_ascii_case(&endpoint.method)
        && rule.path == endpoint.path
}

async fn execute_handoff(
    executable: &str,
    wallet_path: &Path,
    request: &SubmitMercatorRequest,
) -> Result<Value, ApiError> {
    let body = serde_json::to_string(&request.handoff.rest.body)?;
    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        // api-rs receives the deployment's shared Secret. The payment process
        // gets only the wallet path and ordinary runtime configuration, never
        // unrelated control-plane credentials.
        .env_clear()
        .env("HOME", "/tmp")
        .env("MERCATOR_WALLET_PATH", wallet_path)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .args([
            "submit",
            "--url",
            &request.handoff.rest.url,
            "--body",
            &body,
            "--max-spend",
            &request.handoff.max_spend,
            "--format",
            "json",
        ]);

    let output = timeout(
        Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
        command.output(),
    )
    .await
    .map_err(|_| {
        ApiError::ServiceUnavailable(
            "Mercator submission timed out; retry with the same idempotency key".to_owned(),
        )
    })?
    .map_err(|error| ApiError::Internal(format!("failed to start Mercator CLI: {error}")))?;

    if !output.status.success() {
        return Err(ApiError::ServiceUnavailable(
            "Mercator rejected the paid job submission; retry with the same idempotency key"
                .to_owned(),
        ));
    }
    if output.stdout.len() > MAX_CLI_OUTPUT_BYTES {
        return Err(ApiError::Internal(
            "Mercator CLI response exceeded the output limit".to_owned(),
        ));
    }
    let result = serde_json::from_slice(&output.stdout).map_err(|error| {
        ApiError::Internal(format!("Mercator CLI returned invalid JSON: {error}"))
    })?;
    Ok(result)
}

/// Per-request writable copy of the read-only Kubernetes Secret mount.
///
/// The Accounts SDK intentionally enforces mode 0600 whenever it reads its
/// filesystem store, which cannot succeed directly on a Kubernetes Secret
/// volume. The canonical copy remains in Centaur's Secret; the CLI receives a
/// private, short-lived copy that is removed when the submission finishes.
struct TemporaryWallet {
    path: PathBuf,
}

impl TemporaryWallet {
    fn copy_from(source: &Path) -> Result<Self, ApiError> {
        if !source.is_file() {
            return Err(wallet_unavailable());
        }
        let contents = fs::read(source).map_err(|_| wallet_unavailable())?;
        let path = env::temp_dir().join(format!("centaur-mercator-{}.json", uuid::Uuid::new_v4()));
        let mut destination = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| wallet_unavailable())?;
        if destination.write_all(&contents).is_err() || destination.sync_all().is_err() {
            let _ = fs::remove_file(&path);
            return Err(wallet_unavailable());
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryWallet {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn wallet_unavailable() -> ApiError {
    ApiError::ServiceUnavailable("Mercator wallet is not configured".to_owned())
}

fn required_env(name: &str) -> Result<String, ApiError> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::ServiceUnavailable("Mercator wallet is not configured".to_owned()))
}

fn amount_units(value: &str) -> Option<u128> {
    let (whole, fractional) = match value.split_once('.') {
        Some(parts) => parts,
        None => (value, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fractional.is_empty() && value.ends_with('.')
        || fractional.len() > 6
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?.checked_mul(AMOUNT_SCALE)?;
    let fractional = if fractional.is_empty() {
        0
    } else {
        fractional.parse::<u128>().ok()? * 10_u128.pow((6 - fractional.len()) as u32)
    };
    whole.checked_add(fractional)
}

fn policy_amount(name: &str) -> Result<u128, ApiError> {
    // A server that has not enabled Mercator gets a fail-closed zero policy.
    let value = env::var(name).unwrap_or_else(|_| "0".to_owned());
    amount_units(value.trim())
        .ok_or_else(|| ApiError::Internal(format!("Mercator policy setting {name} is invalid")))
}

fn policy_json<T>(name: &str) -> Result<T, ApiError>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let value = env::var(name).unwrap_or_default();
    if value.trim().is_empty() {
        return Ok(T::default());
    }
    serde_json::from_str(&value).map_err(|error| {
        ApiError::Internal(format!(
            "Mercator policy setting {name} is invalid: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn request(approved: bool) -> SubmitMercatorRequest {
        SubmitMercatorRequest {
            approved,
            handoff: MercatorHandoff {
                client: MercatorClientHandoff {
                    arguments: vec!["submit".to_owned()],
                    executable: "mercator".to_owned(),
                },
                max_spend: "0.001".to_owned(),
                next_action: "run_rest_request".to_owned(),
                rest: MercatorRestHandoff {
                    body: json!({
                        "idempotencyKey": "centaur-smoke-1",
                        "plan": {"nodes": [{
                            "id": "btc-price",
                            "serviceId": "x402-api",
                            "method": "GET",
                            "path": "/crypto/price/btc/usd/btc-usd",
                            "input": {},
                            "dependsOn": [],
                        }]},
                    }),
                    method: "POST".to_owned(),
                    url: DEFAULT_JOBS_URL.to_owned(),
                },
                status: "payment_required".to_owned(),
            },
        }
    }

    fn policy(auto_approve_max: &str, max_spend_per_job: &str) -> MercatorPolicy {
        MercatorPolicy {
            auto_approve_max: amount_units(auto_approve_max).expect("valid threshold"),
            max_spend_per_job: amount_units(max_spend_per_job).expect("valid limit"),
            allowed_services: Vec::new(),
            denied_services: Vec::new(),
            allowed_endpoints: Vec::new(),
            denied_endpoints: Vec::new(),
        }
    }

    #[test]
    fn validates_canonical_approved_handoff() {
        assert_eq!(
            validate_handoff(&request(true), &policy("0", "0.10"), DEFAULT_JOBS_URL)
                .expect("valid handoff"),
            ApprovalMode::Explicit
        );
    }

    #[test]
    fn automatically_approves_payment_at_or_below_threshold() {
        assert_eq!(
            validate_handoff(&request(false), &policy("0.001", "0.10"), DEFAULT_JOBS_URL,)
                .expect("payment should be automatic"),
            ApprovalMode::Automatic
        );
    }

    #[test]
    fn rejects_unapproved_payment_above_threshold() {
        let error = validate_handoff(&request(false), &policy("0.0009", "0.10"), DEFAULT_JOBS_URL)
            .expect_err("approval must be required");
        assert!(error.to_string().contains("explicit user approval"));
    }

    #[test]
    fn rejects_payment_above_hard_limit_even_when_approved() {
        let error = validate_handoff(
            &request(true),
            &policy("0.0005", "0.0009"),
            DEFAULT_JOBS_URL,
        )
        .expect_err("hard limit must be enforced");
        assert!(error.to_string().contains("per-job limit"));
    }

    #[test]
    fn rejects_non_mercator_destination() {
        let mut request = request(true);
        request.handoff.rest.url = "https://example.com/v1/jobs".to_owned();
        let error = validate_handoff(&request, &policy("0", "0.10"), DEFAULT_JOBS_URL)
            .expect_err("destination must be pinned");
        assert!(error.to_string().contains("URL is not allowed"));
    }

    #[test]
    fn validates_max_spend_format() {
        for valid in ["0", "0.1", "10.000001"] {
            assert!(amount_units(valid).is_some(), "{valid}");
        }
        for invalid in ["", ".1", "1.", "1.0000001", "-1", "1e3"] {
            assert!(amount_units(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn applies_service_and_endpoint_rules_with_deny_precedence() {
        let allowed_endpoint = EndpointRule {
            service_id: "x402-api".to_owned(),
            method: "get".to_owned(),
            path: "/crypto/price/btc/usd/btc-usd".to_owned(),
        };
        let mut policy = policy("0.001", "0.10");
        policy.allowed_services = vec!["x402-api".to_owned()];
        policy.allowed_endpoints = vec![allowed_endpoint.clone()];
        validate_handoff(&request(false), &policy, DEFAULT_JOBS_URL).expect("allowlisted endpoint");

        policy.denied_endpoints = vec![allowed_endpoint];
        let error = validate_handoff(&request(false), &policy, DEFAULT_JOBS_URL)
            .expect_err("denylist must win");
        assert!(error.to_string().contains("denied"));
    }

    #[test]
    fn rejects_service_outside_nonempty_allowlist() {
        let mut policy = policy("0.001", "0.10");
        policy.allowed_services = vec!["another-service".to_owned()];
        let error = validate_handoff(&request(false), &policy, DEFAULT_JOBS_URL)
            .expect_err("service must be allowlisted");
        assert!(error.to_string().contains("outside the allowlist"));
    }

    #[test]
    fn stages_wallet_in_private_writable_file() {
        let source = env::temp_dir().join(format!("centaur-secret-{}.json", uuid::Uuid::new_v4()));
        fs::write(&source, br#"{"wallet":"test"}"#).expect("write source wallet");

        let staged_path;
        {
            let wallet = TemporaryWallet::copy_from(&source).expect("stage wallet");
            staged_path = wallet.path().to_path_buf();
            assert_eq!(
                fs::read(wallet.path()).expect("read staged wallet"),
                br#"{"wallet":"test"}"#
            );
            assert_eq!(
                fs::metadata(wallet.path())
                    .expect("staged wallet metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        assert!(!staged_path.exists());
        fs::remove_file(source).expect("remove source wallet");
    }
}
