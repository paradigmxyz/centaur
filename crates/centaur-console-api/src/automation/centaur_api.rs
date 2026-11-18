//! The console's calls to api-rs: Slack Connect team ids, workflow
//! schedules and runs, and the scheduled-task runner's reconcile and run
//! hints. Requests carry the console service JWT. A non-2xx answer is an
//! error whose message is the body's `error`, `message`, or `detail`.

use std::time::Duration;

use centaur_console_auth::tokens;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Value, json};

use crate::{AppState, http::without_url};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CentaurApiError {
    pub message: String,
    /// The HTTP status, when api-rs answered.
    pub status: Option<u16>,
}

impl CentaurApiError {
    fn local(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: None,
        }
    }
}

type Result<T> = std::result::Result<T, CentaurApiError>;

pub struct CentaurApiClient<'a> {
    state: &'a AppState,
    base_url: String,
    timeout: Duration,
}

const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'_')
    .remove(b'.')
    .remove(b'-')
    .remove(b'~');

/// One path segment, percent-encoded.
fn segment(value: &str) -> String {
    utf8_percent_encode(value, SEGMENT).to_string()
}

impl<'a> CentaurApiClient<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self::with_timeout(state, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(state: &'a AppState, timeout: Duration) -> Self {
        let base_url = state
            .config
            .env
            .present("CENTAUR_API_URL")
            .unwrap_or("http://localhost:8080")
            .trim_end_matches('/')
            .to_owned();
        Self {
            state,
            base_url,
            timeout,
        }
    }

    /// The status and decoded JSON body (an empty or non-JSON body is `{}`).
    async fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        let token =
            tokens::console_service(&self.state.config.jwt_signing_secret, self.state.now())
                .map_err(|error| CentaurApiError::local(error.to_string()))?;
        let mut request = self
            .state
            .http
            .request(method, format!("{}{path}", self.base_url))
            .timeout(self.timeout)
            .bearer_auth(token)
            .header("Accept", "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CentaurApiError::local(without_url(error).to_string()))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| CentaurApiError::local(without_url(error).to_string()))?;
        let parsed = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
        Ok((status, parsed))
    }

    async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let (status, parsed) = self.send(method, path, body).await?;
        if (200..300).contains(&status) {
            return Ok(parsed);
        }
        let message = ["error", "message", "detail"]
            .iter()
            .find_map(|key| parsed.get(*key).and_then(Value::as_str))
            .filter(|message| !message.trim().is_empty())
            .map_or_else(
                || format!("Centaur API returned HTTP {status}"),
                str::to_owned,
            );
        Err(CentaurApiError {
            message,
            status: Some(status),
        })
    }

    fn team_ids(body: &Value) -> Vec<Value> {
        body.get("slack_external_team_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn slack_external_team_ids(&self) -> Result<Vec<Value>> {
        let body = self
            .request(Method::GET, "/api/slack/external-team-ids", None)
            .await?;
        Ok(Self::team_ids(&body))
    }

    pub async fn replace_slack_external_team_ids(&self, ids: &[String]) -> Result<Vec<Value>> {
        let body = self
            .request(
                Method::PUT,
                "/api/slack/external-team-ids",
                Some(&json!({ "slack_external_team_ids": ids })),
            )
            .await?;
        Ok(Self::team_ids(&body))
    }

    pub async fn list_workflow_schedules(&self) -> Result<Value> {
        self.request(Method::GET, "/api/workflows/schedules", None)
            .await
    }

    pub async fn get_workflow_run(&self, run_id: &str) -> Result<Value> {
        self.request(
            Method::GET,
            &format!("/api/workflows/runs/{}", segment(run_id)),
            None,
        )
        .await
    }

    pub async fn create_workflow_run(
        &self,
        workflow_name: &str,
        input: Option<&Value>,
    ) -> Result<Value> {
        let mut body = json!({ "workflow_name": workflow_name });
        if let Some(input) = input.filter(|input| !input.is_null()) {
            body["input"] = input.clone();
        }
        self.request(Method::POST, "/api/workflows/runs", Some(&body))
            .await
    }

    /// A hint that a scheduled task changed (api-rs also reconciles on an
    /// interval). Failures are logged by the caller's policy.
    pub async fn reconcile_scheduled_task(&self, oid: &str) -> Result<Value> {
        self.request(
            Method::POST,
            &format!("/api/admin/scheduled-tasks/{}/reconcile", segment(oid)),
            None,
        )
        .await
    }

    /// Asks for a manual run; whether api-rs queued it (a `202`).
    pub async fn run_scheduled_task(&self, oid: &str) -> Result<bool> {
        let (status, _) = self
            .send(
                Method::POST,
                &format!("/api/admin/scheduled-tasks/{}/run", segment(oid)),
                None,
            )
            .await?;
        Ok(status == 202)
    }
}
