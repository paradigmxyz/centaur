use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{process::Command, time::timeout};

use crate::{ApiError, auth::AuthenticatedCaller};

const JOBS_URL: &str = "https://mercator.tempoxyz.dev/v1/jobs";
const CLI_PATH: &str = "/usr/local/bin/mercator";
const WALLET_PATH: &str = "/var/run/secrets/centaur/mercator/wallet.json";
const DEFAULT_TIMEOUT_SECONDS: u64 = 45;
const MAX_CLI_OUTPUT_BYTES: usize = 1024 * 1024;
const AMOUNT_SCALE: u128 = 1_000_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitMercatorRequest {
    /// Caller assertion that the user accepted a quote above the automatic threshold.
    ///
    /// This is a UX acknowledgement, not an authentication boundary. The
    /// authenticated principal, pinned destination, service policy, and quoted
    /// maximum passed to the payment client are enforced independently.
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
    UserAcknowledged,
}

#[derive(Debug)]
struct MercatorPolicy {
    auto_approve_max: u128,
    allowed_services: Vec<String>,
    denied_services: Vec<String>,
}

impl MercatorPolicy {
    fn from_env() -> Result<Self, ApiError> {
        Ok(Self {
            auto_approve_max: policy_amount("CENTAUR_MERCATOR_AUTO_APPROVE_MAX")?,
            allowed_services: policy_json("CENTAUR_MERCATOR_ALLOWED_SERVICES")?,
            denied_services: policy_json("CENTAUR_MERCATOR_DENIED_SERVICES")?,
        })
    }
}

pub(crate) async fn submit_mercator_job(
    Extension(caller): Extension<AuthenticatedCaller>,
    Json(request): Json<SubmitMercatorRequest>,
) -> Result<Json<SubmitMercatorResponse>, ApiError> {
    let policy = MercatorPolicy::from_env()?;
    let validated = validate_handoff(&request, &policy)?;

    tracing::info!(
        caller = %fingerprint(caller.principal_subject().unwrap_or("unknown")),
        request = %validated.audit.request,
        max_spend = %request.handoff.max_spend,
        plan_nodes = validated.audit.plan_nodes,
        approval = ?validated.approval,
        "Mercator paid job authorized"
    );

    let wallet = TemporaryWallet::copy_from(Path::new(WALLET_PATH))?;

    let result = execute_handoff(CLI_PATH, wallet.path(), &request).await?;
    tracing::info!(
        caller = %fingerprint(caller.principal_subject().unwrap_or("unknown")),
        request = %validated.audit.request,
        job_id = result_job_id(&result).unwrap_or("unknown"),
        "Mercator paid job submitted"
    );
    Ok(Json(SubmitMercatorResponse {
        ok: true,
        approval: validated.approval,
        result,
    }))
}

fn validate_handoff(
    request: &SubmitMercatorRequest,
    policy: &MercatorPolicy,
) -> Result<ValidatedHandoff, ApiError> {
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

    if handoff.rest.url != JOBS_URL {
        return Err(ApiError::BadRequest(
            "Mercator handoff URL is not allowed".to_owned(),
        ));
    }
    let max_spend = amount_units(&handoff.max_spend).ok_or_else(|| {
        ApiError::BadRequest(
            "Mercator maxSpend must be a non-negative decimal with at most 6 decimals".to_owned(),
        )
    })?;
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
        validate_service(node, policy)?;
    }
    let approval = if max_spend <= policy.auto_approve_max {
        ApprovalMode::Automatic
    } else if request.approved {
        ApprovalMode::UserAcknowledged
    } else {
        return Err(ApiError::BadRequest(
            "user approval acknowledgement is required above the automatic payment threshold"
                .to_owned(),
        ));
    };
    Ok(ValidatedHandoff {
        approval,
        audit: MercatorAudit {
            request: fingerprint(idempotency_key),
            plan_nodes: nodes.len(),
        },
    })
}

fn validate_service(node: &Value, policy: &MercatorPolicy) -> Result<(), ApiError> {
    let node = node
        .as_object()
        .ok_or_else(|| ApiError::BadRequest("Mercator plan node must be an object".to_owned()))?;
    let service_id = node
        .get("serviceId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::BadRequest("Mercator plan node is missing serviceId".to_owned())
        })?;

    if policy
        .denied_services
        .iter()
        .any(|value| value == service_id)
    {
        return Err(ApiError::Forbidden(
            "Mercator plan contains a denied service".to_owned(),
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
    Ok(())
}

#[derive(Debug)]
struct MercatorAudit {
    request: String,
    plan_nodes: usize,
}

#[derive(Debug)]
struct ValidatedHandoff {
    approval: ApprovalMode,
    audit: MercatorAudit,
}

fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(&digest[..8])
}

fn result_job_id(result: &Value) -> Option<&str> {
    result
        .get("jobId")
        .or_else(|| result.get("job").and_then(|job| job.get("jobId")))
        .or_else(|| result.get("job").and_then(|job| job.get("id")))
        .and_then(Value::as_str)
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
                    url: JOBS_URL.to_owned(),
                },
                status: "payment_required".to_owned(),
            },
        }
    }

    fn policy(auto_approve_max: &str) -> MercatorPolicy {
        MercatorPolicy {
            auto_approve_max: amount_units(auto_approve_max).expect("valid threshold"),
            allowed_services: Vec::new(),
            denied_services: Vec::new(),
        }
    }

    #[test]
    fn validates_canonical_approved_handoff() {
        assert_eq!(
            validate_handoff(&request(true), &policy("0"))
                .expect("valid handoff")
                .approval,
            ApprovalMode::UserAcknowledged
        );
    }

    #[test]
    fn automatically_approves_payment_at_or_below_threshold() {
        assert_eq!(
            validate_handoff(&request(false), &policy("0.001"))
                .expect("payment should be automatic")
                .approval,
            ApprovalMode::Automatic
        );
    }

    #[test]
    fn rejects_unapproved_payment_above_threshold() {
        let error = validate_handoff(&request(false), &policy("0.0009"))
            .expect_err("approval must be required");
        assert!(error.to_string().contains("approval acknowledgement"));
    }

    #[test]
    fn rejects_non_mercator_destination() {
        let mut request = request(true);
        request.handoff.rest.url = "https://example.com/v1/jobs".to_owned();
        let error =
            validate_handoff(&request, &policy("0")).expect_err("destination must be pinned");
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
    fn applies_service_rules_with_deny_precedence() {
        let mut policy = policy("0.001");
        policy.allowed_services = vec!["x402-api".to_owned()];
        validate_handoff(&request(false), &policy).expect("allowlisted service");

        policy.denied_services = vec!["x402-api".to_owned()];
        let error = validate_handoff(&request(false), &policy).expect_err("denylist must win");
        assert!(error.to_string().contains("denied"));
    }

    #[test]
    fn rejects_service_outside_nonempty_allowlist() {
        let mut policy = policy("0");
        policy.allowed_services = vec!["another-service".to_owned()];
        let error =
            validate_handoff(&request(false), &policy).expect_err("service must be allowlisted");
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

    #[test]
    fn audit_values_are_stable_and_do_not_expose_identifiers() {
        let request = request(true);
        let audit = validate_handoff(&request, &policy("0"))
            .expect("valid handoff")
            .audit;
        assert_eq!(audit.request, fingerprint("centaur-smoke-1"));
        assert_eq!(audit.plan_nodes, 1);
        assert!(!audit.request.contains("centaur-smoke-1"));
    }
}
