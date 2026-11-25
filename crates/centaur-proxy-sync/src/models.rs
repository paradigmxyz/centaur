use std::sync::Arc;

use active_record_encryption::ActiveRecordEncryption;
use centaur_codemode_protocol::token::TurnTokenVerifier;
use chrono::{DateTime, Utc};
use metrics_exporter_prometheus::PrometheusHandle;
use serde_json::Value;
use sqlx::PgPool;

use crate::cache::SyncCache;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) ssh_targets: Vec<crate::ssh::TargetPolicy>,
    pub(crate) pool: PgPool,
    pub(crate) encryption: Arc<ActiveRecordEncryption>,
    pub(crate) jwt_secret: Option<String>,
    /// Bearer token the tenancy iron-proxy presents to the principal config
    /// route. `None` leaves that route disabled.
    pub(crate) codemode_sync_token: Option<String>,
    /// Verifies the turn token iron-proxy forwards on the principal config
    /// route. `None` makes that route refuse every tenant token.
    pub(crate) codemode_token_verifier: Option<TurnTokenVerifier>,
    pub(crate) api_hosts: Vec<String>,
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
    pub(crate) console_user_admin: bool,
    pub(crate) console_user_status: Option<String>,
    pub(crate) slack_bot_home_team: bool,
    pub(crate) slack_history_channel_ids: Value,
    /// An owner-only token's liveness (expiry, owner status, app state) is
    /// not covered by the principal cache version, so such configs are always
    /// assembled live.
    pub(crate) has_owner_only_grant: bool,
}

impl ProxyRecord {
    pub(crate) fn principal_field(&self, field: &str) -> &Value {
        self.principal
            .as_ref()
            .and_then(|principal| principal.get(field))
            .unwrap_or(&Value::Null)
    }

    /// The same rule api-rs re-checks live: a Slack DM in the bot's home
    /// workspace linked to an active Console admin may act as that admin on the
    /// Console API.
    pub(crate) fn console_delegate(&self) -> bool {
        self.principal_field("kind").as_str() == Some("slack_dm")
            && self.slack_bot_home_team
            && self.console_user_admin
            && self.console_user_status.as_deref() == Some("active")
    }
}
