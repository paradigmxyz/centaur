//! Request and response types for the iron-control admin API.
//!
//! iron-control wraps every request and single-resource response in a
//! ``{ "data": ... }`` envelope; [`DataEnvelope`] handles both directions.
//! Object IDs are typed-prefix strings (``prn_``, ``role_``, ``ssr_``,
//! ``gas_``, ``ots_``, ``hms_``, ``aas_``, ``bcr_``, ``grant_``, ``prx_``). Resources with a ``foreign_id``
//! support upsert: a PUT whose path segment is a ``foreign_id`` (not an OID)
//! creates the resource if absent and updates it otherwise.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The ``{ "data": T }`` envelope used for request bodies and single-resource
/// responses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DataEnvelope<T> {
    pub data: T,
}

impl<T> DataEnvelope<T> {
    pub(crate) fn new(data: T) -> Self {
        Self { data }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Secret sources
// ---------------------------------------------------------------------------

/// Where iron-control resolves a credential value from.
///
/// ``source_type`` selects the resolver (``env``, ``aws_sm``, ``aws_ssm``,
/// ``1password``, ``1password_connect``, ``control_plane``, ``token_broker``)
/// and ``config`` carries the resolver-specific fields. ``secret`` is only set
/// for the ``control_plane`` inline source, which stores the value directly.
// Not `Eq`: `config` is an arbitrary `serde_json::Value`, which is only `PartialEq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecretSource {
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub config: Value,
}

impl SecretSource {
    /// An environment-variable source resolved on the iron-proxy container.
    pub fn env(var: impl Into<String>) -> Self {
        Self {
            source_type: "env".to_owned(),
            secret: None,
            config: serde_json::json!({ "var": var.into() }),
        }
    }

    /// A 1Password Connect source resolving ``op://`` style refs.
    pub fn onepassword_connect(secret_ref: impl Into<String>) -> Self {
        Self {
            source_type: "1password_connect".to_owned(),
            secret: None,
            config: serde_json::json!({ "secret_ref": secret_ref.into() }),
        }
    }

