//! Translate iron-proxy [`ProxyFragment`]s into iron-control resources and
//! register them.
//!
//! Today the proxy config is rendered from fragments and baked into a
//! per-sandbox ConfigMap. Under iron-control the same fragments become durable
//! control-plane state: each fragment's secrets are upserted as typed secret
//! resources and granted to a role. api-rs registers infra and harness
//! fragments against the shared infra role.
//!
//! [`secret_inputs_from_fragment`] is the pure translation (fragment → secret
//! inputs) and is unit-tested without a server; [`register_role`] drives the
//! client to upsert the role, upsert each secret, and grant it to the role.

use std::collections::{BTreeMap, BTreeSet};

use centaur_iron_proxy::{
    PgDsnSetting, PgDsnSettingValueFrom, PostgresListener, ProxyFragment, Secret, SecretReplace,
    SourceKind, SourcePolicy, pg_foreign_id,
};
use serde_json::{Value as JsonValue, json};
use serde_yaml::Value as YamlValue;

use crate::client::IronControlClient;
use crate::error::IronControlError;
use crate::models::{
    AwsAuthSecretInput, GcpAuthSecretInput, GcpIdTokenSecretInput, GrantSecret, Grantee,
    HmacSecretInput, IdentityInput, InjectConfig, OAuthTokenSecretInput, PgDsnSecretInput,
    PgDsnSettingInput, PgDsnSettingValueFromInput, ReplaceConfig, RequestRule, SecretSource,
    StaticSecretInput,
};
use crate::util::{managed_labels, slugify};

/// A role to register secrets against. ``foreign_id`` is the stable upsert key
/// (e.g. ``infra`` or ``tool-github``); ``name`` is the human label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleSpec {
    pub foreign_id: String,
    pub name: String,
}

impl RoleSpec {
    /// The single shared infra role.
    pub fn infra() -> Self {
        Self {
            foreign_id: "infra".to_owned(),
            name: "Infra".to_owned(),
        }
    }

    /// A role scoped to one discovered tool.
    pub fn tool(name: &str) -> Self {
        Self {
            foreign_id: format!("tool-{}", slugify(name)),
            name: format!("Tool {name}"),
        }
    }
}

/// One translated secret, tagged so [`register_role`] can pick the matching
/// upsert endpoint and grant variant.
#[derive(Clone, Debug, PartialEq)]
pub enum SecretInput {
    Static(StaticSecretInput),
    OAuthToken(OAuthTokenSecretInput),
    GcpAuth(GcpAuthSecretInput),
    GcpIdToken(GcpIdTokenSecretInput),
    PgDsn(PgDsnSecretInput),
    Hmac(HmacSecretInput),
    AwsAuth(AwsAuthSecretInput),
}

/// A fragment transform iron-control cannot represent, or a malformed entry.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    #[error("iron-control cannot represent {what}; no tool uses it today")]
    Unsupported { what: String },
    #[error("malformed iron-proxy secret in role {role}: {detail}")]
    Malformed { role: String, detail: String },
}

