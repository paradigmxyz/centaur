use std::sync::Arc;

use active_record_encryption::ActiveRecordEncryption;
use chrono::{DateTime, Utc};
use metrics_exporter_prometheus::PrometheusHandle;
use serde_json::Value;
use sqlx::PgPool;

use crate::cache::SyncCache;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) pool: PgPool,
    pub(crate) encryption: Arc<ActiveRecordEncryption>,
    pub(crate) jwt_secret: Option<String>,
    pub(crate) api_hosts: Vec<String>,
    pub(crate) console_host: Option<String>,
    pub(crate) sync_cache: Arc<SyncCache>,
    pub(crate) metrics: PrometheusHandle,
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
    pub(crate) principal_cache_version: Option<i64>,
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
