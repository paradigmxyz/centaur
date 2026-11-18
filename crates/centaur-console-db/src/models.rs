//! Console's credential records, in the shape proxy configuration is built from.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialKind {
    Static,
    GcpAuth,
    GcpIdToken,
    AwsAuth,
    OauthToken,
    PgDsn,
    Hmac,
}

impl CredentialKind {
    pub fn parse(value: &str) -> Option<Self> {
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
pub struct Credential {
    pub kind: CredentialKind,
    pub id: i64,
    pub priority: i32,
    pub data: CredentialData,
    pub sources: Vec<SecretSource>,
    pub rules: Vec<RequestRule>,
}

#[derive(Debug)]
pub enum CredentialData {
    Static(StaticData),
    GcpAuth(GcpAuthData),
    GcpIdToken(GcpIdTokenData),
    AwsAuth(AwsAuthData),
    OauthToken(OauthTokenData),
    PgDsn(PgDsnData),
    Hmac(HmacData),
}

#[derive(Debug, Deserialize)]
pub struct StaticData {
    pub inject_config: Option<Value>,
    pub replace_config: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct GcpAuthData {
    pub credentials_provider: Option<Value>,
    pub subject: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GcpIdTokenData {
    pub audience: String,
    pub header: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AwsAuthData {
    pub allowed_regions: Vec<String>,
    pub allowed_services: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct OauthTokenData {
    pub grant: String,
    pub token_endpoint: String,
    pub audience: Option<String>,
    pub scopes: Vec<String>,
    pub header: Option<String>,
    pub value_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PgDsnData {
    pub foreign_id: String,
    pub database: String,
    pub role: Option<String>,
    pub settings: Vec<PostgresSetting>,
}

#[derive(Debug, Deserialize)]
pub struct PostgresSetting {
    pub name: String,
    pub value: Option<Value>,
    pub value_from: Option<PostgresValueFrom>,
}

#[derive(Debug, Deserialize)]
pub struct PostgresValueFrom {
    pub principal_label: Option<String>,
    pub proxy_label: Option<String>,
    pub principal_field: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HmacData {
    pub timestamp_format: String,
    pub signature_algorithm: String,
    pub signature_key_encoding: String,
    pub signature_output_encoding: String,
    pub signature_message: String,
    pub headers: Vec<HmacHeader>,
    pub allow_chunked_body: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HmacHeader {
    pub name: String,
    pub value: String,
}

/// `Debug` redacts `secret`, `broker_access_token`, and the config values.
#[derive(Deserialize)]
pub struct SecretSource {
    pub source_type: String,
    pub config: Map<String, Value>,
    pub secret: Option<String>,
    pub broker_access_token: Option<String>,
    pub role: Option<String>,
    pub role_kind: Option<String>,
}

impl std::fmt::Debug for SecretSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::redact::{self, Keys};
        f.debug_struct("SecretSource")
            .field("source_type", &self.source_type)
            .field("config", &Keys(self.config.keys()))
            .field("secret", &redact::option(&self.secret))
            .field(
                "broker_access_token",
                &redact::option(&self.broker_access_token),
            )
            .field("role", &self.role)
            .field("role_kind", &self.role_kind)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct RequestRule {
    pub host: Option<String>,
    pub cidr: Option<String>,
    pub http_methods: Vec<String>,
    pub paths: Vec<String>,
}

/// A principal's proxy configuration: what iron-proxy receives on sync.
/// `Debug` shows counts only (the entries carry secret values).
#[derive(Default)]
pub struct Config {
    pub secrets: Vec<Value>,
    pub transforms: Vec<Value>,
    pub postgres: Vec<Value>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("secrets", &self.secrets.len())
            .field("transforms", &self.transforms.len())
            .field("postgres", &self.postgres.len())
            .finish_non_exhaustive()
    }
}