/// Failure registering a role: either translation or an iron-control call.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error(transparent)]
    Translate(#[from] TranslateError),
    #[error(transparent)]
    Control(#[from] IronControlError),
}

/// The default GCP OAuth scope applied when a ``gcp_auth`` secret declares
/// none, mirroring ``GCP_AUTH_SCOPES`` in the Python ``proxy_config``. iron-
/// control requires a non-empty ``scopes``, so a scope-less entry would
/// otherwise be rejected.
pub const GCP_AUTH_DEFAULT_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// ``scopes`` if non-empty, else the single default GCP scope.
pub fn gcp_auth_scopes_or_default(scopes: Vec<String>) -> Vec<String> {
    if scopes.is_empty() {
        vec![GCP_AUTH_DEFAULT_SCOPE.to_owned()]
    } else {
        scopes
    }
}

/// Upsert ``role``, upsert every secret the fragment declares, and grant each
/// to the role. Idempotent: foreign-id upserts mean re-running converges.
/// Returns the role's iron-control OID so callers can assign it to principals
/// without a follow-up lookup.
pub async fn register_role(
    client: &IronControlClient,
    namespace: &str,
    role: &RoleSpec,
    fragment: &ProxyFragment,
    policy: &SourcePolicy,
) -> Result<String, RegisterError> {
    let inputs = secret_inputs_from_fragment(namespace, &role.foreign_id, fragment, policy)?;
    let role_record = client
        .upsert_role(&IdentityInput {
            namespace: namespace.to_owned(),
            foreign_id: role.foreign_id.clone(),
            name: role.name.clone(),
            labels: managed_labels(),
        })
        .await?;
    grant_inputs_to_role(client, &role_record.id, inputs).await?;
    Ok(role_record.id)
}

/// Upsert each secret in ``inputs`` and grant it to the role identified by
/// ``role_oid``, returning the grant OIDs in input order. Idempotent when the
/// inputs carry stable ``foreign_id``s: re-running re-upserts each secret and
/// reuses the role's existing grant for it instead of posting a duplicate.
/// This is the wire-driving half of [`register_role`], factored out so callers
/// that build [`SecretInput`]s from a source other than an iron-proxy fragment
/// (e.g. a tool's ``pyproject.toml``) can reuse it.
pub async fn grant_inputs_to_role(
    client: &IronControlClient,
    role_oid: &str,
    inputs: Vec<SecretInput>,
) -> Result<Vec<String>, IronControlError> {
    let existing = client.list_role_grants(role_oid).await?;
    let mut grant_ids = Vec::with_capacity(inputs.len());
    for input in inputs {
        let secret = match input {
            SecretInput::Static(input) => {
                GrantSecret::Static(client.upsert_static_secret(&input).await?.id)
            }
            SecretInput::OAuthToken(input) => {
                GrantSecret::OAuthToken(client.upsert_oauth_token_secret(&input).await?.id)
            }
            SecretInput::GcpAuth(input) => {
                GrantSecret::GcpAuth(client.upsert_gcp_auth_secret(&input).await?.id)
            }
            SecretInput::GcpIdToken(input) => {
                GrantSecret::GcpIdToken(client.upsert_gcp_id_token_secret(&input).await?.id)
            }
            SecretInput::PgDsn(input) => {
                GrantSecret::PgDsn(client.upsert_pg_dsn_secret(&input).await?.id)
            }
            SecretInput::Hmac(input) => {
                GrantSecret::Hmac(client.upsert_hmac_secret(&input).await?.id)
            }
            SecretInput::AwsAuth(input) => {
                GrantSecret::AwsAuth(client.upsert_aws_auth_secret(&input).await?.id)
            }
        };
        if let Some(grant) = existing
            .iter()
            .find(|grant| grant.secret_id() == Some(secret.oid()))
        {
            grant_ids.push(grant.id.clone());
            continue;
        }
        let grant = client
            .create_grant(&Grantee::Role(role_oid.to_owned()), &secret)
            .await?;
        grant_ids.push(grant.id);
    }
    Ok(grant_ids)
}

/// Pure translation: a fragment's transforms → the secret resources to upsert.
///
/// Only the transform shapes Centaur uses are translated: the ``secrets``
/// transform (replace and inject, including ``token_broker`` sources),
/// ``oauth_token``, ``gcp_auth``, ``gcp_id_token``, and ``aws_auth``. Postgres
/// listeners translate to ``pg_dsn`` secrets (one per listener). ``hmac_sign``
/// errors out here: it is represented in iron-control (see [`HmacSecretInput`]),
/// but only the infra and harness fragments flow through this fragment
/// translator and none sign requests — tool ``hmac_sign`` secrets are
/// operator-managed via the ``centaur-perms`` CLI, which parses
/// ``pyproject.toml`` directly.
pub fn secret_inputs_from_fragment(
    namespace: &str,
    role_foreign_id: &str,
    fragment: &ProxyFragment,
    policy: &SourcePolicy,
) -> Result<Vec<SecretInput>, TranslateError> {
    let mut inputs = Vec::new();
    let mut used_foreign_ids = BTreeSet::new();

    for listener in &fragment.postgres {
        let mut input = pg_dsn_from_listener(namespace, role_foreign_id, listener, policy)?;
        input.foreign_id = unique_foreign_id(input.foreign_id, &mut used_foreign_ids);
        inputs.push(SecretInput::PgDsn(input));
    }
    for transform in &fragment.transforms {
        match transform.name.as_str() {
            "secrets" => {
                for secret in &transform.config.secrets {
                    let mut input =
                        static_secret_from_secret(namespace, role_foreign_id, secret, policy)?;
                    input.foreign_id = unique_foreign_id(input.foreign_id, &mut used_foreign_ids);
                    inputs.push(SecretInput::Static(input));
                }
            }
            "oauth_token" => {
                for token in tokens_of(transform) {
                    let mut input =
                        oauth_token_from_value(namespace, role_foreign_id, token, policy)?;
                    input.foreign_id = unique_foreign_id(input.foreign_id, &mut used_foreign_ids);
                    inputs.push(SecretInput::OAuthToken(input));
                }
            }
            "gcp_auth" => {
                let mut input =
                    gcp_auth_from_transform(namespace, role_foreign_id, transform, policy)?;
                if let Some(foreign_id) = input.foreign_id.take() {
                    input.foreign_id = Some(unique_foreign_id(foreign_id, &mut used_foreign_ids));
                }
                inputs.push(SecretInput::GcpAuth(input));
            }
            "gcp_id_token" => {
                let mut input =
                    gcp_id_token_from_transform(namespace, role_foreign_id, transform, policy)?;
                input.foreign_id = unique_foreign_id(input.foreign_id, &mut used_foreign_ids);
                inputs.push(SecretInput::GcpIdToken(input));
            }
            "hmac_sign" => {
                // Representable in iron-control, but never reached: only infra/
                // harness fragments come through here and neither signs requests.
                // Tool hmac_sign secrets are registered via the centaur-perms CLI.
                return Err(TranslateError::Unsupported {
                    what: "hmac_sign request signing in an infra/harness fragment".to_owned(),
                });
            }
            "aws_auth" => {
                let mut input =
                    aws_auth_from_transform(namespace, role_foreign_id, transform, policy)?;
                input.foreign_id = unique_foreign_id(input.foreign_id, &mut used_foreign_ids);
                inputs.push(SecretInput::AwsAuth(input));
            }
            // Base-config transforms (allowlist, header_allowlist) and any
            // future unmanaged entries carry no secrets to register.
            _ => {}
        }
    }
    Ok(inputs)
}

// ---------------------------------------------------------------------------
// Static secrets (the only transform any fragment uses today)
// ---------------------------------------------------------------------------

fn static_secret_from_secret(
    namespace: &str,
    role: &str,
    secret: &Secret,
    policy: &SourcePolicy,
) -> Result<StaticSecretInput, TranslateError> {
    let source = source_from_secret(namespace, role, secret, policy)?;
    let (inject_config, replace_config) = match (&secret.inject, &secret.replace) {
        (Some(inject), None) => (Some(inject_config_from_value(role, inject)?), None),
        (None, Some(replace)) => (None, Some(replace_config_from(role, replace)?)),
        (Some(_), Some(_)) => {
            return Err(malformed(role, "secret declares both inject and replace"));
        }
        (None, None) => {
            return Err(malformed(
                role,
                "secret declares neither inject nor replace",
            ));
        }
    };
    let rules = rules_from_values(role, &secret.rules)?;
    let identity = static_secret_identity(secret);
    Ok(StaticSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: format!("{role}-{}", slugify(&identity)),
        name: identity,
        description: None,
        labels: resource_labels(secret.extra.get("labels")),
        inject_config,
        replace_config,
        source,
        rules,
    })
}

