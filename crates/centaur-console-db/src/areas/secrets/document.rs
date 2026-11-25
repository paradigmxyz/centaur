//! Request-document parsing and model validation for the secret kinds the
//! console and its APIs write.
//!
//! One document writer covers every secret kind the console and its APIs write:
//! a small set of scalar fields plus `labels` / kind-specific filters, with the
//! nested source and rules built in memory and every record validated before the
//! database is touched. This module builds that document in Rust and validates
//! the whole document before any write. Messages are plain Console text, not
//! copies of a validator's or JSON Schema's wording; nested errors are reported
//! under path keys (`source.config`, `rules[0].cidr`) so the SPA can show them
//! beside the field that failed.
//!
//! [`SecretDocumentPayload`] is the typed wire document: every field is the
//! JSON type the column accepts, so a numeric `name` or a `labels` array is a
//! 422 from the request extractor instead of an implicit cast. The nested schema
//! checks below stay because they are real model validations.

use std::collections::HashSet;
use std::fmt;

use serde::{
    Deserialize,
    de::{self, Visitor},
};
use serde_json::{Map, Number, Value, json};

use crate::validation::ValidateFields;
use crate::{Error, Result};

use super::write::{GcpAuthParent, GcpIdTokenParent, ParentRow, PgDsnParent, StaticParent};
use super::{SecretFields, SecretKind, SecretRow};

/// A write failure the API layer turns into the matching HTTP error.
#[derive(Debug)]
pub enum WriteError {
    /// A missing row; the message is rendered verbatim.
    NotFound(String),
    /// An invalid record; `details` already has the exact keys.
    Validation(Value),
    /// A malformed request (e.g. `data` missing).
    BadRequest(String),
    /// Anything unexpected.
    Internal(Error),
}

impl From<sqlx::Error> for WriteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(Error::from(error))
    }
}

impl From<Error> for WriteError {
    fn from(error: Error) -> Self {
        Self::Internal(error)
    }
}

/// Ordered validation errors. The secret validators were the first users of
/// this shape; it is now the shared [`crate::FieldErrors`], so the console and
/// the data layer render the same `details` value.
pub use crate::errors::FieldErrors as Errors;

/// Whether a PATCH that omits an inline source's `secret` retains the stored
/// value and whether an explicit `foreign_id: null` clears it.
#[derive(Clone, Copy, Debug, Default)]
pub struct WriteOptions {
    pub retain_inline_secret: bool,
    pub explicit_foreign_id_clear: bool,
}

/// How a write addresses its record.
#[derive(Clone, Debug)]
pub enum WriteTarget {
    New,
    /// The opaque id from the URL (must exist for a write addressed by oid).
    Oid(String),
    /// Any non-oid identifier; created when no secret has that foreign id.
    ForeignId(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ForeignIdValue {
    /// The column is left untouched (an omitted or blank value on update).
    Omitted,
    Set(Option<String>),
}

/// A JSON scalar: the only values Console's permitted `labels`, source
/// `config`, and settings `value_from` hashes keep. Nested arrays and objects
/// are a request error instead of being silently dropped.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
}

impl Scalar {
    pub fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(value),
            Self::Number(value) => Value::Number(value),
            Self::String(value) => Value::String(value),
        }
    }
}

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct ScalarVisitor;

        impl<'de> Visitor<'de> for ScalarVisitor {
            type Value = Scalar;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a scalar JSON value")
            }

            fn visit_unit<E: de::Error>(self) -> std::result::Result<Scalar, E> {
                Ok(Scalar::Null)
            }

            fn visit_none<E: de::Error>(self) -> std::result::Result<Scalar, E> {
                Ok(Scalar::Null)
            }

            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> std::result::Result<Scalar, D::Error> {
                Scalar::deserialize(deserializer)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Scalar, E> {
                Ok(Scalar::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Scalar, E> {
                Ok(Scalar::Number(value.into()))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Scalar, E> {
                Ok(Scalar::Number(value.into()))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Scalar, E> {
                Number::from_f64(value)
                    .map(Scalar::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Scalar, E> {
                Ok(Scalar::String(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<Scalar, E> {
                Ok(Scalar::String(value))
            }
        }

        deserializer.deserialize_any(ScalarVisitor)
    }
}

/// A permitted parameter value: a scalar, or an array of scalars. `labels`,
/// source `config`, and settings `value_from` hashes keep exactly that shape;
/// nested hashes are a request error instead of being silently dropped.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Scalar(Scalar),
    List(Vec<Scalar>),
}

impl ParamValue {
    pub fn into_value(self) -> Value {
        match self {
            Self::Scalar(value) => value.into_value(),
            Self::List(values) => {
                Value::Array(values.into_iter().map(Scalar::into_value).collect())
            }
        }
    }
}

impl<'de> Deserialize<'de> for ParamValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Scalar(Scalar),
            List(Vec<Scalar>),
        }

        Ok(match Either::deserialize(deserializer)? {
            Either::Scalar(value) => Self::Scalar(value),
            Either::List(values) => Self::List(values),
        })
    }
}

/// A hash of permitted JSON parameter values (`labels`, source `config`,
/// `value_from`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScalarMap(pub Map<String, Value>);

impl ScalarMap {
    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

impl<'de> Deserialize<'de> for ScalarMap {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let values = std::collections::BTreeMap::<String, ParamValue>::deserialize(deserializer)?;
        Ok(Self(
            values
                .into_iter()
                .map(|(key, value)| (key, value.into_value()))
                .collect(),
        ))
    }
}

/// One nested source (`source`, `dsn`, or `keyfile`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourcePayload {
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub config: Option<ScalarMap>,
}

impl SourcePayload {
    fn into_document(self) -> SourceDocument {
        SourceDocument {
            source_type: self.source_type,
            secret: self.secret,
            config: self
                .config
                .map(ScalarMap::into_value)
                .unwrap_or_else(empty_object),
        }
    }
}

/// One `pg_dsn` session setting.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SettingPayload {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub value: Option<ParamValue>,
    #[serde(default)]
    pub value_from: Option<ScalarMap>,
}

impl SettingPayload {
    fn into_value(self) -> Value {
        let mut setting = Map::new();
        if let Some(name) = self.name {
            setting.insert("name".to_owned(), Value::String(name));
        }
        if let Some(value) = self.value {
            setting.insert("value".to_owned(), value.into_value());
        }
        if let Some(value_from) = self.value_from {
            setting.insert("value_from".to_owned(), value_from.into_value());
        }
        Value::Object(setting)
    }
}

/// One request rule. `http_methods`/`paths` keep the scalar values the model
/// validates, so a non-scalar element is reported instead of dropped.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RulePayload {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub cidr: Option<String>,
    #[serde(default)]
    pub http_methods: Option<Vec<Scalar>>,
    #[serde(default)]
    pub paths: Option<Vec<Scalar>>,
}

