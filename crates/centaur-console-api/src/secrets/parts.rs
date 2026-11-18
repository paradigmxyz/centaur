//! What a secret owns besides its own row: credential sources
//! (`secret_sources`) and request rules (`request_rules`). Request shapes,
//! validation, and the plain SQL that reads and replaces them.

use centaur_console_db::{Ciphertext, Encryptor};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use super::kind::Kind;
use crate::{ApiError, FieldErrors, support::cidr};

/// `role_kind` of a source that fills a named credential field.
pub const CREDENTIAL_FIELD: &str = "credential_field";
/// `role_kind` of a source that fills a token endpoint header.
pub const ENDPOINT_HEADER: &str = "endpoint_header";

pub const SOURCE_TYPES: &[&str] = &[
    "env",
    "aws_sm",
    "aws_ssm",
    "1password",
    "1password_connect",
    "control_plane",
    "token_broker",
];

/// Validation messages by field, in the order they were found.
#[derive(Debug, Default)]
pub struct Errors(Vec<(String, String)>);

impl Errors {
    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.0.push((field.to_owned(), message.into()));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn has(&self, field: &str) -> bool {
        self.0.iter().any(|(name, _)| name == field)
    }

    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(name, _)| name.as_str())
    }

    /// A 422 with every message, or `Ok` when there are none.
    pub fn finish(self) -> Result<(), ApiError> {
        let mut errors = FieldErrors::new();
        for (field, message) in self.0 {
            errors.add(&field, message);
        }
        errors.finish()
    }
}

pub fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

pub fn blank_str(value: Option<&str>) -> bool {
    value.is_none_or(|text| text.trim().is_empty())
}

// --- sources -------------------------------------------------------------

/// A source as a request carries it.
#[derive(Deserialize)]
pub struct SourceIn {
    pub source_type: Option<String>,
    pub secret: Option<String>,
    pub config: Option<Value>,
}

/// A credential source. `Debug` hides the inline value.
#[derive(Clone, PartialEq)]
pub struct Source {
    /// The credential field or header name, for kinds keyed by role.
    pub role: Option<String>,
    pub role_kind: Option<&'static str>,
    pub source_type: String,
    pub config: Value,
    /// The inline value of a `control_plane` source (plaintext).
    pub secret: Option<String>,
    pub broker_credential_id: Option<i64>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("role", &self.role)
            .field("role_kind", &self.role_kind)
            .field("source_type", &self.source_type)
            .field("config", &self.config)
            .field("secret", &centaur_console_db::redact::option(&self.secret))
            .field("broker_credential_id", &self.broker_credential_id)
            .finish()
    }
}

impl Source {
    pub fn from_input(
        input: SourceIn,
        role: Option<String>,
        role_kind: Option<&'static str>,
    ) -> Self {
        Self {
            role,
            role_kind,
            source_type: input.source_type.unwrap_or_default(),
            config: input.config.unwrap_or_else(|| json!({})),
            secret: input.secret,
            broker_credential_id: None,
        }
    }

    pub fn inline(&self) -> bool {
        self.source_type == "control_plane"
    }

