//! The console's internal scheduled-task endpoints (RFC 0006a "Console
//! endpoints") and the runner JWT that authenticates api-rs to them.

use std::{sync::Arc, time::Duration};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use centaur_console_auth::internal::{
    AUDIENCE as RUNNER_JWT_AUDIENCE, ISSUER as RUNNER_JWT_ISSUER, SUBJECT as RUNNER_JWT_SUBJECT,
    TOKEN_USE as RUNNER_JWT_TOKEN_USE,
};
/// `CentaurApiClient::DEFAULT_TIMEOUT_SECONDS`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Body of `claim`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClaimRequest {
    pub scheduled_for: String,
    #[serde(default)]
    pub manual: bool,
}

/// `outcome` of a claim.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimOutcome {
    Claimed,
    Skipped,
    Failed,
}

/// `200` body of `claim`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ClaimResponse {
    pub outcome: ClaimOutcome,
    pub reason: Option<String>,
    pub error: Option<String>,
    /// Key order is kept (it is passed through to the workflow input).
    pub api_input: Option<serde_json::Value>,
    pub enabled: bool,
    pub next_run_at: Option<String>,
}

/// Body of `result`: exactly one of `run_id` and `error`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ResultRequest {
    pub scheduled_for: String,
    #[serde(default)]
    pub manual: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `200` body of `result`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ResultResponse {
    pub enabled: bool,
    pub next_run_at: Option<String>,
}

/// A console call that did not produce a documented answer. Every variant is
/// retried (RFC 0006a "Retry policy").
#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    #[error("console request failed: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("console returned HTTP {status}: {message}")]
    Status { status: u16, message: String },
    #[error("console returned an invalid body: {0}")]
    InvalidBody(String),
    #[error("runner JWT could not be minted: CENTAUR_JWT_SIGNING_SECRET is blank")]
    Token,
}

impl ConsoleError {
    /// 4xx answers other than 404 mean misconfiguration or a contract bug.
    pub fn is_client_error(&self) -> bool {
        matches!(self, Self::Status { status, .. } if (400..500).contains(status))
    }
}

/// Calls the console's `/internal/v1/scheduled_tasks/*` endpoints.
#[derive(Clone)]
pub struct ConsoleClient {
    base_url: Arc<str>,
    secret: Arc<str>,
    http: reqwest::Client,
    host: Option<Arc<str>>,
}

impl ConsoleClient {
    /// `base_url` is the console Service URL (`IRON_CONTROL_URL`); `secret` is
    /// `CENTAUR_JWT_SIGNING_SECRET`.
    pub fn new(base_url: &str, secret: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        // The console is a cluster-internal Service: never through a proxy.
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()?;
        Ok(Self {
            base_url: Arc::from(base_url.trim().trim_end_matches('/')),
            secret: Arc::from(secret),
            http,
            host: None,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The api-rs service token the console's `/internal/v1/*` namespace
    /// verifies (`centaur_console_auth::internal`).
    pub fn token(&self) -> Result<String, ConsoleError> {
        centaur_console_auth::tokens::internal_service(&self.secret, chrono::Utc::now())
            .map_err(|_| ConsoleError::Token)
    }

    /// `POST /internal/v1/scheduled_tasks/{oid}/claim`; `None` is a 404.
    pub async fn claim(
        &self,
        oid: &str,
        request: &ClaimRequest,
    ) -> Result<Option<ClaimResponse>, ConsoleError> {
        self.post(oid, "claim", request).await
    }

    /// `POST /internal/v1/scheduled_tasks/{oid}/result`; `None` is a 404.
    pub async fn result(
        &self,
        oid: &str,
        request: &ResultRequest,
    ) -> Result<Option<ResultResponse>, ConsoleError> {
        self.post(oid, "result", request).await
    }

    async fn post<B: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        oid: &str,
        action: &str,
        body: &B,
    ) -> Result<Option<R>, ConsoleError> {
        let url = format!(
            "{}/internal/v1/scheduled_tasks/{oid}/{action}",
            self.base_url
        );
        let mut request = self.http.post(url);
        if let Some(host) = &self.host {
            request = request.header(reqwest::header::HOST, host.as_ref());
        }
        let response = request
            .bearer_auth(self.token()?)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(body)
            .send()
            .await
            .map_err(ConsoleError::Transport)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(ConsoleError::Transport)?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(ConsoleError::Status {
                status: status.as_u16(),
                message: error_message(&bytes),
            });
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| ConsoleError::InvalidBody(error.to_string()))
    }
}

/// The `error` (or `message`) of an error body, bounded for logs.
fn error_message(bytes: &[u8]) -> String {
    let parsed = serde_json::from_slice::<Value>(bytes).ok();
    let message = parsed
        .as_ref()
        .and_then(|body| body.get("error").or_else(|| body.get("message")))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
    message.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_tokens_are_the_console_internal_service_token() {
        let client =
            ConsoleClient::new("http://console.local/", "secret", DEFAULT_TIMEOUT).expect("client");
        assert_eq!(client.base_url(), "http://console.local");
        let token = client.token().unwrap();
        let now = chrono::Utc::now();
        assert!(centaur_console_auth::internal::verify(
            &token, "secret", now
        ));
        assert!(!centaur_console_auth::internal::verify(
            &token, "other", now
        ));
        assert!(
            ConsoleClient::new("http://console.local", " ", DEFAULT_TIMEOUT)
                .unwrap()
                .token()
                .is_err()
        );
    }

    #[test]
    fn claim_responses_round_trip_the_documented_shape() {
        let body = serde_json::json!({
            "outcome": "failed",
            "reason": "delivery_destination_unavailable",
            "error": "Slack delivery destination is no longer available to the task owner",
            "api_input": null,
            "enabled": true,
            "next_run_at": "2026-08-19T13:00:00Z"
        });
        let parsed: ClaimResponse = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(parsed.outcome, ClaimOutcome::Failed);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), body);
        let result = ResultRequest {
            scheduled_for: "2026-08-19T12:00:00Z".into(),
            manual: false,
            run_id: Some("run-1".into()),
            error: None,
        };
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({"scheduled_for": "2026-08-19T12:00:00Z", "manual": false, "run_id": "run-1"})
        );
    }

    #[test]
    fn error_messages_prefer_the_json_error_field() {
        assert_eq!(error_message(br#"{"error":"nope"}"#), "nope");
        assert_eq!(error_message(b"plain"), "plain");
    }
}