impl RulePayload {
    fn into_document(self, position: i32) -> RuleDocument {
        RuleDocument {
            host: self.host,
            cidr: self.cidr,
            position,
            http_methods: scalar_array(self.http_methods),
            paths: scalar_array(self.paths),
        }
    }
}

/// The typed `data` object of a secret create/update.
///
/// Every field is an `Option<Option<T>>`: the outer `None` means the key was
/// omitted and an explicit `null` is `Some(None)`, which clears the stored
/// value.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SecretDocumentPayload {
    #[serde(default, deserialize_with = "explicit_null")]
    pub foreign_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub name: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub labels: Option<Option<ScalarMap>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub kind: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub inject_config: Option<Option<ScalarMap>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub replace_config: Option<Option<ScalarMap>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub database: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub role: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub settings: Option<Option<Vec<SettingPayload>>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub credentials_provider: Option<Option<ScalarMap>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub subject: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub scopes: Option<Option<Vec<Scalar>>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub audience: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub header: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub source: Option<Option<SourcePayload>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub dsn: Option<Option<SourcePayload>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub keyfile: Option<Option<SourcePayload>>,
    #[serde(default, deserialize_with = "explicit_null")]
    pub rules: Option<Option<Vec<RulePayload>>>,
}

impl SecretDocumentPayload {
    /// The nested source a kind reads (`source`, `dsn`, or `keyfile`).
    pub fn source_for(&self, kind: SecretKind) -> Option<&SourcePayload> {
        let field = match kind {
            SecretKind::PgDsn => &self.dsn,
            SecretKind::GcpAuth | SecretKind::GcpIdToken => &self.keyfile,
            _ => &self.source,
        };
        field.as_ref().and_then(Option::as_ref)
    }

    fn provided<T>(field: &Option<Option<T>>) -> Option<&T> {
        field.as_ref().and_then(Option::as_ref)
    }
}

fn explicit_null<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

fn scalar_array(values: Option<Vec<Scalar>>) -> Value {
    Value::Array(
        values
            .unwrap_or_default()
            .into_iter()
            .map(Scalar::into_value)
            .collect(),
    )
}

fn optional_object(field: &Option<Option<ScalarMap>>) -> Value {
    SecretDocumentPayload::provided(field)
        .cloned()
        .map(ScalarMap::into_value)
        .unwrap_or(Value::Null)
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Clone, Debug, PartialEq, garde::Validate)]
#[garde(context(SourceValidation))]
pub struct SourceDocument {
    #[garde(custom(source_type_rule))]
    pub source_type: Option<String>,
    #[garde(custom(source_secret_rule))]
    pub secret: Option<String>,
    #[garde(custom(source_config_rule))]
    pub config: Value,
}

#[derive(Clone, Debug, PartialEq, garde::Validate)]
pub struct RuleDocument {
    #[garde(skip)]
    pub host: Option<String>,
    #[garde(custom(rule_cidr))]
    pub cidr: Option<String>,
    #[garde(skip)]
    pub position: i32,
    #[garde(custom(rule_http_methods))]
    pub http_methods: Value,
    #[garde(custom(rule_paths))]
    pub paths: Value,
}

/// Kind-specific scalar columns. The active variant's fields carry their own
/// garde rules, so they report under their plain attribute names.
#[derive(Clone, Debug, PartialEq, garde::Validate)]
#[garde(context(DocumentValidation))]
pub enum DocumentFields {
    Static {
        #[garde(custom(static_kind))]
        kind: Option<String>,
        #[garde(custom(inject_config_rule))]
        inject_config: Value,
        #[garde(custom(replace_config_rule))]
        replace_config: Value,
    },
    PgDsn {
        #[garde(custom(pg_database_present), custom(pg_database_matches_dsn))]
        database: Option<String>,
        #[garde(skip)]
        role: Option<String>,
        #[garde(custom(pg_settings_rule))]
        settings: Value,
    },
    GcpAuth {
        #[garde(custom(gcp_credentials_provider_rule))]
        credentials_provider: Value,
        #[garde(custom(gcp_subject_rule))]
        subject: Option<String>,
        #[garde(custom(gcp_scopes_rule))]
        scopes: Value,
    },
    GcpIdToken {
        #[garde(custom(gcp_id_token_audience_rule))]
        audience: Option<String>,
        #[garde(custom(gcp_id_token_header_rule))]
        header: Option<String>,
    },
}

/// The merged facts the document's field rules need: the kind being written,
/// the two database answers, and values computed from sibling fields.
#[derive(Clone, Debug)]
pub struct DocumentValidation {
    pub kind: SecretKind,
    pub foreign_id_taken: bool,
    /// The merged source, for the inline DSN check on a PgDsn secret.
    pub source: Option<SourceDocument>,
    /// Whether a GcpAuth document also carries a credentials provider.
    pub has_provider: bool,
}

