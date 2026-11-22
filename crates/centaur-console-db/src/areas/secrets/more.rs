//! The bearer-only secret kinds: `aws_auth`, `oauth_token`, `hmac`, and
//! `ssh_key`.
//!
//! These kinds are only written through `/api/v1` (the bearer operator API);
//! the Console SPA does not edit them, so there is no retained-inline-secret or
//! path-style error-key mode here. The controllers build the whole credential
//! graph and save it once (`Api::V1::{AwsAuth,OauthToken,Hmac,SshKey}SecretsController`),
//! which gives this module its shape:
//!
//! - a document is typed base attributes plus role-keyed sources and positional
//!   rules, deserialized from the request by the extractors in
//!   `centaur_api_server::console::request` (no strong-params filtering or
//!   scalar coercion);
//! - an unchanged document short-circuits (`SyncConfigReplacement.equivalent?`)
//!   and keeps the existing nested rows and cache versions;
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
use sqlx::{PgPool, Postgres, Transaction};

use super::{
    BearerRequest, Errors, ForeignIdValue, RuleDocument, RuleRow, SecretFields, SecretKind,
    SecretRow, SourceDocument, WriteError, WriteResult, WriteTarget, blank, decrypt, encrypt,
    load_secret, value_to_string,
};
use crate::invalidate::{self, Scope, SecretChanges};
use crate::ssh_key::{derive_public_key, normalize_private_key, supported_public_key};
use crate::validation::ValidateFields;
use crate::write;

/// One `{source_type, secret, config}` source block.
///
/// The wire shape is typed: a non-object block, a non-string `secret`, or a
/// non-object `config` is a validation error at the extractor instead of being
/// dropped or coerced the way strong params would.
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
    /// An empty block is absent (`next if src.blank?`).
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
    /// Strictly a JSON boolean: no ActiveModel casting of `"true"` or `1`.
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

/// One credential/header source of the document, with the role columns the
/// owning model requires.
#[derive(Clone, Debug)]
struct SourceDraft {
    role: Option<String>,
    role_kind: Option<String>,
    source_type: Option<String>,
    secret: Option<String>,
    config: Value,
    broker_credential_id: Option<i64>,
    broker_resolved: bool,
}

impl SourceDraft {
    /// A source built from a typed request block.
    fn new(source: SourceInput, role: Option<&str>, role_kind: Option<&str>) -> Self {
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

    /// `SecretSource` validation errors. The aws/oauth_token/hmac APIs only
    /// render `sources: ["is invalid"]`; the SSH API renders the nested keys.
    fn errors(&self, role_owner: bool) -> Errors {
        let source = SourceDocument {
            source_type: self.source_type.clone(),
            secret: self.secret.clone(),
            config: self.config.clone(),
        };
        let mut errors = source.errors(self.broker_resolved);
        if role_owner && blank(self.role.as_deref()) {
            errors.add(
                "role",
                "can't be blank for a oauth_token_secret or hmac_secret or aws_auth_secret source",
            );
        }
        errors
    }
}

/// The replacement document built from a typed request body.
struct Document {
    foreign_id: ForeignIdValue,
    /// The document's base attributes, with the column default for every field
    /// the body omitted (Console's replace semantics).
    base: Map<String, Value>,
    sources: Vec<SourceDraft>,
    rules: Vec<RuleDocument>,
    /// The inline SSH key parsing error, reported on `private_key`.
    private_key_error: Option<String>,
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
    let existing_plain = decrypt_sources(existing.as_ref())?;
    let mut document = Document::from_request(request.clone(), existing.as_ref());
    apply_target_foreign_id(&mut document, &target, existing.as_ref());

    resolve_brokers(&mut transaction, &mut document).await?;

    let role_owner = kind != SecretKind::SshKey;
    let source_errors: Vec<Errors> = document
        .sources
        .iter()
        .map(|source| source.errors(role_owner))
        .collect();
    let rules_invalid = document.rules.iter().any(|rule| !rule.errors().is_empty());

    let foreign_id_taken =
        foreign_id_taken(&mut transaction, kind, &document, existing.as_ref()).await?;

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
    let valid = errors.is_empty();

