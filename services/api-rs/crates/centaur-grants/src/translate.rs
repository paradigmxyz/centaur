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
    GcpAuthSecretInput, InjectConfig, OAuthTokenSecretInput, ReplaceConfig, RequestRule,
    SecretInput, SecretSource, StaticSecretInput, slugify, source_from_placeholder,
};
use centaur_iron_proxy::SourcePolicy;

use crate::tools::{
    FieldSource, GcpAuthSecret, HttpSecret, OAuthTokenSecret, ParsedSecret, SecretMode,
    unique_foreign_id,
};

/// The result of translating a tool's secrets: the iron-control inputs to
/// upsert, plus any entries that have no iron-control representation here.
#[derive(Debug, Default)]
pub struct Translation {
    pub inputs: Vec<SecretInput>,
    /// `(name, type)` of secrets skipped because the type is unsupported.
    pub skipped: Vec<(String, String)>,
}

fn managed_labels() -> BTreeMap<String, String> {
    BTreeMap::from([("managed-by".to_owned(), "centaur".to_owned())])
}

fn rules_from_hosts(hosts: &[String]) -> Vec<RequestRule> {
    hosts.iter().map(RequestRule::host).collect()
}

/// Translate every secret declared by a tool into iron-control inputs to grant
/// to the tool's role (`role_foreign_id`, e.g. `tool-github`).
pub fn translate(
    namespace: &str,
    role_foreign_id: &str,
    secrets: &[ParsedSecret],
    policy: &SourcePolicy,
) -> Translation {
    let mut out = Translation::default();
    let mut used = BTreeSet::new();
    for secret in secrets {
        match secret {
            ParsedSecret::Http(http) => {
                out.inputs
                    .push(SecretInput::Static(static_input(namespace, role_foreign_id, http, policy, &mut used)));
            }
            ParsedSecret::OAuthToken(oauth) => {
                out.inputs.push(SecretInput::OAuthToken(oauth_input(
                    namespace,
                    role_foreign_id,
                    oauth,
                    policy,
                    &mut used,
                )));
            }
            ParsedSecret::GcpAuth(gcp) => {
                out.inputs
                    .push(SecretInput::GcpAuth(gcp_input(namespace, role_foreign_id, gcp, policy, &mut used)));
            }
            ParsedSecret::Unsupported { name, kind } => {
                out.skipped.push((name.clone(), kind.clone()));
            }
        }
    }
    out
}

fn static_input(
    namespace: &str,
    role: &str,
    http: &HttpSecret,
    policy: &SourcePolicy,
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
        labels: managed_labels(),
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
    used: &mut BTreeSet<String>,
) -> OAuthTokenSecretInput {
    let identity = oauth.token_endpoint.as_deref().unwrap_or(&oauth.name);
    OAuthTokenSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: unique_foreign_id(format!("{role}-oauth-{}", slugify(identity)), used),
        name: format!("OAuth {}", oauth.grant),
        grant: oauth.grant.clone(),
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
    used: &mut BTreeSet<String>,
) -> GcpAuthSecretInput {
    GcpAuthSecretInput {
        namespace: namespace.to_owned(),
        foreign_id: Some(unique_foreign_id(format!("{role}-gcp-{}", slugify(&gcp.name)), used)),
        name: Some(format!("GCP Auth ({role})")),
        scopes: gcp.scopes.clone(),
        subject: None,
        keyfile: Some(source_from_placeholder(policy, &gcp.secret_ref, None)),
        credentials_provider: None,
        rules: rules_from_hosts(&gcp.hosts),
    }
}

fn field_sources(fields: &[(String, FieldSource)], policy: &SourcePolicy) -> BTreeMap<String, SecretSource> {
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