#[derive(Clone, Debug, PartialEq, garde::Validate)]
#[garde(context(DocumentValidation))]
pub struct Document {
    #[garde(
        custom(foreign_id_present),
        custom(foreign_id_taken),
        custom(foreign_id_reserved_prefix),
        custom(foreign_id_url_safe)
    )]
    pub foreign_id: ForeignIdValue,
    #[garde(skip)]
    pub name: Option<String>,
    #[garde(skip)]
    pub description: Option<String>,
    #[garde(skip)]
    pub labels: Value,
    #[garde(skip)]
    pub source: Option<SourceDocument>,
    /// GcpIdToken requires a rule; each rule's fields are validated separately.
    #[garde(custom(gcp_id_token_rules))]
    pub rules: Vec<RuleDocument>,
    /// The active kind's fields, validated below with their own attributes.
    #[garde(skip)]
    pub fields: DocumentFields,
}

impl Document {
    /// Builds the replacement document from a typed `data` payload for `kind`.
    /// `existing` and `existing_secret` are the row-locked record and its
    /// decrypted inline value, used only to retain an omitted secret and a
    /// cleared-or-omitted foreign id.
    pub fn from_payload(
        kind: SecretKind,
        payload: &SecretDocumentPayload,
        existing: Option<&SecretRow>,
        existing_secret: Option<&str>,
        options: WriteOptions,
    ) -> std::result::Result<Self, WriteError> {
        let existing_foreign = existing
            .and_then(|row| row.foreign_id.as_deref())
            .filter(|value| !value.trim().is_empty());
        let foreign_id = match &payload.foreign_id {
            None if existing_foreign.is_some() => ForeignIdValue::Omitted,
            None => ForeignIdValue::Set(None),
            Some(None) if existing_foreign.is_some() && !options.explicit_foreign_id_clear => {
                ForeignIdValue::Omitted
            }
            Some(None) => ForeignIdValue::Set(None),
            Some(Some(value)) if value.trim().is_empty() && existing_foreign.is_some() => {
                ForeignIdValue::Omitted
            }
            Some(Some(value)) => ForeignIdValue::Set(Some(value.clone())),
        };
        let name = SecretDocumentPayload::provided(&payload.name).cloned();
        let description = SecretDocumentPayload::provided(&payload.description).cloned();
        let labels = SecretDocumentPayload::provided(&payload.labels)
            .cloned()
            .map(ScalarMap::into_value)
            .unwrap_or_else(empty_object);

        let rules_key = matches!(
            kind,
            SecretKind::Static | SecretKind::GcpAuth | SecretKind::GcpIdToken
        );
        // An empty `{...}` hash is absent; any key (even with a null value)
        // builds a source that validation checks.
        let source = payload.source_for(kind).filter(|source| {
            source.config.is_some() || source.source_type.is_some() || source.secret.is_some()
        });
        let source = source.map(|source| source.clone().into_document());
        // A PATCH may omit `secret` for an unchanged inline source; the caller
        // supplies the stored plaintext after taking the row lock.
        let source = source.map(|mut source| {
            if options.retain_inline_secret
                && source.source_type.as_deref() == Some("control_plane")
                && blank(source.secret.as_deref())
                && existing.and_then(|row| row.source()).is_some_and(|stored| {
                    stored.source_type == "control_plane" && stored.secret.is_some()
                })
            {
                source.secret = existing_secret.map(str::to_owned);
            }
            source
        });

        let rules = match SecretDocumentPayload::provided(&payload.rules) {
            Some(rules) if rules_key => rules
                .iter()
                .cloned()
                .enumerate()
                .map(|(position, rule)| {
                    rule.into_document(i32::try_from(position).unwrap_or(i32::MAX))
                })
                .collect(),
            _ => Vec::new(),
        };

        let fields = match kind {
            SecretKind::Static => {
                let kind = match &payload.kind {
                    None => Some(
                        existing
                            .and_then(|row| match &row.fields {
                                SecretFields::Static { kind, .. } => Some(kind.clone()),
                                _ => None,
                            })
                            .unwrap_or_else(|| "custom".to_owned()),
                    ),
                    Some(None) => None,
                    Some(Some(value)) => Some(value.clone()),
                };
                DocumentFields::Static {
                    kind,
                    inject_config: optional_object(&payload.inject_config),
                    replace_config: optional_object(&payload.replace_config),
                }
            }
            SecretKind::PgDsn => DocumentFields::PgDsn {
                database: SecretDocumentPayload::provided(&payload.database).cloned(),
                role: SecretDocumentPayload::provided(&payload.role).cloned(),
                settings: Value::Array(
                    SecretDocumentPayload::provided(&payload.settings)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(SettingPayload::into_value)
                        .collect(),
                ),
            },
            SecretKind::GcpAuth => DocumentFields::GcpAuth {
                credentials_provider: optional_object(&payload.credentials_provider),
                subject: SecretDocumentPayload::provided(&payload.subject).cloned(),
                scopes: scalar_array(SecretDocumentPayload::provided(&payload.scopes).cloned()),
            },
            SecretKind::GcpIdToken => DocumentFields::GcpIdToken {
                audience: SecretDocumentPayload::provided(&payload.audience).cloned(),
                header: normalize_header(SecretDocumentPayload::provided(&payload.header).cloned()),
            },
            _ => {
                return Err(WriteError::BadRequest(format!(
                    "{} secrets are not writable through this API",
                    kind.label()
                )));
            }
        };

        let mut document = Self {
            foreign_id,
            name,
            description,
            labels,
            source,
            rules,
            fields,
        };
        document.apply_kind_defaults();
        Ok(document)
    }