    /// The `credential_id` a token broker source names.
    fn broker_reference(&self) -> Option<String> {
        if self.source_type != "token_broker" {
            return None;
        }
        match self.config.get("credential_id")? {
            Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }

    /// Links a token broker source to the broker credential it names (by
    /// oid or foreign id); unknown references stay unlinked and fail
    /// validation.
    pub async fn resolve_broker(&mut self, conn: &mut PgConnection) -> Result<(), ApiError> {
        self.broker_credential_id = None;
        let Some(reference) = self.broker_reference() else {
            return Ok(());
        };
        self.broker_credential_id = match centaur_console_db::decode_oid("bcr", &reference) {
            Some(id) => {
                sqlx::query_scalar("SELECT id FROM broker_credentials WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *conn)
                    .await?
            }
            None => {
                sqlx::query_scalar("SELECT id FROM broker_credentials WHERE foreign_id = $1")
                    .bind(&reference)
                    .fetch_optional(&mut *conn)
                    .await?
            }
        };
        Ok(())
    }

    /// The source as the API renders it: never the inline value.
    pub fn payload(&self) -> Value {
        json!({"source_type": self.source_type, "config": self.config})
    }
}

/// `(required, optional)` config keys of a source type.
fn config_keys(source_type: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match source_type {
        "env" => (&["var"], &[]),
        "aws_sm" => (&["secret_id"], &["region"]),
        "aws_ssm" => (&["name"], &["region", "with_decryption"]),
        "1password" => (&["secret_ref"], &["token_env"]),
        "1password_connect" => (&["secret_ref"], &["host_env", "token_env"]),
        "control_plane" => (&[], &[]),
        "token_broker" => (&["credential_id"], &[]),
        _ => return None,
    })
}

/// Keys every source type accepts.
const COMMON_CONFIG_KEYS: &[&str] = &["json_key", "ttl"];

/// Validates a source; its errors are recorded under `{prefix}.{field}`.
pub fn validate_source(source: &Source, prefix: &str, errors: &mut Errors) {
    let mut add = |field: &str, message: String| errors.add(&format!("{prefix}.{field}"), message);
    if source.source_type.trim().is_empty() {
        add("source_type", "can't be blank".into());
    } else if !SOURCE_TYPES.contains(&source.source_type.as_str()) {
        add("source_type", "is not included in the list".into());
    }
    match (&source.config, config_keys(&source.source_type)) {
        (Value::Object(config), Some((required, optional))) => {
            for key in required {
                if !config.contains_key(*key) {
                    add(
                        "config",
                        format!(
                            "is missing required key \"{key}\" for source_type \"{}\"",
                            source.source_type
                        ),
                    );
                }
            }
            for key in config.keys() {
                let known = [required, optional, COMMON_CONFIG_KEYS]
                    .iter()
                    .any(|keys| keys.contains(&key.as_str()));
                if !known {
                    add(
                        "config",
                        format!(
                            "has unknown key \"{key}\" for source_type \"{}\"",
                            source.source_type
                        ),
                    );
                }
            }
        }
        (Value::Object(_), None) => {}
        _ => add("config", "must be a hash".into()),
    }
    let has_secret = !blank_str(source.secret.as_deref());
    if source.inline() && !has_secret {
        add(
            "secret",
            "can't be blank for source_type \"control_plane\"".into(),
        );
    } else if !source.inline() && has_secret {
        add(
            "secret",
            "is only allowed for source_type \"control_plane\"".into(),
        );
    }
    if source.broker_reference().is_some() && source.broker_credential_id.is_none() {
        add(
            "config",
            "credential_id does not reference an existing broker credential".into(),
        );
    }
}

/// A stored source row; the inline value is still encrypted.
#[derive(Clone, sqlx::FromRow)]
pub struct StoredSource {
    pub owner_id: i64,
    role: Option<String>,
    role_kind: Option<String>,
    pub source_type: String,
    pub config: Value,
    secret: Option<Ciphertext>,
    broker_credential_id: Option<i64>,
}

impl StoredSource {
    pub fn role_kind(&self) -> Option<&str> {
        self.role_kind.as_deref()
    }

    pub fn is_token_broker(&self) -> bool {
        self.source_type == "token_broker"
    }

    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    pub fn payload(&self) -> Value {
        json!({"source_type": self.source_type, "config": self.config})
    }

