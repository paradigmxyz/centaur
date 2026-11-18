//! A secret document, parsed from a request and checked against its kind's
//! rules, before anything is written.
//!
//! [`parse`] turns the request body into a [`Draft`]: the columns the
//! document writes (omitted optional columns take their defaults, so a
//! write replaces the document), its sources, and its rules. The write path
//! then fills in what only the database knows (the inline value a metadata
//! edit keeps, broker credentials), and [`Draft::validate`] records every
//! rule the document breaks.

use std::{collections::BTreeMap, sync::LazyLock};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    dsn,
    kind::Kind,
    parts::{
        CREDENTIAL_FIELD, ENDPOINT_HEADER, Errors, Rule, RuleIn, Source, SourceIn, blank_str,
        is_blank, validate_rule, validate_source,
    },
};
use crate::{ApiError, support::json_schema::Schema};

pub const URL_SAFE_MESSAGE: &str =
    "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)";

pub struct Draft {
    pub kind: Kind,
    /// The document's columns as JSON; every column of
    /// `Kind::document_columns` is present.
    pub columns: Map<String, Value>,
    /// Sources in request order.
    pub sources: Vec<Source>,
    pub rules: Vec<Rule>,
    pub errors: Errors,
}

// --- request shapes -------------------------------------------------------

/// `Some(None)` for an explicit `null`, `None` when the key is absent.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize, Default)]
struct Common {
    /// Absent keeps the stored value; `null` clears it.
    #[serde(default, deserialize_with = "present")]
    foreign_id: Option<Option<String>>,
    name: Option<String>,
    description: Option<String>,
    labels: Option<Map<String, Value>>,
}

#[derive(Deserialize, Default)]
struct StaticIn {
    kind: Option<String>,
    inject_config: Option<Value>,
    replace_config: Option<Value>,
    source: Option<SourceIn>,
    rules: Option<Vec<RuleIn>>,
}

#[derive(Deserialize, Default)]
struct GcpAuthIn {
    credentials_provider: Option<Value>,
    subject: Option<String>,
    scopes: Option<Vec<String>>,
    keyfile: Option<SourceIn>,
    rules: Option<Vec<RuleIn>>,
}

#[derive(Deserialize, Default)]
struct GcpIdTokenIn {
    audience: Option<String>,
    header: Option<String>,
    keyfile: Option<SourceIn>,
    rules: Option<Vec<RuleIn>>,
}

#[derive(Deserialize, Default)]
struct AwsAuthIn {
    allowed_regions: Option<Vec<String>>,
    allowed_services: Option<Vec<String>>,
    access_key_id: Option<SourceIn>,
    secret_access_key: Option<SourceIn>,
    session_token: Option<SourceIn>,
    rules: Option<Vec<RuleIn>>,
}

#[derive(Deserialize, Default)]
struct OauthTokenIn {
    grant: Option<String>,
    token_endpoint: Option<String>,
    audience: Option<String>,
    scopes: Option<Vec<String>>,
    header: Option<String>,
    value_prefix: Option<String>,
    credentials: Option<BTreeMap<String, SourceIn>>,
    token_endpoint_headers: Option<BTreeMap<String, SourceIn>>,
    rules: Option<Vec<RuleIn>>,
}

#[derive(Deserialize, Default)]
struct PgDsnIn {
    database: Option<String>,
    role: Option<String>,
    /// Each `{name, value}` or `{name, value_from: {...}}`.
    settings: Option<Vec<Value>>,
    dsn: Option<SourceIn>,
}

#[derive(Deserialize)]
struct HeaderIn {
    name: Option<String>,
    value: Option<String>,
}

#[derive(Deserialize, Default)]
struct HmacIn {
    timestamp_format: Option<String>,
    signature_algorithm: Option<String>,
    signature_key_encoding: Option<String>,
    signature_output_encoding: Option<String>,
    signature_message: Option<String>,
    allow_chunked_body: Option<bool>,
    headers: Option<Vec<HeaderIn>>,
    credentials: Option<BTreeMap<String, SourceIn>>,
    rules: Option<Vec<RuleIn>>,
}

#[derive(Deserialize, Default)]
struct SshKeyIn {
    public_key: Option<String>,
    private_key: Option<SourceIn>,
}

fn shape<T: serde::de::DeserializeOwned>(body: &Value) -> Result<T, ApiError> {
    // serde's message would quote the offending value, which may be a secret.
    serde_json::from_value(body.clone()).map_err(|_| {
        ApiError::BadRequest("the document has a field of the wrong type or shape".into())
    })
}

fn rules(input: Option<Vec<RuleIn>>) -> Vec<Rule> {
    input
        .unwrap_or_default()
        .into_iter()
        .map(Rule::from)
        .collect()
}

fn roles(input: Option<BTreeMap<String, SourceIn>>, role_kind: &'static str) -> Vec<Source> {
    input
        .unwrap_or_default()
        .into_iter()
        .map(|(role, source)| Source::from_input(source, Some(role), Some(role_kind)))
        .collect()
}

fn single(input: Option<SourceIn>) -> Vec<Source> {
    input
        .map(|source| Source::from_input(source, None, None))
        .into_iter()
        .collect()
}

fn strings(values: Option<Vec<String>>) -> Value {
    json!(values.unwrap_or_default())
}

