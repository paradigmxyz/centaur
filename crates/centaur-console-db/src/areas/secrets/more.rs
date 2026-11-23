//! The bearer-only secret kinds: `aws_auth`, `oauth_token`, `hmac`, and
//! `ssh_key`.
//!
//! These kinds are only written through `/api/v1` (the bearer operator API);
//! the Console SPA does not edit them, so there is no retained-inline-secret or
//! path-style error-key mode here. The write builds the whole credential graph
//! and saves it once, which gives this module its shape:
//!
//! - a document is typed base attributes plus role-keyed sources and positional
//!   rules, deserialized from the request by the extractors in
//!   `centaur_api_server::console::request` (no parameter filtering or scalar
//!   coercion);
//! - an unchanged document short-circuits and keeps the existing nested rows and
//!   cache versions;
//! - a document is validated before anything is written, then the parent and
//!   every nested record are replaced in one transaction, and each saved or
//!   destroyed record bumps the sync-config version of the principals the
//!   secret reaches.
//!
//! Reads (lists, lookups, records) and the deletes come from the parent module;
//! this module only adds the four kinds' write documents.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::PgPool;

use super::write::{
    AwsAuthParent, Document, HmacParent, OauthTokenParent, ParentRow, Pending, SourceDraft,
    SshKeyParent, foreign_id_taken, resolve_brokers, resolve_target, run_write, stored_plaintexts,
};
use super::{
    BearerRequest, Errors, ForeignIdValue, RuleDocument, SecretKind, SecretRow, WriteError,
    WriteResult, WriteTarget, blank,
};
use crate::ssh_key::{derive_public_key, normalize_private_key, supported_public_key};
use crate::validation::ValidateFields;

/// Which API the write came from: the bearer replacement.
const FAMILY: super::write::Family = super::write::Family::Bearer;

/// One `{source_type, secret, config}` source block.
///
/// The wire shape is typed: a non-object block, a non-string `secret`, or a
/// non-object `config` is a validation error at the extractor instead of being
/// dropped or coerced.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SourceInput {
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
}

impl SourceInput {
    /// An empty block is absent.
    fn is_blank(&self) -> bool {
        self.source_type.is_none() && self.secret.is_none() && self.config.is_empty()
    }
}

/// One request rule (`rules: [{host, cidr, http_methods, paths}]`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RuleInput {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub cidr: Option<String>,
    #[serde(default)]
    pub http_methods: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

/// One hmac companion header (`headers: [{name, value}]`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct HeaderInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

/// `POST /api/v1/aws_auth_secrets` and its PUT/PATCH upsert body.
#[derive(Clone, Debug, Default, Deserialize, garde::Validate)]
#[garde(context(MoreValidation))]
pub struct AwsAuthRequest {
    #[serde(default)]
    #[garde(skip)]
    pub foreign_id: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub name: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub description: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub labels: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    #[garde(custom(string_array))]
    pub allowed_regions: Option<Vec<String>>,
    #[serde(default)]
    #[garde(custom(string_array))]
    pub allowed_services: Option<Vec<String>>,
    #[serde(default)]
    #[garde(skip)]
    pub access_key_id: Option<SourceInput>,
    #[serde(default)]
    #[garde(skip)]
    pub secret_access_key: Option<SourceInput>,
    #[serde(default)]
    #[garde(skip)]
    pub session_token: Option<SourceInput>,
    #[serde(default)]
    #[garde(skip)]
    pub rules: Option<Vec<RuleInput>>,
}

/// `POST /api/v1/oauth_token_secrets` and its PUT/PATCH upsert body.
#[derive(Clone, Debug, Default, Deserialize, garde::Validate)]
#[garde(context(MoreValidation))]
pub struct OauthTokenRequest {
    #[serde(default)]
    #[garde(skip)]
    pub foreign_id: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub name: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub description: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub labels: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    #[garde(custom(oauth_grant))]
    pub grant: Option<String>,
    #[serde(default)]
    #[garde(custom(required_string))]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    #[garde(custom(oauth_audience))]
    pub audience: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub header: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub value_prefix: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    #[garde(skip)]
    pub credentials: Option<BTreeMap<String, SourceInput>>,
    #[serde(default)]
    #[garde(skip)]
    pub token_endpoint_headers: Option<BTreeMap<String, SourceInput>>,
    #[serde(default)]
    #[garde(skip)]
    pub rules: Option<Vec<RuleInput>>,
}

/// `POST /api/v1/hmac_secrets` and its PUT/PATCH upsert body.
#[derive(Clone, Debug, Default, Deserialize, garde::Validate)]
#[garde(context(MoreValidation))]
pub struct HmacRequest {
    #[serde(default)]
    #[garde(skip)]
    pub foreign_id: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub name: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub description: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub labels: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    #[garde(custom(hmac_timestamp_format))]
    pub timestamp_format: Option<String>,
    #[serde(default)]
    #[garde(custom(hmac_signature_algorithm))]
    pub signature_algorithm: Option<String>,
    #[serde(default)]
    #[garde(custom(hmac_signature_key_encoding))]
    pub signature_key_encoding: Option<String>,
    #[serde(default)]
    #[garde(custom(hmac_signature_output_encoding))]
    pub signature_output_encoding: Option<String>,
    #[serde(default)]
    #[garde(custom(required_string))]
    pub signature_message: Option<String>,
    /// Strictly a JSON boolean: no implicit casting of `"true"` or `1`.
    #[serde(default)]
    #[garde(skip)]
    pub allow_chunked_body: Option<bool>,
    #[serde(default)]
    #[garde(custom(hmac_headers))]
    pub headers: Option<Vec<HeaderInput>>,
    #[serde(default)]
    #[garde(skip)]
    pub credentials: Option<BTreeMap<String, SourceInput>>,
    #[serde(default)]
    #[garde(skip)]
    pub rules: Option<Vec<RuleInput>>,
}

