//! Typed JSON payloads for the secrets area.
//!
//! These structs are the fixed object rendered per record. Every
//! timestamp goes through `centaur_console_db::timestamp`, so the payloads do
//! not format one by hand.

use std::collections::BTreeMap;

use centaur_console_db::areas::secrets::{
    GrantRow, RoleSummary, RuleRow, SecretFields, SecretKind, SecretRow, SourceRow,
};
use centaur_console_db::model::{self, ConsoleModel};
use serde::Serialize;
use serde_json::Value;
use time::PrimitiveDateTime;

/// The secret detail page's role-grant response: the grants and the roles the
/// secret is not yet granted to.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RoleGrantsResponse {
    pub(crate) data: Vec<GrantPayload>,
    pub(crate) assignable_roles: Vec<RolePayload>,
}

/// A source's kind and config; never its value.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SourcePayload {
    source_type: String,
    config: Value,
}

impl From<&SourceRow> for SourcePayload {
    fn from(source: &SourceRow) -> Self {
        Self {
            source_type: source.source_type.clone(),
            config: source.config.clone(),
        }
    }
}

fn source(source: Option<&SourceRow>) -> Option<SourcePayload> {
    source.map(SourcePayload::from)
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RulePayload {
    host: Option<String>,
    cidr: Option<String>,
    position: i32,
    http_methods: Value,
    paths: Value,
}

impl From<&RuleRow> for RulePayload {
    fn from(rule: &RuleRow) -> Self {
        Self {
            host: rule.host.clone(),
            cidr: rule.cidr.clone(),
            position: rule.position,
            http_methods: rule.http_methods.clone(),
            paths: rule.paths.clone(),
        }
    }
}

pub(crate) fn rules(rules: &[RuleRow]) -> Vec<RulePayload> {
    rules.iter().map(RulePayload::from).collect()
}

/// `id`, `foreign_id`, `name`, `description`, `labels` — the base of every
/// bearer record. `#[serde(flatten)]` keeps each kind's struct flat instead of
/// nesting a second object the SPA never sees.
#[derive(Clone, Debug, Serialize)]
struct RecordBase {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    labels: Value,
}

impl From<&SecretRow> for RecordBase {
    fn from(row: &SecretRow) -> Self {
        Self {
            id: row.oid(),
            foreign_id: row.foreign_id.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            labels: row.labels.clone(),
        }
    }
}

macro_rules! record {
    (
        $(#[$doc:meta])*
        $name:ident { $($field:ident: $ty:ty),* $(,)? }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Serialize)]
        pub(crate) struct $name {
            #[serde(flatten)]
            base: RecordBase,
            $($field: $ty,)*
            #[serde(with = "centaur_console_db::timestamp::primitive")]
            created_at: PrimitiveDateTime,
            #[serde(with = "centaur_console_db::timestamp::primitive")]
            updated_at: PrimitiveDateTime,
        }
    };
}

record! {
    /// `POST/PUT .../static_secrets`
    StaticRecord {
        kind: String,
        inject_config: Value,
        replace_config: Value,
        source: Option<SourcePayload>,
        rules: Vec<RulePayload>,
    }
}

record! {
    /// `.../pg_dsn_secrets`
    PgDsnRecord {
        database: String,
        role: Option<String>,
        settings: Value,
        dsn: Option<SourcePayload>,
    }
}

record! {
    /// `.../gcp_auth_secrets`
    GcpAuthRecord {
        credentials_provider: Value,
        subject: Option<String>,
        scopes: Value,
        keyfile: Option<SourcePayload>,
        rules: Vec<RulePayload>,
    }
}

record! {
    /// `.../gcp_id_token_secrets`
    GcpIdTokenRecord {
        audience: String,
        header: Option<String>,
        keyfile: Option<SourcePayload>,
        rules: Vec<RulePayload>,
    }
}

