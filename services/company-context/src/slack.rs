use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{config::Config, errors::rejected};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Slack sends Retry-After with every rate limit; this covers a missing header.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Ingested conversation types and the read and history scopes each requires.
const INGESTED_CONVERSATION_TYPES: &[(&str, [&str; 2])] = &[
    ("public_channel", ["channels:read", "channels:history"]),
    ("private_channel", ["groups:read", "groups:history"]),
];

/// Slack errors that retrying the same request with the same token cannot fix.
const PERMANENT_ERRORS: &[&str] = &[
    "account_inactive",
    "ekm_access_denied",
    "invalid_auth",
    "invalid_cursor",
    "missing_scope",
    "no_permission",
    "not_allowed_token_type",
    "not_authed",
    "team_access_not_granted",
    "token_expired",
    "token_revoked",
];

/// Returns the ingested conversation types the scopes can list and read.
pub fn conversation_types(scopes: &[String]) -> Vec<&'static str> {
    INGESTED_CONVERSATION_TYPES
        .iter()
        .filter(|(_, required)| {
            required
                .iter()
                .all(|scope| scopes.iter().any(|granted| granted == scope))
        })
        .map(|(kind, _)| *kind)
        .collect()
}

/// Slack Web API methods paced against the app's shared rate limits.
#[derive(Clone, Copy, Debug)]
pub enum SlackMethod {
    UsersConversations,
}

impl SlackMethod {
    pub fn name(self) -> &'static str {
        match self {
            Self::UsersConversations => "users.conversations",
        }
    }

    /// Documented requests per minute of the method's Slack rate-limit tier.
    pub fn tier_per_minute(self) -> f64 {
        match self {
            // Tier 3.
            Self::UsersConversations => 50.0,
        }
    }
}

#[derive(Clone)]
pub struct SlackClient {
    http: Client,
    base_url: String,
}

#[derive(Debug)]
pub enum SlackReply {
    Ok(Value),
    RateLimited(Duration),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthTest {
    pub team_id: String,
    pub user_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConversationsPage {
    #[serde(default)]
    pub channels: Vec<Conversation>,
    #[serde(default)]
    pub response_metadata: ResponseMetadata,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ResponseMetadata {
    #[serde(default)]
    pub next_cursor: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Conversation {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_private: bool,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub is_im: bool,
    #[serde(default)]
    pub is_mpim: bool,
    #[serde(default)]
    pub topic: TextValue,
    #[serde(default)]
    pub purpose: TextValue,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TextValue {
    #[serde(default)]
    pub value: String,
}

impl Conversation {
    pub fn kind(&self) -> &'static str {
        if self.is_im {
            "im"
        } else if self.is_mpim {
            "mpim"
        } else if self.is_private {
            "private_channel"
        } else {
            "public_channel"
        }
    }
}

impl SlackClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Client::builder().timeout(REQUEST_TIMEOUT).build()?,
            base_url: config.slack_api_base_url.clone(),
        })
    }

    /// Calls a Web API method once. Rate limits are returned rather than
    /// retried so the caller can pace the retry against the shared limit.
    pub async fn call(
        &self,
        method: &str,
        access_token: &str,
        params: &[(&str, String)],
    ) -> Result<SlackReply> {
        let response = self
            .http
            .post(format!("{}/{method}", self.base_url))
            .bearer_auth(access_token)
            .form(params)
            .send()
            .await
            .with_context(|| format!("send Slack {method} request"))?;
        let status = response.status();
        let retry_after = retry_after(&response);
        if status == StatusCode::TOO_MANY_REQUESTS {
            record_request(method, "rate_limited");
            return Ok(SlackReply::RateLimited(retry_after));
        }
        if !status.is_success() {
            record_request(method, "error");
            if status.is_server_error() {
                bail!("Slack {method} returned HTTP {status}");
            }
            return Err(rejected(format!("Slack {method} returned HTTP {status}")));
        }
        let body: Value = response
            .json()
            .await
            .with_context(|| format!("decode Slack {method} response"))?;
        if body.get("ok").and_then(Value::as_bool) == Some(true) {
            record_request(method, "ok");
            return Ok(SlackReply::Ok(body));
        }
        let error = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        if error == "ratelimited" {
            record_request(method, "rate_limited");
            return Ok(SlackReply::RateLimited(retry_after));
        }
        record_request(method, "error");
        if PERMANENT_ERRORS.contains(&error) {
            return Err(rejected(format!("Slack {method} failed: {error}")));
        }
        bail!("Slack {method} failed: {error}")
    }
}

