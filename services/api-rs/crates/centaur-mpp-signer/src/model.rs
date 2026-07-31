use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Catalog {
    pub services: Vec<Service>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub service_url: Option<String>,
    pub url: Option<String>,
    pub realm: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub endpoints: Vec<Endpoint>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Service {
    pub fn base_url(&self) -> Option<&str> {
        self.service_url.as_deref().or(self.url.as_deref())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
    pub description: Option<String>,
    pub payment: Option<PaymentOffer>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PaymentOffer {
    pub intent: Option<String>,
    pub method: Option<String>,
    pub amount: Option<String>,
    pub currency: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct RegistrySnapshot {
    pub catalog: Catalog,
    pub fetched_at: OffsetDateTime,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RegisteredRoute {
    pub service: Service,
    pub endpoint: Endpoint,
}

#[derive(Clone, Debug)]
pub struct ActiveExecution {
    pub execution_id: String,
    pub sandbox_id: String,
    pub thread_key: String,
}

#[derive(Clone, Debug)]
pub struct NewAttempt {
    pub attempt_id: Uuid,
    pub challenge_hash: String,
    pub service_id: String,
    pub method: String,
    pub path_template: String,
    pub amount_atomic: i64,
    pub currency: String,
    pub sandbox_id: String,
    pub execution_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BeginAttempt {
    Created,
    Duplicate {
        attempt_id: Uuid,
        sandbox_id: String,
        execution_id: String,
    },
    BudgetDenied {
        reason: &'static str,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionOutcome {
    Settled,
    Released,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct CompletedAttempt {
    pub service_id: String,
    pub method: String,
    pub amount_atomic: i64,
    pub currency: String,
    pub outcome: CompletionOutcome,
    pub reason: String,
}

impl CompletionOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Settled => "settled",
            Self::Released => "released",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuthorizeRequest {
    pub host: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    #[serde(default)]
    pub response_headers: HashMap<String, Vec<String>>,
    #[serde(default = "default_true")]
    pub replayable: bool,
    pub sandbox_id: String,
    pub traceparent: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthorizeResponse {
    pub retry: bool,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

impl AuthorizeResponse {
    pub fn decline(reason: &'static str) -> Self {
        Self {
            retry: false,
            headers: HashMap::new(),
            attempt_id: None,
            traceparent: None,
            reason: Some(reason),
        }
    }

    pub fn retry(attempt_id: Uuid, authorization: String) -> Self {
        Self {
            retry: true,
            headers: HashMap::from([("Authorization".to_owned(), authorization)]),
            attempt_id: Some(attempt_id),
            traceparent: None,
            reason: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct CompleteRequest {
    pub attempt_id: Uuid,
    pub replay_status: Option<u16>,
    #[serde(default)]
    pub response_headers: HashMap<String, Vec<String>>,
    pub transport_error: Option<String>,
    pub traceparent: Option<String>,
    pub replay_duration_ms: Option<u64>,
    pub charge_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompleteResponse {
    pub ok: bool,
    pub outcome: &'static str,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PolicyRule {
    pub effect: PolicyEffect,
    pub service: Option<String>,
    pub category: Option<String>,
    pub realm: Option<String>,
    pub methods: Option<Vec<String>>,
    pub path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PolicyEffect {
    Allow,
    Deny,
}