    /// A token-broker source; ``credential_id`` names the broker credential
    /// whose current access token iron-control delivers inline. When
    /// ``credential_id`` is a ``foreign_id`` (rather than a ``bcr_`` OID),
    /// ``credential_namespace`` is required so iron-control can resolve it.
    pub fn token_broker(
        credential_id: impl Into<String>,
        credential_namespace: impl Into<String>,
    ) -> Self {
        Self {
            source_type: "token_broker".to_owned(),
            secret: None,
            config: serde_json::json!({
                "credential_id": credential_id.into(),
                "credential_namespace": credential_namespace.into(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Request rules
// ---------------------------------------------------------------------------

/// Scopes a credential to matching outbound requests. Exactly one of ``host``
/// or ``cidr`` is required; ``http_methods`` and ``paths`` further narrow it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cidr: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl RequestRule {
    /// A rule matching every request to ``host``.
    pub fn host(host: impl Into<String>) -> Self {
        Self {
            host: Some(host.into()),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Static secrets
// ---------------------------------------------------------------------------

/// Adds a credential to the request itself; the tool never sees the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatter: Option<String>,
}

/// Replaces a tool-written placeholder token with the resolved credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceConfig {
    pub proxy_value: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub match_headers: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub match_body: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub match_path: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub match_query: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub require: bool,
}

/// Request body for ``POST``/``PUT /api/v1/static_secrets``. Exactly one of
/// ``inject_config`` or ``replace_config`` must be set.
// Not `Eq`: holds a `SecretSource` (arbitrary `Value` config).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StaticSecretInput {
    pub namespace: String,
    pub foreign_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_config: Option<InjectConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_config: Option<ReplaceConfig>,
    pub source: SecretSource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RequestRule>,
}

// ---------------------------------------------------------------------------
// OAuth token secrets
// ---------------------------------------------------------------------------

/// Request body for ``POST``/``PUT /api/v1/oauth_token_secrets``.
// Not `Eq`: holds `SecretSource` values (arbitrary `Value` config).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OAuthTokenSecretInput {
    pub namespace: String,
    pub foreign_id: String,
    pub name: String,
    pub grant: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub credentials: BTreeMap<String, SecretSource>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub token_endpoint_headers: BTreeMap<String, SecretSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RequestRule>,
}

// ---------------------------------------------------------------------------
// GCP auth secrets
// ---------------------------------------------------------------------------

/// Request body for ``POST``/``PUT /api/v1/gcp_auth_secrets``. Exactly one of
/// ``keyfile`` or ``credentials_provider`` must be set.
// Not `Eq`: `credentials_provider` is an arbitrary `Value`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GcpAuthSecretInput {
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyfile: Option<SecretSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_provider: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RequestRule>,
}

// ---------------------------------------------------------------------------
// GCP ID token secrets
// ---------------------------------------------------------------------------

/// Headers supported by iron-proxy's ``gcp_id_token`` transform.
pub const GCP_ID_TOKEN_ALLOWED_HEADERS: &[&str] = &["authorization", "x-serverless-authorization"];

/// Return the canonical lower-case header name when ``value`` is a supported
/// ``gcp_id_token`` injection header.
pub fn normalize_gcp_id_token_header(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    GCP_ID_TOKEN_ALLOWED_HEADERS
        .contains(&normalized.as_str())
        .then_some(normalized)
}

/// Request body for ``POST``/``PUT /api/v1/gcp_id_token_secrets``. iron-proxy
/// mints a Google-signed OIDC ID token for ``audience`` from the service-account
/// ``keyfile`` and injects it into ``Authorization`` by default, or
/// ``X-Serverless-Authorization`` when ``header`` is set accordingly.
// Not `Eq`: holds a `SecretSource` (arbitrary `Value` config).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GcpIdTokenSecretInput {
    pub namespace: String,
    pub foreign_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub audience: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub keyfile: SecretSource,
    pub rules: Vec<RequestRule>,
}

// ---------------------------------------------------------------------------
// AWS auth secrets
// ---------------------------------------------------------------------------

/// Request body for ``POST``/``PUT /api/v1/aws_auth_secrets``. iron-proxy
/// re-signs outbound AWS SigV4 requests: the tool signs each request with
/// throwaway placeholder credentials, and iron-proxy reads the region/service
/// from the inbound signature's credential scope, strips the signature, and
/// re-signs with the real keys it resolves from ``access_key_id`` /
/// ``secret_access_key`` (and optional ``session_token`` for STS). The real keys
/// never reach the sandbox. ``allowed_regions`` / ``allowed_services`` scope
/// which regions/services the proxy will sign for (empty = unscoped).
// Not `Eq`: holds `SecretSource` values (arbitrary `Value` config).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AwsAuthSecretInput {
    pub namespace: String,
    pub foreign_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub access_key_id: SecretSource,
    pub secret_access_key: SecretSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_token: Option<SecretSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_regions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_services: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RequestRule>,
}

// ---------------------------------------------------------------------------
// Postgres DSN secrets
// ---------------------------------------------------------------------------

/// Request body for ``POST``/``PUT /api/v1/pg_dsn_secrets``. ``dsn`` is the
/// upstream connection string, resolved like any other secret source;
/// ``database`` is the database name to connect to on both the proxied and the
/// upstream connection (the ``dsn`` source is opaque, so it can't be parsed out
/// of the connection string); ``role`` is an optional Postgres role the proxy
/// issues ``SET ROLE`` for. ``settings`` are optional Postgres GUCs the proxy
/// sets after connecting. A setting can carry either a literal ``value`` or a
/// structured ``value_from`` reference that iron-control resolves against the
/// proxy's assigned principal at sync time.
// Not `Eq`: holds a `SecretSource` (arbitrary `Value` config).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PgDsnSecretInput {
    pub namespace: String,
    pub foreign_id: String,
    pub name: String,
    pub database: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<PgDsnSettingInput>,
    pub dsn: SecretSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PgDsnSettingInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_from: Option<PgDsnSettingValueFromInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PgDsnSettingValueFromInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_label: Option<String>,
}

// ---------------------------------------------------------------------------
// HMAC signing secrets
// ---------------------------------------------------------------------------

/// One header iron-proxy writes onto the signed request. ``value`` is a Go
/// template evaluated against the signing context (``.Timestamp``,
/// ``.Signature``, ``.Credentials.<name>``).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HmacSecretHeader {
    pub name: String,
    pub value: String,
}