/// `POST /api/v1/ssh_key_secrets` and its PUT/PATCH upsert body.
#[derive(Clone, Debug, Default, Deserialize, garde::Validate)]
#[garde(context(MoreValidation))]
pub struct SshKeyRequest {
    #[serde(default)]
    #[garde(skip)]
    pub foreign_id: Option<String>,
    #[serde(default)]
    #[garde(custom(required_string), length(chars, max = 100))]
    pub name: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub description: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub labels: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    #[garde(custom(ssh_public_key))]
    pub public_key: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub private_key: Option<SourceInput>,
}

impl SourceDraft {
    /// A source built from a typed request block.
    fn from_input(source: SourceInput, role: Option<&str>, role_kind: Option<&str>) -> Self {
        Self {
            role: role.map(str::to_owned),
            role_kind: role_kind.map(str::to_owned),
            source_type: source.source_type,
            secret: source.secret,
            config: Value::Object(source.config.into_iter().collect()),
            broker_credential_id: None,
            broker_resolved: true,
        }
    }
}

pub async fn write_secret(
    pool: &PgPool,
    target: WriteTarget,
    request: BearerRequest,
    operator_id: Option<i64>,
) -> std::result::Result<WriteResult, WriteError> {
    let kind = request.kind();
    let mut transaction = pool.begin().await?;
    let (existing, was_new) = resolve_target(&mut transaction, kind, &target).await?;
    let mut document = Document::from_request(request.clone(), existing.as_ref());
    apply_target_foreign_id(&mut document, &target, existing.as_ref());

    resolve_brokers(&mut transaction, &mut document.sources).await?;

    let role_owner = kind != SecretKind::SshKey;
    let source_errors: Vec<Errors> = document
        .sources
        .iter()
        .map(|source| source.errors(role_owner))
        .collect();
    let rules_invalid = document.rules.iter().any(|rule| !rule.errors().is_empty());

    let foreign_id_taken = foreign_id_taken(
        &mut transaction,
        kind,
        &document.foreign_id,
        existing.as_ref(),
    )
    .await?;

    // Validate the whole document before writing anything: the parent's own
    // validators plus the nested sources and rules.
    let mut errors = document.parent_errors(kind, foreign_id_taken, &request);
    if source_errors.iter().any(|errors| !errors.is_empty()) {
        if kind == SecretKind::SshKey {
            // The key's single `key_source` reports its own errors under
            // `key_source.<attribute>`.
            for source in source_errors.iter().filter(|errors| !errors.is_empty()) {
                for (attribute, message) in error_entries(source) {
                    errors.add(&format!("key_source.{attribute}"), message);
                }
            }
        } else {
            errors.add("sources", "is invalid");
        }
    }
    if rules_invalid {
        errors.add("rules", "is invalid");
    }

    let pending = Pending {
        document,
        errors,
        existing_plain: stored_plaintexts(existing.as_ref())?,
    };
    run_write(
        pool,
        kind,
        transaction,
        existing.as_ref(),
        was_new,
        pending,
        operator_id,
        FAMILY,
    )
    .await
}

/// A PUT addressed by a foreign id creates the record with that identity when
/// the body omits one.
fn apply_target_foreign_id(
    document: &mut Document,
    target: &WriteTarget,
    existing: Option<&SecretRow>,
) {
    if existing.is_none()
        && let WriteTarget::ForeignId(identifier) = target
        && matches!(
            document.foreign_id,
            ForeignIdValue::Omitted | ForeignIdValue::Set(None)
        )
    {
        document.foreign_id = ForeignIdValue::Set(Some(identifier.clone()));
    }
}

// ---------------------------------------------------------------------------
// Document construction
// ---------------------------------------------------------------------------

impl Document {
    fn from_request(request: BearerRequest, existing: Option<&SecretRow>) -> Self {
        match request {
            BearerRequest::AwsAuth(request) => aws_auth_document(request, existing),
            BearerRequest::OauthToken(request) => oauth_token_document(request, existing),
            BearerRequest::Hmac(request) => hmac_document(request, existing),
            BearerRequest::SshKey(request) => ssh_key_document(request, existing),
        }
    }
}

fn aws_auth_document(request: AwsAuthRequest, existing: Option<&SecretRow>) -> Document {
    let parent = ParentRow::AwsAuth(AwsAuthParent {
        name: request.name,
        description: request.description,
        labels: json!(request.labels.unwrap_or_default()),
        allowed_regions: json!(request.allowed_regions.unwrap_or_default()),
        allowed_services: json!(request.allowed_services.unwrap_or_default()),
    });
    let sources = [
        ("access_key_id", request.access_key_id),
        ("secret_access_key", request.secret_access_key),
        ("session_token", request.session_token),
    ]
    .into_iter()
    .filter_map(|(role, source)| {
        let source = source.filter(|source| !source.is_blank())?;
        Some(SourceDraft::from_input(
            source,
            Some(role),
            Some("credential_field"),
        ))
    })
    .collect();
    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        parent,
        sources,
        rules: parse_rules(request.rules.unwrap_or_default()),
        private_key_error: None,
    }
}

fn oauth_token_document(request: OauthTokenRequest, existing: Option<&SecretRow>) -> Document {
    let parent = ParentRow::OauthToken(OauthTokenParent {
        name: request.name,
        description: request.description,
        labels: json!(request.labels.unwrap_or_default()),
        grant: request.grant,
        token_endpoint: request.token_endpoint,
        audience: request.audience,
        header: request.header,
        value_prefix: request.value_prefix,
        scopes: json!(request.scopes.unwrap_or_default()),
    });
    let mut sources = role_sources(request.credentials, "credential_field");
    sources.extend(role_sources(
        request.token_endpoint_headers,
        "endpoint_header",
    ));
    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        parent,
        sources,
        rules: parse_rules(request.rules.unwrap_or_default()),
        private_key_error: None,
    }
}