    /// Applies the credential profile's defaults (only github_token has one):
    /// canonical replace config and the default host rules.
    fn apply_kind_defaults(&mut self) {
        let DocumentFields::Static {
            kind,
            inject_config,
            replace_config,
        } = &mut self.fields
        else {
            return;
        };
        if kind.as_deref() != Some("github_token") {
            return;
        }
        if !ruby_present(inject_config)
            && (ruby_blank(replace_config)
                || *replace_config == canonical_replace_config()
                || *replace_config == canonical_replace_config_without_require())
        {
            *replace_config = canonical_replace_config();
        }
        if self.rules.is_empty() {
            self.rules = github_default_rules();
        }
    }

    /// The parent's validation errors. Cross-field rules and the nested-record
    /// presence checks stay here because they see the whole merged document.
    pub fn parent_errors(&self, kind: SecretKind, foreign_id_taken: bool) -> Errors {
        let has_provider = matches!(
            &self.fields,
            DocumentFields::GcpAuth {
                credentials_provider,
                ..
            } if ruby_present(credentials_provider)
        );
        let context = DocumentValidation {
            kind,
            foreign_id_taken,
            source: self.source.clone(),
            has_provider,
        };
        let mut errors = self
            .validate_fields_with(&context)
            .err()
            .unwrap_or_default();
        errors.extend(
            self.fields
                .validate_fields_with(&context)
                .err()
                .unwrap_or_default(),
        );
        match (&self.fields, kind) {
            (
                DocumentFields::Static {
                    kind: secret_kind,
                    inject_config,
                    replace_config,
                },
                SecretKind::Static,
            ) => validate_static_configuration(
                secret_kind.as_deref(),
                inject_config,
                replace_config,
                &self.rules,
                &mut errors,
            ),
            (DocumentFields::PgDsn { .. }, SecretKind::PgDsn) => {
                if self.source.is_none() {
                    errors.add("dsn_source", "can't be blank");
                }
            }
            (
                DocumentFields::GcpAuth {
                    credentials_provider,
                    ..
                },
                SecretKind::GcpAuth,
            ) => match (self.source.is_some(), ruby_present(credentials_provider)) {
                (false, false) => errors.add(
                    "base",
                    "must define one of keyfile (source) or credentials_provider",
                ),
                (true, true) => errors.add(
                    "base",
                    "keyfile and credentials_provider are mutually exclusive",
                ),
                _ => {}
            },
            (DocumentFields::GcpIdToken { .. }, SecretKind::GcpIdToken)
                if self.source.is_none() =>
            {
                errors.add("keyfile_source", "can't be blank");
            }
            _ => {}
        }
        errors
    }
}

fn foreign_id_present(value: &ForeignIdValue, context: &DocumentValidation) -> garde::Result {
    if context.kind == SecretKind::PgDsn && matches!(value, ForeignIdValue::Set(None)) {
        return Err(garde::Error::new("can't be blank"));
    }
    Ok(())
}

fn foreign_id_taken(value: &ForeignIdValue, context: &DocumentValidation) -> garde::Result {
    if context.foreign_id_taken && matches!(value, ForeignIdValue::Set(_)) {
        return Err(garde::Error::new("has already been taken"));
    }
    Ok(())
}

fn foreign_id_reserved_prefix(
    value: &ForeignIdValue,
    context: &DocumentValidation,
) -> garde::Result {
    let ForeignIdValue::Set(Some(foreign_id)) = value else {
        return Ok(());
    };
    let reserved = format!("{}_", context.kind.oid_prefix());
    if foreign_id.starts_with(&reserved) {
        return Err(garde::Error::new(format!(
            "must not start with {}, which is reserved for opaque ids",
            quote(&reserved)
        )));
    }
    Ok(())
}

fn foreign_id_url_safe(value: &ForeignIdValue, context: &DocumentValidation) -> garde::Result {
    let Some(foreign_id) = (match value {
        ForeignIdValue::Omitted => return Ok(()),
        ForeignIdValue::Set(foreign_id) => foreign_id.as_deref(),
    }) else {
        // PgDsnSecret validates presence and format without `allow_nil`, so a
        // nil value reports both errors. The other models allow_nil.
        return if context.kind == SecretKind::PgDsn {
            Err(garde::Error::new(URL_SAFE_MESSAGE))
        } else {
            Ok(())
        };
    };
    if !foreign_id.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '~')
    }) {
        return Err(garde::Error::new(URL_SAFE_MESSAGE));
    }
    Ok(())
}

const URL_SAFE_MESSAGE: &str = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)";

fn gcp_id_token_rules(value: &[RuleDocument], context: &DocumentValidation) -> garde::Result {
    if context.kind == SecretKind::GcpIdToken && value.is_empty() {
        return Err(garde::Error::new("must include at least one rule"));
    }
    Ok(())
}

/// `StaticSecret`'s cross-field rules: the inject/replace choice, the GitHub
/// placeholder, and the GitHub rule confinement.
fn validate_static_configuration(
    kind: Option<&str>,
    inject_config: &Value,
    replace_config: &Value,
    rules: &[RuleDocument],
    errors: &mut Errors,
) {
    let inject = ruby_present(inject_config);
    let replace = ruby_present(replace_config);
    if !inject && !replace {
        errors.add("base", "must define one of inject_config or replace_config");
    } else if inject && replace {
        errors.add(
            "base",
            "inject_config and replace_config are mutually exclusive",
        );
    }
    if kind == Some("github_token") {
        if !(ruby_blank(inject_config) && *replace_config == canonical_replace_config()) {
            errors.add(
                "base",
                "github_token credentials must use the canonical Authorization placeholder replacement",
            );
        }
        let allowed = ["api.github.com", "github.com", "api.githubcopilot.com"];
        let confined = !rules.is_empty()
            && rules.iter().all(|rule| {
                rule.host
                    .as_deref()
                    .is_some_and(|host| allowed.contains(&host))
                    && blank(rule.cidr.as_deref())
                    && ruby_blank(&rule.http_methods)
                    && ruby_blank(&rule.paths)
            });
        if !confined {
            errors.add(
                "rules",
                "github_token credentials must target only api.github.com, github.com, api.githubcopilot.com",
            );
        }
    }
}