/// Request body for ``POST``/``PUT /api/v1/hmac_secrets``. iron-proxy resolves
/// each entry in ``credentials`` from its own source, composes the
/// ``signature_message`` template, HMACs it with the credential named
/// ``secret`` (decoded per ``signature_key_encoding``), encodes the digest per
/// ``signature_output_encoding``, and writes ``headers`` onto the upstream
/// request. The credentials and signing key never reach the sandbox.
// Not `Eq`: `credentials` holds `SecretSource` values (arbitrary `Value` config).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HmacSecretInput {
    pub namespace: String,
    pub foreign_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub timestamp_format: String,
    pub signature_algorithm: String,
    pub signature_key_encoding: String,
    pub signature_output_encoding: String,
    pub signature_message: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_chunked_body: bool,
    pub headers: Vec<HmacSecretHeader>,
    pub credentials: BTreeMap<String, SecretSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RequestRule>,
}

// ---------------------------------------------------------------------------
// Broker credentials
// ---------------------------------------------------------------------------

/// Request body for ``POST``/``PUT /api/v1/broker_credentials``. iron-control
/// owns the OAuth refresh loop for this credential and delivers the current
/// access token inline to proxies that reference it through a ``token_broker``
/// source. Unlike the secret resources, ``client_id`` is a literal value (not a
/// [`SecretSource`]) and is echoed back in responses; ``client_secret`` and
/// ``refresh_token`` are write-only literals (the latter a seed that, when set,
/// (re)bootstraps the credential). Tuning fields default in iron-control when
/// omitted.
// Not `Eq`: `early_refresh_fraction` is an `f64`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrokerCredentialInput {
    pub namespace: String,
    pub foreign_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub token_endpoint: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub token_endpoint_headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub early_refresh_slack_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub early_refresh_fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_refresh_interval_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_timeout_seconds: Option<u64>,
}

/// A broker credential as returned by iron-control. Only identity and the
/// read-only health fields callers display are captured; secret material is
/// never echoed. Unknown fields are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BrokerCredentialRecord {
    pub id: String,
    pub namespace: String,
    #[serde(default)]
    pub foreign_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Lifecycle state (``bootstrapping``, ``live``, ``dead``).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Principals and roles
// ---------------------------------------------------------------------------

/// Request body for ``POST``/``PUT /api/v1/principals`` and ``/roles`` — both
/// take the same ``namespace``/``foreign_id``/``name``/``labels`` shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IdentityInput {
    pub namespace: String,
    pub foreign_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A principal as returned by iron-control. Unknown fields are ignored, so this
/// captures only what callers need.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Principal {
    pub id: String,
    pub namespace: String,
    pub foreign_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub sandbox_observability_enabled: bool,
    #[serde(default = "default_true")]
    pub sandbox_api_server_enabled: bool,
}

/// Request body for creating/updating one Slack permission row on a principal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlackChannelPermissionInput {
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
    pub upload_enabled: bool,
    pub download_enabled: bool,
    pub history_enabled: bool,
}

