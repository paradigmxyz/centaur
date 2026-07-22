//! Translate parsed `pyproject.toml` secrets into iron-control [`SecretInput`]s.
//!
//! This mirrors `centaur_iron_control::registry`'s fragment translator, but
//! sources from a tool's `[tool.centaur]` block instead of an iron-proxy
//! fragment. The foreign-id conventions (`{role}-{slug}`, `{role}-oauth-{slug}`,
//! `{role}-gcp-{slug}`), the `managed-by: centaur` label, and the
//! [`source_from_placeholder`] policy resolution are kept identical so the
//! resources this CLI writes match what api-rs would register.

use std::collections::{BTreeMap, BTreeSet};

use centaur_iron_control::{
    AwsAuthSecretInput, GcpAuthSecretInput, GcpIdTokenSecretInput, HmacSecretHeader,
    HmacSecretInput, InjectConfig, OAuthTokenSecretInput, PgDsnSecretInput, PgDsnSettingInput,
    PgDsnSettingValueFromInput, ReplaceConfig, RequestRule, SecretInput, SecretSource,
    StaticSecretInput, gcp_auth_scopes_or_default, managed_labels, slugify,
    source_from_placeholder, unique_foreign_id,
};
use centaur_iron_proxy::SourcePolicy;
use centaur_iron_proxy::{PgDsnSetting, PgDsnSettingValueFrom};

use crate::tools::{
    AwsAuthSecret, BrokerTokenSecret, FieldSource, GcpAuthSecret, GcpIdTokenSecret, HmacSignSecret,
    HttpSecret, OAuthTokenSecret, ParsedSecret, PgDsnSecret, SecretMode,
};

/// The result of translating a tool's secrets: the iron-control inputs to upsert.
#[derive(Debug, Default)]
pub struct Translation {
    pub inputs: Vec<SecretInput>,
}

#[derive(Debug, Clone)]
pub struct ToolLabels {
    pub tool: String,
    pub overlay: String,
}

fn rules_from_hosts(hosts: &[String]) -> Vec<RequestRule> {
    hosts.iter().map(RequestRule::host).collect()
}

/// Translate every secret declared by a tool into iron-control inputs to grant
/// to the tool's role (`role_foreign_id`, e.g. `tool-github`).
#[cfg(test)]
pub fn translate(
    namespace: &str,
    role_foreign_id: &str,
    secrets: &[ParsedSecret],
    policy: &SourcePolicy,
) -> Translation {
    translate_with_labels(
        namespace,
        role_foreign_id,
        secrets,
        policy,
        &managed_labels(),
    )
}

pub fn translate_for_tool(
    namespace: &str,
    role_foreign_id: &str,
    labels: &ToolLabels,
    secrets: &[ParsedSecret],
    policy: &SourcePolicy,
) -> Translation {
    translate_with_labels(
        namespace,
        role_foreign_id,
        secrets,
        policy,
        &tool_labels(labels),
    )
}

fn translate_with_labels(
    namespace: &str,
    role_foreign_id: &str,
    secrets: &[ParsedSecret],
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
) -> Translation {
    let mut out = Translation::default();
    let mut used = BTreeSet::new();
    for secret in secrets {
        match secret {
            ParsedSecret::Http(http) => {
                out.inputs.push(SecretInput::Static(static_input(
                    namespace,
                    role_foreign_id,
                    http,
                    policy,
                    labels,
                    &mut used,
                )));
            }
            ParsedSecret::OAuthToken(oauth) => {
                out.inputs.push(SecretInput::OAuthToken(oauth_input(
                    namespace,
                    role_foreign_id,
                    oauth,
                    policy,
                    labels,
                    &mut used,
                )));
            }
            ParsedSecret::GcpAuth(gcp) => {
                out.inputs.push(SecretInput::GcpAuth(gcp_input(
                    namespace,
                    role_foreign_id,
                    gcp,
                    policy,
                    labels,
                    &mut used,
                )));
            }
            ParsedSecret::GcpIdToken(gcp) => {
                out.inputs.push(SecretInput::GcpIdToken(gcp_id_token_input(
                    namespace,
                    role_foreign_id,
                    gcp,
                    policy,
                    labels,
                    &mut used,
                )));
            }
            ParsedSecret::PgDsn(pg) => {
                out.inputs.push(SecretInput::PgDsn(pg_dsn_input(
                    namespace, pg, policy, labels,
                )));
            }
            ParsedSecret::Hmac(hmac) => {
                out.inputs.push(SecretInput::Hmac(hmac_input(
                    namespace,
                    role_foreign_id,
                    hmac,
                    policy,
                    labels,
                    &mut used,
                )));
            }
            ParsedSecret::BrokerToken(broker) => {
                out.inputs.push(SecretInput::Static(broker_token_input(
                    namespace,
                    role_foreign_id,
                    broker,
                    labels,
                    &mut used,
                )));
            }
            ParsedSecret::AwsAuth(aws) => {
                out.inputs.push(SecretInput::AwsAuth(aws_input(
                    namespace,
                    role_foreign_id,
                    aws,
                    policy,
                    labels,
                    &mut used,
                )));
            }
        }
    }
    out
}

