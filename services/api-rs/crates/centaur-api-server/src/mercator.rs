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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitMercatorRequest {
    /// Set only after the user has accepted the quoted total amount.
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
    result: Value,
}

pub(crate) async fn submit_mercator_job(
    Json(request): Json<SubmitMercatorRequest>,
) -> Result<Json<SubmitMercatorResponse>, ApiError> {
    validate_handoff(&request)?;

    let wallet_path = required_env("MERCATOR_WALLET_PATH")?;
    let wallet = TemporaryWallet::copy_from(Path::new(&wallet_path))?;

    let executable = env::var("CENTAUR_MERCATOR_CLI")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/bin/mercator".to_owned());
    let result = execute_handoff(&executable, wallet.path(), &request).await?;
    Ok(Json(SubmitMercatorResponse { ok: true, result }))
}

fn validate_handoff(request: &SubmitMercatorRequest) -> Result<(), ApiError> {
    if !request.approved {
        return Err(ApiError::BadRequest(
            "explicit user approval is required before payment".to_owned(),
        ));
    }

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

    let configured_jobs_url = env::var("CENTAUR_MERCATOR_JOBS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_JOBS_URL.to_owned());
    if handoff.rest.url != configured_jobs_url {
        return Err(ApiError::BadRequest(
            "Mercator handoff URL is not allowed".to_owned(),
        ));
    }
    if !valid_amount(&handoff.max_spend) {
        return Err(ApiError::BadRequest(
            "Mercator maxSpend must be a non-negative decimal with at most 6 decimals".to_owned(),
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
    if !body.get("plan").is_some_and(Value::is_object) {
        return Err(ApiError::BadRequest(
            "Mercator handoff is missing its plan".to_owned(),
        ));
    }
    Ok(())
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

fn valid_amount(value: &str) -> bool {
    let (whole, fractional) = match value.split_once('.') {
        Some(parts) => parts,
        None => (value, ""),
    };
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fractional.len() <= 6
        && fractional.bytes().all(|byte| byte.is_ascii_digit())
        && !value.ends_with('.')
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
                        "plan": {"nodes": []},
                    }),
                    method: "POST".to_owned(),
                    url: DEFAULT_JOBS_URL.to_owned(),
                },
                status: "payment_required".to_owned(),
            },
        }
    }

    #[test]
    fn validates_canonical_approved_handoff() {
        validate_handoff(&request(true)).expect("valid handoff");
    }

    #[test]
    fn rejects_unapproved_payment() {
        let error = validate_handoff(&request(false)).expect_err("approval must be required");
        assert!(error.to_string().contains("explicit user approval"));
    }

    #[test]
    fn rejects_non_mercator_destination() {
        let mut request = request(true);
        request.handoff.rest.url = "https://example.com/v1/jobs".to_owned();
        let error = validate_handoff(&request).expect_err("destination must be pinned");
        assert!(error.to_string().contains("URL is not allowed"));
    }

    #[test]
    fn validates_max_spend_format() {
        for valid in ["0", "0.1", "10.000001"] {
            assert!(valid_amount(valid), "{valid}");
        }
        for invalid in ["", ".1", "1.", "1.0000001", "-1", "1e3"] {
            assert!(!valid_amount(invalid), "{invalid}");
        }
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