/// A JSON config that is blank (`null`, `{}`) is stored as NULL.
fn config_or_null(value: Option<Value>) -> Value {
    value
        .filter(|value| !is_blank(value))
        .unwrap_or(Value::Null)
}

/// Parses a request body for `kind`. `current` is the stored row's columns
/// when the document updates one: `foreign_id` and a static secret's `kind`
/// keep their stored value when the request does not mention them.
pub fn parse(
    kind: Kind,
    body: &Value,
    current: Option<&Map<String, Value>>,
) -> Result<Draft, ApiError> {
    let common: Common = shape(body)?;
    let stored = |column: &str| current.and_then(|row| row.get(column)).cloned();
    let mut columns = Map::new();
    columns.insert(
        "foreign_id".into(),
        match common.foreign_id {
            // A blank string leaves the identity alone, like leaving it out.
            Some(Some(value)) if !value.trim().is_empty() => json!(value),
            Some(None) => Value::Null,
            _ => stored("foreign_id").unwrap_or(Value::Null),
        },
    );
    columns.insert("name".into(), json!(common.name));
    columns.insert("description".into(), json!(common.description));
    columns.insert(
        "labels".into(),
        Value::Object(common.labels.unwrap_or_default()),
    );
    let mut draft = Draft {
        kind,
        columns,
        sources: Vec::new(),
        rules: Vec::new(),
        errors: Errors::default(),
    };
    let set = |draft: &mut Draft, column: &str, value: Value| {
        draft.columns.insert(column.into(), value);
    };
    match kind {
        Kind::Static => {
            let input: StaticIn = shape(body)?;
            let kind_value = input
                .kind
                .map(Value::from)
                .or_else(|| stored("kind"))
                .unwrap_or_else(|| "custom".into());
            set(&mut draft, "kind", kind_value);
            set(
                &mut draft,
                "inject_config",
                config_or_null(input.inject_config),
            );
            set(
                &mut draft,
                "replace_config",
                config_or_null(input.replace_config),
            );
            draft.sources = single(input.source);
            draft.rules = rules(input.rules);
        }
        Kind::GcpAuth => {
            let input: GcpAuthIn = shape(body)?;
            set(
                &mut draft,
                "credentials_provider",
                config_or_null(input.credentials_provider),
            );
            set(&mut draft, "subject", json!(input.subject));
            set(&mut draft, "scopes", strings(input.scopes));
            draft.sources = single(input.keyfile);
            draft.rules = rules(input.rules);
        }
        Kind::GcpIdToken => {
            let input: GcpIdTokenIn = shape(body)?;
            set(&mut draft, "audience", json!(input.audience));
            // Header names are case-insensitive: store them lower-cased.
            let header = input
                .header
                .map(|header| header.trim().to_lowercase())
                .filter(|header| !header.is_empty());
            set(&mut draft, "header", json!(header));
            draft.sources = single(input.keyfile);
            draft.rules = rules(input.rules);
        }
        Kind::AwsAuth => {
            let input: AwsAuthIn = shape(body)?;
            set(
                &mut draft,
                "allowed_regions",
                strings(input.allowed_regions),
            );
            set(
                &mut draft,
                "allowed_services",
                strings(input.allowed_services),
            );
            for (role, source) in [
                ("access_key_id", input.access_key_id),
                ("secret_access_key", input.secret_access_key),
                ("session_token", input.session_token),
            ] {
                if let Some(source) = source {
                    draft.sources.push(Source::from_input(
                        source,
                        Some(role.into()),
                        Some(CREDENTIAL_FIELD),
                    ));
                }
            }
            draft.rules = rules(input.rules);
        }
        Kind::OauthToken => {
            let input: OauthTokenIn = shape(body)?;
            set(&mut draft, "grant", json!(input.grant));
            set(&mut draft, "token_endpoint", json!(input.token_endpoint));
            set(&mut draft, "audience", json!(input.audience));
            set(&mut draft, "scopes", strings(input.scopes));
            set(&mut draft, "header", json!(input.header));
            set(&mut draft, "value_prefix", json!(input.value_prefix));
            draft.sources = roles(input.credentials, CREDENTIAL_FIELD);
            draft
                .sources
                .extend(roles(input.token_endpoint_headers, ENDPOINT_HEADER));
            draft.rules = rules(input.rules);
        }
        Kind::PgDsn => {
            let input: PgDsnIn = shape(body)?;
            set(&mut draft, "database", json!(input.database));
            set(&mut draft, "role", json!(input.role));
            let settings = input
                .settings
                .unwrap_or_default()
                .into_iter()
                .map(normalize_setting)
                .collect();
            set(&mut draft, "settings", Value::Array(settings));
            draft.sources = single(input.dsn);
        }
        Kind::Hmac => {
            let input: HmacIn = shape(body)?;
            set(
                &mut draft,
                "timestamp_format",
                json!(input.timestamp_format),
            );
            set(
                &mut draft,
                "signature_algorithm",
                json!(input.signature_algorithm),
            );
            set(
                &mut draft,
                "signature_key_encoding",
                json!(input.signature_key_encoding),
            );
            set(
                &mut draft,
                "signature_output_encoding",
                json!(input.signature_output_encoding),
            );
            set(
                &mut draft,
                "signature_message",
                json!(input.signature_message),
            );
            set(
                &mut draft,
                "allow_chunked_body",
                json!(input.allow_chunked_body.unwrap_or(false)),
            );
            let headers: Vec<Value> = input
                .headers
                .unwrap_or_default()
                .into_iter()
                .map(|header| json!({"name": header.name, "value": header.value}))
                .collect();
            set(&mut draft, "headers", Value::Array(headers));
            draft.sources = roles(input.credentials, CREDENTIAL_FIELD);
            draft.rules = rules(input.rules);
        }
        Kind::SshKey => {
            let input: SshKeyIn = shape(body)?;
            let name = draft.columns.get("name").and_then(Value::as_str);
            let name = name.map(|name| name.trim().to_owned());
            set(&mut draft, "name", json!(name));
            // `<type> <base64>`: the comment is not part of the key.
            let public_key = input
                .public_key
                .map(|key| key.split_whitespace().take(2).collect::<Vec<_>>().join(" "));
            set(&mut draft, "public_key", json!(public_key));
            draft.sources = single(input.private_key);
        }
    }
    Ok(draft)
}