fn static_kind(value: &Option<String>, _: &DocumentValidation) -> garde::Result {
    match value {
        Some(kind) if !kind.trim().is_empty() => {
            if matches!(kind.as_str(), "custom" | "github_token") {
                Ok(())
            } else {
                Err(garde::Error::new("is not included in the list"))
            }
        }
        _ => Err(garde::Error::new("can't be blank")),
    }
}

fn inject_config_rule(value: &Value, _: &DocumentValidation) -> garde::Result {
    if ruby_blank(value) {
        return Ok(());
    }
    let Some(map) = value.as_object() else {
        return Err(garde::Error::new("must be a hash"));
    };
    if let Some(header) = map.get("header") {
        match header {
            Value::String(text) if text.is_empty() => {
                return Err(garde::Error::new("header can't be blank"));
            }
            Value::String(_) => {}
            _ => return Err(garde::Error::new("header must be a string")),
        }
    }
    if let Some(formatter) = map.get("formatter")
        && !formatter.is_string()
    {
        return Err(garde::Error::new("formatter must be a string"));
    }
    if let Some(query) = map.get("query_param") {
        match query {
            Value::String(text) if text.is_empty() => {
                return Err(garde::Error::new("query_param can't be blank"));
            }
            Value::String(_) => {}
            _ => return Err(garde::Error::new("query_param must be a string")),
        }
    }
    if let Some(key) = map
        .keys()
        .find(|key| !matches!(key.as_str(), "header" | "formatter" | "query_param"))
    {
        return Err(garde::Error::new(format!(
            "contains unknown key {}",
            quote(key)
        )));
    }
    match (map.contains_key("header"), map.contains_key("query_param")) {
        (false, false) => Err(garde::Error::new(
            "must define one of header or query_param",
        )),
        (true, true) => Err(garde::Error::new(
            "header and query_param are mutually exclusive",
        )),
        _ => Ok(()),
    }
}

fn replace_config_rule(value: &Value, _: &DocumentValidation) -> garde::Result {
    if ruby_blank(value) {
        return Ok(());
    }
    let Some(map) = value.as_object() else {
        return Err(garde::Error::new("must be a hash"));
    };
    if let Some(proxy) = map.get("proxy_value") {
        match proxy {
            Value::String(text) if text.is_empty() => {
                return Err(garde::Error::new("proxy_value can't be blank"));
            }
            Value::String(_) => {}
            _ => return Err(garde::Error::new("proxy_value must be a string")),
        }
    }
    if let Some(headers) = map.get("match_headers") {
        match headers.as_array() {
            None => return Err(garde::Error::new("match_headers must be an array")),
            Some(values) if values.iter().any(|value| !value.is_string()) => {
                return Err(garde::Error::new("match_headers must all be strings"));
            }
            Some(_) => {}
        }
    }
    for key in ["match_body", "match_path", "match_query", "require"] {
        if let Some(value) = map.get(key)
            && !value.is_boolean()
        {
            return Err(garde::Error::new(format!("{key} must be a boolean")));
        }
    }
    if let Some(key) = map.keys().find(|key| {
        !matches!(
            key.as_str(),
            "proxy_value"
                | "match_headers"
                | "match_body"
                | "match_path"
                | "match_query"
                | "require"
        )
    }) {
        return Err(garde::Error::new(format!(
            "contains unknown key {}",
            quote(key)
        )));
    }
    if !map.contains_key("proxy_value") {
        return Err(garde::Error::new("proxy_value is required"));
    }
    Ok(())
}

fn pg_database_present(value: &Option<String>, _: &DocumentValidation) -> garde::Result {
    if blank(value.as_deref()) {
        return Err(garde::Error::new("can't be blank"));
    }
    Ok(())
}

fn pg_database_matches_dsn(value: &Option<String>, context: &DocumentValidation) -> garde::Result {
    let (Some(database), Some(source)) = (value.as_deref(), context.source.as_ref()) else {
        return Ok(());
    };
    if source.source_type.as_deref() != Some("control_plane") {
        return Ok(());
    }
    inline_dsn_error(database, source)
        .map(garde::Error::new)
        .map_or(Ok(()), Err)
}

fn pg_settings_rule(value: &Value, _: &DocumentValidation) -> garde::Result {
    let Some(values) = value.as_array() else {
        return Err(garde::Error::new("must be an array"));
    };
    let mut seen: HashSet<String> = HashSet::new();
    for (index, setting) in values.iter().enumerate() {
        if let Some(reason) = setting_error(setting, &mut seen) {
            return Err(garde::Error::new(format!("[{index}] {reason}")));
        }
    }
    Ok(())
}

fn gcp_credentials_provider_rule(value: &Value, _: &DocumentValidation) -> garde::Result {
    if ruby_blank(value) {
        return Ok(());
    }
    let Some(map) = value.as_object() else {
        return Err(garde::Error::new("must be a hash"));
    };
    if let Some(kind) = map.get("type")
        && kind != &Value::String("workload_identity".to_owned())
    {
        return Err(garde::Error::new("type must be \"workload_identity\""));
    }
    if let Some(key) = map.keys().find(|key| key.as_str() != "type") {
        return Err(garde::Error::new(format!(
            "contains unknown key {}",
            quote(key)
        )));
    }
    if !map.contains_key("type") {
        return Err(garde::Error::new("type is required"));
    }
    Ok(())
}

fn gcp_subject_rule(value: &Option<String>, context: &DocumentValidation) -> garde::Result {
    if !blank(value.as_deref()) && context.has_provider {
        return Err(garde::Error::new("is only allowed with a keyfile source"));
    }
    Ok(())
}