record! {
    /// `.../aws_auth_secrets`
    AwsAuthRecord {
        allowed_regions: Value,
        allowed_services: Value,
        access_key_id: Option<SourcePayload>,
        secret_access_key: Option<SourcePayload>,
        session_token: Option<SourcePayload>,
        rules: Vec<RulePayload>,
    }
}

record! {
    /// `.../oauth_token_secrets`
    OauthTokenRecord {
        grant: String,
        token_endpoint: String,
        audience: Option<String>,
        scopes: Value,
        header: Option<String>,
        value_prefix: Option<String>,
        credentials: BTreeMap<String, SourcePayload>,
        token_endpoint_headers: BTreeMap<String, SourcePayload>,
        rules: Vec<RulePayload>,
    }
}

record! {
    /// `.../hmac_secrets`
    HmacRecord {
        timestamp_format: String,
        signature_algorithm: String,
        signature_key_encoding: String,
        signature_output_encoding: String,
        signature_message: String,
        allow_chunked_body: bool,
        headers: Value,
        credentials: BTreeMap<String, SourcePayload>,
        rules: Vec<RulePayload>,
    }
}

record! {
    /// `.../ssh_key_secrets`
    SshKeyRecord {
        public_key: String,
        personal: bool,
        private_key: Option<SourcePayload>,
    }
}

/// The row's sources keyed by role, optionally only those of one role kind.
fn sources_by_role(row: &SecretRow, role_kind: Option<&str>) -> BTreeMap<String, SourcePayload> {
    row.sources
        .iter()
        .filter(|source| role_kind.is_none() || source.role_kind.as_deref() == role_kind)
        .filter_map(|source| {
            source
                .role
                .as_ref()
                .map(|role| (role.clone(), SourcePayload::from(source)))
        })
        .collect()
}

