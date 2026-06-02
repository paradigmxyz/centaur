//! Request and response types for the iron-control admin API.
//!
//! iron-control wraps every request and single-resource response in a
//! ``{ "data": ... }`` envelope; [`DataEnvelope`] handles both directions.
//! Object IDs are typed-prefix strings (``prn_``, ``role_``, ``ssr_``,
//! ``gas_``, ``ots_``, ``grant_``, ``prx_``). Resources with a ``foreign_id``
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
    /// whose current access token iron-proxy injects.
    pub fn token_broker(credential_id: impl Into<String>) -> Self {
        Self {
            source_type: "token_broker".to_owned(),
            secret: None,
            config: serde_json::json!({ "credential_id": credential_id.into() }),
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
}

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
    OAuthToken(String),
}

/// A created grant as returned by ``POST /api/v1/grants``.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Grant {
    pub id: String,
}

// ---------------------------------------------------------------------------
// Proxies
// ---------------------------------------------------------------------------

/// Request body for ``POST /api/v1/proxies``.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProxyInput {
    pub name: String,
    pub principal_id: String,
}

/// A registered proxy. ``token`` (the plaintext ``iprx_`` bearer) is only
/// present on the create response.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Proxy {
    pub id: String,
    pub name: String,
    pub principal_id: String,
    #[serde(default)]
    pub token: Option<String>,
}