/// A principal's effective config — the same secrets/postgres the principal's
/// proxy syncs. api-rs reads it to wire a sandbox's env. Only the fields api-rs
/// needs are captured; the proxy owns the rest (sources, rules, dsn, role).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct EffectiveConfig {
    #[serde(default)]
    pub secrets: Vec<EffectiveSecret>,
    #[serde(default)]
    pub postgres: Vec<EffectivePgDsn>,
}

/// One synced secret. Only replace secrets surface a placeholder the sandbox
/// must send; inject/oauth/gcp secrets are proxy-side and carry no `replace`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EffectiveSecret {
    #[serde(default)]
    pub replace: Option<EffectiveReplace>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EffectiveReplace {
    pub proxy_value: String,
}

/// One synced Postgres upstream. iron-control returns the `foreign_id` and the
/// `database` to connect to on both the proxied and upstream connection; the
/// proxy multiplexes every upstream through a single listener, routing by
/// `database`. The `dsn`/`role` are proxy-side, so they're not captured here;
/// api-rs derives the sandbox DSN env var name from `foreign_id` and assigns the
/// listener's shared local port and client credential.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EffectivePgDsn {
    pub foreign_id: String,
    pub database: String,
}

/// A role as returned by iron-control.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Role {
    pub id: String,
    pub namespace: String,
    pub foreign_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// A secret resource as returned by any of the ``*_secrets`` endpoints. Only
/// the identity fields are captured; grants reference the secret by ``id``.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SecretRecord {
    pub id: String,
    pub namespace: String,
    pub foreign_id: Option<String>,
    /// Human label. Optional because some secret types allow a null ``name``.
    #[serde(default)]
    pub name: Option<String>,
}

/// Every secret type as ``(type label, REST collection, OID prefix)``. Mirrors
/// the arms of [`Grant::secret_target`] and [`GrantSecret`]; callers sweep this
/// to enumerate secrets across all types and to route an OID to its endpoint by
/// prefix.
pub const SECRET_TYPES: &[(&str, &str, &str)] = &[
    ("static", "static_secrets", "ssr_"),
    ("oauth_token", "oauth_token_secrets", "ots_"),
    ("gcp_auth", "gcp_auth_secrets", "gas_"),
    ("gcp_id_token", "gcp_id_token_secrets", "gid_"),
    ("pg_dsn", "pg_dsn_secrets", "pgs_"),
    ("hmac", "hmac_secrets", "hms_"),
    ("aws_auth", "aws_auth_secrets", "aas_"),
];

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// The entity a grant attaches a secret to — a principal or a role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grantee {
    Principal(String),
    Role(String),
}

/// The secret a grant attaches, by iron-control OID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantSecret {
    Static(String),
    GcpAuth(String),
    GcpIdToken(String),
    OAuthToken(String),
    PgDsn(String),
    Hmac(String),
    AwsAuth(String),
}

impl GrantSecret {
    /// The wrapped secret OID, whichever secret type it is.
    pub fn oid(&self) -> &str {
        match self {
            Self::Static(id)
            | Self::GcpAuth(id)
            | Self::GcpIdToken(id)
            | Self::OAuthToken(id)
            | Self::PgDsn(id)
            | Self::Hmac(id)
            | Self::AwsAuth(id) => id,
        }
    }

    /// Route an OID to its grant variant by prefix (see [`SECRET_TYPES`]).
    /// `None` when the OID matches no known secret type.
    pub fn from_oid(oid: &str) -> Option<Self> {
        let (label, ..) = SECRET_TYPES
            .iter()
            .find(|(_, _, prefix)| oid.starts_with(prefix))?;
        let id = oid.to_owned();
        Some(match *label {
            "static" => Self::Static(id),
            "oauth_token" => Self::OAuthToken(id),
            "gcp_auth" => Self::GcpAuth(id),
            "gcp_id_token" => Self::GcpIdToken(id),
            "pg_dsn" => Self::PgDsn(id),
            "hmac" => Self::Hmac(id),
            "aws_auth" => Self::AwsAuth(id),
            _ => return None,
        })
    }
}