fn gcp_scopes_rule(value: &Value, _: &DocumentValidation) -> garde::Result {
    let Some(values) = value.as_array() else {
        return Err(garde::Error::new("must be an array"));
    };
    if values.is_empty() {
        return Err(garde::Error::new("must include at least one scope"));
    }
    if !values
        .iter()
        .all(|value| value.as_str().is_some_and(|text| !text.is_empty()))
    {
        return Err(garde::Error::new("must all be strings"));
    }
    Ok(())
}

fn gcp_id_token_audience_rule(value: &Option<String>, _: &DocumentValidation) -> garde::Result {
    if blank(value.as_deref()) {
        return Err(garde::Error::new("can't be blank"));
    }
    Ok(())
}

fn gcp_id_token_header_rule(value: &Option<String>, _: &DocumentValidation) -> garde::Result {
    match value.as_deref() {
        Some("authorization" | "x-serverless-authorization") | None => Ok(()),
        Some(text) if text.trim().is_empty() => Ok(()),
        Some(_) => Err(garde::Error::new(
            "must be one of authorization, x-serverless-authorization",
        )),
    }
}

fn setting_error(setting: &Value, seen: &mut HashSet<String>) -> Option<String> {
    let Some(setting) = setting.as_object() else {
        return Some("must be an object".to_owned());
    };
    let name = setting.get("name").map(value_to_string).unwrap_or_default();
    if name.is_empty() {
        return Some("name is required".to_owned());
    }
    if !guc_name_valid(&name) {
        return Some(format!("invalid setting name {}", quote(&name)));
    }
    let lower = name.to_lowercase();
    if matches!(lower.as_str(), "role" | "session_authorization") {
        return Some(format!(
            "{} is managed by the proxy; use the role field",
            quote(&name)
        ));
    }
    if !seen.insert(lower) {
        return Some(format!("duplicate setting {}", quote(&name)));
    }
    value_from_error(setting)
}

fn guc_name_valid(name: &str) -> bool {
    fn identifier(value: &str) -> bool {
        let mut chars = value.chars();
        match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => return false,
        }
        chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
    }
    let mut parts = name.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(first), None, None) => identifier(first),
        (Some(first), Some(second), None) => identifier(first) && identifier(second),
        _ => false,
    }
}

fn value_from_error(setting: &Map<String, Value>) -> Option<String> {
    let reference = match setting.get("value_from") {
        None | Some(Value::Null) => return None,
        Some(value) => value,
    };
    if !setting.get("value").is_none_or(Value::is_null) {
        return Some("value and value_from are mutually exclusive".to_owned());
    }
    let Some(reference) = reference.as_object() else {
        return Some("value_from must be an object".to_owned());
    };
    if reference.len() != 1 {
        return Some(
            "value_from must have exactly one of principal_label or principal_field or proxy_label"
                .to_owned(),
        );
    }
    let (key, value) = reference.iter().next().expect("one key");
    match key.as_str() {
        "principal_label" => {
            if value_to_string(value).is_empty() {
                Some("principal_label can't be blank".to_owned())
            } else {
                None
            }
        }
        "proxy_label" => {
            if value_to_string(value).is_empty() {
                Some("proxy_label can't be blank".to_owned())
            } else {
                None
            }
        }
        "principal_field" => {
            let field = value_to_string(value);
            if PRINCIPAL_FIELDS.contains(&field.as_str()) {
                None
            } else {
                Some(format!(
                    "unknown principal_field {} (one of: {})",
                    quote(&field),
                    PRINCIPAL_FIELDS.join(", ")
                ))
            }
        }
        _ => Some(
            "value_from must have exactly one of principal_label or principal_field or proxy_label"
                .to_owned(),
        ),
    }
}

const PRINCIPAL_FIELDS: [&str; 11] = [
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

fn inline_dsn_error(database: &str, source: &SourceDocument) -> Option<String> {
    let secret = source.secret.as_deref()?;
    if secret.trim().is_empty() {
        return None;
    }
    let value = match source.config.get("json_key").map(value_to_string) {
        Some(key) if !key.is_empty() => match serde_json::from_str::<Value>(secret) {
            Ok(Value::Object(map)) => match map.get(&key) {
                Some(Value::String(text)) => text.clone(),
                Some(other) => value_to_string(other),
                None => return None,
            },
            _ => return None,
        },
        _ => secret.to_owned(),
    };
    match parse_dsn(&value) {
        Some(Some(named)) if named != database => {
            Some(format!("must match the DSN database ({})", quote(&named)))
        }
        Some(None) => Some(format!(
            "DSN names no database; it must match {}",
            quote(database)
        )),
        _ => None,
    }
}

/// A minimal `conninfo_parse`: returns `None` when malformed (the proxy stays
/// authoritative), `Some(None)` when it has no dbname, and `Some(Some(db))`.
fn parse_dsn(value: &str) -> Option<Option<String>> {
    let trimmed = value.trim();
    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        let url = url::Url::parse(trimmed).ok()?;
        let database = url.path().split('/').nth(1).unwrap_or_default();
        let database = percent_encoding::percent_decode_str(database).decode_utf8_lossy();
        return Some((!database.is_empty()).then(|| database.into_owned()));
    }
    let mut database = None;
    let mut chars = trimmed.chars().peekable();
    while chars.peek().is_some() {
        while chars
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            chars.next();
        }
        let mut key = String::new();
        while let Some(&character) = chars.peek() {
            if character == '=' || character.is_whitespace() {
                break;
            }
            key.push(character);
            chars.next();
        }
        if key.is_empty() {
            if chars.peek().is_none() {
                break;
            }
            return None;
        }
        if chars.next() != Some('=') {
            return None;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'\'') {
            chars.next();
            loop {
                match chars.next() {
                    Some('\\') => value.push(chars.next().unwrap_or('\\')),
                    Some('\'') => break,
                    Some(character) => value.push(character),
                    None => return None,
                }
            }
        } else {
            while let Some(&character) = chars.peek() {
                if character.is_whitespace() {
                    break;
                }
                if character == '\\' {
                    chars.next();
                    value.push(chars.next().unwrap_or('\\'));
                    continue;
                }
                value.push(character);
                chars.next();
            }
        }
        if key == "dbname" {
            database = (!value.is_empty()).then_some(value);
        }
    }
    Some(database)
}