    let equivalent = existing
        .as_ref()
        .is_some_and(|row| document.equivalent(row, &existing_plain));

    if equivalent {
        // An unchanged document keeps the nested rows, the cache versions, and
        // the id sequences untouched.
        transaction.rollback().await?;
        let row = existing.expect("equivalence requires an existing record");
        return Ok(WriteResult {
            row,
            created: false,
        });
    }

    if !valid {
        // Validate first, write after: a rejected document leaves no rows behind
        // and consumes no ids.
        transaction.rollback().await?;
        return Err(WriteError::Validation(errors.into_value()));
    }

    let parent_changed = existing
        .as_ref()
        .is_some_and(|row| !document.base_matches(row));

    if was_new {
        let id = insert_parent(&mut transaction, kind, &document, operator_id).await?;
        insert_sources(&mut transaction, kind, id, &document.sources).await?;
        insert_rules(&mut transaction, kind, id, &document.rules).await?;
        let changes = SecretChanges::created(document.sources.len(), document.rules.len());
        invalidate::invalidate(&mut transaction, Scope::secret_write(kind, id, changes)).await?;
        transaction.commit().await?;
        let row = reload(pool, kind, id).await?;
        return Ok(WriteResult { row, created: true });
    }

    let id = existing.as_ref().map(|row| row.id).unwrap_or_default();
    let old_sources = existing.as_ref().map(|row| row.sources.len()).unwrap_or(0);
    let old_rules = existing.as_ref().map(|row| row.rules.len()).unwrap_or(0);

    if parent_changed {
        update_parent(&mut transaction, kind, id, &document).await?;
    }
    delete_nested(&mut transaction, kind, id).await?;
    insert_sources(&mut transaction, kind, id, &document.sources).await?;
    insert_rules(&mut transaction, kind, id, &document.rules).await?;
    if kind == SecretKind::SshKey {
        // The key's source belongs_to it with `touch: true`, so replacing the
        // source also bumps the key's updated_at.
        sqlx::query(&write::touch_sql(kind.table()))
            .bind(id)
            .execute(&mut *transaction)
            .await?;
    }
    let changes = SecretChanges::replaced(
        parent_changed,
        old_sources,
        document.sources.len(),
        old_rules,
        document.rules.len(),
    );
    invalidate::invalidate(&mut transaction, Scope::secret_write(kind, id, changes)).await?;
    transaction.commit().await?;
    let row = reload(pool, kind, id).await?;
    Ok(WriteResult {
        row,
        created: false,
    })
}

async fn resolve_target(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    target: &WriteTarget,
) -> std::result::Result<(Option<SecretRow>, bool), WriteError> {
    match target {
        WriteTarget::New => Ok((None, true)),
        WriteTarget::Oid(oid) => {
            let Some(id) = kind.decode_oid(oid) else {
                return Err(not_found(kind));
            };
            let locked = sqlx::query(&format!(
                "SELECT id FROM {} WHERE id = $1 FOR UPDATE",
                kind.table()
            ))
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?;
            if locked.is_none() {
                return Err(not_found(kind));
            }
            let row = load_secret(transaction, kind, id).await?;
            Ok((row, false))
        }
        WriteTarget::ForeignId(foreign_id) => {
            let id = sqlx::query_scalar::<_, i64>(&format!(
                "SELECT id FROM {} WHERE foreign_id = $1 FOR UPDATE",
                kind.table()
            ))
            .bind(foreign_id)
            .fetch_optional(&mut **transaction)
            .await?;
            match id {
                Some(id) => {
                    let row = load_secret(transaction, kind, id).await?;
                    Ok((row, false))
                }
                None => Ok((None, true)),
            }
        }
    }
}

fn not_found(kind: SecretKind) -> WriteError {
    WriteError::NotFound(format!("{} not found", kind.model_name()))
}

async fn reload(
    pool: &PgPool,
    kind: SecretKind,
    id: i64,
) -> std::result::Result<SecretRow, WriteError> {
    let mut connection = pool.acquire().await?;
    load_secret(&mut connection, kind, id)
        .await?
        .ok_or_else(|| WriteError::NotFound(format!("secret {id} disappeared")))
}

/// The stored plaintext of each source, for the replacement comparison.
fn decrypt_sources(
    existing: Option<&SecretRow>,
) -> std::result::Result<Vec<Option<String>>, WriteError> {
    let Some(row) = existing else {
        return Ok(Vec::new());
    };
    row.sources
        .iter()
        .map(|source| {
            source
                .secret
                .as_deref()
                .map(decrypt)
                .transpose()
                .map_err(WriteError::Internal)
        })
        .collect()
}

async fn foreign_id_taken(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    document: &Document,
    existing: Option<&SecretRow>,
) -> std::result::Result<bool, WriteError> {
    let ForeignIdValue::Set(Some(foreign_id)) = &document.foreign_id else {
        return Ok(false);
    };
    if foreign_id.is_empty() {
        return Ok(false);
    }
    let excluded = existing.map(|row| row.id).unwrap_or_default();
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE foreign_id = $1 AND id <> $2)",
        kind.table()
    ))
    .bind(foreign_id)
    .bind(excluded)
    .fetch_one(&mut **transaction)
    .await?)
}