/// Keeps only the keys a setting stores.
fn normalize_setting(setting: Value) -> Value {
    let Value::Object(map) = setting else {
        return setting;
    };
    Value::Object(
        map.into_iter()
            .filter(|(key, _)| matches!(key.as_str(), "name" | "value" | "value_from"))
            .collect(),
    )
}

// --- validation -------------------------------------------------------------

const GITHUB_TOKEN: &str = "github_token";
const GITHUB_HOSTS: &[&str] = &["api.github.com", "github.com", "api.githubcopilot.com"];

fn github_replace_config() -> Value {
    json!({"proxy_value": "GITHUB_TOKEN", "match_headers": ["Authorization"], "require": false})
}

static INJECT_CONFIG: LazyLock<Schema> = LazyLock::new(|| {
    Schema::new(&json!({
        "type": "object",
        "properties": {
            "header": {"type": "string", "minLength": 1},
            "formatter": {"type": "string"},
            "query_param": {"type": "string", "minLength": 1},
        },
        "additionalProperties": false,
        "oneOf": [{"required": ["header"]}, {"required": ["query_param"]}],
    }))
});

static REPLACE_CONFIG: LazyLock<Schema> = LazyLock::new(|| {
    Schema::new(&json!({
        "type": "object",
        "required": ["proxy_value"],
        "properties": {
            "proxy_value": {"type": "string", "minLength": 1},
            "match_headers": {"type": "array", "items": {"type": "string"}},
            "match_body": {"type": "boolean"},
            "match_path": {"type": "boolean"},
            "match_query": {"type": "boolean"},
            "require": {"type": "boolean"},
        },
        "additionalProperties": false,
    }))
});

static CREDENTIALS_PROVIDER: LazyLock<Schema> = LazyLock::new(|| {
    Schema::new(&json!({
        "type": "object",
        "required": ["type"],
        "properties": {"type": {"enum": ["workload_identity"]}},
        "additionalProperties": false,
    }))
});

const ID_TOKEN_HEADERS: &[&str] = &["authorization", "x-serverless-authorization"];
const GRANTS: &[&str] = &[
    "refresh_token",
    "client_credentials",
    "password",
    "jwt_bearer",
];
const CREDENTIAL_FIELDS: &[&str] = &[
    "refresh_token",
    "client_id",
    "client_secret",
    "username",
    "password",
    "issuer",
    "subject",
    "private_key",
    "private_key_id",
];
const AWS_REQUIRED: &[&str] = &["access_key_id", "secret_access_key"];
const TIMESTAMP_FORMATS: &[&str] = &["unix_seconds", "unix_millis", "unix_nanos", "rfc3339"];
const ALGORITHMS: &[&str] = &["sha256", "sha512", "sha1"];
const KEY_ENCODINGS: &[&str] = &["raw", "base64", "hex"];
const OUTPUT_ENCODINGS: &[&str] = &["base64", "hex"];
/// `VALUE_FROM` references a pg_dsn setting may use.
pub const VALUE_FROM_KEYS: &[&str] = &["principal_label", "principal_field", "proxy_label"];
const PRINCIPAL_FIELDS: &[&str] = &[
    "id",
    "foreign_id",
    "name",
    "kind",
    "slack_user_id",
    "slack_channel_id",
    "slack_team_id",
    "slack_email",
    "console_user_id",
    "console_user_email",
    "slack_history_channel_ids",
];
/// Session settings the proxy manages itself.
const RESERVED_SETTINGS: &[&str] = &["role", "session_authorization"];

/// `(required, optional)` credential fields of an OAuth grant.
fn grant_fields(grant: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match grant {
        "refresh_token" => (&["refresh_token", "client_id"], &["client_secret"]),
        "client_credentials" => (&["client_id", "client_secret"], &[]),
        "password" => (&["username", "password", "client_id"], &["client_secret"]),
        "jwt_bearer" => (&["issuer", "subject", "private_key"], &["private_key_id"]),
        _ => return None,
    })
}

