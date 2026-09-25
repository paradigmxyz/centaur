use std::sync::Arc;

use active_record_encryption::ActiveRecordEncryption;
use chrono::{DateTime, Utc};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CredentialKind {
    Static,
    GcpAuth,
    GcpIdToken,
    AwsAuth,
    OauthToken,
    PgDsn,
    Hmac,
}

impl CredentialKind {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "static" => Some(Self::Static),
            "gcp_auth" => Some(Self::GcpAuth),
            "gcp_id_token" => Some(Self::GcpIdToken),
            "aws_auth" => Some(Self::AwsAuth),
            "oauth_token" => Some(Self::OauthToken),
            "pg_dsn" => Some(Self::PgDsn),
            "hmac" => Some(Self::Hmac),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Credential {
    pub(crate) kind: CredentialKind,
    pub(crate) id: i64,
    pub(crate) priority: i32,
    pub(crate) data: CredentialData,
    pub(crate) sources: Vec<SecretSource>,
    pub(crate) rules: Vec<RequestRule>,
}

#[derive(Debug)]
pub(crate) enum CredentialData {
    Static(StaticData),
    GcpAuth(GcpAuthData),
    GcpIdToken(GcpIdTokenData),
    AwsAuth(AwsAuthData),
    OauthToken(OauthTokenData),
    PgDsn(PgDsnData),
    Hmac(HmacData),
}

#[derive(Debug, Deserialize)]
pub(crate) struct StaticData {
    pub(crate) inject_config: Option<Value>,
    pub(crate) replace_config: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GcpAuthData {
    pub(crate) credentials_provider: Option<Value>,
    pub(crate) subject: Option<String>,
    pub(crate) scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GcpIdTokenData {
    pub(crate) audience: String,
    pub(crate) header: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AwsAuthData {
    pub(crate) allowed_regions: Vec<String>,
    pub(crate) allowed_services: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OauthTokenData {
    pub(crate) grant: String,
    pub(crate) token_endpoint: String,
    pub(crate) audience: Option<String>,
    pub(crate) scopes: Vec<String>,
    pub(crate) header: Option<String>,
    pub(crate) value_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PgDsnData {
    pub(crate) foreign_id: String,
    pub(crate) database: String,
    pub(crate) role: Option<String>,
    pub(crate) settings: Vec<PostgresSetting>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PostgresSetting {
    pub(crate) name: String,
    pub(crate) value: Option<Value>,
    pub(crate) value_from: Option<PostgresValueFrom>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PostgresValueFrom {
    pub(crate) principal_label: Option<String>,
    pub(crate) proxy_label: Option<String>,
    pub(crate) principal_field: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HmacData {
    pub(crate) timestamp_format: String,
    pub(crate) signature_algorithm: String,
    pub(crate) signature_key_encoding: String,
    pub(crate) signature_output_encoding: String,
    pub(crate) signature_message: String,
    pub(crate) headers: Vec<HmacHeader>,
    pub(crate) allow_chunked_body: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct HmacHeader {
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SecretSource {
    pub(crate) source_type: String,
    pub(crate) config: Map<String, Value>,
    pub(crate) secret: Option<String>,
    pub(crate) broker_access_token: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) role_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RequestRule {
    pub(crate) host: Option<String>,
    pub(crate) cidr: Option<String>,
    pub(crate) http_methods: Vec<String>,
    pub(crate) paths: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Config {
    pub(crate) secrets: Vec<Value>,
    pub(crate) transforms: Vec<Value>,
    pub(crate) postgres: Vec<Value>,
}