impl From<&SecretRow> for StaticRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::Static {
            kind,
            inject_config,
            replace_config,
        } = &row.fields
        else {
            unreachable!("kind selects the fields");
        };
        StaticRecord {
            base: RecordBase::from(row),
            kind: kind.clone(),
            inject_config: inject_config.clone(),
            replace_config: replace_config.clone(),
            source: source(row.source()),
            rules: rules(&row.rules),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for PgDsnRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::PgDsn {
            database,
            role,
            settings,
        } = &row.fields
        else {
            unreachable!("kind selects the fields");
        };
        PgDsnRecord {
            base: RecordBase::from(row),
            database: database.clone(),
            role: role.clone(),
            settings: settings.clone(),
            dsn: source(row.source()),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for GcpAuthRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::GcpAuth {
            credentials_provider,
            subject,
            scopes,
        } = &row.fields
        else {
            unreachable!("kind selects the fields");
        };
        GcpAuthRecord {
            base: RecordBase::from(row),
            credentials_provider: credentials_provider.clone(),
            subject: subject.clone(),
            scopes: scopes.clone(),
            keyfile: source(row.source()),
            rules: rules(&row.rules),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for GcpIdTokenRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::GcpIdToken { audience, header } = &row.fields else {
            unreachable!("kind selects the fields");
        };
        GcpIdTokenRecord {
            base: RecordBase::from(row),
            audience: audience.clone(),
            header: header.clone(),
            keyfile: source(row.source()),
            rules: rules(&row.rules),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for AwsAuthRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::AwsAuth {
            allowed_regions,
            allowed_services,
        } = &row.fields
        else {
            unreachable!("kind selects the fields");
        };
        let by_role = sources_by_role(row, None);
        AwsAuthRecord {
            base: RecordBase::from(row),
            allowed_regions: allowed_regions.clone(),
            allowed_services: allowed_services.clone(),
            access_key_id: by_role.get("access_key_id").cloned(),
            secret_access_key: by_role.get("secret_access_key").cloned(),
            session_token: by_role.get("session_token").cloned(),
            rules: rules(&row.rules),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for OauthTokenRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::OauthToken {
            grant,
            token_endpoint,
            audience,
            scopes,
            header,
            value_prefix,
        } = &row.fields
        else {
            unreachable!("kind selects the fields");
        };
        OauthTokenRecord {
            base: RecordBase::from(row),
            grant: grant.clone(),
            token_endpoint: token_endpoint.clone(),
            audience: audience.clone(),
            scopes: scopes.clone(),
            header: header.clone(),
            value_prefix: value_prefix.clone(),
            credentials: sources_by_role(row, Some("credential_field")),
            token_endpoint_headers: sources_by_role(row, Some("endpoint_header")),
            rules: rules(&row.rules),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for HmacRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::Hmac {
            timestamp_format,
            signature_algorithm,
            signature_key_encoding,
            signature_output_encoding,
            signature_message,
            headers,
            allow_chunked_body,
        } = &row.fields
        else {
            unreachable!("kind selects the fields");
        };
        HmacRecord {
            base: RecordBase::from(row),
            timestamp_format: timestamp_format.clone(),
            signature_algorithm: signature_algorithm.clone(),
            signature_key_encoding: signature_key_encoding.clone(),
            signature_output_encoding: signature_output_encoding.clone(),
            signature_message: signature_message.clone(),
            allow_chunked_body: *allow_chunked_body,
            headers: headers.clone(),
            credentials: sources_by_role(row, None),
            rules: rules(&row.rules),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl From<&SecretRow> for SshKeyRecord {
    fn from(row: &SecretRow) -> Self {
        let SecretFields::SshKey { public_key } = &row.fields else {
            unreachable!("kind selects the fields");
        };
        SshKeyRecord {
            base: RecordBase::from(row),
            public_key: public_key.clone(),
            personal: row.owner_id.is_some(),
            private_key: source(row.source()),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Console list and detail
// ---------------------------------------------------------------------------

/// The grants summary plus the console list's view of a secret.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SecretSummary {
    kind: &'static str,
    kind_label: &'static str,
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
    grant_key: &'static str,
    description: Option<String>,
    labels: Value,
    source_types: Vec<String>,
    managed_by: Option<ManagedByPayload>,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ManagedByPayload {
    broker_credential_id: String,
    oauth_app_id: Option<String>,
    oauth_app_slug: Option<String>,
}

impl From<&centaur_console_db::areas::secrets::BrokerRow> for ManagedByPayload {
    fn from(broker: &centaur_console_db::areas::secrets::BrokerRow) -> Self {
        Self {
            broker_credential_id: model::BrokerCredential::oid(broker.id),
            oauth_app_id: broker.oauth_app_id.map(model::OauthApp::oid),
            oauth_app_slug: broker.oauth_app_slug.clone(),
        }
    }
}

pub(crate) fn summary(row: &SecretRow) -> SecretSummary {
    SecretSummary {
        kind: row.kind.slug(),
        kind_label: row.kind.label(),
        id: row.oid(),
        foreign_id: row.foreign_id.clone(),
        name: row.name.clone(),
        grant_key: row.kind.grant_column(),
        description: row.description.clone(),
        labels: row.labels.clone(),
        source_types: source_types(row),
        managed_by: row.broker.as_ref().map(ManagedByPayload::from),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// The rendered form of a secret's raw segments.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SourceSegmentPayload {
    role: Option<String>,
    #[serde(rename = "type")]
    source_type: String,
    type_label: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
}

/// One configuration row.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ConfigurationRow {
    label: &'static str,
    value: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SignedHeaderPayload {
    name: Value,
    value: Value,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SecretDetail {
    #[serde(flatten)]
    summary: SecretSummary,
    editable: bool,
    injection: Option<String>,
    source_segments: Vec<SourceSegmentPayload>,
    configuration: Vec<ConfigurationRow>,
    signed_headers: Option<Vec<SignedHeaderPayload>>,
    rules: Option<Vec<RulePayload>>,
}

pub(crate) fn detail(row: &SecretRow) -> SecretDetail {
    SecretDetail {
        summary: summary(row),
        editable: row.kind.editable(),
        injection: injection(row),
        source_segments: source_segments(row),
        configuration: configuration(row),
        signed_headers: signed_headers(row),
        rules: row
            .kind
            .rule_owner_column()
            .is_some()
            .then(|| rules(&row.rules)),
    }
}

/// The identifying metadata a grant payload embeds.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct GrantablePayload {
    kind: &'static str,
    kind_label: &'static str,
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RolePayload {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
}

impl From<&RoleSummary> for RolePayload {
    fn from(role: &RoleSummary) -> Self {
        Self {
            id: role.oid(),
            foreign_id: role.foreign_id.clone(),
            name: role.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GrantPayload {
    id: String,
    role_id: String,
    role: RolePayload,
    grantable: GrantablePayload,
    priority: i32,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
}

pub(crate) fn grant(grant: &GrantRow, secret: &SecretRow) -> GrantPayload {
    GrantPayload {
        id: model::Grant::oid(grant.id),
        role_id: grant.role.oid(),
        role: RolePayload::from(&grant.role),
        grantable: GrantablePayload {
            kind: secret.kind.slug(),
            kind_label: secret.kind.label(),
            id: secret.oid(),
            foreign_id: secret.foreign_id.clone(),
            name: secret.name.clone(),
        },
        priority: grant.priority,
        created_at: grant.created_at,
        updated_at: grant.updated_at,
    }
}

// ---------------------------------------------------------------------------
// Secret view
// ---------------------------------------------------------------------------

fn source_types(row: &SecretRow) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for segment in raw_segments(row) {
        let label = source_label(&segment.source_type).to_owned();
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

fn source_label(source_type: &str) -> &str {
    match source_type {
        "env" => "Env",
        "aws_sm" => "AWS-SM",
        "aws_ssm" => "AWS-SSM",
        "1password" => "1Password",
        "1password_connect" => "1Password-Connect",
        "control_plane" => "Inline",
        "token_broker" => "Token-Broker",
        "workload_identity" => "Workload-Identity",
        other => other,
    }
}

/// One raw-segment entry.
struct SourceSegment {
    role: Option<String>,
    source_type: String,
    reference: Option<String>,
}

/// One entry per source association, or the
/// synthetic workload-identity entry of a provider-only GCP auth secret.
fn raw_segments(row: &SecretRow) -> Vec<SourceSegment> {
    match row.kind {
        SecretKind::GcpAuth if row.sources.is_empty() => match &row.fields {
            SecretFields::GcpAuth {
                credentials_provider,
                ..
            } => credentials_provider
                .get("type")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| {
                    vec![SourceSegment {
                        role: None,
                        source_type: value.to_owned(),
                        reference: None,
                    }]
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        },
        SecretKind::Static
        | SecretKind::PgDsn
        | SecretKind::GcpAuth
        | SecretKind::GcpIdToken
        | SecretKind::SshKey => row
            .sources
            .first()
            .map(|source| vec![source_segment(source, None)])
            .unwrap_or_default(),
        SecretKind::AwsAuth | SecretKind::OauthToken | SecretKind::Hmac => {
            let mut sources: Vec<&SourceRow> = row.sources.iter().collect();
            sources.sort_by(|left, right| left.role.cmp(&right.role));
            sources
                .into_iter()
                .map(|source| source_segment(source, source.role.clone()))
                .collect()
        }
    }
}

fn source_segment(source: &SourceRow, role: Option<String>) -> SourceSegment {
    SourceSegment {
        role,
        source_type: source.source_type.clone(),
        reference: source_reference(source),
    }
}

fn source_segments(row: &SecretRow) -> Vec<SourceSegmentPayload> {
    raw_segments(row)
        .into_iter()
        .map(|segment| SourceSegmentPayload {
            role: segment.role,
            type_label: source_label(&segment.source_type).to_owned(),
            source_type: segment.source_type,
            reference: segment.reference,
        })
        .collect()
}

fn source_reference(source: &SourceRow) -> Option<String> {
    if source.source_type == "control_plane" {
        return Some("inline".to_owned());
    }
    let key = match source.source_type.as_str() {
        "env" => "var",
        "aws_sm" => "secret_id",
        "aws_ssm" => "name",
        "1password" | "1password_connect" => "secret_ref",
        "token_broker" => "credential_id",
        _ => return None,
    };
    source
        .config
        .get(key)
        .map(value_to_string)
        .filter(|value| !value.is_empty())
}

fn injection(row: &SecretRow) -> Option<String> {
    match (&row.fields, row.kind) {
        (
            SecretFields::Static {
                inject_config,
                replace_config,
                ..
            },
            SecretKind::Static,
        ) => {
            if present(inject_config) {
                if let Some(header) = inject_config.get("header").and_then(Value::as_str)
                    && !header.is_empty()
                {
                    return Some(header.to_owned());
                }
                if let Some(query) = inject_config.get("query_param").and_then(Value::as_str)
                    && !query.is_empty()
                {
                    return Some(format!("?{query}"));
                }
            } else if present(replace_config) {
                let headers = replace_config
                    .get("match_headers")
                    .and_then(Value::as_array)
                    .map(|headers| headers.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                    .unwrap_or_default();
                if !headers.is_empty() {
                    return Some(headers.join(", "));
                }
            }
            None
        }
        (SecretFields::GcpAuth { .. }, SecretKind::GcpAuth) => {
            Some("Authorization: Bearer".to_owned())
        }
        (SecretFields::GcpIdToken { header, .. }, SecretKind::GcpIdToken) => Some(format!(
            "{}: Bearer",
            header
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("authorization")
        )),
        (SecretFields::AwsAuth { .. }, SecretKind::AwsAuth) => {
            Some("Authorization: AWS4-HMAC-SHA256".to_owned())
        }
        (
            SecretFields::OauthToken {
                header,
                value_prefix,
                ..
            },
            SecretKind::OauthToken,
        ) => {
            let header = header
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("Authorization");
            let prefix = value_prefix
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            Some(match prefix {
                Some(prefix) => format!("{header}: {prefix} …"),
                None => header.to_owned(),
            })
        }
        (SecretFields::Hmac { headers, .. }, SecretKind::Hmac) => {
            let names: Vec<&str> = headers
                .as_array()
                .map(|headers| {
                    headers
                        .iter()
                        .filter_map(|header| header.get("name").and_then(Value::as_str))
                        .filter(|name| !name.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            (!names.is_empty()).then(|| names.join(", "))
        }
        _ => None,
    }
}

fn configuration(row: &SecretRow) -> Vec<ConfigurationRow> {
    let mut rows = Vec::new();
    if let Some(injection) = injection(row) {
        rows.push(ConfigurationRow {
            label: "Injects into",
            value: injection,
        });
    }
    match (&row.fields, row.kind) {
        (
            SecretFields::Static {
                kind,
                replace_config,
                ..
            },
            SecretKind::Static,
        ) => {
            rows.push(ConfigurationRow {
                label: "Credential profile",
                value: kind.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Mode",
                value: if present(replace_config) {
                    "replace"
                } else {
                    "inject"
                }
                .to_owned(),
            });
        }
        (
            SecretFields::GcpAuth {
                subject, scopes, ..
            },
            SecretKind::GcpAuth,
        ) => {
            if let Some(subject) = subject.as_deref().filter(|value| !value.is_empty()) {
                rows.push(ConfigurationRow {
                    label: "Subject",
                    value: subject.to_owned(),
                });
            }
            if present(scopes) {
                rows.push(ConfigurationRow {
                    label: "Scopes",
                    value: string_lines(scopes),
                });
            }
        }
        (SecretFields::GcpIdToken { audience, header }, SecretKind::GcpIdToken) => {
            rows.push(ConfigurationRow {
                label: "Audience",
                value: audience.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Header",
                value: header
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .unwrap_or("authorization")
                    .to_owned(),
            });
        }
        (
            SecretFields::AwsAuth {
                allowed_regions,
                allowed_services,
            },
            SecretKind::AwsAuth,
        ) => {
            if present(allowed_regions) {
                rows.push(ConfigurationRow {
                    label: "Allowed regions",
                    value: string_lines(allowed_regions),
                });
            }
            if present(allowed_services) {
                rows.push(ConfigurationRow {
                    label: "Allowed services",
                    value: string_lines(allowed_services),
                });
            }
        }
        (
            SecretFields::OauthToken {
                grant,
                token_endpoint,
                audience,
                scopes,
                ..
            },
            SecretKind::OauthToken,
        ) => {
            rows.push(ConfigurationRow {
                label: "Grant",
                value: grant.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Token endpoint",
                value: token_endpoint.clone(),
            });
            if let Some(audience) = audience.as_deref().filter(|value| !value.is_empty()) {
                rows.push(ConfigurationRow {
                    label: "Audience",
                    value: audience.to_owned(),
                });
            }
            if present(scopes) {
                rows.push(ConfigurationRow {
                    label: "Scopes",
                    value: string_lines(scopes),
                });
            }
        }
        (
            SecretFields::Hmac {
                timestamp_format,
                signature_algorithm,
                signature_key_encoding,
                signature_output_encoding,
                signature_message,
                ..
            },
            SecretKind::Hmac,
        ) => {
            rows.push(ConfigurationRow {
                label: "Timestamp format",
                value: timestamp_format.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Algorithm",
                value: signature_algorithm.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Key encoding",
                value: signature_key_encoding.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Output encoding",
                value: signature_output_encoding.clone(),
            });
            rows.push(ConfigurationRow {
                label: "Message",
                value: signature_message.clone(),
            });
        }
        (
            SecretFields::PgDsn {
                database,
                role,
                settings,
            },
            SecretKind::PgDsn,
        ) => {
            if !database.is_empty() {
                rows.push(ConfigurationRow {
                    label: "Database",
                    value: database.clone(),
                });
            }
            if let Some(role) = role.as_deref().filter(|value| !value.is_empty()) {
                rows.push(ConfigurationRow {
                    label: "Role",
                    value: role.to_owned(),
                });
            }
            if present(settings) {
                rows.push(ConfigurationRow {
                    label: "Session settings",
                    value: settings
                        .as_array()
                        .map(|settings| {
                            settings
                                .iter()
                                .map(pg_setting_row)
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default(),
                });
            }
        }
        _ => {}
    }
    rows
}

fn signed_headers(row: &SecretRow) -> Option<Vec<SignedHeaderPayload>> {
    let SecretFields::Hmac { headers, .. } = &row.fields else {
        return None;
    };
    Some(
        headers
            .as_array()
            .map(|headers| {
                headers
                    .iter()
                    .filter(|header| header.is_object())
                    .map(|header| SignedHeaderPayload {
                        name: header.get("name").cloned().unwrap_or(Value::Null),
                        value: header.get("value").cloned().unwrap_or(Value::Null),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn string_lines(value: &Value) -> String {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn pg_setting_row(setting: &Value) -> String {
    let setting = setting.as_object();
    let name = setting
        .and_then(|setting| setting.get("name"))
        .map(value_to_string)
        .unwrap_or_default();
    if let Some(reference) = setting
        .and_then(|setting| setting.get("value_from"))
        .and_then(Value::as_object)
        && let Some(kind) = ["principal_label", "principal_field", "proxy_label"]
            .into_iter()
            .find(|key| reference.contains_key(*key))
    {
        return format!(
            "{name} = {kind}({})",
            reference.get(kind).map(value_to_string).unwrap_or_default()
        );
    }
    format!(
        "{name} = {}",
        setting
            .and_then(|setting| setting.get("value"))
            .map(value_to_string)
            .unwrap_or_default()
    )
}

fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(boolean) => *boolean,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
        Value::Number(_) => true,
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}