async fn resolve_brokers(
    transaction: &mut Transaction<'_, Postgres>,
    document: &mut Document,
) -> std::result::Result<(), WriteError> {
    for source in &mut document.sources {
        if source.source_type.as_deref() != Some("token_broker") {
            source.broker_credential_id = None;
            source.broker_resolved = true;
            continue;
        }
        let reference = source
            .config
            .get("credential_id")
            .map(value_to_string)
            .filter(|value| !value.is_empty());
        source.broker_credential_id = match reference {
            Some(reference) => match crate::decode_oid("bcr", &reference) {
                Some(id) => {
                    sqlx::query_scalar::<_, i64>("SELECT id FROM broker_credentials WHERE id = $1")
                        .bind(id)
                        .fetch_optional(&mut **transaction)
                        .await?
                }
                None => {
                    sqlx::query_scalar::<_, i64>(
                        "SELECT id FROM broker_credentials WHERE foreign_id = $1",
                    )
                    .bind(&reference)
                    .fetch_optional(&mut **transaction)
                    .await?
                }
            },
            None => None,
        };
        source.broker_resolved = source.broker_credential_id.is_some();
    }
    Ok(())
}

/// `BaseController#resolve_for_upsert`: a PUT addressed by a foreign id creates
/// the record with that identity when the body omits one.
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

/// The base attributes every kind carries, with the column default for an
/// omitted field (replace semantics).
fn base_attributes(
    name: Option<String>,
    description: Option<String>,
    labels: Option<BTreeMap<String, Value>>,
) -> Map<String, Value> {
    let mut base = Map::new();
    base.insert("name".to_owned(), json!(name));
    base.insert("description".to_owned(), json!(description));
    base.insert("labels".to_owned(), json!(labels.unwrap_or_default()));
    base
}

fn aws_auth_document(request: AwsAuthRequest, existing: Option<&SecretRow>) -> Document {
    let mut base = base_attributes(request.name, request.description, request.labels);
    base.insert(
        "allowed_regions".to_owned(),
        json!(request.allowed_regions.unwrap_or_default()),
    );
    base.insert(
        "allowed_services".to_owned(),
        json!(request.allowed_services.unwrap_or_default()),
    );
    let sources = [
        ("access_key_id", request.access_key_id),
        ("secret_access_key", request.secret_access_key),
        ("session_token", request.session_token),
    ]
    .into_iter()
    .filter_map(|(role, source)| {
        let source = source.filter(|source| !source.is_blank())?;
        Some(SourceDraft::new(
            source,
            Some(role),
            Some("credential_field"),
        ))
    })
    .collect();
    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        base,
        sources,
        rules: parse_rules(request.rules.unwrap_or_default()),
        private_key_error: None,
    }
}