fn source_type_rule(value: &Option<String>, _: &SourceValidation) -> garde::Result {
    match value {
        Some(source_type) if !source_type.trim().is_empty() => {
            if SOURCE_TYPES.contains(&source_type.as_str()) {
                Ok(())
            } else {
                Err(garde::Error::new("is not included in the list"))
            }
        }
        _ => Err(garde::Error::new("can't be blank")),
    }
}

fn source_secret_rule(value: &Option<String>, context: &SourceValidation) -> garde::Result {
    if context.source_type.as_deref() == Some("control_plane") {
        if blank(value.as_deref()) {
            return Err(garde::Error::new(
                "can't be blank for source_type \"control_plane\"",
            ));
        }
    } else if !blank(value.as_deref()) {
        return Err(garde::Error::new(
            "is only allowed for source_type \"control_plane\"",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SourceValidation {
    source_type: Option<String>,
    broker_resolves: bool,
}

fn source_config_rule(value: &Value, context: &SourceValidation) -> garde::Result {
    let Some(map) = value.as_object() else {
        return Err(garde::Error::new("must be a hash"));
    };
    if let Some(source_type) = context.source_type.as_deref()
        && let Some(schema) = source_schema(source_type)
    {
        for required in schema.0 {
            if !map.contains_key(*required) {
                return Err(garde::Error::new(format!(
                    "is missing required key {} for source_type {}",
                    quote(required),
                    quote(source_type)
                )));
            }
        }
        for key in map.keys() {
            if !schema.0.contains(&key.as_str())
                && !schema.1.contains(&key.as_str())
                && !UNIVERSAL_OPTIONAL.contains(&key.as_str())
            {
                return Err(garde::Error::new(format!(
                    "has unknown key {} for source_type {}",
                    quote(key),
                    quote(source_type)
                )));
            }
        }
    }
    if context.source_type.as_deref() == Some("token_broker")
        && !context.broker_resolves
        && let Some(reference) = map
            .get("credential_id")
            .map(value_to_string)
            .filter(|value| !value.is_empty())
    {
        return Err(garde::Error::new(format!(
            "credential_id {} does not reference an existing broker credential",
            quote(&reference)
        )));
    }
    Ok(())
}

impl SourceDocument {
    /// Validates the source the way `SecretSource` does. `broker_resolves` is the
    /// resolved token_broker reference, when there is one.
    pub fn errors(&self, broker_resolves: bool) -> Errors {
        self.validate_fields_with(&SourceValidation {
            source_type: self.source_type.clone(),
            broker_resolves,
        })
        .err()
        .unwrap_or_default()
    }
}

fn rule_cidr(value: &Option<String>, _: &()) -> garde::Result {
    if let Some(cidr) = value.as_deref().filter(|value| !value.trim().is_empty())
        && !cidr_valid(cidr)
    {
        return Err(garde::Error::new("is not a valid CIDR"));
    }
    Ok(())
}

fn rule_http_methods(value: &Value, _: &()) -> garde::Result {
    let Some(methods) = value.as_array() else {
        return Err(garde::Error::new("must be an array"));
    };
    if let Some(method) = methods.iter().find(|method| {
        !method
            .as_str()
            .is_some_and(|method| method == "*" || HTTP_METHODS.contains(&method))
    }) {
        return Err(garde::Error::new(format!(
            "{} is not a supported HTTP method",
            quote_value(method)
        )));
    }
    Ok(())
}

fn rule_paths(value: &Value, _: &()) -> garde::Result {
    let Some(paths) = value.as_array() else {
        return Err(garde::Error::new("must be an array"));
    };
    if let Some(path) = paths
        .iter()
        .find(|path| !path.as_str().is_some_and(|path| path.starts_with('/')))
    {
        return Err(garde::Error::new(format!(
            "{} must be a string starting with /",
            quote_value(path)
        )));
    }
    Ok(())
}

impl RuleDocument {
    pub fn errors(&self) -> Errors {
        let mut errors = self.validate_fields().err().unwrap_or_default();
        let host = self
            .host
            .as_deref()
            .filter(|value| !value.trim().is_empty());
        let cidr = self
            .cidr
            .as_deref()
            .filter(|value| !value.trim().is_empty());
        if host.is_some() && cidr.is_some() {
            errors.add("base", "host and cidr are mutually exclusive");
        } else if host.is_none() && cidr.is_none() {
            errors.add("base", "either host or cidr must be present");
        }
        errors
    }
}

fn cidr_valid(value: &str) -> bool {
    let (address, prefix) = match value.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (value, None),
    };
    let Ok(address) = address.parse::<std::net::IpAddr>() else {
        return false;
    };
    match prefix {
        None => true,
        Some(prefix) => {
            let Ok(prefix) = prefix.parse::<u8>() else {
                return false;
            };
            match address {
                std::net::IpAddr::V4(_) => prefix <= 32,
                std::net::IpAddr::V6(_) => prefix <= 128,
            }
        }
    }
}

const HTTP_METHODS: [&str; 8] = [
    "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "CONNECT",
];

const SOURCE_TYPES: [&str; 7] = [
    "env",
    "aws_sm",
    "aws_ssm",
    "1password",
    "1password_connect",
    "control_plane",
    "token_broker",
];

const UNIVERSAL_OPTIONAL: [&str; 2] = ["json_key", "ttl"];

type SourceSchema = (&'static [&'static str], &'static [&'static str]);

fn source_schema(source_type: &str) -> Option<SourceSchema> {
    const ENV: SourceSchema = (&["var"], &[]);
    const AWS_SM: SourceSchema = (&["secret_id"], &["region"]);
    const AWS_SSM: SourceSchema = (&["name"], &["region", "with_decryption"]);
    const ONEPASSWORD: SourceSchema = (&["secret_ref"], &["token_env"]);
    const ONEPASSWORD_CONNECT: SourceSchema = (&["secret_ref"], &["host_env", "token_env"]);
    const CONTROL_PLANE: SourceSchema = (&[], &[]);
    const TOKEN_BROKER: SourceSchema = (&["credential_id"], &[]);
    match source_type {
        "env" => Some(ENV),
        "aws_sm" => Some(AWS_SM),
        "aws_ssm" => Some(AWS_SSM),
        "1password" => Some(ONEPASSWORD),
        "1password_connect" => Some(ONEPASSWORD_CONNECT),
        "control_plane" => Some(CONTROL_PLANE),
        "token_broker" => Some(TOKEN_BROKER),
        _ => None,
    }
}

fn normalize_header(header: Option<String>) -> Option<String> {
    header
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
}

fn canonical_replace_config() -> Value {
    json!({
        "proxy_value": "GITHUB_TOKEN",
        "match_headers": ["Authorization"],
        "require": false,
    })
}

fn canonical_replace_config_without_require() -> Value {
    json!({
        "proxy_value": "GITHUB_TOKEN",
        "match_headers": ["Authorization"],
    })
}

fn github_default_rules() -> Vec<RuleDocument> {
    [
        ("api.github.com", 0),
        ("github.com", 1),
        ("api.githubcopilot.com", 2),
    ]
    .into_iter()
    .map(|(host, position)| RuleDocument {
        host: Some(host.to_owned()),
        cidr: None,
        position,
        http_methods: Value::Array(Vec::new()),
        paths: Value::Array(Vec::new()),
    })
    .collect()
}

pub(crate) fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(true) => "true".to_owned(),
        Value::Bool(false) => "false".to_owned(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

/// Quotes a value plainly. Escaping is not part of the API contract, so a name
/// with a quote or a backslash is shown as-is.
fn quote(value: &str) -> String {
    format!("\"{value}\"")
}

fn quote_value(value: &Value) -> String {
    match value {
        Value::String(text) => quote(text),
        other => other.to_string(),
    }
}

/// Blankness for the JSON values the document carries.
pub(crate) fn ruby_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(value) => !value,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Number(_) => false,
    }
}