fn hmac_document(request: HmacRequest, existing: Option<&SecretRow>) -> Document {
    let parent = ParentRow::Hmac(HmacParent {
        name: request.name,
        description: request.description,
        labels: json!(request.labels.unwrap_or_default()),
        timestamp_format: request.timestamp_format,
        signature_algorithm: request.signature_algorithm,
        signature_key_encoding: request.signature_key_encoding,
        signature_output_encoding: request.signature_output_encoding,
        signature_message: request.signature_message,
        allow_chunked_body: request.allow_chunked_body.unwrap_or(false),
        headers: Value::Array(
            request
                .headers
                .unwrap_or_default()
                .into_iter()
                .map(header_value)
                .collect(),
        ),
    });
    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        parent,
        sources: role_sources(request.credentials, "credential_field"),
        rules: parse_rules(request.rules.unwrap_or_default()),
        private_key_error: None,
    }
}

fn ssh_key_document(request: SshKeyRequest, existing: Option<&SecretRow>) -> Document {
    // `SshKeySecret` normalizes both attributes: the name is stripped and the
    // public key keeps only "<type> <base64>" (comments are labels).
    let mut parent = SshKeyParent {
        name: request.name.map(|name| name.trim().to_owned()),
        description: request.description,
        labels: json!(request.labels.unwrap_or_default()),
        public_key: normalize_public_key(request.public_key),
    };

    let mut sources = Vec::new();
    let mut private_key_error = None;
    if let Some(source) = request.private_key.filter(|source| !source.is_blank()) {
        let mut draft = SourceDraft::from_input(source, None, None);
        if draft.source_type.as_deref() == Some("control_plane") {
            // An inline key's public key is derived, and its value is stored
            // normalized.
            parent.public_key = None;
            if let Some(secret) = draft
                .secret
                .as_deref()
                .filter(|secret| !secret.trim().is_empty())
            {
                draft.secret = Some(normalize_private_key(secret));
            }
            match derive_public_key(draft.secret.as_deref().unwrap_or_default()) {
                Ok(public_key) => parent.public_key = Some(public_key),
                Err(message) => private_key_error = Some(message),
            }
        }
        sources.push(draft);
    }

    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        parent: ParentRow::SshKey(parent),
        sources,
        rules: Vec::new(),
        private_key_error,
    }
}

/// The role-keyed sources of an `oauth_token`/`hmac` document, in request order.
fn role_sources(
    sources: Option<BTreeMap<String, SourceInput>>,
    role_kind: &str,
) -> Vec<SourceDraft> {
    sources
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, source)| !source.is_blank())
        .map(|(role, source)| SourceDraft::from_input(source, Some(&role), Some(role_kind)))
        .collect()
}

fn parse_foreign_id(foreign_id: Option<String>, existing: Option<&SecretRow>) -> ForeignIdValue {
    let blank_value = foreign_id
        .as_deref()
        .is_none_or(|value| value.trim().is_empty());
    let existing_foreign = existing.and_then(|row| row.foreign_id.as_deref());
    if blank_value && existing_foreign.is_some_and(|value| !value.trim().is_empty()) {
        return ForeignIdValue::Omitted;
    }
    ForeignIdValue::Set(foreign_id)
}

fn parse_rules(rules: Vec<RuleInput>) -> Vec<RuleDocument> {
    rules
        .into_iter()
        .enumerate()
        .map(|(position, rule)| RuleDocument {
            host: rule.host,
            cidr: rule.cidr,
            position: i32::try_from(position).unwrap_or(i32::MAX),
            http_methods: json!(rule.http_methods),
            paths: json!(rule.paths),
        })
        .collect()
}

/// A stored header keeps only the keys the block carried, so a header missing
/// `name` or `value` fails the model's own validation message.
fn header_value(header: HeaderInput) -> Value {
    let mut value = Map::new();
    if let Some(name) = header.name {
        value.insert("name".to_owned(), json!(name));
    }
    if let Some(text) = header.value {
        value.insert("value".to_owned(), json!(text));
    }
    Value::Object(value)
}

fn normalize_public_key(value: Option<String>) -> Option<String> {
    value.map(|value| {
        value
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    })
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl Document {
    fn grant(&self) -> Option<&str> {
        match &self.parent {
            ParentRow::OauthToken(parent) => parent.grant.as_deref(),
            _ => None,
        }
    }

    fn public_key(&self) -> Option<&str> {
        match &self.parent {
            ParentRow::SshKey(parent) => parent.public_key.as_deref(),
            _ => None,
        }
    }

    /// The typed request's field rules plus the merged, database-backed and
    /// cross-field checks for this kind.
    fn parent_errors(
        &self,
        kind: SecretKind,
        foreign_id_taken: bool,
        request: &BearerRequest,
    ) -> Errors {
        let context = MoreValidation {
            grant: self.grant().map(str::to_owned),
            private_key_error: self.private_key_error.clone(),
        };
        let mut errors = match request {
            BearerRequest::AwsAuth(request) => request
                .validate_fields_with(&context)
                .err()
                .unwrap_or_default(),
            BearerRequest::OauthToken(request) => request
                .validate_fields_with(&context)
                .err()
                .unwrap_or_default(),
            BearerRequest::Hmac(request) => request
                .validate_fields_with(&context)
                .err()
                .unwrap_or_default(),
            BearerRequest::SshKey(request) => {
                // An inline control-plane key derives the public key, so the
                // merged document's value is the one that must be supported.
                let mut request = request.clone();
                request.public_key = self.public_key().map(str::to_owned);
                request
                    .validate_fields_with(&context)
                    .err()
                    .unwrap_or_default()
            }
        };
        self.foreign_id_errors(kind, foreign_id_taken, &mut errors);
        match kind {
            SecretKind::AwsAuth => self.aws_errors(&mut errors),
            SecretKind::OauthToken => self.oauth_token_errors(&mut errors),
            SecretKind::Hmac => self.hmac_errors(&mut errors),
            SecretKind::SshKey => self.ssh_key_errors(&mut errors),
            _ => {}
        }
        errors
    }

    /// `foreign_id` is checked after the target has been merged in, and its
    /// uniqueness is a database answer, so it cannot be a request attribute.
    fn foreign_id_errors(&self, kind: SecretKind, taken: bool, errors: &mut Errors) {
        let ForeignIdValue::Set(Some(foreign_id)) = &self.foreign_id else {
            return;
        };
        if taken {
            errors.add("foreign_id", "has already been taken");
        }
        let reserved = format!("{}_", kind.oid_prefix());
        if foreign_id.starts_with(&reserved) {
            errors.add(
                "foreign_id",
                format!("must not start with \"{reserved}\", which is reserved for opaque ids"),
            );
        }
        if !foreign_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_' | '~')
        }) {
            errors.add(
                "foreign_id",
                "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)",
            );
        }
    }
}