fn tool_labels(tool: &ToolLabels) -> BTreeMap<String, String> {
    let mut labels = managed_labels();
    labels.insert("centaur-tool".to_owned(), tool.tool.clone());
    labels.insert("centaur-tool-overlay".to_owned(), tool.overlay.clone());
    labels
}

fn static_input(
    namespace: &str,
    role: &str,
    http: &HttpSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> StaticSecretInput {
    let (inject_config, replace_config) = match http.mode {
        SecretMode::Replace => (
            None,
            Some(ReplaceConfig {
                proxy_value: http.replacer.clone(),
                match_headers: http.match_headers.clone(),
                match_body: false,
                match_path: http.match_path,
                match_query: http.match_query,
                require: false,
            }),
        ),
        SecretMode::Inject => (
            Some(InjectConfig {
                header: http.inject_header.clone(),
                query_param: http.inject_query_param.clone(),
                formatter: http.inject_formatter.clone(),
            }),
            None,
        ),
    };
    StaticSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-{}", slugify(&http.name)), used),
        name: http.name.clone(),
        description: None,
        labels: labels.clone(),
        inject_config,
        replace_config,
        source: source_from_placeholder(policy, &http.secret_ref, None),
        rules: rules_from_hosts(&http.hosts),
    }
}

fn oauth_input(
    namespace: &str,
    role: &str,
    oauth: &OAuthTokenSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> OAuthTokenSecretInput {
    let identity = oauth.token_endpoint.as_deref().unwrap_or(&oauth.name);
    OAuthTokenSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-oauth-{}", slugify(identity)), used),
        name: format!("OAuth {}", oauth.grant),
        grant: oauth.grant.clone(),
        labels: labels.clone(),
        token_endpoint: oauth.token_endpoint.clone(),
        scopes: oauth.scopes.clone(),
        audience: oauth.audience.clone(),
        credentials: field_sources(&oauth.fields, policy),
        token_endpoint_headers: field_sources(&oauth.token_endpoint_headers, policy),
        rules: rules_from_hosts(&oauth.hosts),
    }
}

fn gcp_input(
    namespace: &str,
    role: &str,
    gcp: &GcpAuthSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> GcpAuthSecretInput {
    GcpAuthSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: Some(unique_foreign_id(
            format!("{role}-gcp-{}", slugify(&gcp.name)),
            used,
        )),
        name: Some(format!("GCP Auth ({role})")),
        labels: labels.clone(),
        scopes: gcp_auth_scopes_or_default(gcp.scopes.clone()),
        subject: None,
        keyfile: Some(source_from_placeholder(policy, &gcp.secret_ref, None)),
        credentials_provider: None,
        rules: rules_from_hosts(&gcp.hosts),
    }
}

fn gcp_id_token_input(
    namespace: &str,
    role: &str,
    gcp: &GcpIdTokenSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> GcpIdTokenSecretInput {
    let mut identity = format!("{}-{}", gcp.secret_ref, gcp.audience);
    if let Some(header) = &gcp.header {
        identity.push('-');
        identity.push_str(header);
    }
    GcpIdTokenSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-gcp-id-token-{}", slugify(&identity)), used),
        name: Some(format!("GCP ID Token ({role})")),
        description: None,
        labels: labels.clone(),
        audience: gcp.audience.clone(),
        header: gcp.header.clone(),
        keyfile: source_from_placeholder(policy, &gcp.secret_ref, None),
        rules: rules_from_hosts(&gcp.hosts),
    }
}

/// Translate a `pg_dsn` secret into a [`PgDsnSecretInput`].
///
/// Unlike the other secret types, the pg_dsn `foreign_id` is **not**
/// role-prefixed or deduped: api-rs re-derives the sandbox's DSN env var name
/// from the `foreign_id` (via `pg_sandbox_env_var`), so the id has to round-trip
/// back to the tool's declared `name`. `pg_sandbox_env_var` appends `_DSN`, so a
/// trailing `_dsn`/`-dsn` is stripped before slugifying — e.g. `RESHIFT_DSN`
/// becomes `reshift`, which `pg_sandbox_env_var` turns back into `RESHIFT_DSN`.
fn pg_dsn_input(
    namespace: &str,
    pg: &PgDsnSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
) -> PgDsnSecretInput {
    PgDsnSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: pg_dsn_foreign_id(&pg.name),
        name: pg.name.clone(),
        database: pg.database.clone(),
        description: None,
        role: pg.role.clone(),
        labels: labels.clone(),
        settings: pg.settings.iter().map(pg_setting_input).collect(),
        dsn: source_from_placeholder(policy, &pg.secret_ref, None),
    }
}