/// The replace-mode placeholder, if any. ``Secret::proxy_value`` is crate-
/// private to iron-proxy, so we read the public fields directly.
fn replace_proxy_value(secret: &Secret) -> Option<&str> {
    secret
        .replace
        .as_ref()
        .and_then(|replace| replace.proxy_value.as_deref())
}

/// A stable, human-meaningful identity for a static secret, used for both the
/// foreign-id slug and the display name.
fn static_secret_identity(secret: &Secret) -> String {
    if let Some(id) = &secret.id {
        return id.to_owned();
    }
    if let Some(proxy_value) = replace_proxy_value(secret) {
        return proxy_value.to_owned();
    }
    if let Some(source) = &secret.source {
        if let Some(credential_id) = yaml_str(source, "credential_id") {
            return credential_id.to_owned();
        }
        if let Some(placeholder) = yaml_str(source, "placeholder") {
            return placeholder.to_owned();
        }
    }
    if let Some(inject) = &secret.inject {
        if let Some(header) = yaml_str(inject, "header") {
            return header.to_owned();
        }
        if let Some(query_param) = yaml_str(inject, "query_param") {
            return query_param.to_owned();
        }
    }
    "secret".to_owned()
}

/// Resolve a ``token_broker`` source to a [`SecretSource`], or ``Ok(None)`` if
/// ``value`` is not a token_broker source. ``what`` prefixes the error so the
/// caller's context (e.g. ``"pg_dsn "``) appears in malformed messages.
fn token_broker_source(
    namespace: &str,
    role: &str,
    value: &YamlValue,
    what: &str,
) -> Result<Option<SecretSource>, TranslateError> {
    if yaml_str(value, "type") != Some("token_broker") {
        return Ok(None);
    }
    let credential_id = yaml_str(value, "credential_id").ok_or_else(|| {
        malformed(
            role,
            &format!("{what}token_broker source missing credential_id"),
        )
    })?;
    Ok(Some(SecretSource::token_broker(credential_id, namespace)))
}

fn source_from_secret(
    namespace: &str,
    role: &str,
    secret: &Secret,
    policy: &SourcePolicy,
) -> Result<SecretSource, TranslateError> {
    if let Some(source) = &secret.source {
        if let Some(broker) = token_broker_source(namespace, role, source, "")? {
            return Ok(broker);
        }
        if let Some(placeholder) = yaml_str(source, "placeholder") {
            return Ok(source_from_placeholder(
                policy,
                placeholder,
                yaml_str(source, "json_key"),
            ));
        }
        return Err(malformed(
            role,
            "secret source must be a placeholder or token_broker reference",
        ));
    }
    if let Some(proxy_value) = replace_proxy_value(secret) {
        return Ok(source_from_placeholder(policy, proxy_value, None));
    }
    Err(malformed(
        role,
        "secret has no source and no replace.proxy_value to derive one from",
    ))
}

/// Resolve a placeholder into an iron-control source, honoring the deployment's
/// [`SourcePolicy`] (env vs 1Password), mirroring how the proxy renderer
/// resolves the same placeholder. Public so callers that build secret inputs
/// from a source other than a fragment (e.g. a tool's ``pyproject.toml``)
/// resolve ``secret_ref``s exactly as api-rs does.
pub fn source_from_placeholder(
    policy: &SourcePolicy,
    placeholder: &str,
    json_key: Option<&str>,
) -> SecretSource {
    match policy.kind {
        SourceKind::Env => {
            let mut config = json!({ "var": placeholder });
            insert_json_key(&mut config, json_key);
            SecretSource {
                source_type: "env".to_owned(),
                secret: None,
                config,
            }
        }
        SourceKind::OnePassword => onepassword_source("1password", policy, placeholder, json_key),
        SourceKind::OnePasswordConnect => {
            onepassword_source("1password_connect", policy, placeholder, json_key)
        }
    }
}

fn onepassword_source(
    source_type: &str,
    policy: &SourcePolicy,
    placeholder: &str,
    json_key: Option<&str>,
) -> SecretSource {
    let mut config = json!({
        "secret_ref": format!("op://{}/{placeholder}/credential", policy.op_vault),
        "ttl": policy.ttl,
    });
    insert_json_key(&mut config, json_key);
    SecretSource {
        source_type: source_type.to_owned(),
        secret: None,
        config,
    }
}

fn insert_json_key(config: &mut JsonValue, json_key: Option<&str>) {
    if let (Some(json_key), Some(map)) = (json_key, config.as_object_mut()) {
        map.insert("json_key".to_owned(), json!(json_key));
    }
}