/// Sibling or derived values the request's field rules need.
pub struct MoreValidation {
    grant: Option<String>,
    private_key_error: Option<String>,
}

/// `allowed_regions`/`allowed_services`: the wire type is a string array, so
/// only blank entries can still fail.
fn string_array(value: &Option<Vec<String>>, _: &MoreValidation) -> garde::Result {
    if value
        .as_ref()
        .is_some_and(|items| items.iter().any(|item| item.trim().is_empty()))
    {
        return Err(garde::Error::new("must all be strings"));
    }
    Ok(())
}

fn required_string(value: &Option<String>, _: &MoreValidation) -> garde::Result {
    if blank(value.as_deref()) {
        return Err(garde::Error::new("can't be blank"));
    }
    Ok(())
}

const GRANTS: [&str; 4] = [
    "refresh_token",
    "client_credentials",
    "password",
    "jwt_bearer",
];

fn oauth_grant(value: &Option<String>, _: &MoreValidation) -> garde::Result {
    if !value
        .as_deref()
        .is_some_and(|grant| GRANTS.contains(&grant))
    {
        return Err(garde::Error::new(format!(
            "must be one of {}",
            GRANTS.join(", ")
        )));
    }
    Ok(())
}

fn oauth_audience(value: &Option<String>, context: &MoreValidation) -> garde::Result {
    if context.grant.as_deref() == Some("jwt_bearer") && blank(value.as_deref()) {
        return Err(garde::Error::new("can't be blank for the jwt_bearer grant"));
    }
    Ok(())
}

fn hmac_timestamp_format(value: &Option<String>, _: &MoreValidation) -> garde::Result {
    hmac_inclusion(
        value.as_deref(),
        &["unix_seconds", "unix_millis", "unix_nanos", "rfc3339"],
    )
}

fn hmac_signature_algorithm(value: &Option<String>, _: &MoreValidation) -> garde::Result {
    hmac_inclusion(value.as_deref(), &["sha256", "sha512", "sha1"])
}

fn hmac_signature_key_encoding(value: &Option<String>, _: &MoreValidation) -> garde::Result {
    hmac_inclusion(value.as_deref(), &["raw", "base64", "hex"])
}

fn hmac_signature_output_encoding(value: &Option<String>, _: &MoreValidation) -> garde::Result {
    hmac_inclusion(value.as_deref(), &["base64", "hex"])
}

fn hmac_inclusion(value: Option<&str>, allowed: &[&str]) -> garde::Result {
    if !value.is_some_and(|value| allowed.contains(&value)) {
        return Err(garde::Error::new(format!(
            "must be one of {}",
            allowed.join(", ")
        )));
    }
    Ok(())
}

fn hmac_headers(value: &Option<Vec<HeaderInput>>, _: &MoreValidation) -> garde::Result {
    let Some(headers) = value.as_ref().filter(|headers| !headers.is_empty()) else {
        return Err(garde::Error::new("must include at least one header"));
    };
    if headers
        .iter()
        .any(|header| blank(header.name.as_deref()) || blank(header.value.as_deref()))
    {
        return Err(garde::Error::new(
            "each header must have a non-blank name and value",
        ));
    }
    Ok(())
}

fn ssh_public_key(value: &Option<String>, context: &MoreValidation) -> garde::Result {
    if context.private_key_error.is_none()
        && !supported_public_key(value.as_deref().unwrap_or_default())
    {
        return Err(garde::Error::new(
            "must be an Ed25519, RSA, or ECDSA public key in OpenSSH format",
        ));
    }
    Ok(())
}

impl Document {
    fn aws_errors(&self, errors: &mut Errors) {
        let roles: Vec<&str> = self
            .sources
            .iter()
            .filter_map(|source| source.role.as_deref())
            .collect();
        let known = ["access_key_id", "secret_access_key", "session_token"];
        let mut unknown: Vec<&str> = roles
            .iter()
            .copied()
            .filter(|role| !known.contains(role))
            .collect();
        unknown.dedup();
        if !unknown.is_empty() {
            errors.add(
                "sources",
                format!("has unknown credential role(s): {}", unknown.join(", ")),
            );
        }
        let mut unique = roles.clone();
        unique.sort_unstable();
        unique.dedup();
        if roles.len() != unique.len() {
            errors.add("sources", "has duplicate credential roles");
        }
        for required in ["access_key_id", "secret_access_key"] {
            if !roles.contains(&required) {
                errors.add(
                    "sources",
                    format!("is missing the required \"{required}\" credential"),
                );
            }
        }
        if self.rules.is_empty() {
            errors.add("rules", "must include at least one rule");
        }
    }

    fn oauth_token_errors(&self, errors: &mut Errors) {
        let grant = self.grant();
        if let Some(grant) = grant
            && let Some((required, optional)) = oauth_grant_fields(grant)
        {
            let credential_roles: Vec<&str> = self
                .sources
                .iter()
                .filter(|source| source.role_kind.as_deref() == Some("credential_field"))
                .filter_map(|source| source.role.as_deref())
                .collect();
            let known: Vec<&str> = required.iter().chain(optional.iter()).copied().collect();
            for role in &credential_roles {
                if !known.contains(role) {
                    errors.add(
                        "sources",
                        format!("\"{role}\" is not a valid credential field"),
                    );
                }
            }
            for missing in required {
                if !credential_roles.contains(missing) {
                    errors.add(
                        "sources",
                        format!("is missing required field \"{missing}\" for the {grant} grant"),
                    );
                }
            }
            let allowed = required
                .iter()
                .chain(optional.iter())
                .copied()
                .collect::<Vec<_>>();
            for unused in &credential_roles {
                if known.contains(unused) && !allowed.contains(unused) {
                    errors.add(
                        "sources",
                        format!("field \"{unused}\" is not used by the {grant} grant"),
                    );
                }
            }
        }
        if self.rules.is_empty() {
            errors.add("rules", "must include at least one rule");
        }
    }