pub(crate) fn ruby_present(value: &Value) -> bool {
    !ruby_blank(value)
}

pub(crate) fn blank(value: Option<&str>) -> bool {
    value.is_none_or(|text| text.trim().is_empty())
}

/// Decrypts an inline source value (`secret_sources.secret` is encrypted).
pub fn decrypt(value: &str) -> Result<String> {
    crate::areas::encryption::decrypt(value)
}

pub(crate) fn encrypt(value: &str) -> Result<String> {
    crate::areas::encryption::encrypt(value)
}

impl Document {
    /// The shared write document: this kind's parent columns, the nested source
    /// and rules, and the identity. The writer in [`super::write`] persists it.
    pub(super) fn into_write(
        self,
        kind: SecretKind,
        sources: Vec<super::write::SourceDraft>,
    ) -> super::write::Document {
        let name = self.name;
        let description = self.description;
        let labels = self.labels;
        let parent = match (self.fields, kind) {
            (
                DocumentFields::Static {
                    kind,
                    inject_config,
                    replace_config,
                },
                SecretKind::Static,
            ) => ParentRow::Static(StaticParent {
                name,
                description,
                labels,
                kind: Some(kind.unwrap_or_else(|| "custom".to_owned())),
                inject_config,
                replace_config,
            }),
            (
                DocumentFields::PgDsn {
                    database,
                    role,
                    settings,
                },
                SecretKind::PgDsn,
            ) => ParentRow::PgDsn(PgDsnParent {
                name,
                description,
                database,
                role,
                labels,
                settings,
            }),
            (
                DocumentFields::GcpAuth {
                    credentials_provider,
                    subject,
                    scopes,
                },
                SecretKind::GcpAuth,
            ) => ParentRow::GcpAuth(GcpAuthParent {
                name,
                description,
                subject,
                labels,
                credentials_provider,
                scopes,
            }),
            (DocumentFields::GcpIdToken { audience, header }, SecretKind::GcpIdToken) => {
                ParentRow::GcpIdToken(GcpIdTokenParent {
                    name,
                    description,
                    audience,
                    header,
                    labels,
                })
            }
            (fields, _) => unreachable!("kind selects the parent fields: {fields:?}"),
        };
        super::write::Document {
            foreign_id: self.foreign_id,
            parent,
            sources,
            rules: self.rules,
            private_key_error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_dsn;

    fn database(value: &str) -> Option<Option<String>> {
        parse_dsn(value)
    }

    #[test]
    fn a_dsn_names_its_database_or_none() {
        assert_eq!(
            database("postgres://user:pw@db.example.com:5432/app?sslmode=require"),
            Some(Some("app".to_owned()))
        );
        assert_eq!(
            database("postgresql://db.example.com/app"),
            Some(Some("app".to_owned()))
        );
        // Percent escapes decode; a plus is a literal character in a path.
        assert_eq!(
            database("postgres://db.example.com/a%20b"),
            Some(Some("a b".to_owned()))
        );
        assert_eq!(
            database("postgres://db.example.com/a+b"),
            Some(Some("a+b".to_owned()))
        );
        assert_eq!(database("postgres://db.example.com"), Some(None));
        assert_eq!(
            database("dbname=app host=db.example.com"),
            Some(Some("app".to_owned()))
        );
        assert_eq!(database("host=db.example.com"), Some(None));
        // Malformed DSNs are not an error: the proxy stays authoritative.
        assert_eq!(database("dbname"), None);
    }
}