fn inject_config_from_value(
    role: &str,
    inject: &YamlValue,
) -> Result<InjectConfig, TranslateError> {
    let header = yaml_str(inject, "header").map(ToOwned::to_owned);
    let query_param = yaml_str(inject, "query_param").map(ToOwned::to_owned);
    if header.is_none() && query_param.is_none() {
        return Err(malformed(
            role,
            "inject secret must set header or query_param",
        ));
    }
    Ok(InjectConfig {
        header,
        query_param,
        formatter: yaml_str(inject, "formatter").map(ToOwned::to_owned),
    })
}

fn replace_config_from(
    role: &str,
    replace: &SecretReplace,
) -> Result<ReplaceConfig, TranslateError> {
    let proxy_value = replace
        .proxy_value
        .clone()
        .ok_or_else(|| malformed(role, "replace secret missing proxy_value"))?;
    Ok(ReplaceConfig {
        proxy_value,
        match_headers: yaml_string_array(replace.extra.get("match_headers")),
        match_body: yaml_bool(replace.extra.get("match_body")),
        match_path: yaml_bool(replace.extra.get("match_path")),
        match_query: yaml_bool(replace.extra.get("match_query")),
        require: yaml_bool(replace.extra.get("require")),
    })
}

fn rules_from_values(role: &str, rules: &[YamlValue]) -> Result<Vec<RequestRule>, TranslateError> {
    rules
        .iter()
        .map(|rule| {
            let host = yaml_str(rule, "host").map(ToOwned::to_owned);
            let cidr = yaml_str(rule, "cidr").map(ToOwned::to_owned);
            if host.is_none() && cidr.is_none() {
                return Err(malformed(role, "request rule must set host or cidr"));
            }
            Ok(RequestRule {
                host,
                cidr,
                http_methods: yaml_string_array(yaml_get(rule, "http_methods")),
                paths: yaml_string_array(yaml_get(rule, "paths")),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Postgres DSN secrets
// ---------------------------------------------------------------------------

fn pg_dsn_from_listener(
    namespace: &str,
    role: &str,
    listener: &PostgresListener,
    policy: &SourcePolicy,
) -> Result<PgDsnSecretInput, TranslateError> {
    let name = listener
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| malformed(role, "postgres listener missing name"))?;
    let dsn_value = listener
        .upstream
        .as_ref()
        .and_then(|upstream| upstream.dsn.as_ref())
        .ok_or_else(|| {
            malformed(
                role,
                &format!("postgres listener {name} missing upstream.dsn"),
            )
        })?;
    let dsn = pg_dsn_source(namespace, role, dsn_value, policy)?;
    let database = listener
        .sandbox_env
        .as_ref()
        .and_then(|sandbox_env| sandbox_env.database.as_deref())
        .map(str::trim)
        .filter(|database| !database.is_empty())
        .ok_or_else(|| {
            malformed(
                role,
                &format!("postgres listener {name} missing sandbox_env.database"),
            )
        })?
        .to_owned();
    let role_to_set = listener
        .extra
        .get("role")
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(ToOwned::to_owned);
    Ok(PgDsnSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: pg_foreign_id(name),
        name: name.to_owned(),
        database,
        description: None,
        role: role_to_set,
        labels: resource_labels(listener.extra.get("labels")),
        settings: listener
            .settings
            .iter()
            .map(pg_setting_from_listener)
            .collect(),
        dsn,
    })
}

fn pg_setting_from_listener(setting: &PgDsnSetting) -> PgDsnSettingInput {
    PgDsnSettingInput {
        name: setting.name.clone(),
        value: setting.value.clone(),
        value_from: setting.value_from.as_ref().map(
            |PgDsnSettingValueFrom {
                 principal_label,
                 principal_field,
             }| {
                PgDsnSettingValueFromInput {
                    principal_label: principal_label.clone(),
                    principal_field: principal_field.clone(),
                }
            },
        ),
    }
}

/// Resolve a listener's ``upstream.dsn`` into an iron-control secret source.
/// Accepts a ``token_broker`` reference, an explicit ``env`` source, or a
/// placeholder (resolved against the deployment's [`SourcePolicy`], like any
/// other secret).
fn pg_dsn_source(
    namespace: &str,
    role: &str,
    dsn: &YamlValue,
    policy: &SourcePolicy,
) -> Result<SecretSource, TranslateError> {
    if let Some(broker) = token_broker_source(namespace, role, dsn, "pg_dsn ")? {
        return Ok(broker);
    }
    if yaml_str(dsn, "type") == Some("env") {
        let var =
            yaml_str(dsn, "var").ok_or_else(|| malformed(role, "pg_dsn env source missing var"))?;
        return Ok(SecretSource::env(var));
    }
    if let Some(placeholder) = yaml_str(dsn, "placeholder").or_else(|| dsn.as_str()) {
        return Ok(source_from_placeholder(
            policy,
            placeholder,
            yaml_str(dsn, "json_key"),
        ));
    }
    Err(malformed(
        role,
        "pg_dsn upstream.dsn must be a placeholder or a typed (env/token_broker) source",
    ))
}

// ---------------------------------------------------------------------------
// OAuth token secrets
// ---------------------------------------------------------------------------

/// Keys on an ``oauth_token`` entry that are not credential fields.
const OAUTH_RESERVED_KEYS: &[&str] = &[
    "grant",
    "token_endpoint",
    "token_endpoint_headers",
    "rules",
    "scopes",
    "audience",
    "header",
    "labels",
    "value_prefix",
];

fn tokens_of(transform: &centaur_iron_proxy::Transform) -> Vec<&YamlValue> {
    transform
        .config
        .extra
        .get("tokens")
        .and_then(YamlValue::as_sequence)
        .map(|tokens| tokens.iter().collect())
        .unwrap_or_default()
}

fn oauth_token_from_value(
    namespace: &str,
    role: &str,
    token: &YamlValue,
    policy: &SourcePolicy,
) -> Result<OAuthTokenSecretInput, TranslateError> {
    let grant = yaml_str(token, "grant")
        .ok_or_else(|| malformed(role, "oauth_token entry missing grant"))?
        .to_owned();
    let mapping = token
        .as_mapping()
        .ok_or_else(|| malformed(role, "oauth_token entry must be a mapping"))?;

    let mut credentials = BTreeMap::new();
    for (key, value) in mapping {
        let Some(field) = key.as_str() else { continue };
        if OAUTH_RESERVED_KEYS.contains(&field) {
            continue;
        }
        credentials.insert(
            field.to_owned(),
            oauth_field_source(role, field, value, policy)?,
        );
    }
    if credentials.is_empty() {
        return Err(malformed(
            role,
            "oauth_token entry has no credential fields",
        ));
    }

    let mut token_endpoint_headers = BTreeMap::new();
    if let Some(headers) = yaml_get(token, "token_endpoint_headers").and_then(YamlValue::as_mapping)
    {
        for (key, value) in headers {
            if let Some(name) = key.as_str() {
                token_endpoint_headers.insert(
                    name.to_owned(),
                    oauth_field_source(role, name, value, policy)?,
                );
            }
        }
    }

    let rules = rules_from_values(role, &sequence(yaml_get(token, "rules")))?;
    let identity = yaml_str(token, "token_endpoint").unwrap_or(&grant);
    Ok(OAuthTokenSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: format!("{role}-oauth-{}", slugify(identity)),
        name: format!("OAuth {grant}"),
        grant,
        labels: resource_labels(yaml_get(token, "labels")),
        token_endpoint: yaml_str(token, "token_endpoint").map(ToOwned::to_owned),
        scopes: yaml_string_array(yaml_get(token, "scopes")),
        audience: yaml_str(token, "audience").map(ToOwned::to_owned),
        credentials,
        token_endpoint_headers,
        rules,
    })
}

fn oauth_field_source(
    role: &str,
    field: &str,
    value: &YamlValue,
    policy: &SourcePolicy,
) -> Result<SecretSource, TranslateError> {
    let placeholder = yaml_str(value, "placeholder")
        .or_else(|| value.as_str())
        .ok_or_else(|| malformed(role, &format!("oauth field {field} must be a placeholder")))?;
    Ok(source_from_placeholder(
        policy,
        placeholder,
        yaml_str(value, "json_key"),
    ))
}

// ---------------------------------------------------------------------------
// GCP auth secrets
// ---------------------------------------------------------------------------

fn gcp_auth_from_transform(
    namespace: &str,
    role: &str,
    transform: &centaur_iron_proxy::Transform,
    policy: &SourcePolicy,
) -> Result<GcpAuthSecretInput, TranslateError> {
    let config = &transform.config.extra;
    let scopes = gcp_auth_scopes_or_default(yaml_string_array(config.get("scopes")));
    let rules = rules_from_values(role, &sequence(config.get("rules")))?;

    let (keyfile, foreign_id) = match config.get("keyfile") {
        Some(keyfile) => {
            let placeholder = yaml_str(keyfile, "placeholder")
                .ok_or_else(|| malformed(role, "gcp_auth keyfile must be a placeholder"))?;
            (
                Some(source_from_placeholder(policy, placeholder, None)),
                Some(format!("{role}-gcp-{}", slugify(placeholder))),
            )
        }
        None => (None, None),
    };
    Ok(GcpAuthSecretInput {
        namespace: namespace.to_owned(),
        foreign_id,
        name: Some(format!("GCP Auth ({role})")),
        labels: resource_labels(config.get("labels")),
        scopes,
        subject: config
            .get("subject")
            .and_then(YamlValue::as_str)
            .map(ToOwned::to_owned),
        keyfile,
        credentials_provider: None,
        rules,
    })
}

// ---------------------------------------------------------------------------
// GCP ID token secrets
// ---------------------------------------------------------------------------

fn gcp_id_token_from_transform(
    namespace: &str,
    role: &str,
    transform: &centaur_iron_proxy::Transform,
    policy: &SourcePolicy,
) -> Result<GcpIdTokenSecretInput, TranslateError> {
    let config = &transform.config.extra;
    let audience = config
        .get("audience")
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| malformed(role, "gcp_id_token missing audience"))?;
    let keyfile_value = config
        .get("keyfile")
        .ok_or_else(|| malformed(role, "gcp_id_token missing keyfile"))?;
    let placeholder = yaml_str(keyfile_value, "placeholder")
        .or_else(|| keyfile_value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| malformed(role, "gcp_id_token keyfile must be a placeholder"))?;
    let rules = rules_from_values(role, &sequence(config.get("rules")))?;
    if rules.is_empty() {
        return Err(malformed(
            role,
            "gcp_id_token must declare at least one rule",
        ));
    }
    let header = config
        .get("header")
        .and_then(YamlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut identity = format!("{placeholder}-{audience}");
    if let Some(header) = &header {
        identity.push('-');
        identity.push_str(header);
    }
    Ok(GcpIdTokenSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: format!("{role}-gcp-id-token-{}", slugify(&identity)),
        name: Some(format!("GCP ID Token ({role})")),
        description: None,
        labels: resource_labels(config.get("labels")),
        audience: audience.to_owned(),
        header,
        keyfile: source_from_placeholder(policy, placeholder, None),
        rules,
    })
}

// ---------------------------------------------------------------------------
// AWS auth secrets
// ---------------------------------------------------------------------------

/// Translate an ``aws_auth`` transform into an [`AwsAuthSecretInput`]. The
/// credential refs (``access_key_id``/``secret_access_key`` and the optional
/// ``session_token``) are placeholders resolved through the deployment
/// [`SourcePolicy`], like the ``gcp_auth`` keyfile; ``allowed_regions`` /
/// ``allowed_services`` scope which the proxy signs for, and ``rules`` mirror the
/// shared request-rule shape. The ``foreign_id`` keys on the access-key
/// placeholder so the same credential set is one stable secret.
fn aws_auth_from_transform(
    namespace: &str,
    role: &str,
    transform: &centaur_iron_proxy::Transform,
    policy: &SourcePolicy,
) -> Result<AwsAuthSecretInput, TranslateError> {
    let config = &transform.config.extra;
    let access_key_id_value = config
        .get("access_key_id")
        .ok_or_else(|| malformed(role, "aws_auth missing access_key_id"))?;
    let (access_key_id, placeholder) =
        aws_source(role, "access_key_id", access_key_id_value, policy)?;
    let secret_access_key_value = config
        .get("secret_access_key")
        .ok_or_else(|| malformed(role, "aws_auth missing secret_access_key"))?;
    let (secret_access_key, _) =
        aws_source(role, "secret_access_key", secret_access_key_value, policy)?;
    let session_token = config
        .get("session_token")
        .map(|value| aws_source(role, "session_token", value, policy).map(|(source, _)| source))
        .transpose()?;
    Ok(AwsAuthSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: format!("{role}-aws-{}", slugify(&placeholder)),
        name: Some(format!("AWS Auth ({role})")),
        description: None,
        labels: resource_labels(config.get("labels")),
        access_key_id,
        secret_access_key,
        session_token,
        allowed_regions: yaml_string_array(config.get("allowed_regions")),
        allowed_services: yaml_string_array(config.get("allowed_services")),
        rules: rules_from_values(role, &sequence(config.get("rules")))?,
    })
}