    fn hmac_errors(&self, errors: &mut Errors) {
        let roles: Vec<&str> = self
            .sources
            .iter()
            .filter_map(|source| source.role.as_deref())
            .collect();
        for role in &roles {
            if role.trim().is_empty() {
                errors.add("sources", "credential name can't be blank");
            }
        }
        if !roles.contains(&"secret") {
            errors.add("sources", "is missing the required \"secret\" credential");
        }
        if self.rules.is_empty() {
            errors.add("rules", "must include at least one rule");
        }
    }

    fn ssh_key_errors(&self, errors: &mut Errors) {
        match self.sources.first() {
            None => errors.add("private_key", "can't be blank"),
            Some(source) => {
                if source.source_type.as_deref() == Some("token_broker") {
                    errors.add("private_key", "can't come from a token broker");
                } else if let Some(message) = &self.private_key_error {
                    errors.add("private_key", message.clone());
                }
            }
        }
    }
}

fn error_entries(errors: &Errors) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for (attribute, messages) in errors.entries() {
        for message in messages {
            entries.push((attribute.to_owned(), message.clone()));
        }
    }
    entries
}

fn oauth_grant_fields(grant: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match grant {
        "refresh_token" => Some((&["refresh_token", "client_id"], &["client_secret"])),
        "client_credentials" => Some((&["client_id", "client_secret"], &[])),
        "password" => Some((&["username", "password", "client_id"], &["client_secret"])),
        "jwt_bearer" => Some((&["issuer", "subject", "private_key"], &["private_key_id"])),
        _ => None,
    }
}

/// DB-backed write lifecycle tests. They skip locally when
/// `CONSOLE_DB_TEST_DATABASE_URL` is unset and panic under `CI`, matching the
/// crate's other schema tests.
#[cfg(test)]
mod db_tests {
    use serde_json::json;
    use sqlx::{PgPool, postgres::PgPoolOptions};

    use super::{
        AwsAuthRequest, BearerRequest, HmacRequest, OauthTokenRequest, SshKeyRequest, write_secret,
    };
    use crate::areas::secrets::write::{
        AwsAuthParent, GcpAuthParent, GcpIdTokenParent, HmacParent, OauthTokenParent, ParentRow,
        PgDsnParent, SshKeyParent, StaticParent,
    };
    use crate::areas::secrets::{
        SecretFields, SecretKind, WriteDocument, WriteOptions, WriteTarget, decrypt, delete_secret,
        find_secret,
    };

    /// The typed request an SSH key write deserializes to.
    fn ssh_request(body: serde_json::Value) -> BearerRequest {
        serde_json::from_value::<SshKeyRequest>(body)
            .map(BearerRequest::SshKey)
            .expect("valid ssh key request")
    }

    const PRIVATE_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
QyNTUxOQAAACDbAM6gXS+z5h+XCdFu0GkJZZRh+2YOIv0yZ18j2+xvVQAAAJBRQ6vQUUOr\n\
0AAAAAtzc2gtZWQyNTUxOQAAACDbAM6gXS+z5h+XCdFu0GkJZZRh+2YOIv0yZ18j2+xvVQ\n\
AAAEA+fiwdF2SiSWHpmwQRiukTTd0k6fOg6o0nYUJ0NwuL2tsAzqBdL7PmH5cJ0W7QaQll\n\
lGH7Zg4i/TJnXyPb7G9VAAAACGRpZmYtb25lAQIDBAU=\n\
-----END OPENSSH PRIVATE KEY-----\n";
    const PUBLIC_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINsAzqBdL7PmH5cJ0W7QaQlllGH7Zg4i/TJnXyPb7G9V";
    const PRIVATE_KEY_2: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
QyNTUxOQAAACDoVZzA88VJKXcdwesgNsr6BPcXL9goZkMYLJ7NDcqVxAAAAJAmMtnzJjLZ\n\
8wAAAAtzc2gtZWQyNTUxOQAAACDoVZzA88VJKXcdwesgNsr6BPcXL9goZkMYLJ7NDcqVxA\n\
AAAECrMSzVG6Cr7yM069kwaaMtntHcjfI/9ETZwo67w6E2eOhVnMDzxUkpdx3B6yA2yvoE\n\
9xcv2ChmQxgsns0NypXEAAAACXJvdW5kdHJpcAECAwQ=\n\
-----END OPENSSH PRIVATE KEY-----\n";
    const PUBLIC_KEY_2: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOhVnMDzxUkpdx3B6yA2yvoE9xcv2ChmQxgsns0NypXE";