fn url_safe(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

/// A Postgres setting name: `name` or `class.name`.
fn setting_name(name: &str) -> bool {
    let identifier = |part: &str| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    match name.split_once('.') {
        Some((class, rest)) => identifier(class) && identifier(rest),
        None => identifier(name),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl Draft {
    fn text(&self, column: &str) -> Option<&str> {
        self.columns.get(column).and_then(Value::as_str)
    }

    fn column_blank(&self, column: &str) -> bool {
        self.columns.get(column).is_none_or(is_blank)
    }

    fn add(&mut self, field: &str, message: impl Into<String>) {
        self.errors.add(field, message);
    }

    fn require(&mut self, column: &str) {
        if self.column_blank(column) {
            self.add(column, "can't be blank");
        }
    }

    fn inclusion(&mut self, column: &str, allowed: &[&str]) {
        if !self
            .text(column)
            .is_some_and(|value| allowed.contains(&value))
        {
            self.add(column, format!("must be one of {}", allowed.join(", ")));
        }
    }

    fn foreign_id(&mut self, required: bool) {
        let reserved = format!("{}_", self.kind.oid_prefix());
        match self.columns.get("foreign_id").cloned() {
            Some(Value::String(foreign_id)) => {
                if foreign_id.starts_with(&reserved) {
                    self.add(
                        "foreign_id",
                        format!(
                            "must not start with \"{reserved}\", which is reserved for opaque ids"
                        ),
                    );
                }
                if !url_safe(&foreign_id) {
                    self.add("foreign_id", URL_SAFE_MESSAGE);
                }
            }
            _ if required => self.add("foreign_id", "can't be blank"),
            _ => {}
        }
    }

    fn string_list(&mut self, column: &str) {
        let blank_item = self
            .columns
            .get(column)
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.as_str().is_none_or(|s| s.trim().is_empty()))
            });
        if blank_item {
            self.add(column, "must not contain blank entries");
        }
    }

    fn schema(&mut self, column: &str, schema: &Schema) {
        if let Some(value) = self
            .columns
            .get(column)
            .filter(|value| !is_blank(value))
            .cloned()
        {
            for message in schema.errors(&value) {
                self.add(column, message);
            }
        }
    }

    fn at_least_one_rule(&mut self) {
        if self.rules.is_empty() {
            self.add("rules", "must include at least one rule");
        }
    }

    fn source_roles(&self, role_kind: &str) -> Vec<&str> {
        self.sources
            .iter()
            .filter(|source| source.role_kind == Some(role_kind))
            .map(|source| source.role.as_deref().unwrap_or_default())
            .collect()
    }

    /// Records every rule the document breaks in `errors`.
    pub fn validate(&mut self) {
        self.foreign_id(self.kind == Kind::PgDsn);
        match self.kind {
            Kind::Static => self.validate_static(),
            Kind::GcpAuth => self.validate_gcp_auth(),
            Kind::GcpIdToken => self.validate_gcp_id_token(),
            Kind::AwsAuth => self.validate_aws_auth(),
            Kind::OauthToken => self.validate_oauth_token(),
            Kind::PgDsn => self.validate_pg_dsn(),
            Kind::Hmac => self.validate_hmac(),
            Kind::SshKey => self.validate_ssh_key(),
        }
        self.validate_parts();
    }

    /// Sources and rules: each on its own, under the request key it came
    /// from.
    fn validate_parts(&mut self) {
        let single_key = self.kind.source_key();
        let mut errors = std::mem::take(&mut self.errors);
        for source in &self.sources {
            let prefix = match (single_key, &source.role, source.role_kind) {
                (Some(key), None, _) => key.to_owned(),
                (_, Some(role), Some(ENDPOINT_HEADER)) => format!("token_endpoint_headers.{role}"),
                (_, Some(role), _) if self.kind == Kind::AwsAuth => role.clone(),
                (_, Some(role), _) => format!("credentials.{role}"),
                _ => "source".to_owned(),
            };
            if source.role_kind.is_some() && blank_str(source.role.as_deref()) {
                errors.add(&format!("{prefix}.role"), "can't be blank");
            }
            validate_source(source, &prefix, &mut errors);
        }
        for (index, rule) in self.rules.iter().enumerate() {
            validate_rule(rule, index, &mut errors);
        }
        self.errors = errors;
    }

    fn validate_static(&mut self) {
        self.require("kind");
        if !self.column_blank("kind") {
            self.inclusion("kind", &["custom", GITHUB_TOKEN]);
        }
        let inject = !self.column_blank("inject_config");
        let replace = !self.column_blank("replace_config");
        match (inject, replace) {
            (false, false) => {
                self.add("base", "must define one of inject_config or replace_config")
            }
            (true, true) => self.add(
                "base",
                "inject_config and replace_config are mutually exclusive",
            ),
            _ => {}
        }
        self.schema("inject_config", &INJECT_CONFIG);
        self.schema("replace_config", &REPLACE_CONFIG);
        if self.text("kind") == Some(GITHUB_TOKEN) {
            if inject || self.columns.get("replace_config") != Some(&github_replace_config()) {
                self.add(
                    "base",
                    "github_token credentials must use the canonical Authorization placeholder replacement",
                );
            }
            let confined = !self.rules.is_empty()
                && self.rules.iter().all(|rule| {
                    rule.host
                        .as_deref()
                        .is_some_and(|host| GITHUB_HOSTS.contains(&host))
                        && blank_str(rule.cidr.as_deref())
                        && rule.http_methods.is_empty()
                        && rule.paths.is_empty()
                });
            if !confined {
                self.add(
                    "rules",
                    format!(
                        "github_token credentials must target only {}",
                        GITHUB_HOSTS.join(", ")
                    ),
                );
            }
        }
    }

    /// The `github_token` profile fills in its canonical placeholder
    /// replacement and host rules when the document leaves them out.
    pub fn apply_profile(&mut self) {
        if self.kind != Kind::Static || self.text("kind") != Some(GITHUB_TOKEN) {
            return;
        }
        let canonical = match self.columns.get("replace_config") {
            None | Some(Value::Null) => true,
            Some(replace) => {
                let mut without_require = github_replace_config();
                if let Some(map) = without_require.as_object_mut() {
                    map.remove("require");
                }
                *replace == github_replace_config() || *replace == without_require
            }
        };
        if self.column_blank("inject_config") && canonical {
            self.columns
                .insert("replace_config".into(), github_replace_config());
        }
        if self.rules.is_empty() {
            self.rules = GITHUB_HOSTS.iter().map(|host| Rule::host(host)).collect();
        }
    }

    fn validate_gcp_auth(&mut self) {
        let keyfile = !self.sources.is_empty();
        let provider = !self.column_blank("credentials_provider");
        match (keyfile, provider) {
            (false, false) => self.add(
                "base",
                "must define one of keyfile (source) or credentials_provider",
            ),
            (true, true) => self.add(
                "base",
                "keyfile and credentials_provider are mutually exclusive",
            ),
            _ => {}
        }
        if self
            .columns
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            self.add("scopes", "must include at least one scope");
        }
        self.string_list("scopes");
        if provider && !blank_str(self.text("subject")) {
            self.add("subject", "is only allowed with a keyfile source");
        }
        self.schema("credentials_provider", &CREDENTIALS_PROVIDER);
    }

    fn validate_gcp_id_token(&mut self) {
        self.require("audience");
        if self.sources.is_empty() {
            self.add("keyfile_source", "can't be blank");
        }
        if let Some(header) = self.text("header")
            && !ID_TOKEN_HEADERS.contains(&header)
        {
            self.add(
                "header",
                format!("must be one of {}", ID_TOKEN_HEADERS.join(", ")),
            );
        }
        self.at_least_one_rule();
    }

    fn validate_aws_auth(&mut self) {
        self.string_list("allowed_regions");
        self.string_list("allowed_services");
        let present = self.source_roles(CREDENTIAL_FIELD);
        let missing: Vec<&str> = AWS_REQUIRED
            .iter()
            .copied()
            .filter(|role| !present.contains(role))
            .collect();
        for role in missing {
            self.add(
                "sources",
                format!("is missing the required \"{role}\" credential"),
            );
        }
        self.at_least_one_rule();
    }

    fn validate_oauth_token(&mut self) {
        let grant = self.text("grant").map(str::to_owned);
        if !grant
            .as_deref()
            .is_some_and(|grant| GRANTS.contains(&grant))
        {
            self.add("grant", format!("must be one of {}", GRANTS.join(", ")));
        }
        self.require("token_endpoint");
        if grant.as_deref() == Some("jwt_bearer") && blank_str(self.text("audience")) {
            self.add("audience", "can't be blank for the jwt_bearer grant");
        }
        self.string_list("scopes");
        if let Some(grant) = &grant
            && let Some((required, optional)) = grant_fields(grant)
        {
            let present: Vec<String> = self
                .source_roles(CREDENTIAL_FIELD)
                .into_iter()
                .map(str::to_owned)
                .collect();
            for role in &present {
                if !CREDENTIAL_FIELDS.contains(&role.as_str()) {
                    self.add(
                        "sources",
                        format!("\"{role}\" is not a valid credential field"),
                    );
                } else if !required.contains(&role.as_str()) && !optional.contains(&role.as_str()) {
                    self.add(
                        "sources",
                        format!("field \"{role}\" is not used by the {grant} grant"),
                    );
                }
            }
            for field in required {
                if !present.iter().any(|role| role == field) {
                    self.add(
                        "sources",
                        format!("is missing required field \"{field}\" for the {grant} grant"),
                    );
                }
            }
        }
        self.at_least_one_rule();
    }

    fn validate_pg_dsn(&mut self) {
        self.require("database");
        let settings = self
            .columns
            .get("settings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut seen = Vec::new();
        for (index, setting) in settings.iter().enumerate() {
            if let Some(reason) = setting_error(setting, &mut seen) {
                self.add("settings", format!("[{index}] {reason}"));
            }
        }
        if self.sources.is_empty() {
            self.add("dsn_source", "can't be blank");
        }
        // An inline DSN that names a database must name this secret's.
        let database = self.text("database").filter(|name| !name.trim().is_empty());
        let inline = self.sources.first().filter(|source| source.inline());
        if let (Some(database), Some(inline)) = (database, inline)
            && let Some(named) = inline_dsn(inline).as_deref().and_then(dsn::dbname)
        {
            match named {
                Some(named) if named != database => self.add(
                    "database",
                    format!("must match the DSN database (\"{named}\")"),
                ),
                None => self.add(
                    "database",
                    format!("DSN names no database; it must match \"{database}\""),
                ),
                Some(_) => {}
            }
        }
    }

    fn validate_hmac(&mut self) {
        for (column, allowed) in [
            ("timestamp_format", TIMESTAMP_FORMATS),
            ("signature_algorithm", ALGORITHMS),
            ("signature_key_encoding", KEY_ENCODINGS),
            ("signature_output_encoding", OUTPUT_ENCODINGS),
        ] {
            self.inclusion(column, allowed);
        }
        self.require("signature_message");
        let headers = self
            .columns
            .get("headers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if headers.is_empty() {
            self.add("headers", "must include at least one header");
        }
        let text = |header: &Value, key: &str| {
            header
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty())
        };
        if headers
            .iter()
            .any(|header| !(text(header, "name") && text(header, "value")))
        {
            self.add(
                "headers",
                "each header must have a non-blank name and value",
            );
        }
        if !self.source_roles(CREDENTIAL_FIELD).contains(&"secret") {
            self.add("sources", "is missing the required \"secret\" credential");
        }
        self.at_least_one_rule();
    }

    /// Derives the public key of an inline private key (`normalized` form
    /// stored); other sources keep the document's public key.
    pub fn derive_ssh_public_key(&mut self) {
        if self.kind != Kind::SshKey {
            return;
        }
        let Some(source) = self.sources.first_mut().filter(|source| source.inline()) else {
            return;
        };
        let secret = source.secret.clone().unwrap_or_default();
        if !secret.trim().is_empty() {
            source.secret = Some(centaur_console_db::keys::normalize(&secret));
        }
        match centaur_console_db::keys::inspect(&secret) {
            Ok(info) => {
                self.columns
                    .insert("public_key".into(), json!(info.public_key));
            }
            Err(error) => self.add("private_key", error.to_string()),
        }
    }

    fn validate_ssh_key(&mut self) {
        self.require("name");
        if self
            .text("name")
            .is_some_and(|name| name.chars().count() > 100)
        {
            self.add("name", "is too long (maximum is 100 characters)");
        }
        match self.sources.first() {
            None => self.add("private_key", "can't be blank"),
            Some(source) if source.source_type == "token_broker" => {
                self.add("private_key", "can't come from a token broker");
            }
            Some(_) => {}
        }
        // An inline key that failed to parse already reported its error.
        if !self.errors_on("private_key")
            && !self
                .text("public_key")
                .is_some_and(centaur_console_db::keys::valid_public_key)
        {
            self.add(
                "public_key",
                "must be an Ed25519, RSA, or ECDSA public key in OpenSSH format",
            );
        }
    }

    fn errors_on(&self, field: &str) -> bool {
        self.errors.has(field)
    }
}