fn pg_setting_input(setting: &PgDsnSetting) -> PgDsnSettingInput {
    PgDsnSettingInput {
        name: setting.name.clone(),
        value: setting.value.clone(),
        value_from: setting.value_from.as_ref().map(
            |PgDsnSettingValueFrom {
                 principal_label,
                 principal_field,
                 proxy_label,
             }| {
                PgDsnSettingValueFromInput {
                    principal_label: principal_label.clone(),
                    principal_field: principal_field.clone(),
                    proxy_label: proxy_label.clone(),
                }
            },
        ),
    }
}

/// The pg_dsn `foreign_id` for a secret `name`: drop a trailing `_dsn`/`-dsn`
/// (case-insensitive) so the `_DSN` suffix `pg_sandbox_env_var` re-appends isn't
/// doubled, then slugify.
fn pg_dsn_foreign_id(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let base = if lower.ends_with("_dsn") || lower.ends_with("-dsn") {
        &name[..name.len() - 4]
    } else {
        name
    };
    slugify(base)
}

/// Translate an `hmac_sign` secret into a [`HmacSecretInput`]. iron-control
/// delivers each granted HMAC secret to iron-proxy as its own `hmac_sign`
/// transform with its own rules (like a `gcp_auth` secret), so the `foreign_id`
/// is role-prefixed and deduped (`{role}-hmac-{slug}`).
fn hmac_input(
    namespace: &str,
    role: &str,
    hmac: &HmacSignSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> HmacSecretInput {
    HmacSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-hmac-{}", slugify(&hmac.name)), used),
        name: hmac.name.clone(),
        description: None,
        labels: labels.clone(),
        timestamp_format: hmac.timestamp_format.clone(),
        signature_algorithm: hmac.algorithm.clone(),
        signature_key_encoding: hmac.key_encoding.clone(),
        signature_output_encoding: hmac.output_encoding.clone(),
        signature_message: hmac.message.clone(),
        allow_chunked_body: hmac.allow_chunked_body,
        headers: hmac
            .headers
            .iter()
            .map(|h| HmacSecretHeader {
                name: h.name.clone(),
                value: h.value.clone(),
            })
            .collect(),
        credentials: field_sources(&hmac.credentials, policy),
        rules: rules_from_hosts(&hmac.hosts),
    }
}

/// Translate an `aws_auth` secret into an [`AwsAuthSecretInput`]. iron-control
/// delivers each granted AWS auth secret to iron-proxy as its own `aws_auth`
/// transform with its own rules (like a `gcp_auth`/`hmac_sign` secret), so the
/// `foreign_id` is role-prefixed and deduped (`{role}-aws-{slug}`). The
/// credential refs resolve through the deployment's [`SourcePolicy`] like every
/// other secret source.
fn aws_input(
    namespace: &str,
    role: &str,
    aws: &AwsAuthSecret,
    policy: &SourcePolicy,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> AwsAuthSecretInput {
    AwsAuthSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-aws-{}", slugify(&aws.name)), used),
        name: Some(format!("AWS Auth ({role})")),
        description: None,
        labels: labels.clone(),
        access_key_id: source_from_placeholder(policy, &aws.access_key_id_ref, None),
        secret_access_key: source_from_placeholder(policy, &aws.secret_access_key_ref, None),
        session_token: aws
            .session_token_ref
            .as_ref()
            .map(|r| source_from_placeholder(policy, r, None)),
        allowed_regions: aws.allowed_regions.clone(),
        allowed_services: aws.allowed_services.clone(),
        rules: rules_from_hosts(&aws.hosts),
    }
}

/// Translate a `brokered_token` secret into a [`StaticSecretInput`] whose source
/// is a `token_broker` reference to the named broker credential. iron-control
/// mints the access token from the broker credential and iron-proxy injects it
/// per `inject_config`. The broker credential is provisioned out of band (see
/// `centaur-perms broker create`); nothing here creates it. The `foreign_id` is
/// role-prefixed and deduped like the other secret types.
fn broker_token_input(
    namespace: &str,
    role: &str,
    broker: &BrokerTokenSecret,
    labels: &BTreeMap<String, String>,
    used: &mut BTreeSet<String>,
) -> StaticSecretInput {
    StaticSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-{}", slugify(&broker.name)), used),
        name: broker.name.clone(),
        description: None,
        labels: labels.clone(),
        inject_config: Some(InjectConfig {
            header: Some(broker.inject_header.clone()),
            query_param: None,
            formatter: Some(broker.inject_formatter.clone()),
        }),
        replace_config: None,
        source: SecretSource::token_broker(&broker.credential, namespace),
        rules: rules_from_hosts(&broker.hosts),
    }
}

fn field_sources(
    fields: &[(String, FieldSource)],
    policy: &SourcePolicy,
) -> BTreeMap<String, SecretSource> {
    fields
        .iter()
        .map(|(field, src)| {
            (
                field.clone(),
                source_from_placeholder(policy, &src.secret_ref, src.json_key.as_deref()),
            )
        })
        .collect()
}