/// Resolve one ``aws_auth`` credential ref — a ``{placeholder: NAME}`` mapping or
/// a bare ``NAME`` string — into its [`SecretSource`], returning the placeholder
/// too so the caller can derive the secret's ``foreign_id``.
fn aws_source(
    role: &str,
    field: &str,
    value: &YamlValue,
    policy: &SourcePolicy,
) -> Result<(SecretSource, String), TranslateError> {
    let placeholder = yaml_str(value, "placeholder")
        .or_else(|| value.as_str())
        .ok_or_else(|| malformed(role, &format!("aws_auth {field} must be a placeholder")))?;
    Ok((
        source_from_placeholder(policy, placeholder, None),
        placeholder.to_owned(),
    ))
}

// ---------------------------------------------------------------------------
// serde_yaml helpers and slugging
// ---------------------------------------------------------------------------

fn yaml_get<'a>(value: &'a YamlValue, key: &str) -> Option<&'a YamlValue> {
    value
        .as_mapping()?
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}

fn yaml_str<'a>(value: &'a YamlValue, key: &str) -> Option<&'a str> {
    yaml_get(value, key).and_then(YamlValue::as_str)
}

fn yaml_string_array(value: Option<&YamlValue>) -> Vec<String> {
    value
        .and_then(YamlValue::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(YamlValue::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn yaml_bool(value: Option<&YamlValue>) -> bool {
    value.and_then(YamlValue::as_bool).unwrap_or(false)
}

fn resource_labels(value: Option<&YamlValue>) -> BTreeMap<String, String> {
    let mut labels = managed_labels();
    if let Some(mapping) = value.and_then(YamlValue::as_mapping) {
        for (key, value) in mapping {
            let (Some(key), Some(value)) = (key.as_str(), value.as_str()) else {
                continue;
            };
            if !key.is_empty() {
                labels.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    labels
}

fn sequence(value: Option<&YamlValue>) -> Vec<YamlValue> {
    value
        .and_then(YamlValue::as_sequence)
        .cloned()
        .unwrap_or_default()
}

fn malformed(role: &str, detail: &str) -> TranslateError {
    TranslateError::Malformed {
        role: role.to_owned(),
        detail: detail.to_owned(),
    }
}

/// Deduplicate a foreign id against the set of ids already used in this batch,
/// suffixing `-2`, `-3`, … on collision. Shared with `centaur-perms`, which
/// translates tool-manifest secrets with the same id conventions.
pub fn unique_foreign_id(candidate: String, used: &mut BTreeSet<String>) -> String {
    if used.insert(candidate.clone()) {
        return candidate;
    }
    let mut counter = 2;
    loop {
        let next = format!("{candidate}-{counter}");
        if used.insert(next.clone()) {
            return next;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_iron_proxy::load_fragment_str;

    fn env_policy() -> SourcePolicy {
        SourcePolicy::env()
    }

    #[test]
    fn translates_replace_secret_with_derived_env_source() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: secrets
    config:
      secrets:
        - replace:
            proxy_value: XAI_API_KEY
            match_headers: ["Authorization"]
          labels:
            centaur-tool: xai
            centaur-tool-overlay: centaur
          rules: [{ host: api.x.ai }]
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "infra", &fragment, &env_policy()).unwrap();
        assert_eq!(inputs.len(), 1);
        let SecretInput::Static(input) = &inputs[0] else {
            panic!("expected a static secret");
        };
        assert_eq!(input.foreign_id, "infra-xai-api-key");
        assert_eq!(input.name, "XAI_API_KEY");
        let replace = input.replace_config.as_ref().unwrap();
        assert_eq!(replace.proxy_value, "XAI_API_KEY");
        assert_eq!(replace.match_headers, vec!["Authorization".to_owned()]);
        assert!(input.inject_config.is_none());
        assert_eq!(input.source.source_type, "env");
        assert_eq!(input.source.config, json!({ "var": "XAI_API_KEY" }));
        assert_eq!(
            input.labels.get("managed-by").map(String::as_str),
            Some("centaur")
        );
        assert_eq!(
            input.labels.get("centaur-tool").map(String::as_str),
            Some("xai")
        );
        assert_eq!(
            input.labels.get("centaur-tool-overlay").map(String::as_str),
            Some("centaur")
        );
        assert_eq!(input.rules.len(), 1);
        assert_eq!(input.rules[0].host.as_deref(), Some("api.x.ai"));
    }

    #[test]
    fn translates_token_broker_inject_secret() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: secrets
    config:
      secrets:
        - source:
            type: token_broker
            credential_id: openai-codex
          inject:
            header: Authorization
            formatter: "Bearer {{.Value}}"
          rules: [{ host: chatgpt.com }]
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "tool-codex", &fragment, &env_policy()).unwrap();
        let SecretInput::Static(input) = &inputs[0] else {
            panic!("expected a static secret");
        };
        assert_eq!(input.foreign_id, "tool-codex-openai-codex");
        assert_eq!(input.source.source_type, "token_broker");
        assert_eq!(
            input.source.config,
            json!({ "credential_id": "openai-codex", "credential_namespace": "default" })
        );
        let inject = input.inject_config.as_ref().unwrap();
        assert_eq!(inject.header.as_deref(), Some("Authorization"));
        assert_eq!(inject.formatter.as_deref(), Some("Bearer {{.Value}}"));
        assert!(input.replace_config.is_none());
    }

    #[test]
    fn placeholder_inject_secret_derives_source() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: secrets
    config:
      secrets:
        - source:
            placeholder: OPENAI_CODEX_ACCOUNT_ID
          inject:
            header: chatgpt-account-id
          rules: [{ host: chatgpt.com }]
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "tool-codex", &fragment, &env_policy()).unwrap();
        let SecretInput::Static(input) = &inputs[0] else {
            panic!("expected a static secret");
        };
        assert_eq!(input.source.source_type, "env");
        assert_eq!(
            input.source.config,
            json!({ "var": "OPENAI_CODEX_ACCOUNT_ID" })
        );
        // Identity comes from the placeholder (the actual secret), not the header.
        assert_eq!(input.foreign_id, "tool-codex-openai-codex-account-id");
        assert_eq!(input.name, "OPENAI_CODEX_ACCOUNT_ID");
    }

    #[test]
    fn onepassword_policy_builds_op_ref() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: secrets
    config:
      secrets:
        - replace:
            proxy_value: GITHUB_TOKEN
            match_headers: ["Authorization"]
          rules: [{ host: api.github.com }]
"#,
        )
        .unwrap();
        let policy = SourcePolicy::onepassword_connect("ai-agents", "10m");
        let inputs = secret_inputs_from_fragment("default", "infra", &fragment, &policy).unwrap();
        let SecretInput::Static(input) = &inputs[0] else {
            panic!("expected a static secret");
        };
        assert_eq!(input.source.source_type, "1password_connect");
        assert_eq!(
            input.source.config,
            json!({ "secret_ref": "op://ai-agents/GITHUB_TOKEN/credential", "ttl": "10m" })
        );
    }

    #[test]
    fn postgres_listener_translates_to_pg_dsn_secret() {
        let fragment = load_fragment_str(
            r#"
postgres:
  - name: analytics
    listen: "0.0.0.0:6432"
    upstream:
      dsn: { placeholder: PG_ANALYTICS_DSN }
    client:
      user: app
    sandbox_env:
      database: analytics_db
    role: readonly
    settings:
      - name: centaur.slack_channel_id
        value_from:
          principal_label: slack_channel_id
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "tools", &fragment, &env_policy()).unwrap();
        assert_eq!(inputs.len(), 1);
        let SecretInput::PgDsn(input) = &inputs[0] else {
            panic!("expected a pg_dsn secret");
        };
        assert_eq!(input.foreign_id, "pg-analytics");
        assert_eq!(input.name, "analytics");
        assert_eq!(input.database, "analytics_db");
        assert_eq!(input.role.as_deref(), Some("readonly"));
        assert_eq!(input.settings.len(), 1);
        assert_eq!(input.settings[0].name, "centaur.slack_channel_id");
        assert_eq!(
            input.settings[0]
                .value_from
                .as_ref()
                .and_then(|value_from| value_from.principal_label.as_deref()),
            Some("slack_channel_id")
        );
        assert_eq!(input.dsn.source_type, "env");
        assert_eq!(input.dsn.config, json!({ "var": "PG_ANALYTICS_DSN" }));
    }

    #[test]
    fn hmac_sign_is_unsupported() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: hmac_sign
    config:
      extra: {}
"#,
        )
        .unwrap();
        let err =
            secret_inputs_from_fragment("default", "tool-x", &fragment, &env_policy()).unwrap_err();
        assert!(matches!(err, TranslateError::Unsupported { .. }));
    }

    #[test]
    fn translates_aws_auth_transform() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: aws_auth
    config:
      access_key_id: { placeholder: AWS_ACCESS_KEY_ID }
      secret_access_key: { placeholder: AWS_SECRET_ACCESS_KEY }
      allowed_services: [logs, monitoring]
      rules:
        - { host: logs.us-east-1.amazonaws.com }
        - { host: monitoring.us-east-1.amazonaws.com }
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "infra", &fragment, &env_policy()).unwrap();
        let SecretInput::AwsAuth(input) = &inputs[0] else {
            panic!("expected an aws_auth secret");
        };
        assert_eq!(input.foreign_id, "infra-aws-aws-access-key-id");
        assert_eq!(input.name.as_deref(), Some("AWS Auth (infra)"));
        assert_eq!(input.access_key_id.source_type, "env");
        assert_eq!(
            input.access_key_id.config,
            json!({ "var": "AWS_ACCESS_KEY_ID" })
        );
        assert_eq!(
            input.secret_access_key.config,
            json!({ "var": "AWS_SECRET_ACCESS_KEY" })
        );
        assert!(input.session_token.is_none());
        assert_eq!(
            input.allowed_services,
            vec!["logs".to_owned(), "monitoring".to_owned()]
        );
        assert!(input.allowed_regions.is_empty());
        assert_eq!(input.rules.len(), 2);
        assert_eq!(
            input.rules[0].host.as_deref(),
            Some("logs.us-east-1.amazonaws.com")
        );
    }

    #[test]
    fn translates_gcp_id_token_transform() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: gcp_id_token
    config:
      keyfile: { placeholder: CLOUD_RUN_KEYFILE }
      audience: https://my-service-abc123-uc.a.run.app
      header: x-serverless-authorization
      labels:
        centaur-tool: cloud-run
      rules:
        - { host: my-service-abc123-uc.a.run.app, http_methods: [POST] }
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "infra", &fragment, &env_policy()).unwrap();
        let SecretInput::GcpIdToken(input) = &inputs[0] else {
            panic!("expected a gcp_id_token secret");
        };
        assert_eq!(
            input.foreign_id,
            "infra-gcp-id-token-cloud-run-keyfile-https-my-service-abc123-uc-a-run-app-x-serverless-authorization"
        );
        assert_eq!(input.name.as_deref(), Some("GCP ID Token (infra)"));
        assert_eq!(
            input.audience,
            "https://my-service-abc123-uc.a.run.app".to_owned()
        );
        assert_eq!(input.header.as_deref(), Some("x-serverless-authorization"));
        assert_eq!(input.keyfile.source_type, "env");
        assert_eq!(input.keyfile.config, json!({ "var": "CLOUD_RUN_KEYFILE" }));
        assert_eq!(
            input.labels.get("managed-by").map(String::as_str),
            Some("centaur")
        );
        assert_eq!(
            input.labels.get("centaur-tool").map(String::as_str),
            Some("cloud-run")
        );
        assert_eq!(input.rules.len(), 1);
        assert_eq!(
            input.rules[0].host.as_deref(),
            Some("my-service-abc123-uc.a.run.app")
        );
        assert_eq!(input.rules[0].http_methods, vec!["POST".to_owned()]);
    }

    #[test]
    fn gcp_id_token_requires_rules() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: gcp_id_token
    config:
      keyfile: { placeholder: CLOUD_RUN_KEYFILE }
      audience: https://my-service-abc123-uc.a.run.app