    async fn database() -> Option<PgPool> {
        let Ok(url) = std::env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "CONSOLE_DB_TEST_DATABASE_URL must be set in CI"
            );
            eprintln!(
                "skipping: set CONSOLE_DB_TEST_DATABASE_URL to a database with Console's schema"
            );
            return None;
        };
        // Inline secret values are Active Record encrypted; the values only
        // have to be stable for this process.
        // SAFETY: no other thread reads these variables before the first
        // write in this test binary.
        unsafe {
            std::env::set_var(
                "CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY",
                "dev_ar_encryption_primary_key_0000000000000000",
            );
            std::env::set_var(
                "CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT",
                "dev_ar_encryption_key_derivation_salt_000000000",
            );
        }
        Some(
            PgPoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .expect("connect to the test database"),
        )
    }

    async fn operator(pool: &PgPool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (email, name, status, admin, created_at, updated_at) \
             VALUES ($1, 'Secrets More', 'active', true, now(), now()) RETURNING id",
        )
        .bind(format!(
            "secrets-more-{}@example.test",
            rand::random::<u64>()
        ))
        .fetch_one(pool)
        .await
        .expect("insert operator")
    }

    fn suffix() -> u64 {
        rand::random()
    }

    #[derive(Clone, Copy)]
    enum Phase {
        Create,
        Update,
    }

    impl Phase {
        fn label(self) -> &'static str {
            match self {
                Self::Create => "create",
                Self::Update => "update",
            }
        }
    }

    fn value(prefix: &str, phase: Phase) -> String {
        format!("{prefix}-{}", phase.label())
    }

    fn labels(phase: Phase) -> serde_json::Value {
        json!({ "stage": value("label", phase), "team": value("team", phase) })
    }

    fn list(prefix: &str, phase: Phase) -> serde_json::Value {
        json!([value(prefix, phase)])
    }

    fn source(prefix: &str, phase: Phase) -> serde_json::Value {
        json!({ "source_type": "env", "config": { "var": value(prefix, phase) } })
    }

    fn body(kind: SecretKind, phase: Phase, suffix: u64) -> serde_json::Value {
        let foreign_id = format!("round-trip-{}-{suffix}", kind.slug());
        let name = value("name", phase);
        let description = value("description", phase);
        let labels = labels(phase);
        match kind {
            SecretKind::Static => match phase {
                Phase::Create => json!({
                    "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                    "kind": "custom", "inject_config": { "header": value("header", phase) },
                }),
                Phase::Update => json!({
                    "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                    "kind": "github_token",
                    "replace_config": { "proxy_value": "GITHUB_TOKEN", "match_headers": ["Authorization"] },
                    "rules": [{ "host": "api.github.com" }],
                }),
            },
            SecretKind::PgDsn => json!({
                "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                "database": value("database", phase), "role": value("role", phase),
                "settings": [{ "name": "search_path", "value": value("setting", phase) }],
                "dsn": source("pg-dsn", phase),
            }),
            SecretKind::GcpAuth => match phase {
                Phase::Create => json!({
                    "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                    "credentials_provider": { "type": "workload_identity" },
                    "scopes": list("scope", phase),
                }),
                Phase::Update => json!({
                    "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                    "keyfile": source("gcp-key", phase),
                    "subject": value("subject", phase),
                    "scopes": list("scope", phase),
                }),
            },
            SecretKind::GcpIdToken => json!({
                "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                "audience": value("audience", phase),
                "header": match phase {
                    Phase::Create => "authorization",
                    Phase::Update => "x-serverless-authorization",
                },
                "keyfile": source("gcp-id-key", phase),
                "rules": [{ "host": value("host", phase) }],
            }),
            SecretKind::AwsAuth => json!({
                "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                "allowed_regions": list("region", phase),
                "allowed_services": list("service", phase),
                "access_key_id": source("aws-access", phase),
                "secret_access_key": source("aws-secret", phase),
                "session_token": source("aws-session", phase),
                "rules": [{ "host": value("host", phase), "http_methods": ["GET"] }],
            }),
            SecretKind::OauthToken => {
                let (grant, credentials) = match phase {
                    Phase::Create => (
                        "refresh_token",
                        json!({
                            "refresh_token": source("oauth-refresh", phase),
                            "client_id": source("oauth-client", phase),
                            "client_secret": source("oauth-client-secret", phase),
                        }),
                    ),
                    Phase::Update => (
                        "password",
                        json!({
                            "username": source("oauth-user", phase),
                            "password": source("oauth-password", phase),
                            "client_id": source("oauth-client", phase),
                            "client_secret": source("oauth-client-secret", phase),
                        }),
                    ),
                };
                json!({
                    "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                    "grant": grant,
                    "token_endpoint": value("token-endpoint", phase),
                    "audience": value("oauth-audience", phase),
                    "header": match phase {
                        Phase::Create => "authorization",
                        Phase::Update => "x-custom-authorization",
                    },
                    "value_prefix": value("prefix", phase),
                    "scopes": list("oauth-scope", phase),
                    "credentials": credentials,
                    "token_endpoint_headers": { "x-endpoint": source("oauth-endpoint", phase) },
                    "rules": [{ "host": value("host", phase) }],
                })
            }
            SecretKind::Hmac => json!({
                "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                "timestamp_format": "unix_seconds",
                "signature_algorithm": match phase { Phase::Create => "sha256", Phase::Update => "sha512" },
                "signature_key_encoding": match phase { Phase::Create => "raw", Phase::Update => "base64" },
                "signature_output_encoding": match phase { Phase::Create => "base64", Phase::Update => "hex" },
                "signature_message": value("message", phase),
                "allow_chunked_body": matches!(phase, Phase::Create),
                "headers": [{ "name": value("signed-header", phase), "value": value("signed-value", phase) }],
                "credentials": { "secret": source("hmac-secret", phase) },
                "rules": [{ "host": value("host", phase) }],
            }),
            SecretKind::SshKey => json!({
                "foreign_id": foreign_id, "name": name, "description": description, "labels": labels,
                "private_key": {
                    "source_type": "control_plane",
                    "secret": match phase { Phase::Create => PRIVATE_KEY, Phase::Update => PRIVATE_KEY_2 },
                },
            }),
        }
    }

    fn request(kind: SecretKind, phase: Phase, suffix: u64) -> WriteDocument {
        let body = body(kind, phase, suffix);
        match kind {
            SecretKind::Static
            | SecretKind::PgDsn
            | SecretKind::GcpAuth
            | SecretKind::GcpIdToken => {
                WriteDocument::Console(serde_json::from_value(body).expect("console request"))
            }
            SecretKind::AwsAuth => WriteDocument::from(
                serde_json::from_value::<AwsAuthRequest>(body).expect("aws request"),
            ),
            SecretKind::OauthToken => WriteDocument::from(
                serde_json::from_value::<OauthTokenRequest>(body).expect("oauth request"),
            ),
            SecretKind::Hmac => WriteDocument::from(
                serde_json::from_value::<HmacRequest>(body).expect("hmac request"),
            ),
            SecretKind::SshKey => WriteDocument::from(
                serde_json::from_value::<SshKeyRequest>(body).expect("ssh request"),
            ),
        }
    }

    fn parent(kind: SecretKind, phase: Phase) -> ParentRow {
        let name = Some(value("name", phase));
        let description = Some(value("description", phase));
        let labels = labels(phase);
        match kind {
            SecretKind::Static => ParentRow::Static(match phase {
                Phase::Create => StaticParent {
                    name,
                    description,
                    labels,
                    kind: Some("custom".to_owned()),
                    inject_config: json!({ "header": value("header", phase) }),
                    replace_config: serde_json::Value::Null,
                },
                Phase::Update => StaticParent {
                    name,
                    description,
                    labels,
                    kind: Some("github_token".to_owned()),
                    inject_config: serde_json::Value::Null,
                    replace_config: json!({
                        "proxy_value": "GITHUB_TOKEN",
                        "match_headers": ["Authorization"],
                        "require": false,
                    }),
                },
            }),
            SecretKind::PgDsn => ParentRow::PgDsn(PgDsnParent {
                name,
                description,
                database: Some(value("database", phase)),
                role: Some(value("role", phase)),
                labels,
                settings: json!([{ "name": "search_path", "value": value("setting", phase) }]),
            }),
            SecretKind::GcpAuth => ParentRow::GcpAuth(match phase {
                Phase::Create => GcpAuthParent {
                    name,
                    description,
                    subject: None,
                    labels,
                    credentials_provider: json!({ "type": "workload_identity" }),
                    scopes: list("scope", phase),
                },
                Phase::Update => GcpAuthParent {
                    name,
                    description,
                    subject: Some(value("subject", phase)),
                    labels,
                    credentials_provider: serde_json::Value::Null,
                    scopes: list("scope", phase),
                },
            }),
            SecretKind::GcpIdToken => ParentRow::GcpIdToken(GcpIdTokenParent {
                name,
                description,
                audience: Some(value("audience", phase)),
                header: Some(
                    match phase {
                        Phase::Create => "authorization",
                        Phase::Update => "x-serverless-authorization",
                    }
                    .to_owned(),
                ),
                labels,
            }),
            SecretKind::AwsAuth => ParentRow::AwsAuth(AwsAuthParent {
                name,
                description,
                labels,
                allowed_regions: list("region", phase),
                allowed_services: list("service", phase),
            }),
            SecretKind::OauthToken => ParentRow::OauthToken(OauthTokenParent {
                name,
                description,
                labels,
                grant: Some(
                    match phase {
                        Phase::Create => "refresh_token",
                        Phase::Update => "password",
                    }
                    .to_owned(),
                ),
                token_endpoint: Some(value("token-endpoint", phase)),
                audience: Some(value("oauth-audience", phase)),
                header: Some(
                    match phase {
                        Phase::Create => "authorization",
                        Phase::Update => "x-custom-authorization",
                    }
                    .to_owned(),
                ),
                value_prefix: Some(value("prefix", phase)),
                scopes: list("oauth-scope", phase),
            }),
            SecretKind::Hmac => ParentRow::Hmac(HmacParent {
                name,
                description,
                labels,
                timestamp_format: Some("unix_seconds".to_owned()),
                signature_algorithm: Some(
                    match phase {
                        Phase::Create => "sha256",
                        Phase::Update => "sha512",
                    }
                    .to_owned(),
                ),
                signature_key_encoding: Some(
                    match phase {
                        Phase::Create => "raw",
                        Phase::Update => "base64",
                    }
                    .to_owned(),
                ),
                signature_output_encoding: Some(
                    match phase {
                        Phase::Create => "base64",
                        Phase::Update => "hex",
                    }
                    .to_owned(),
                ),
                signature_message: Some(value("message", phase)),
                allow_chunked_body: matches!(phase, Phase::Create),
                headers: json!([{
                    "name": value("signed-header", phase),
                    "value": value("signed-value", phase),
                }]),
            }),
            SecretKind::SshKey => ParentRow::SshKey(SshKeyParent {
                name,
                description,
                labels,
                public_key: Some(
                    match phase {
                        Phase::Create => PUBLIC_KEY,
                        Phase::Update => PUBLIC_KEY_2,
                    }
                    .to_owned(),
                ),
            }),
        }
    }

    async fn principal(pool: &PgPool, operator_id: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, name, kind, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Round trip', 'slack_dm', '{}', $2, now(), now()) RETURNING id",
        )
        .bind(format!("round-trip-principal-{}", suffix()))
        .bind(operator_id)
        .fetch_one(pool)
        .await
        .expect("insert principal")
    }

    /// Every kind's parent row is written and read back through both its DB row
    /// and the loaded [`SecretRow`] the Console payloads render. Each expected
    /// parent is an explicit struct literal, so a new kind or parent field is a
    /// compile error until this test covers it.
    #[tokio::test]
    async fn every_kind_round_trips_every_parent_field() {
        let Some(pool) = database().await else {
            return;
        };
        let admin = operator(&pool).await;
        for kind in SecretKind::ALL {
            let suffix = suffix();
            let created = crate::areas::secrets::write_secret(
                &pool,
                kind,
                WriteTarget::New,
                request(kind, Phase::Create, suffix),
                Some(admin),
                WriteOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("create {kind:?}: {error:?}"));
            assert_eq!(
                ParentRow::from_secret_row(&created.row),
                parent(kind, Phase::Create),
                "create {kind:?}"
            );

            let oid = created.row.oid();
            let updated = crate::areas::secrets::write_secret(
                &pool,
                kind,
                WriteTarget::Oid(oid.clone()),
                request(kind, Phase::Update, suffix),
                Some(admin),
                WriteOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("update {kind:?}: {error:?}"));
            assert_eq!(
                ParentRow::from_secret_row(&updated.row),
                parent(kind, Phase::Update),
                "update {kind:?}"
            );

            // A grant makes the cache bump observable: an unchanged replay must
            // neither bump `updated_at` nor touch the principal's version.
            let principal_id = principal(&pool, admin).await;
            sqlx::query(&format!(
                "INSERT INTO grants ({}, principal_id, priority, created_by_id, created_at, updated_at) \
                 VALUES ($1, $2, 0, $3, now(), now())",
                kind.grant_column()
            ))
            .bind(updated.row.id)
            .bind(principal_id)
            .bind(admin)
            .execute(&pool)
            .await
            .expect("insert grant");
            let version_before: i64 = sqlx::query_scalar(
                "SELECT sync_config_cache_version FROM principals WHERE id = $1",
            )
            .bind(principal_id)
            .fetch_one(&pool)
            .await
            .expect("cache version");

            let replay = crate::areas::secrets::write_secret(
                &pool,
                kind,
                WriteTarget::Oid(oid),
                request(kind, Phase::Update, suffix),
                Some(admin),
                WriteOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("replay {kind:?}: {error:?}"));
            assert!(!replay.created);
            assert_eq!(
                replay.row.updated_at, updated.row.updated_at,
                "replay {kind:?} updated_at"
            );
            let version_after: i64 = sqlx::query_scalar(
                "SELECT sync_config_cache_version FROM principals WHERE id = $1",
            )
            .bind(principal_id)
            .fetch_one(&pool)
            .await
            .expect("cache version");
            assert_eq!(version_after, version_before, "replay {kind:?} cache bump");

            delete_secret(&pool, kind, updated.row.id)
                .await
                .expect("delete");
            sqlx::query("DELETE FROM principals WHERE id = $1")
                .bind(principal_id)
                .execute(&pool)
                .await
                .expect("delete principal");
        }
    }

    fn oauth_token_request(grant: &str) -> BearerRequest {
        let mut credentials = json!({
            "client_id": { "source_type": "env", "config": { "var": "CID" } },
            "client_secret": { "source_type": "env", "config": { "var": "CSECRET" } },
        });
        if grant == "refresh_token" {
            credentials["refresh_token"] =
                json!({ "source_type": "env", "config": { "var": "RT" } });
        }
        serde_json::from_value::<OauthTokenRequest>(json!({
            "name": "DB OAuth",
            "grant": grant,
            "token_endpoint": "https://auth.example.test/token",
            "scopes": ["read"],
            "credentials": credentials,
            "rules": [{ "host": "api.example.test", "http_methods": ["GET"] }],
        }))
        .map(BearerRequest::OauthToken)
        .expect("valid oauth token request")
    }

    /// `grant` is a reserved word in Postgres; the writer must still store it on
    /// create and on update.
    #[tokio::test]
    async fn oauth_token_grant_is_stored_on_create_and_update() {
        let Some(pool) = database().await else {
            return;
        };
        let admin = operator(&pool).await;
        let created = write_secret(
            &pool,
            WriteTarget::ForeignId(format!("db-oauth-{}", suffix())),
            oauth_token_request("refresh_token"),
            Some(admin),
        )
        .await
        .expect("create");
        let stored = |id: i64| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT \"grant\" FROM oauth_token_secrets WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("stored grant")
            }
        };
        assert_eq!(
            stored(created.row.id).await.as_deref(),
            Some("refresh_token")
        );

        let updated = write_secret(
            &pool,
            WriteTarget::Oid(created.row.oid()),
            oauth_token_request("client_credentials"),
            Some(admin),
        )
        .await
        .expect("update");
        assert!(!updated.created);
        assert_eq!(
            stored(created.row.id).await.as_deref(),
            Some("client_credentials")
        );
    }

    #[tokio::test]
    async fn ssh_keys_derive_rotate_and_reject_invalid_inline_values() {
        let Some(pool) = database().await else {
            return;
        };
        let admin = operator(&pool).await;
        let foreign_id = format!("db-ssh-{}", suffix());
        let body = json!({
            "foreign_id": foreign_id,
            "name": "DB SSH",
            "private_key": { "source_type": "control_plane", "secret": PRIVATE_KEY },
        });
        let created = write_secret(
            &pool,
            WriteTarget::New,
            ssh_request(body.clone()),
            Some(admin),
        )
        .await
        .expect("create");
        assert_eq!(
            created.row.fields,
            SecretFields::SshKey {
                public_key: PUBLIC_KEY.to_owned()
            }
        );
        assert_eq!(created.row.sources.len(), 1);
        assert_eq!(
            decrypt(
                created.row.sources[0]
                    .secret
                    .as_deref()
                    .expect("stored value")
            )
            .expect("decrypt"),
            PRIVATE_KEY,
            "the inline value is normalized and encrypted at rest"
        );

        let oid = created.row.oid();
        let replay = write_secret(
            &pool,
            WriteTarget::Oid(oid.clone()),
            ssh_request(body.clone()),
            Some(admin),
        )
        .await
        .expect("replay");
        assert!(!replay.created);
        let source_id: i64 =
            sqlx::query_scalar("SELECT id FROM secret_sources WHERE ssh_key_secret_id = $1")
                .bind(created.row.id)
                .fetch_one(&pool)
                .await
                .expect("source id");
        assert_eq!(replay.row.sources[0].id, source_id);

        let invalid = json!({
            "name": "DB SSH",
            "private_key": { "source_type": "control_plane", "secret": "not a key" },
        });
        let error = write_secret(
            &pool,
            WriteTarget::Oid(oid.clone()),
            ssh_request(invalid),
            Some(admin),
        )
        .await
        .expect_err("invalid key");
        match error {
            super::WriteError::Validation(details) => {
                assert!(details.get("private_key").is_some());
                assert!(!details.to_string().contains("not a key"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        // The rejected write left the previous key in place.
        let reloaded = find_secret(&pool, SecretKind::SshKey, &oid)
            .await
            .expect("reload")
            .expect("key still exists");
        assert_eq!(
            reloaded.fields,
            SecretFields::SshKey {
                public_key: PUBLIC_KEY.to_owned()
            }
        );

        let broker = json!({
            "name": "DB SSH Broker",
            "public_key": PUBLIC_KEY,
            "private_key": { "source_type": "token_broker", "config": { "credential_id": "bcr_missing" } },
        });
        let error = write_secret(&pool, WriteTarget::New, ssh_request(broker), Some(admin))
            .await
            .expect_err("token broker source");
        match error {
            super::WriteError::Validation(details) => {
                assert!(details.get("private_key").is_some());
                assert!(details.get("key_source.config").is_some());
            }
            other => panic!("unexpected error: {other:?}"),
        }

        delete_secret(&pool, SecretKind::SshKey, created.row.id)
            .await
            .expect("delete");
    }
}
