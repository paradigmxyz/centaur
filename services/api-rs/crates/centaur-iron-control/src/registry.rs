//! Translate iron-proxy [`ProxyFragment`]s into iron-control resources and
//! register them.
//!
//! Today the proxy config is rendered from fragments and baked into a
//! per-sandbox ConfigMap. Under iron-control the same fragments become durable
//! control-plane state: each fragment's secrets are upserted as typed secret
//! resources and granted to a role. The fragment's *origin* decides the role —
//! the infra fragment grants to the single infra role, and each tool/harness
//! fragment grants to its own per-tool role.
//!
//! [`secret_inputs_from_fragment`] is the pure translation (fragment → secret
//! inputs) and is unit-tested without a server; [`register_role`] drives the
//! client to upsert the role, upsert each secret, and grant it to the role.

use std::collections::{BTreeMap, BTreeSet};

use centaur_iron_proxy::{ProxyFragment, Secret, SecretReplace, SourceKind, SourcePolicy};
use serde_json::{Value as JsonValue, json};
use serde_yaml::Value as YamlValue;

use crate::client::IronControlClient;
use crate::error::IronControlError;
use crate::models::{
    GcpAuthSecretInput, GrantSecret, Grantee, IdentityInput, InjectConfig, OAuthTokenSecretInput,
    ReplaceConfig, RequestRule, SecretSource, StaticSecretInput,
};
use crate::util::slugify;

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

    /// A per-tool (or per-harness) role keyed by tool name.
    pub fn tool(name: &str) -> Self {
        Self {
            foreign_id: format!("tool-{}", slugify(name)),
            name: format!("Tool: {name}"),
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

const MANAGED_LABEL_KEY: &str = "managed-by";
const MANAGED_LABEL_VALUE: &str = "centaur";

fn managed_labels() -> BTreeMap<String, String> {
    BTreeMap::from([(MANAGED_LABEL_KEY.to_owned(), MANAGED_LABEL_VALUE.to_owned())])
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
        };
        client
            .create_grant(&Grantee::Role(role_record.id.clone()), &secret)
            .await?;
    }
    Ok(role_record.id)
}

/// Pure translation: a fragment's transforms → the secret resources to upsert.
///
/// Only the transform shapes Centaur uses are translated: the ``secrets``
/// transform (replace and inject, including ``token_broker`` sources),
/// ``oauth_token``, and ``gcp_auth``. ``hmac_sign`` and Postgres listeners
/// have no iron-control representation and error out (no tool uses them).
pub fn secret_inputs_from_fragment(
    namespace: &str,
    role_foreign_id: &str,
    fragment: &ProxyFragment,
    policy: &SourcePolicy,
) -> Result<Vec<SecretInput>, TranslateError> {
    if !fragment.postgres.is_empty() {
        return Err(TranslateError::Unsupported {
            what: "pg_dsn / postgres listeners".to_owned(),
        });
    }

    let mut inputs = Vec::new();
    let mut used_foreign_ids = BTreeSet::new();
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
            "hmac_sign" => {
                return Err(TranslateError::Unsupported {
                    what: "hmac_sign request signing".to_owned(),
                });
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
    let source = source_from_secret(role, secret, policy)?;
    let (inject_config, replace_config) = match (&secret.inject, &secret.replace) {
        (Some(inject), None) => (Some(inject_config_from_value(role, inject)?), None),
        (None, Some(replace)) => (None, Some(replace_config_from(role, replace)?)),
        (Some(_), Some(_)) => {
            return Err(malformed(role, "secret declares both inject and replace"));
        }
        (None, None) => {
            return Err(malformed(role, "secret declares neither inject nor replace"));
        }
    };
    let rules = rules_from_values(role, &secret.rules)?;
    let identity = static_secret_identity(secret);
    Ok(StaticSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: format!("{role}-{}", slugify(&identity)),
        name: identity,
        description: None,
        labels: managed_labels(),
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

fn source_from_secret(
    role: &str,
    secret: &Secret,
    policy: &SourcePolicy,
) -> Result<SecretSource, TranslateError> {
    if let Some(source) = &secret.source {
        if yaml_str(source, "type") == Some("token_broker") {
            let credential_id = yaml_str(source, "credential_id")
                .ok_or_else(|| malformed(role, "token_broker source missing credential_id"))?;
            return Ok(SecretSource::token_broker(credential_id));
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

/// Resolve a fragment placeholder into an iron-control source, honoring the
/// deployment's [`SourcePolicy`] (env vs 1Password), mirroring how the proxy
/// renderer resolves the same placeholder.
fn source_from_placeholder(
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

fn inject_config_from_value(role: &str, inject: &YamlValue) -> Result<InjectConfig, TranslateError> {
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

fn replace_config_from(role: &str, replace: &SecretReplace) -> Result<ReplaceConfig, TranslateError> {
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
        credentials.insert(field.to_owned(), oauth_field_source(role, field, value, policy)?);
    }
    if credentials.is_empty() {
        return Err(malformed(role, "oauth_token entry has no credential fields"));
    }

    let mut token_endpoint_headers = BTreeMap::new();
    if let Some(headers) = yaml_get(token, "token_endpoint_headers").and_then(YamlValue::as_mapping)
    {
        for (key, value) in headers {
            if let Some(name) = key.as_str() {
                token_endpoint_headers
                    .insert(name.to_owned(), oauth_field_source(role, name, value, policy)?);
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
    let scopes = yaml_string_array(config.get("scopes"));
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

fn unique_foreign_id(candidate: String, used: &mut BTreeSet<String>) -> String {
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
          rules: [{ host: api.x.ai }]
"#,
        )
        .unwrap();
        let inputs = secret_inputs_from_fragment("default", "infra", &fragment, &env_policy())
            .unwrap();
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
        assert_eq!(input.source.config, json!({ "credential_id": "openai-codex" }));
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
        assert_eq!(input.source.config, json!({ "var": "OPENAI_CODEX_ACCOUNT_ID" }));
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
        let inputs =
            secret_inputs_from_fragment("default", "infra", &fragment, &policy).unwrap();
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
    fn postgres_listeners_are_unsupported() {
        let fragment = load_fragment_str(
            r#"
postgres:
  - name: core
    listen: "0.0.0.0:5432"
"#,
        )
        .unwrap();
        let err = secret_inputs_from_fragment("default", "infra", &fragment, &env_policy())
            .unwrap_err();
        assert!(matches!(err, TranslateError::Unsupported { .. }));
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
        let err = secret_inputs_from_fragment("default", "tool-x", &fragment, &env_policy())
            .unwrap_err();
        assert!(matches!(err, TranslateError::Unsupported { .. }));
    }

    #[test]
    fn duplicate_identities_get_unique_foreign_ids() {
        let mut used = BTreeSet::new();
        assert_eq!(unique_foreign_id("infra-x".to_owned(), &mut used), "infra-x");
        assert_eq!(unique_foreign_id("infra-x".to_owned(), &mut used), "infra-x-2");
        assert_eq!(unique_foreign_id("infra-x".to_owned(), &mut used), "infra-x-3");
    }
}
