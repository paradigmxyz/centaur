use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::active_record_encryption::ActiveRecordEncryption;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) pool: PgPool,
    pub(crate) encryption: Arc<ActiveRecordEncryption>,
    pub(crate) jwt_secret: Option<String>,
    pub(crate) api_hosts: Vec<String>,
    pub(crate) console_host: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ProxyRecord {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) labels: Value,
    pub(crate) principal_id: Option<i64>,
    pub(crate) requester_principal_id: Option<i64>,
    pub(crate) principal_assigned_at: Option<DateTime<Utc>>,
    pub(crate) requester_principal_assigned_at: Option<DateTime<Utc>>,
    pub(crate) principal: Option<Value>,
    pub(crate) console_user_email: Option<String>,
    pub(crate) console_user_id: Option<i64>,
    pub(crate) slack_history_channel_ids: Value,
}

impl ProxyRecord {
    pub(crate) fn principal_field(&self, field: &str) -> &Value {
        self.principal
            .as_ref()
            .and_then(|principal| principal.get(field))
            .unwrap_or(&Value::Null)
    }
}

#[derive(Debug)]
pub(crate) struct Credential {
    pub(crate) kind: String,
    pub(crate) id: i64,
    pub(crate) priority: i32,
    pub(crate) data: Value,
    pub(crate) sources: Vec<Value>,
    pub(crate) rules: Vec<Value>,
}

#[derive(Default)]
pub(crate) struct Config {
    pub(crate) secrets: Vec<Value>,
    pub(crate) transforms: Vec<Value>,
    pub(crate) postgres: Vec<Value>,
}