"#,
        )
        .unwrap();
        let err =
            secret_inputs_from_fragment("default", "infra", &fragment, &env_policy()).unwrap_err();
        assert!(matches!(err, TranslateError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn translates_aws_auth_with_session_token() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: aws_auth
    config:
      access_key_id: { placeholder: AWS_ACCESS_KEY_ID }
      secret_access_key: { placeholder: AWS_SECRET_ACCESS_KEY }
      session_token: { placeholder: AWS_SESSION_TOKEN }
      allowed_regions: [us-west-2]
      rules:
        - { host: logs.us-west-2.amazonaws.com }
"#,
        )
        .unwrap();
        let inputs =
            secret_inputs_from_fragment("default", "infra", &fragment, &env_policy()).unwrap();
        let SecretInput::AwsAuth(input) = &inputs[0] else {
            panic!("expected an aws_auth secret");
        };
        let session = input.session_token.as_ref().unwrap();
        assert_eq!(session.config, json!({ "var": "AWS_SESSION_TOKEN" }));
        assert_eq!(input.allowed_regions, vec!["us-west-2".to_owned()]);
    }

    #[test]
    fn aws_auth_missing_access_key_is_malformed() {
        let fragment = load_fragment_str(
            r#"
transforms:
  - name: aws_auth
    config:
      secret_access_key: { placeholder: AWS_SECRET_ACCESS_KEY }
      rules: [{ host: logs.us-east-1.amazonaws.com }]
"#,
        )
        .unwrap();
        let err =
            secret_inputs_from_fragment("default", "infra", &fragment, &env_policy()).unwrap_err();
        assert!(matches!(err, TranslateError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn duplicate_identities_get_unique_foreign_ids() {
        let mut used = BTreeSet::new();
        assert_eq!(
            unique_foreign_id("infra-x".to_owned(), &mut used),
            "infra-x"
        );
        assert_eq!(
            unique_foreign_id("infra-x".to_owned(), &mut used),
            "infra-x-2"
        );
        assert_eq!(
            unique_foreign_id("infra-x".to_owned(), &mut used),
            "infra-x-3"
        );
    }
}