fn record_request(method: &str, outcome: &'static str) {
    metrics::counter!(
        "company_context_slack_requests_total",
        "method" => method.to_owned(),
        "outcome" => outcome
    )
    .increment(1);
}

fn retry_after(response: &reqwest::Response) -> Duration {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_RETRY_AFTER)
}

#[cfg(test)]
mod tests {
    use axum::{
        Form, Router,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
    };
    use serde_json::json;
    use std::collections::HashMap;

    use super::*;
    use crate::errors::is_rejected;

    #[test]
    fn conversation_types_require_both_read_and_history_scopes() {
        let scopes = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| value.to_string()).collect()
        };
        assert_eq!(
            conversation_types(&scopes(&[
                "channels:read",
                "channels:history",
                "groups:read",
                "groups:history"
            ])),
            ["public_channel", "private_channel"]
        );
        assert_eq!(
            conversation_types(&scopes(&["channels:read", "groups:history"])),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn conversation_kind_follows_slack_flags() {
        let kind = |value: Value| {
            serde_json::from_value::<Conversation>(value)
                .unwrap()
                .kind()
        };
        assert_eq!(kind(json!({ "id": "C1" })), "public_channel");
        assert_eq!(
            kind(json!({ "id": "C1", "is_private": true })),
            "private_channel"
        );
        assert_eq!(
            kind(json!({ "id": "C1", "is_private": true, "is_mpim": true })),
            "mpim"
        );
        assert_eq!(kind(json!({ "id": "D1", "is_im": true })), "im");
    }

    /// Serves Slack-shaped responses selected by the request's `case` parameter.
    async fn fake_slack(
        headers: HeaderMap,
        Form(params): Form<HashMap<String, String>>,
    ) -> Response {
        assert_eq!(headers["authorization"], "Bearer token-1");
        match params["case"].as_str() {
            "ok" => axum::Json(json!({ "ok": true, "user_id": "U1" })).into_response(),
            "throttled" => (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "7")]).into_response(),
            "revoked" => {
                axum::Json(json!({ "ok": false, "error": "token_revoked" })).into_response()
            }
            "internal" => {
                axum::Json(json!({ "ok": false, "error": "internal_error" })).into_response()
            }
            _ => StatusCode::BAD_GATEWAY.into_response(),
        }
    }

    #[tokio::test]
    async fn classifies_slack_replies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/{method}", post(fake_slack))).await
        });
        let client = SlackClient {
            http: Client::new(),
            base_url: format!("http://{address}"),
        };
        let call = |case: &str| {
            let client = client.clone();
            let params = vec![("case", case.to_owned())];
            async move { client.call("auth.test", "token-1", &params).await }
        };

        assert!(
            matches!(call("ok").await.unwrap(), SlackReply::Ok(body) if body["user_id"] == "U1")
        );
        assert!(matches!(
            call("throttled").await.unwrap(),
            SlackReply::RateLimited(delay) if delay == Duration::from_secs(7)
        ));
        assert!(is_rejected(&call("revoked").await.unwrap_err()));
        let internal = call("internal").await.unwrap_err();
        assert!(!is_rejected(&internal), "Slack internal errors are retried");
        let gateway = call("gateway").await.unwrap_err();
        assert!(!is_rejected(&gateway), "server errors are retried");
        server.abort();
    }
}