/// The connection string an inline pg_dsn source holds: its value, or one
/// key of it when the source's config names a `json_key`.
fn inline_dsn(source: &Source) -> Option<String> {
    let secret = source.secret.as_deref()?;
    let key = source.config.get("json_key").filter(|key| !is_blank(key));
    let Some(key) = key else {
        return Some(secret.to_owned());
    };
    let parsed: Value = serde_json::from_str(secret).ok()?;
    parsed.get(value_text(key))?.as_str().map(str::to_owned)
}

fn setting_error(setting: &Value, seen: &mut Vec<String>) -> Option<String> {
    let Value::Object(setting) = setting else {
        return Some("must be an object".into());
    };
    let name = setting.get("name").map(value_text).unwrap_or_default();
    if name.trim().is_empty() {
        return Some("name is required".into());
    }
    if !setting_name(&name) {
        return Some(format!("invalid setting name \"{name}\""));
    }
    let lower = name.to_lowercase();
    if RESERVED_SETTINGS.contains(&lower.as_str()) {
        return Some(format!(
            "\"{name}\" is managed by the proxy; use the role field"
        ));
    }
    if seen.contains(&lower) {
        return Some(format!("duplicate setting \"{name}\""));
    }
    seen.push(lower);
    let reference = setting.get("value_from").filter(|value| !value.is_null())?;
    if setting.get("value").is_some_and(|value| !value.is_null()) {
        return Some("value and value_from are mutually exclusive".into());
    }
    let Value::Object(reference) = reference else {
        return Some("value_from must be an object".into());
    };
    let mut keys = reference.keys();
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return Some(format!(
            "value_from must have exactly one of {}",
            VALUE_FROM_KEYS.join(" or ")
        ));
    };
    if !VALUE_FROM_KEYS.contains(&key.as_str()) {
        return Some(format!(
            "value_from must have exactly one of {}",
            VALUE_FROM_KEYS.join(" or ")
        ));
    }
    let value = value_text(&reference[key]);
    match key.as_str() {
        "principal_label" | "proxy_label" if value.trim().is_empty() => {
            Some(format!("{key} can't be blank"))
        }
        "principal_field" if !PRINCIPAL_FIELDS.contains(&value.as_str()) => Some(format!(
            "unknown principal_field \"{value}\" (one of: {})",
            PRINCIPAL_FIELDS.join(", ")
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(kind: Kind, body: Value) -> Draft {
        let mut draft = parse(kind, &body, None).unwrap();
        draft.apply_profile();
        draft.derive_ssh_public_key();
        draft.validate();
        draft
    }

    fn fields(draft: &Draft) -> Vec<String> {
        draft.errors.fields().map(str::to_owned).collect()
    }

    #[test]
    fn a_wrong_shaped_document_is_a_bad_request() {
        let error = parse(Kind::Static, &json!({"name": 5}), None)
            .err()
            .unwrap();
        assert!(matches!(error, ApiError::BadRequest(_)));
        // The message never quotes the value.
        assert!(!error.to_string().contains('5'));
    }

    #[test]
    fn static_secrets_need_exactly_one_mode() {
        let source = json!({"source_type": "env", "config": {"var": "X"}});
        let rules = json!([{"host": "api.test"}]);
        let ok = draft(
            Kind::Static,
            json!({"inject_config": {"header": "X-Key"}, "source": source, "rules": rules}),
        );
        assert!(ok.errors.is_empty(), "{:?}", fields(&ok));
        let none = draft(Kind::Static, json!({"source": source}));
        assert!(fields(&none).contains(&"base".to_owned()));
        let both = draft(
            Kind::Static,
            json!({"inject_config": {"header": "X"}, "replace_config": {"proxy_value": "P"}, "source": source}),
        );
        assert!(fields(&both).contains(&"base".to_owned()));
        let bad = draft(
            Kind::Static,
            json!({"inject_config": {"header": "X", "query_param": "q"}, "source": source}),
        );
        assert!(fields(&bad).contains(&"inject_config".to_owned()));
    }

    #[test]
    fn the_github_profile_fills_in_its_canonical_form() {
        let draft = draft(
            Kind::Static,
            json!({"kind": "github_token", "source": {"source_type": "env", "config": {"var": "X"}}}),
        );
        assert!(draft.errors.is_empty(), "{:?}", fields(&draft));
        assert_eq!(draft.columns["replace_config"], github_replace_config());
        assert_eq!(draft.rules.len(), 3);
        let off_host = parse(
            Kind::Static,
            &json!({"kind": "github_token", "rules": [{"host": "example.com"}],
                    "source": {"source_type": "env", "config": {"var": "X"}}}),
            None,
        )
        .unwrap();
        let mut off_host = off_host;
        off_host.apply_profile();
        off_host.validate();
        assert!(fields(&off_host).contains(&"rules".to_owned()));
    }

    #[test]
    fn foreign_ids_are_url_safe_and_not_opaque_ids() {
        let source = json!({"source_type": "env", "config": {"var": "X"}});
        let rules = json!([{"host": "a.test"}]);
        let bad = |foreign_id: &str| {
            let draft = draft(
                Kind::Static,
                json!({"foreign_id": foreign_id, "inject_config": {"header": "X"},
                       "source": source, "rules": rules}),
            );
            fields(&draft).contains(&"foreign_id".to_owned())
        };
        assert!(!bad("good-id_1.2~"));
        assert!(bad("has space"));
        assert!(bad("ssr_reserved"));
    }

    #[test]
    fn a_pg_dsn_needs_a_foreign_id_database_and_source() {
        let empty = draft(Kind::PgDsn, json!({}));
        let fields = fields(&empty);
        for field in ["foreign_id", "database", "dsn_source"] {
            assert!(fields.contains(&field.to_owned()), "{field}");
        }
    }

    #[test]
    fn an_inline_dsn_must_name_the_secrets_database() {
        let body = |dsn: &str| {
            json!({"foreign_id": "pg", "database": "shop",
                   "dsn": {"source_type": "control_plane", "secret": dsn, "config": {}}})
        };
        assert!(
            draft(Kind::PgDsn, body("postgres://u:p@h:5432/shop"))
                .errors
                .is_empty()
        );
        assert!(
            draft(Kind::PgDsn, body("host=h dbname=shop"))
                .errors
                .is_empty()
        );
        assert!(
            fields(&draft(Kind::PgDsn, body("postgres://u:p@h/other")))
                .contains(&"database".to_owned())
        );
        assert!(
            fields(&draft(Kind::PgDsn, body("postgres://u:p@h"))).contains(&"database".to_owned())
        );
        // Not a connection string at all: nothing to compare.
        assert!(draft(Kind::PgDsn, body("opaque")).errors.is_empty());
    }

    #[test]
    fn pg_settings_are_checked() {
        let with = |settings: Value| {
            let draft = draft(
                Kind::PgDsn,
                json!({"foreign_id": "pg", "database": "shop", "settings": settings,
                       "dsn": {"source_type": "env", "config": {"var": "D"}}}),
            );
            draft.errors.is_empty()
        };
        assert!(with(json!([{"name": "app.tenant", "value": "x"},
                            {"name": "app.who", "value_from": {"principal_field": "foreign_id"}}])));
        assert!(!with(json!([{"name": "role", "value": "x"}])));
        assert!(!with(
            json!([{"name": "a", "value": "x"}, {"name": "A", "value": "y"}])
        ));
        assert!(!with(json!([{"name": "1a", "value": "x"}])));
        assert!(!with(
            json!([{"name": "a", "value": "x", "value_from": {"proxy_label": "l"}}])
        ));
        assert!(!with(
            json!([{"name": "a", "value_from": {"principal_field": "nope"}}])
        ));
        assert!(!with(
            json!([{"name": "a", "value_from": {"proxy_label": "l", "principal_label": "m"}}])
        ));
    }

    #[test]
    fn oauth_credentials_follow_the_grant() {
        let env = |var: &str| json!({"source_type": "env", "config": {"var": var}});
        let body = |grant: &str, credentials: Value| {
            json!({"grant": grant, "token_endpoint": "https://idp.test/token",
                   "credentials": credentials, "rules": [{"host": "api.test"}]})
        };
        let ok = draft(
            Kind::OauthToken,
            body(
                "client_credentials",
                json!({"client_id": env("A"), "client_secret": env("B")}),
            ),
        );
        assert!(ok.errors.is_empty(), "{:?}", fields(&ok));
        let missing = draft(
            Kind::OauthToken,
            body("client_credentials", json!({"client_id": env("A")})),
        );
        assert!(fields(&missing).contains(&"sources".to_owned()));
        let unused = draft(
            Kind::OauthToken,
            body(
                "client_credentials",
                json!({"client_id": env("A"), "client_secret": env("B"), "username": env("C")}),
            ),
        );
        assert!(fields(&unused).contains(&"sources".to_owned()));
        let unknown_grant = draft(Kind::OauthToken, body("magic", json!({})));
        assert!(fields(&unknown_grant).contains(&"grant".to_owned()));
    }

    #[test]
    fn aws_needs_its_two_credentials_and_a_rule() {
        let env = |var: &str| json!({"source_type": "env", "config": {"var": var}});
        let ok = draft(
            Kind::AwsAuth,
            json!({"access_key_id": env("A"), "secret_access_key": env("B"),
                   "rules": [{"host": "s3.amazonaws.com"}]}),
        );
        assert!(ok.errors.is_empty(), "{:?}", fields(&ok));
        let bad = draft(Kind::AwsAuth, json!({"access_key_id": env("A")}));
        let fields = fields(&bad);
        assert!(fields.contains(&"sources".to_owned()) && fields.contains(&"rules".to_owned()));
    }

    #[test]
    fn hmac_needs_headers_a_secret_and_known_algorithms() {
        let env = json!({"source_type": "env", "config": {"var": "K"}});
        let body = json!({
            "timestamp_format": "unix_seconds", "signature_algorithm": "sha256",
            "signature_key_encoding": "raw", "signature_output_encoding": "hex",
            "signature_message": "{{ .Body }}",
            "headers": [{"name": "X-Sig", "value": "{{ .Signature }}"}],
            "credentials": {"secret": env}, "rules": [{"host": "api.test"}],
        });
        assert!(draft(Kind::Hmac, body.clone()).errors.is_empty());
        let mut bad = body;
        bad["signature_algorithm"] = json!("md5");
        bad["credentials"] = json!({});
        let fields = fields(&draft(Kind::Hmac, bad));
        assert!(fields.contains(&"signature_algorithm".to_owned()));
        assert!(fields.contains(&"sources".to_owned()));
    }

    #[test]
    fn gcp_auth_takes_a_keyfile_or_a_provider() {
        let keyfile = json!({"source_type": "env", "config": {"var": "K"}});
        let scopes = json!(["https://www.googleapis.com/auth/cloud-platform"]);
        let ok = draft(Kind::GcpAuth, json!({"keyfile": keyfile, "scopes": scopes}));
        assert!(ok.errors.is_empty(), "{:?}", fields(&ok));
        let wi = draft(
            Kind::GcpAuth,
            json!({"credentials_provider": {"type": "workload_identity"}, "scopes": scopes}),
        );
        assert!(wi.errors.is_empty(), "{:?}", fields(&wi));
        let both = draft(
            Kind::GcpAuth,
            json!({"keyfile": keyfile, "credentials_provider": {"type": "workload_identity"}, "scopes": scopes}),
        );
        assert!(fields(&both).contains(&"base".to_owned()));
        let bad_provider = draft(
            Kind::GcpAuth,
            json!({"credentials_provider": {"type": "nope"}, "scopes": scopes}),
        );
        assert!(fields(&bad_provider).contains(&"credentials_provider".to_owned()));
        let no_scopes = draft(Kind::GcpAuth, json!({"keyfile": keyfile}));
        assert!(fields(&no_scopes).contains(&"scopes".to_owned()));
    }

    #[test]
    fn gcp_id_token_normalizes_and_checks_its_header() {
        let body = |header: Value| {
            json!({"audience": "https://svc.run.app", "header": header,
                   "keyfile": {"source_type": "env", "config": {"var": "K"}},
                   "rules": [{"host": "svc.run.app"}]})
        };
        let ok = draft(
            Kind::GcpIdToken,
            body(json!(" X-Serverless-Authorization ")),
        );
        assert!(ok.errors.is_empty(), "{:?}", fields(&ok));
        assert_eq!(ok.columns["header"], "x-serverless-authorization");
        assert_eq!(
            draft(Kind::GcpIdToken, body(json!(""))).columns["header"],
            Value::Null
        );
        assert!(
            fields(&draft(Kind::GcpIdToken, body(json!("x-other")))).contains(&"header".to_owned())
        );
    }

    #[test]
    fn nested_errors_are_keyed_by_where_they_came_from() {
        let draft = draft(
            Kind::GcpIdToken,
            json!({"audience": "a", "keyfile": {"source_type": "env", "config": {}},
                   "rules": [{"host": "ok.test"}, {"host": "x", "cidr": "10.0.0.0/8"}]}),
        );
        let fields = fields(&draft);
        assert!(fields.contains(&"keyfile.config".to_owned()), "{fields:?}");
        assert!(fields.contains(&"rules[1].base".to_owned()), "{fields:?}");
    }
}