fn oauth_token_document(request: OauthTokenRequest, existing: Option<&SecretRow>) -> Document {
    let mut base = base_attributes(request.name, request.description, request.labels);
    for (key, value) in [
        ("grant", request.grant),
        ("token_endpoint", request.token_endpoint),
        ("audience", request.audience),
        ("header", request.header),
        ("value_prefix", request.value_prefix),
    ] {
        base.insert(key.to_owned(), json!(value));
    }
    base.insert(
        "scopes".to_owned(),
        json!(request.scopes.unwrap_or_default()),
    );
    let mut sources = role_sources(request.credentials, "credential_field");
    sources.extend(role_sources(
        request.token_endpoint_headers,
        "endpoint_header",
    ));
    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        base,
        sources,
        rules: parse_rules(request.rules.unwrap_or_default()),
        private_key_error: None,
    }
}

fn hmac_document(request: HmacRequest, existing: Option<&SecretRow>) -> Document {
    let mut base = base_attributes(request.name, request.description, request.labels);
    for (key, value) in [
        ("timestamp_format", request.timestamp_format),
        ("signature_algorithm", request.signature_algorithm),
        ("signature_key_encoding", request.signature_key_encoding),
        (
            "signature_output_encoding",
            request.signature_output_encoding,
        ),
        ("signature_message", request.signature_message),
    ] {
        base.insert(key.to_owned(), json!(value));
    }
    base.insert(
        "allow_chunked_body".to_owned(),
        json!(request.allow_chunked_body.unwrap_or(false)),
    );
    base.insert(
        "headers".to_owned(),
        Value::Array(
            request
                .headers
                .unwrap_or_default()
                .into_iter()
                .map(header_value)
                .collect(),
        ),
    );
    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        base,
        sources: role_sources(request.credentials, "credential_field"),
        rules: parse_rules(request.rules.unwrap_or_default()),
        private_key_error: None,
    }
}