    /// Decrypts the inline value; only for the write paths that compare or
    /// retain it.
    pub fn decrypt(self, encryptor: &Encryptor) -> Result<Source, ApiError> {
        Ok(Source {
            role: self.role,
            role_kind: match self.role_kind.as_deref() {
                Some(CREDENTIAL_FIELD) => Some(CREDENTIAL_FIELD),
                Some(ENDPOINT_HEADER) => Some(ENDPOINT_HEADER),
                _ => None,
            },
            source_type: self.source_type,
            config: self.config,
            secret: encryptor.decrypt_optional(self.secret.as_ref())?,
            broker_credential_id: self.broker_credential_id,
        })
    }
}

/// The sources of the kind's rows `ids`, by role.
pub async fn load_sources(
    conn: &mut PgConnection,
    kind: Kind,
    ids: &[i64],
) -> Result<Vec<StoredSource>, ApiError> {
    let owner = kind.owner_column();
    Ok(sqlx::query_as(&format!(
        "SELECT {owner} AS owner_id, role, role_kind, source_type, config, secret, \
         broker_credential_id FROM secret_sources WHERE {owner} = ANY($1) \
         ORDER BY role_kind, role, id"
    ))
    .bind(ids)
    .fetch_all(conn)
    .await?)
}

/// Sources in the order comparisons use.
pub fn sorted(mut sources: Vec<Source>) -> Vec<Source> {
    sources.sort_by(|a, b| (a.role_kind, &a.role).cmp(&(b.role_kind, &b.role)));
    sources
}

/// Deletes the row's sources and inserts `sources`.
pub async fn replace_sources(
    conn: &mut PgConnection,
    kind: Kind,
    id: i64,
    sources: &[Source],
    encryptor: &Encryptor,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let owner = kind.owner_column();
    sqlx::query(&format!("DELETE FROM secret_sources WHERE {owner} = $1"))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for source in sources {
        let secret = encryptor.encrypt_optional(source.secret.as_deref())?;
        sqlx::query(&format!(
            "INSERT INTO secret_sources ({owner}, role, role_kind, source_type, config, secret, \
             broker_credential_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)"
        ))
        .bind(id)
        .bind(&source.role)
        .bind(source.role_kind)
        .bind(&source.source_type)
        .bind(&source.config)
        .bind(secret)
        .bind(source.broker_credential_id)
        .bind(now.naive_utc())
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

// --- request rules ---------------------------------------------------------

/// A request rule as a request carries it.
#[derive(Deserialize)]
pub struct RuleIn {
    pub host: Option<String>,
    pub cidr: Option<String>,
    pub http_methods: Option<Vec<String>>,
    pub paths: Option<Vec<String>>,
}

/// Where a secret may be used: a host or a CIDR, optionally narrowed by
/// method and path.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct Rule {
    pub host: Option<String>,
    pub cidr: Option<String>,
    #[sqlx(json)]
    pub http_methods: Vec<String>,
    #[sqlx(json)]
    pub paths: Vec<String>,
}

impl From<RuleIn> for Rule {
    fn from(rule: RuleIn) -> Self {
        Self {
            host: rule.host,
            cidr: rule.cidr,
            http_methods: rule.http_methods.unwrap_or_default(),
            paths: rule.paths.unwrap_or_default(),
        }
    }
}

impl Rule {
    pub fn host(host: &str) -> Self {
        Self {
            host: Some(host.to_owned()),
            cidr: None,
            http_methods: Vec::new(),
            paths: Vec::new(),
        }
    }

    pub fn payload(&self, position: usize) -> Value {
        json!({
            "host": self.host,
            "cidr": self.cidr,
            "position": position,
            "http_methods": self.http_methods,
            "paths": self.paths,
        })
    }
}

const HTTP_METHODS: &[&str] = &[
    "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "CONNECT",
];

/// Validates rule number `index`; errors go under `rules[{index}].{field}`.
pub fn validate_rule(rule: &Rule, index: usize, errors: &mut Errors) {
    let mut add =
        |field: &str, message: String| errors.add(&format!("rules[{index}].{field}"), message);
    let host = !blank_str(rule.host.as_deref());
    let has_cidr = !blank_str(rule.cidr.as_deref());
    if host && has_cidr {
        add("base", "host and cidr are mutually exclusive".into());
    } else if !host && !has_cidr {
        add("base", "either host or cidr must be present".into());
    }
    if has_cidr && !cidr::valid(rule.cidr.as_deref().unwrap_or_default()) {
        add("cidr", "is not a valid CIDR".into());
    }
    for method in &rule.http_methods {
        if method != "*" && !HTTP_METHODS.contains(&method.as_str()) {
            add(
                "http_methods",
                format!("\"{method}\" is not a supported HTTP method"),
            );
        }
    }
    for path in &rule.paths {
        if !path.starts_with('/') {
            add(
                "paths",
                format!("\"{path}\" must be a string starting with /"),
            );
        }
    }
}

/// A rule with the row that owns it.
#[derive(sqlx::FromRow)]
pub struct OwnedRule {
    pub owner_id: i64,
    #[sqlx(flatten)]
    pub rule: Rule,
}

/// The rules of the kind's rows `ids`, in position order.
pub async fn load_rules(
    conn: &mut PgConnection,
    kind: Kind,
    ids: &[i64],
) -> Result<Vec<OwnedRule>, ApiError> {
    if !kind.has_rules() {
        return Ok(Vec::new());
    }
    let owner = kind.owner_column();
    Ok(sqlx::query_as(&format!(
        "SELECT {owner} AS owner_id, host, cidr, http_methods, paths FROM request_rules \
         WHERE {owner} = ANY($1) ORDER BY position, id"
    ))
    .bind(ids)
    .fetch_all(conn)
    .await?)
}

/// Deletes the row's rules and inserts `rules` at their positions.
pub async fn replace_rules(
    conn: &mut PgConnection,
    kind: Kind,
    id: i64,
    rules: &[Rule],
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    if !kind.has_rules() {
        return Ok(());
    }
    let owner = kind.owner_column();
    sqlx::query(&format!("DELETE FROM request_rules WHERE {owner} = $1"))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for (position, rule) in rules.iter().enumerate() {
        sqlx::query(&format!(
            "INSERT INTO request_rules ({owner}, host, cidr, http_methods, paths, position, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)"
        ))
        .bind(id)
        .bind(&rule.host)
        .bind(&rule.cidr)
        .bind(json!(rule.http_methods))
        .bind(json!(rule.paths))
        .bind(position as i32)
        .bind(now.naive_utc())
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(source_type: &str, config: Value) -> Source {
        Source {
            role: None,
            role_kind: None,
            source_type: source_type.into(),
            config,
            secret: None,
            broker_credential_id: None,
        }
    }

    fn errors_of(source: &Source) -> Errors {
        let mut errors = Errors::default();
        validate_source(source, "source", &mut errors);
        errors
    }

    #[test]
    fn sources_validate_their_config_and_secret() {
        assert!(errors_of(&source("env", json!({"var": "X", "ttl": "5m"}))).is_empty());
        assert!(!errors_of(&source("env", json!({}))).is_empty());
        assert!(!errors_of(&source("env", json!({"var": "X", "bogus": 1}))).is_empty());
        assert!(!errors_of(&source("vault", json!({}))).is_empty());
        assert!(!errors_of(&source("env", json!("nope"))).is_empty());
        // inline needs a value; the others must not carry one.
        assert!(!errors_of(&source("control_plane", json!({}))).is_empty());
        let mut inline = source("control_plane", json!({}));
        inline.secret = Some("v".into());
        assert!(errors_of(&inline).is_empty());
        let mut env = source("env", json!({"var": "X"}));
        env.secret = Some("v".into());
        assert!(!errors_of(&env).is_empty());
    }

    #[test]
    fn debug_hides_the_inline_value() {
        let mut inline = source("control_plane", json!({}));
        inline.secret = Some("sentinel-value".into());
        assert!(!format!("{inline:?}").contains("sentinel-value"));
    }

    #[test]
    fn rules_need_a_host_or_a_cidr() {
        let check = |rule: &Rule| {
            let mut errors = Errors::default();
            validate_rule(rule, 0, &mut errors);
            errors.is_empty()
        };
        assert!(check(&Rule::host("api.example.test")));
        let mut cidr_rule = Rule::host("x");
        cidr_rule.host = None;
        cidr_rule.cidr = Some("10.0.0.0/8".into());
        assert!(check(&cidr_rule));
        cidr_rule.cidr = Some("nope".into());
        assert!(!check(&cidr_rule));
        let mut both = Rule::host("x");
        both.cidr = Some("10.0.0.0/8".into());
        assert!(!check(&both));
        let mut methods = Rule::host("x");
        methods.http_methods = vec!["GET".into(), "*".into()];
        methods.paths = vec!["/v1".into()];
        assert!(check(&methods));
        methods.http_methods = vec!["get".into()];
        assert!(!check(&methods));
        methods.http_methods = vec![];
        methods.paths = vec!["v1".into()];
        assert!(!check(&methods));
    }
}