/// A grant as returned by ``POST /api/v1/grants`` and the grantee-scoped list
/// endpoints (``GET /api/v1/{principals,roles}/:id/grants``). Create responses
/// populate the grantee/secret references too; only the relevant ids are set.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Grant {
    pub id: String,
    #[serde(default)]
    pub principal_id: Option<String>,
    #[serde(default)]
    pub role_id: Option<String>,
    #[serde(default)]
    pub static_secret_id: Option<String>,
    #[serde(default)]
    pub oauth_token_secret_id: Option<String>,
    #[serde(default)]
    pub gcp_auth_secret_id: Option<String>,
    #[serde(default)]
    pub gcp_id_token_secret_id: Option<String>,
    #[serde(default)]
    pub pg_dsn_secret_id: Option<String>,
    #[serde(default)]
    pub hmac_secret_id: Option<String>,
    #[serde(default)]
    pub aws_auth_secret_id: Option<String>,
}

impl Grant {
    /// The granted secret's OID, whichever secret type it is.
    pub fn secret_id(&self) -> Option<&str> {
        self.static_secret_id
            .as_deref()
            .or(self.oauth_token_secret_id.as_deref())
            .or(self.gcp_auth_secret_id.as_deref())
            .or(self.gcp_id_token_secret_id.as_deref())
            .or(self.pg_dsn_secret_id.as_deref())
            .or(self.hmac_secret_id.as_deref())
            .or(self.aws_auth_secret_id.as_deref())
    }

    /// The granted secret's ``(type label, REST collection, OID)``, whichever
    /// secret type it is. The collection is the path segment for fetching the
    /// secret (e.g. to resolve its name).
    pub fn secret_target(&self) -> Option<(&'static str, &'static str, &str)> {
        if let Some(id) = &self.static_secret_id {
            Some(("static", "static_secrets", id))
        } else if let Some(id) = &self.oauth_token_secret_id {
            Some(("oauth_token", "oauth_token_secrets", id))
        } else if let Some(id) = &self.gcp_auth_secret_id {
            Some(("gcp_auth", "gcp_auth_secrets", id))
        } else if let Some(id) = &self.gcp_id_token_secret_id {
            Some(("gcp_id_token", "gcp_id_token_secrets", id))
        } else if let Some(id) = &self.pg_dsn_secret_id {
            Some(("pg_dsn", "pg_dsn_secrets", id))
        } else if let Some(id) = &self.hmac_secret_id {
            Some(("hmac", "hmac_secrets", id))
        } else if let Some(id) = &self.aws_auth_secret_id {
            Some(("aws_auth", "aws_auth_secrets", id))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Proxies
// ---------------------------------------------------------------------------

/// Request body for ``POST /api/v1/proxies``.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProxyInput {
    pub name: String,
    pub principal_id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A registered proxy. ``token`` (the plaintext ``iprx_`` bearer) is only
/// present on the create response.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Proxy {
    pub id: String,
    pub name: String,
    pub principal_id: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub config_hash: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{SlackChannelPermissionInput, normalize_gcp_id_token_header};

    #[test]
    fn normalizes_supported_gcp_id_token_headers() {
        assert_eq!(
            normalize_gcp_id_token_header("Authorization").as_deref(),
            Some("authorization")
        );
        assert_eq!(
            normalize_gcp_id_token_header(" X-Serverless-Authorization ").as_deref(),
            Some("x-serverless-authorization")
        );
        assert_eq!(normalize_gcp_id_token_header("x-other"), None);
    }

    #[test]
    fn slack_channel_permission_serializes_false_values() {
        let value = serde_json::to_value(SlackChannelPermissionInput {
            channel_id: "C0123456789".to_owned(),
            channel_name: None,
            upload_enabled: false,
            download_enabled: true,
            history_enabled: false,
        })
        .unwrap();

        assert_eq!(value["upload_enabled"], false);
        assert_eq!(value["download_enabled"], true);
        assert_eq!(value["history_enabled"], false);
    }
}