fn ssh_key_document(request: SshKeyRequest, existing: Option<&SecretRow>) -> Document {
    // `SshKeySecret` normalizes both attributes: the name is stripped and the
    // public key keeps only "<type> <base64>" (comments are labels).
    let mut base = base_attributes(
        request.name.map(|name| name.trim().to_owned()),
        request.description,
        request.labels,
    );
    base.insert(
        "public_key".to_owned(),
        json!(normalize_public_key(request.public_key)),
    );

    let mut sources = Vec::new();
    let mut private_key_error = None;
    if let Some(source) = request.private_key.filter(|source| !source.is_blank()) {
        let mut draft = SourceDraft::new(source, None, None);
        if draft.source_type.as_deref() == Some("control_plane") {
            // An inline key's public key is derived, and its value is stored
            // normalized.
            base.remove("public_key");
            if let Some(secret) = draft
                .secret
                .as_deref()
                .filter(|secret| !secret.trim().is_empty())
            {
                draft.secret = Some(normalize_private_key(secret));
            }
            match derive_public_key(draft.secret.as_deref().unwrap_or_default()) {
                Ok(public_key) => {
                    base.insert("public_key".to_owned(), json!(public_key));
                }
                Err(message) => private_key_error = Some(message),
            }
        }
        sources.push(draft);
    }

    Document {
        foreign_id: parse_foreign_id(request.foreign_id, existing),
        base,
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
        .map(|(role, source)| SourceDraft::new(source, Some(&role), Some(role_kind)))
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
    fn text(&self, key: &str) -> Option<&str> {
        self.base.get(key).and_then(Value::as_str)
    }

    fn value(&self, key: &str) -> Value {
        self.base.get(key).cloned().unwrap_or(Value::Null)
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
            grant: self.text("grant").map(str::to_owned),
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
                request.public_key = self.text("public_key").map(str::to_owned);
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
        let grant = self.text("grant");
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

    /// `SyncConfigReplacement.equivalent?`: the base attributes and
    /// the sorted nested documents (with inline plaintext) are unchanged.
    fn equivalent(&self, existing: &SecretRow, existing_plain: &[Option<String>]) -> bool {
        if !self.base_matches(existing) {
            return false;
        }
        let mut existing_sources: Vec<Value> = Vec::new();
        for (source, plain) in existing.sources.iter().zip(existing_plain) {
            existing_sources.push(json!({
                "source_type": source.source_type,
                "config": source.config,
                "secret": plain,
                "role": source.role,
                "role_kind": source.role_kind,
                "broker_credential_id": source.broker_credential_id,
            }));
        }
        let mut candidate_sources: Vec<Value> = self
            .sources
            .iter()
            .map(|source| {
                json!({
                    "source_type": source.source_type,
                    "config": source.config,
                    "secret": source.secret,
                    "role": source.role,
                    "role_kind": source.role_kind,
                    "broker_credential_id": source.broker_credential_id,
                })
            })
            .collect();
        sort_documents(&mut existing_sources);
        sort_documents(&mut candidate_sources);
        if existing_sources != candidate_sources {
            return false;
        }
        let mut existing_rules: Vec<Value> = existing.rules.iter().map(rule_document).collect();
        let mut candidate_rules: Vec<Value> = self.rules.iter().map(rule_draft_document).collect();
        sort_documents(&mut existing_rules);
        sort_documents(&mut candidate_rules);
        existing_rules == candidate_rules
    }

    /// Every base attribute equals the stored value.
    fn base_matches(&self, existing: &SecretRow) -> bool {
        self.base
            .iter()
            .all(|(key, value)| existing_value(existing, key) == *value)
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

fn rule_document(rule: &RuleRow) -> Value {
    json!({
        "position": rule.position,
        "host": &rule.host,
        "cidr": &rule.cidr,
        "http_methods": &rule.http_methods,
        "paths": &rule.paths,
    })
}

fn rule_draft_document(rule: &RuleDocument) -> Value {
    json!({
        "position": rule.position,
        "host": &rule.host,
        "cidr": &rule.cidr,
        "http_methods": &rule.http_methods,
        "paths": &rule.paths,
    })
}

/// `SyncConfigReplacement.documents`: sort by role then position, both of which
/// already distinguish every record of one secret.
fn sort_documents(documents: &mut [Value]) {
    documents.sort_by(|left, right| {
        let key = |document: &Value| {
            (
                document
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                document
                    .get("position")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
            )
        };
        key(left).cmp(&key(right))
    });
}

fn existing_value(row: &SecretRow, key: &str) -> Value {
    match key {
        "foreign_id" => json!(row.foreign_id),
        "name" => json!(row.name),
        "description" => json!(row.description),
        "labels" => row.labels.clone(),
        "allowed_regions" => match &row.fields {
            SecretFields::AwsAuth {
                allowed_regions, ..
            } => allowed_regions.clone(),
            _ => Value::Null,
        },
        "allowed_services" => match &row.fields {
            SecretFields::AwsAuth {
                allowed_services, ..
            } => allowed_services.clone(),
            _ => Value::Null,
        },
        "grant" => match &row.fields {
            SecretFields::OauthToken { grant, .. } => json!(grant),
            _ => Value::Null,
        },
        "token_endpoint" => match &row.fields {
            SecretFields::OauthToken { token_endpoint, .. } => {
                json!(token_endpoint)
            }
            _ => Value::Null,
        },
        "audience" => match &row.fields {
            SecretFields::OauthToken { audience, .. } => json!(audience),
            _ => Value::Null,
        },
        "header" => match &row.fields {
            SecretFields::OauthToken { header, .. } => json!(header),
            _ => Value::Null,
        },
        "value_prefix" => match &row.fields {
            SecretFields::OauthToken { value_prefix, .. } => json!(value_prefix),
            _ => Value::Null,
        },
        "scopes" => match &row.fields {
            SecretFields::OauthToken { scopes, .. } => scopes.clone(),
            _ => Value::Null,
        },
        "timestamp_format" => match &row.fields {
            SecretFields::Hmac {
                timestamp_format, ..
            } => json!(timestamp_format),
            _ => Value::Null,
        },
        "signature_algorithm" => match &row.fields {
            SecretFields::Hmac {
                signature_algorithm,
                ..
            } => json!(signature_algorithm),
            _ => Value::Null,
        },
        "signature_key_encoding" => match &row.fields {
            SecretFields::Hmac {
                signature_key_encoding,
                ..
            } => json!(signature_key_encoding),
            _ => Value::Null,
        },
        "signature_output_encoding" => match &row.fields {
            SecretFields::Hmac {
                signature_output_encoding,
                ..
            } => json!(signature_output_encoding),
            _ => Value::Null,
        },
        "signature_message" => match &row.fields {
            SecretFields::Hmac {
                signature_message, ..
            } => json!(signature_message),
            _ => Value::Null,
        },
        "allow_chunked_body" => match &row.fields {
            SecretFields::Hmac {
                allow_chunked_body, ..
            } => json!(allow_chunked_body),
            _ => Value::Null,
        },
        "headers" => match &row.fields {
            SecretFields::Hmac { headers, .. } => headers.clone(),
            _ => Value::Null,
        },
        "public_key" => match &row.fields {
            SecretFields::SshKey { public_key } => json!(public_key),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// The parent columns of each kind, in bind order. `created_by_id` is last.
fn parent_columns(kind: SecretKind) -> &'static [&'static str] {
    match kind {
        SecretKind::AwsAuth => &[
            "foreign_id",
            "name",
            "description",
            "labels",
            "allowed_regions",
            "allowed_services",
            "created_by_id",
        ],
        SecretKind::OauthToken => &[
            "foreign_id",
            "name",
            "description",
            "labels",
            // `grant` is a reserved word in Postgres.
            "\"grant\"",
            "token_endpoint",
            "audience",
            "header",
            "value_prefix",
            "scopes",
            "created_by_id",
        ],
        SecretKind::Hmac => &[
            "foreign_id",
            "name",
            "description",
            "labels",
            "timestamp_format",
            "signature_algorithm",
            "signature_key_encoding",
            "signature_output_encoding",
            "signature_message",
            "allow_chunked_body",
            "headers",
            "created_by_id",
        ],
        SecretKind::SshKey => &[
            "foreign_id",
            "name",
            "description",
            "labels",
            "public_key",
            "created_by_id",
        ],
        _ => &[],
    }
}

async fn insert_parent(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    document: &Document,
    operator_id: Option<i64>,
) -> std::result::Result<i64, WriteError> {
    let columns = parent_columns(kind);
    if columns.is_empty() {
        return Err(WriteError::BadRequest(format!(
            "{} secrets are not writable through this API",
            kind.label()
        )));
    }
    let foreign_id = match &document.foreign_id {
        ForeignIdValue::Set(value) => value.clone(),
        ForeignIdValue::Omitted => None,
    };
    let text = |key: &str| document.text(key).map(str::to_owned);
    let value = |key: &str| document.value(key);
    let sql = write::insert_returning_sql(kind.table(), columns, "id");
    let query = sqlx::query_scalar::<_, i64>(&sql)
        .bind(foreign_id)
        .bind(text("name"))
        .bind(text("description"))
        .bind(value("labels"));
    let query = match kind {
        SecretKind::AwsAuth => query
            .bind(value("allowed_regions"))
            .bind(value("allowed_services")),
        SecretKind::OauthToken => query
            .bind(text("grant"))
            .bind(text("token_endpoint"))
            .bind(text("audience"))
            .bind(text("header"))
            .bind(text("value_prefix"))
            .bind(value("scopes")),
        SecretKind::Hmac => query
            .bind(text("timestamp_format"))
            .bind(text("signature_algorithm"))
            .bind(text("signature_key_encoding"))
            .bind(text("signature_output_encoding"))
            .bind(text("signature_message"))
            .bind(
                document
                    .value("allow_chunked_body")
                    .as_bool()
                    .unwrap_or(false),
            )
            .bind(value("headers")),
        SecretKind::SshKey => query.bind(text("public_key")),
        _ => unreachable!("checked above"),
    };
    Ok(query
        .bind(operator_id)
        .fetch_one(&mut **transaction)
        .await?)
}

async fn update_parent(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
    document: &Document,
) -> std::result::Result<(), WriteError> {
    let mut columns: Vec<&str> = vec!["name", "description", "labels"];
    columns.extend(parent_columns(kind).iter().copied().filter(|column| {
        !matches!(
            *column,
            "foreign_id" | "name" | "description" | "labels" | "created_by_id"
        )
    }));
    let text = |key: &str| document.text(key).map(str::to_owned);
    let value = |key: &str| document.value(key);
    let sql = write::update_sql(kind.table(), &columns);
    let query = sqlx::query(&sql)
        .bind(text("name"))
        .bind(text("description"))
        .bind(value("labels"));
    let query = match kind {
        SecretKind::AwsAuth => query
            .bind(value("allowed_regions"))
            .bind(value("allowed_services")),
        SecretKind::OauthToken => query
            .bind(text("grant"))
            .bind(text("token_endpoint"))
            .bind(text("audience"))
            .bind(text("header"))
            .bind(text("value_prefix"))
            .bind(value("scopes")),
        SecretKind::Hmac => query
            .bind(text("timestamp_format"))
            .bind(text("signature_algorithm"))
            .bind(text("signature_key_encoding"))
            .bind(text("signature_output_encoding"))
            .bind(text("signature_message"))
            .bind(
                document
                    .value("allow_chunked_body")
                    .as_bool()
                    .unwrap_or(false),
            )
            .bind(value("headers")),
        SecretKind::SshKey => query.bind(text("public_key")),
        _ => query,
    };
    query.bind(id).execute(&mut **transaction).await?;
    if let ForeignIdValue::Set(foreign_id) = &document.foreign_id {
        // Rails' `update_columns(foreign_id:)`: the identity changes without a
        // second `updated_at` bump.
        sqlx::query(&format!(
            "UPDATE {} SET foreign_id = $1 WHERE id = $2",
            kind.table()
        ))
        .bind(foreign_id)
        .bind(id)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

async fn delete_nested(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
) -> std::result::Result<(), WriteError> {
    for (table, column) in [
        ("secret_sources", kind.source_owner_column()),
        ("request_rules", kind.rule_owner_column()),
    ]
    .into_iter()
    .filter_map(|(table, column)| column.map(|column| (table, column)))
    {
        sqlx::query(&format!("DELETE FROM {table} WHERE {column} = $1"))
            .bind(id)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

async fn insert_sources(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
    sources: &[SourceDraft],
) -> std::result::Result<(), WriteError> {
    let Some(owner_column) = kind.source_owner_column() else {
        return Ok(());
    };
    let sql = write::insert_sql(
        "secret_sources",
        &[
            owner_column,
            "source_type",
            "config",
            "secret",
            "role",
            "role_kind",
            "broker_credential_id",
        ],
    );
    for source in sources {
        let encrypted = source
            .secret
            .as_deref()
            .map(encrypt)
            .transpose()
            .map_err(WriteError::Internal)?;
        sqlx::query(&sql)
            .bind(id)
            .bind(source.source_type.clone().unwrap_or_default())
            .bind(source.config.clone())
            .bind(encrypted)
            .bind(source.role.clone())
            .bind(source.role_kind.clone())
            .bind(source.broker_credential_id)
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

async fn insert_rules(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
    rules: &[RuleDocument],
) -> std::result::Result<(), WriteError> {
    let Some(owner_column) = kind.rule_owner_column() else {
        return Ok(());
    };
    let sql = write::insert_sql(
        "request_rules",
        &[
            owner_column,
            "host",
            "cidr",
            "position",
            "http_methods",
            "paths",
        ],
    );
    for rule in rules {
        sqlx::query(&sql)
            .bind(id)
            .bind(rule.host.clone())
            .bind(rule.cidr.clone())
            .bind(rule.position)
            .bind(rule.http_methods.clone())
            .bind(rule.paths.clone())
            .execute(&mut **transaction)
            .await?;
    }
    Ok(())
}

/// DB-backed write lifecycle tests. They skip locally when
/// `CONSOLE_DB_TEST_DATABASE_URL` is unset and panic under `CI`, matching the
/// crate's other schema tests.
#[cfg(test)]
mod db_tests {
    use serde_json::json;
    use sqlx::{PgPool, postgres::PgPoolOptions};

    use super::{BearerRequest, SshKeyRequest, write_secret};
    use crate::areas::secrets::{
        SecretFields, SecretKind, WriteTarget, decrypt, delete_secret, find_secret,
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
