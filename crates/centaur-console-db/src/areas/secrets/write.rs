//! Write paths for grantable secrets.
//!
//! Every kind is written by one pipeline. The kind's API layer builds one
//! [`Document`] — the parent columns the kind stores, its sources and rules, and
//! the identity — so the writer only does what every kind does:
//!
//! 1. resolve the target (creating it, or locking the existing row) and decrypt
//!    the stored inline values;
//! 2. build the document and validate the whole thing, reporting a nested error
//!    under its path key (`source.config`, `rules[0].cidr`);
//! 3. short-circuit an unchanged document, keeping the stored nested rows and
//!    cache versions;
//! 4. write the parent and replace its nested records in one transaction, then
//!    bump the sync-config version of every principal the secret reaches.
//!
//! Nothing is written for a rejected document, and the reported errors do not
//! depend on save order: the parent's errors come first and each nested error is
//! reported under its path key.
//!
//! The four Console kinds build and validate their document in
//! [`super::document`]; the four bearer-only kinds in [`super::more`]. This
//! module owns the writer they share, the deletes, and the secret-scoped role
//! grants.

use serde_json::{Map, Value, json};
use sqlx::query::Query;
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgArguments};

use super::document::Document as ConsoleDocument;
use super::{
    Errors, ForeignIdValue, GRANT_COLUMNS, GRANT_FROM, GrantRow, GrantTableRow, RuleDocument,
    RuleRow, SecretDocumentPayload, SecretFields, SecretKind, SecretRow, SourceDocument,
    WriteDocument, WriteError, WriteOptions, WriteTarget, decrypt, load_secret,
};
use crate::invalidate::{self, Scope};
use crate::model::{self, ConsoleModel};
use crate::write;

/// A completed write: the reloaded document and whether it was created.
#[derive(Debug)]
pub struct WriteResult {
    pub row: SecretRow,
    pub created: bool,
}

/// The document every kind writes.
#[derive(Debug)]
pub(super) struct Document {
    pub foreign_id: ForeignIdValue,
    /// The parent attributes by column, without `foreign_id` and `created_by_id`,
    /// which the writer adds.
    pub parent: Map<String, Value>,
    pub sources: Vec<SourceDraft>,
    pub rules: Vec<RuleDocument>,
    /// The inline SSH key parsing error, reported on `private_key`.
    pub private_key_error: Option<String>,
}

/// One credential source of the document, with the role columns the owning
/// model requires.
#[derive(Clone, Debug)]
pub(super) struct SourceDraft {
    pub role: Option<String>,
    pub role_kind: Option<String>,
    pub source_type: Option<String>,
    pub secret: Option<String>,
    pub config: Value,
    pub broker_credential_id: Option<i64>,
    pub broker_resolved: bool,
}

impl SourceDraft {
    /// The single, role-less source the four Console kinds store.
    pub fn from_document(source: SourceDocument) -> Self {
        Self {
            role: None,
            role_kind: None,
            source_type: source.source_type,
            secret: source.secret,
            config: source.config,
            broker_credential_id: None,
            broker_resolved: true,
        }
    }

    /// The stored-document JSON the replacement comparison uses.
    fn json(&self) -> Value {
        json!({
            "source_type": self.source_type.clone().unwrap_or_default(),
            "config": self.config,
            "secret": self.secret,
            "role": self.role,
            "role_kind": self.role_kind,
            "broker_credential_id": self.broker_credential_id,
        })
    }

    /// `SecretSource` validation errors. The bearer-only kinds render
    /// `sources: ["is invalid"]`, the SSH key renders the nested keys, and the
    /// Console kinds render them under the source's own path key.
    pub fn errors(&self, role_owner: bool) -> Errors {
        let source = SourceDocument {
            source_type: self.source_type.clone(),
            secret: self.secret.clone(),
            config: self.config.clone(),
        };
        let mut errors = source.errors(self.broker_resolved);
        if role_owner && super::blank(self.role.as_deref()) {
            errors.add(
                "role",
                "can't be blank for a oauth_token_secret or hmac_secret or aws_auth_secret source",
            );
        }
        errors
    }
}

/// The API a document came from. The two write paths differ only here: the
/// Console SPA leaves an unchanged parent row untouched (so its `updated_at`
/// stays) and reports a rejected document before short-circuiting an unchanged
/// one; the bearer API rewrites the parent on every replacement and
/// short-circuits first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Console,
    Bearer,
}

/// A validated-or-not document plus the stored plaintext it is compared with.
pub(super) struct Pending {
    pub document: Document,
    pub errors: Errors,
    pub existing_plain: Vec<Option<String>>,
}

/// The write entry point for every kind: the console/bearer document or the
/// bearer-only request.
pub async fn write_secret(
    pool: &PgPool,
    kind: SecretKind,
    target: WriteTarget,
    document: WriteDocument,
    operator_id: Option<i64>,
    options: WriteOptions,
) -> std::result::Result<WriteResult, WriteError> {
    match document {
        WriteDocument::Console(payload) => {
            write_console_document(pool, kind, &target, &payload, operator_id, options).await
        }
        WriteDocument::Bearer(request) => {
            super::more::write_secret(pool, target, request, operator_id).await
        }
    }
}

/// `RecordNotUnique` is retried once for an upsert addressed by foreign id,
/// exactly like `assign_upsert_with_retry`.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some("23505"))
}

async fn write_console_document(
    pool: &PgPool,
    kind: SecretKind,
    target: &WriteTarget,
    payload: &SecretDocumentPayload,
    operator_id: Option<i64>,
    options: WriteOptions,
) -> std::result::Result<WriteResult, WriteError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match write_console_once(pool, kind, target, payload, operator_id, options).await {
            Err(WriteError::Internal(crate::Error::Database(error)))
                if is_unique_violation(&error)
                    && matches!(target, WriteTarget::ForeignId(_))
                    && attempts < 2 =>
            {
                continue;
            }
            result => return result,
        }
    }
}

async fn write_console_once(
    pool: &PgPool,
    kind: SecretKind,
    target: &WriteTarget,
    payload: &SecretDocumentPayload,
    operator_id: Option<i64>,
    options: WriteOptions,
) -> std::result::Result<WriteResult, WriteError> {
    let mut transaction = pool.begin().await?;
    let (existing, was_new) = resolve_target(&mut transaction, kind, target).await?;
    let existing_secret = existing
        .as_ref()
        .and_then(SecretRow::source)
        .and_then(|source| source.secret.as_deref())
        .map(decrypt)
        .transpose()?;

    let document = ConsoleDocument::from_payload(
        kind,
        payload,
        existing.as_ref(),
        existing_secret.as_deref(),
        options,
    )?;
    let mut document = document;
    if let (WriteTarget::ForeignId(identifier), None) = (target, existing.as_ref())
        && payload.foreign_id.is_none()
    {
        document.foreign_id = ForeignIdValue::Set(Some(identifier.clone()));
    }

    let foreign_id_taken = foreign_id_taken(
        &mut transaction,
        kind,
        &document.foreign_id,
        existing.as_ref(),
    )
    .await?;

    // Resolve a token_broker source's credential reference before validation so
    // the source row stores the resolved broker link.
    let mut sources: Vec<SourceDraft> = document
        .source
        .iter()
        .cloned()
        .map(SourceDraft::from_document)
        .collect();
    resolve_brokers(&mut transaction, &mut sources).await?;

    // Validate the whole document before writing anything. A nested error is
    // reported under the path key the SPA shows it beside; parent errors are
    // never dropped because a nested record also failed.
    let mut errors = document.parent_errors(kind, foreign_id_taken);
    for source in &sources {
        let source_key = source_key(kind);
        for (attribute, messages) in source.errors(false).entries() {
            for message in messages {
                errors.add(&format!("{source_key}.{attribute}"), message.clone());
            }
        }
    }
    for (index, rule) in document.rules.iter().enumerate() {
        for (attribute, messages) in rule.errors().entries() {
            for message in messages {
                errors.add(&format!("rules[{index}].{attribute}"), message.clone());
            }
        }
    }

    let pending = Pending {
        document: document.into_write(kind, sources),
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
        Family::Console,
    )
    .await
}

/// The shared replacement write: the short-circuit, validation, the parent row,
/// the nested records, and the cache bump.
#[allow(clippy::too_many_arguments)] // One call site per family; the arguments are the transaction's state.
pub(super) async fn run_write(
    pool: &PgPool,
    kind: SecretKind,
    mut transaction: Transaction<'_, Postgres>,
    existing: Option<&SecretRow>,
    was_new: bool,
    pending: Pending,
    operator_id: Option<i64>,
    family: Family,
) -> std::result::Result<WriteResult, WriteError> {
    let unchanged = existing
        .is_some_and(|row| equivalent(kind, row, &pending.existing_plain, &pending.document));
    if unchanged && family == Family::Bearer {
        // An unchanged document keeps the nested rows, the cache versions, and
        // the id sequences untouched.
        let row = existing.expect("equivalence requires an existing record");
        transaction.rollback().await?;
        return Ok(WriteResult {
            row: row.clone(),
            created: false,
        });
    }
    if !pending.errors.is_empty() {
        // Validate first, write after: a rejected document leaves no rows behind
        // and consumes no ids.
        transaction.rollback().await?;
        return Err(WriteError::Validation(pending.errors.into_value()));
    }
    if unchanged {
        let row = existing.expect("equivalence requires an existing record");
        transaction.rollback().await?;
        return Ok(WriteResult {
            row: row.clone(),
            created: false,
        });
    }

    let id = persist_parent(
        &mut transaction,
        kind,
        &pending.document,
        existing,
        operator_id,
        family,
    )
    .await?;
    replace_nested(&mut transaction, kind, id, &pending.document).await?;
    if kind == SecretKind::SshKey {
        // The key's source belongs_to it with `touch: true`, so replacing the
        // source also bumps the key's updated_at.
        sqlx::query(&write::touch_sql(kind.table()))
            .bind(id)
            .execute(&mut *transaction)
            .await?;
    }
    invalidate::invalidate(&mut transaction, Scope::Secret { kind, id }).await?;
    transaction.commit().await?;
    let row = reload(pool, kind, id).await?;
    Ok(WriteResult {
        row,
        created: was_new,
    })
}

pub(super) async fn resolve_target(
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

pub(super) fn not_found(kind: SecretKind) -> WriteError {
    WriteError::NotFound(format!("{} not found", kind.model_name()))
}

/// Whether another row of this kind already holds the document's foreign id.
pub(super) async fn foreign_id_taken(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    foreign_id: &ForeignIdValue,
    existing: Option<&SecretRow>,
) -> std::result::Result<bool, WriteError> {
    let ForeignIdValue::Set(Some(foreign_id)) = foreign_id else {
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

/// Resolves every `token_broker` source's credential reference, so the stored
/// source row carries the broker link. An unresolved reference is a validation
/// error on the source, not a lookup failure.
pub(super) async fn resolve_brokers(
    transaction: &mut Transaction<'_, Postgres>,
    sources: &mut [SourceDraft],
) -> std::result::Result<(), WriteError> {
    for source in sources {
        source.broker_credential_id = None;
        source.broker_resolved = true;
        if source.source_type.as_deref() != Some("token_broker") {
            continue;
        }
        let reference = source
            .config
            .get("credential_id")
            .map(super::value_to_string)
            .filter(|value| !value.is_empty());
        source.broker_credential_id = match reference {
            Some(reference) => match model::BrokerCredential::decode_oid(&reference) {
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

/// The decrypted inline value of each stored source, for the replacement
/// comparison.
pub(super) fn stored_plaintexts(
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

fn source_key(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::PgDsn => "dsn",
        SecretKind::GcpAuth | SecretKind::GcpIdToken => "keyfile",
        _ => "source",
    }
}

// ---------------------------------------------------------------------------
// The parent row
// ---------------------------------------------------------------------------

/// How a parent column stores its value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Text,
    Json,
    Bool,
}

/// The parent columns of each kind in bind order. `foreign_id` and
/// `created_by_id` are written separately.
fn parent_columns(kind: SecretKind) -> &'static [(&'static str, Column)] {
    match kind {
        SecretKind::Static => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("labels", Column::Json),
            ("kind", Column::Text),
            ("inject_config", Column::Json),
            ("replace_config", Column::Json),
        ],
        SecretKind::PgDsn => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("database", Column::Text),
            ("role", Column::Text),
            ("labels", Column::Json),
            ("settings", Column::Json),
        ],
        SecretKind::GcpAuth => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("subject", Column::Text),
            ("labels", Column::Json),
            ("credentials_provider", Column::Json),
            ("scopes", Column::Json),
        ],
        SecretKind::GcpIdToken => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("audience", Column::Text),
            ("header", Column::Text),
            ("labels", Column::Json),
        ],
        SecretKind::AwsAuth => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("labels", Column::Json),
            ("allowed_regions", Column::Json),
            ("allowed_services", Column::Json),
        ],
        SecretKind::OauthToken => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("labels", Column::Json),
            // `grant` is a reserved word in Postgres.
            ("\"grant\"", Column::Text),
            ("token_endpoint", Column::Text),
            ("audience", Column::Text),
            ("header", Column::Text),
            ("value_prefix", Column::Text),
            ("scopes", Column::Json),
        ],
        SecretKind::Hmac => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("labels", Column::Json),
            ("timestamp_format", Column::Text),
            ("signature_algorithm", Column::Text),
            ("signature_key_encoding", Column::Text),
            ("signature_output_encoding", Column::Text),
            ("signature_message", Column::Text),
            ("allow_chunked_body", Column::Bool),
            ("headers", Column::Json),
        ],
        SecretKind::SshKey => &[
            ("name", Column::Text),
            ("description", Column::Text),
            ("labels", Column::Json),
            ("public_key", Column::Text),
        ],
    }
}

/// A JSON parent value: the absent key is `null`.
fn json_value(parent: &Map<String, Value>, column: &str) -> Value {
    parent.get(column).cloned().unwrap_or(Value::Null)
}

/// One parent column's value, bound as its column type.
enum Bind {
    Text(Option<String>),
    Json(Value),
    Bool(bool),
}

fn parent_binds(kind: SecretKind, parent: &Map<String, Value>) -> Vec<Bind> {
    parent_columns(kind)
        .iter()
        .map(|(column, storage)| match storage {
            Column::Text => Bind::Text(
                parent
                    .get(*column)
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ),
            Column::Json => Bind::Json(json_value(parent, column)),
            Column::Bool => Bind::Bool(
                parent
                    .get(*column)
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
        })
        .collect()
}

fn bind_all<'q>(
    mut query: Query<'q, Postgres, PgArguments>,
    binds: Vec<Bind>,
) -> Query<'q, Postgres, PgArguments> {
    for bind in binds {
        query = match bind {
            Bind::Text(value) => query.bind(value),
            Bind::Json(value) => query.bind((!value.is_null()).then_some(value)),
            Bind::Bool(value) => query.bind(value),
        };
    }
    query
}

/// Inserts or updates the parent row, from the kind's column list. The Console
/// SPA leaves an unchanged row alone; the bearer API rewrites it, and writes a
/// supplied foreign id without a second `updated_at` bump.
async fn persist_parent(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    document: &Document,
    existing: Option<&SecretRow>,
    operator_id: Option<i64>,
    family: Family,
) -> std::result::Result<i64, WriteError> {
    let foreign_id = match &document.foreign_id {
        ForeignIdValue::Set(value) => value.clone(),
        ForeignIdValue::Omitted => existing.and_then(|row| row.foreign_id.clone()),
    };
    let Some(existing) = existing else {
        let mut columns = vec!["foreign_id"];
        columns.extend(parent_columns(kind).iter().map(|(column, _)| *column));
        columns.push("created_by_id");
        let sql = write::insert_returning_sql(kind.table(), &columns, "id");
        let row = bind_all(
            sqlx::query(&sql).bind(foreign_id),
            parent_binds(kind, &document.parent),
        )
        .bind(operator_id)
        .fetch_one(&mut **transaction)
        .await?;
        return Ok(row.get("id"));
    };

    let foreign_changed = match &document.foreign_id {
        ForeignIdValue::Set(value) => existing.foreign_id != *value,
        ForeignIdValue::Omitted => false,
    };
    if family == Family::Console {
        if !foreign_changed && parent_matches(kind, &document.parent, existing) {
            return Ok(existing.id);
        }
        let mut columns: Vec<&str> = parent_columns(kind)
            .iter()
            .map(|(column, _)| *column)
            .collect();
        if foreign_changed {
            columns.push("foreign_id");
        }
        let sql = write::update_sql(kind.table(), &columns);
        let mut query = bind_all(sqlx::query(&sql), parent_binds(kind, &document.parent));
        if foreign_changed {
            query = query.bind(foreign_id);
        }
        query.bind(existing.id).execute(&mut **transaction).await?;
        return Ok(existing.id);
    }

    let columns: Vec<&str> = parent_columns(kind)
        .iter()
        .map(|(column, _)| *column)
        .collect();
    let sql = write::update_sql(kind.table(), &columns);
    bind_all(sqlx::query(&sql), parent_binds(kind, &document.parent))
        .bind(existing.id)
        .execute(&mut **transaction)
        .await?;
    if let ForeignIdValue::Set(foreign_id) = &document.foreign_id {
        // Rails' `update_columns(foreign_id:)`: the identity changes without a
        // second `updated_at` bump.
        sqlx::query(&format!(
            "UPDATE {} SET foreign_id = $1 WHERE id = $2",
            kind.table()
        ))
        .bind(foreign_id)
        .bind(existing.id)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(existing.id)
}

/// Replaces the parent's nested records: every stored source and rule is
/// destroyed and the document's are inserted, keeping each rule's position.
async fn replace_nested(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
    document: &Document,
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

    if let Some(owner_column) = kind.source_owner_column() {
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
        for source in &document.sources {
            let encrypted = source
                .secret
                .as_deref()
                .map(super::encrypt)
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
    }
    if let Some(owner_column) = kind.rule_owner_column() {
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
        for rule in &document.rules {
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
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Equivalence
// ---------------------------------------------------------------------------

/// `SyncConfigReplacement.equivalent?`: the identity (when the document sets
/// one), the parent attributes, the sources with their inline plaintext, and the
/// rules are unchanged. Nested records are compared as sorted documents, so the
/// order they were stored in does not matter.
fn equivalent(
    kind: SecretKind,
    existing: &SecretRow,
    existing_plain: &[Option<String>],
    document: &Document,
) -> bool {
    if let ForeignIdValue::Set(foreign_id) = &document.foreign_id
        && existing.foreign_id != *foreign_id
    {
        return false;
    }
    parent_matches(kind, &document.parent, existing)
        && sources_match(&existing.sources, existing_plain, &document.sources)
        && rules_match(&existing.rules, &document.rules)
}

/// Every parent column the kind stores equals the stored value.
fn parent_matches(kind: SecretKind, parent: &Map<String, Value>, existing: &SecretRow) -> bool {
    parent_columns(kind)
        .iter()
        .all(|(column, _)| stored_value(kind, existing, column) == json_value(parent, column))
}

fn sources_match(
    existing: &[super::SourceRow],
    existing_plain: &[Option<String>],
    sources: &[SourceDraft],
) -> bool {
    if existing.len() != sources.len() {
        return false;
    }
    let mut stored: Vec<Value> = existing
        .iter()
        .zip(existing_plain)
        .map(|(source, plain)| {
            json!({
                "source_type": source.source_type,
                "config": source.config,
                "secret": plain,
                "role": source.role,
                "role_kind": source.role_kind,
                "broker_credential_id": source.broker_credential_id,
            })
        })
        .collect();
    let mut draft: Vec<Value> = sources.iter().map(SourceDraft::json).collect();
    sort_documents(&mut stored);
    sort_documents(&mut draft);
    stored == draft
}

fn rules_match(existing: &[RuleRow], rules: &[RuleDocument]) -> bool {
    if existing.len() != rules.len() {
        return false;
    }
    let mut stored: Vec<Value> = existing
        .iter()
        .map(|rule| {
            rule_json(
                rule.position,
                &rule.host,
                &rule.cidr,
                &rule.http_methods,
                &rule.paths,
            )
        })
        .collect();
    let mut draft: Vec<Value> = rules
        .iter()
        .map(|rule| {
            rule_json(
                rule.position,
                &rule.host,
                &rule.cidr,
                &rule.http_methods,
                &rule.paths,
            )
        })
        .collect();
    sort_documents(&mut stored);
    sort_documents(&mut draft);
    stored == draft
}

fn rule_json(
    position: i32,
    host: &Option<String>,
    cidr: &Option<String>,
    http_methods: &Value,
    paths: &Value,
) -> Value {
    json!({
        "position": position,
        "host": host,
        "cidr": cidr,
        "http_methods": http_methods,
        "paths": paths,
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

/// A stored parent attribute by column.
fn stored_value(kind: SecretKind, row: &SecretRow, column: &str) -> Value {
    match column {
        "name" => json!(row.name),
        "description" => json!(row.description),
        "labels" => row.labels.clone(),
        _ => match (&row.fields, kind, column) {
            (SecretFields::Static { kind, .. }, SecretKind::Static, "kind") => json!(kind),
            (SecretFields::Static { inject_config, .. }, SecretKind::Static, "inject_config") => {
                inject_config.clone()
            }
            (SecretFields::Static { replace_config, .. }, SecretKind::Static, "replace_config") => {
                replace_config.clone()
            }
            (SecretFields::PgDsn { database, .. }, SecretKind::PgDsn, "database") => {
                json!(database)
            }
            (SecretFields::PgDsn { role, .. }, SecretKind::PgDsn, "role") => json!(role),
            (SecretFields::PgDsn { settings, .. }, SecretKind::PgDsn, "settings") => {
                settings.clone()
            }
            (
                SecretFields::GcpAuth {
                    credentials_provider,
                    ..
                },
                SecretKind::GcpAuth,
                "credentials_provider",
            ) => credentials_provider.clone(),
            (SecretFields::GcpAuth { subject, .. }, SecretKind::GcpAuth, "subject") => {
                json!(subject)
            }
            (SecretFields::GcpAuth { scopes, .. }, SecretKind::GcpAuth, "scopes") => scopes.clone(),
            (SecretFields::GcpIdToken { audience, .. }, SecretKind::GcpIdToken, "audience") => {
                json!(audience)
            }
            (SecretFields::GcpIdToken { header, .. }, SecretKind::GcpIdToken, "header") => {
                json!(header)
            }
            (
                SecretFields::AwsAuth {
                    allowed_regions, ..
                },
                SecretKind::AwsAuth,
                "allowed_regions",
            ) => allowed_regions.clone(),
            (
                SecretFields::AwsAuth {
                    allowed_services, ..
                },
                SecretKind::AwsAuth,
                "allowed_services",
            ) => allowed_services.clone(),
            (SecretFields::OauthToken { grant, .. }, SecretKind::OauthToken, "grant") => {
                json!(grant)
            }
            (
                SecretFields::OauthToken { token_endpoint, .. },
                SecretKind::OauthToken,
                "token_endpoint",
            ) => json!(token_endpoint),
            (SecretFields::OauthToken { audience, .. }, SecretKind::OauthToken, "audience") => {
                json!(audience)
            }
            (SecretFields::OauthToken { header, .. }, SecretKind::OauthToken, "header") => {
                json!(header)
            }
            (
                SecretFields::OauthToken { value_prefix, .. },
                SecretKind::OauthToken,
                "value_prefix",
            ) => json!(value_prefix),
            (SecretFields::OauthToken { scopes, .. }, SecretKind::OauthToken, "scopes") => {
                scopes.clone()
            }
            (
                SecretFields::Hmac {
                    timestamp_format, ..
                },
                SecretKind::Hmac,
                "timestamp_format",
            ) => json!(timestamp_format),
            (
                SecretFields::Hmac {
                    signature_algorithm,
                    ..
                },
                SecretKind::Hmac,
                "signature_algorithm",
            ) => json!(signature_algorithm),
            (
                SecretFields::Hmac {
                    signature_key_encoding,
                    ..
                },
                SecretKind::Hmac,
                "signature_key_encoding",
            ) => json!(signature_key_encoding),
            (
                SecretFields::Hmac {
                    signature_output_encoding,
                    ..
                },
                SecretKind::Hmac,
                "signature_output_encoding",
            ) => json!(signature_output_encoding),
            (
                SecretFields::Hmac {
                    signature_message, ..
                },
                SecretKind::Hmac,
                "signature_message",
            ) => json!(signature_message),
            (
                SecretFields::Hmac {
                    allow_chunked_body, ..
                },
                SecretKind::Hmac,
                "allow_chunked_body",
            ) => json!(allow_chunked_body),
            (SecretFields::Hmac { headers, .. }, SecretKind::Hmac, "headers") => headers.clone(),
            (SecretFields::SshKey { public_key }, SecretKind::SshKey, "public_key") => {
                json!(public_key)
            }
            _ => Value::Null,
        },
    }
}

/// `destroy!`: cascades to the secret's sources, rules, and grants. Rails'
/// `SyncConfigCacheInvalidation` defers the parent's bump to commit, where the
/// secret's grants are gone and the bump reaches no principal, so the destroyed
/// grants' own bumps are what reach principals.
pub async fn delete_secret(
    pool: &PgPool,
    kind: SecretKind,
    id: i64,
) -> std::result::Result<(), WriteError> {
    let mut transaction = pool.begin().await?;
    let locked = sqlx::query(&format!(
        "SELECT id FROM {} WHERE id = $1 FOR UPDATE",
        kind.table()
    ))
    .bind(id)
    .fetch_optional(&mut *transaction)
    .await?;
    if locked.is_none() {
        return Err(not_found(kind));
    }

    // The four kinds this module's document writes serve bump the owner while
    // its grants still exist; the bearer-only kinds defer to commit. Both are
    // the behavior the two areas shipped with, keyed by kind here.
    if kind.editable() {
        invalidate::invalidate(&mut transaction, Scope::Secret { kind, id }).await?;
    }
    let grants: Vec<GranteeRow> = sqlx::query_as(&format!(
        "SELECT principal_id, role_id FROM grants WHERE {} = $1",
        kind.grant_column()
    ))
    .bind(id)
    .fetch_all(&mut *transaction)
    .await?;
    for grant in grants {
        invalidate::invalidate(
            &mut transaction,
            Scope::Grantee {
                principal_id: grant.principal_id,
                role_id: grant.role_id,
            },
        )
        .await?;
    }
    sqlx::query(&format!(
        "DELETE FROM grants WHERE {} = $1",
        kind.grant_column()
    ))
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    for (table, column) in [
        ("secret_sources", kind.source_owner_column()),
        ("request_rules", kind.rule_owner_column()),
    ]
    .into_iter()
    .filter_map(|(table, column)| column.map(|column| (table, column)))
    {
        sqlx::query(&format!("DELETE FROM {table} WHERE {column} = $1"))
            .bind(id)
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query(&format!("DELETE FROM {} WHERE id = $1", kind.table()))
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

/// A grant's grantee columns, for the invalidation the destroy performs.
#[derive(Debug, sqlx::FromRow)]
struct GranteeRow {
    principal_id: Option<i64>,
    role_id: Option<i64>,
}

pub async fn create_role_grant(
    pool: &PgPool,
    kind: SecretKind,
    secret_id: i64,
    role_id: i64,
    operator_id: Option<i64>,
) -> std::result::Result<(GrantRow, bool), WriteError> {
    let mut transaction = pool.begin().await?;
    let existing = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT id FROM grants WHERE {} = $1 AND role_id = $2",
        kind.grant_column()
    ))
    .bind(secret_id)
    .bind(role_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let (id, created) = match existing {
        Some(id) => (id, false),
        None => {
            sqlx::query(&format!(
                "INSERT INTO grants ({}, role_id, priority, created_by_id, created_at, updated_at) \
                 VALUES ($1, $2, 0, $3, now(), now())",
                kind.grant_column()
            ))
            .bind(secret_id)
            .bind(role_id)
            .bind(operator_id)
            .execute(&mut *transaction)
            .await
            .map_err(crate::Error::Database)?;
            let id = sqlx::query_scalar::<_, i64>(&format!(
                "SELECT id FROM grants WHERE {} = $1 AND role_id = $2",
                kind.grant_column()
            ))
            .bind(secret_id)
            .bind(role_id)
            .fetch_one(&mut *transaction)
            .await?;
            invalidate::invalidate(
                &mut transaction,
                Scope::Grantee {
                    principal_id: None,
                    role_id: Some(role_id),
                },
            )
            .await?;
            (id, true)
        }
    };
    let grant = load_grant(&mut transaction, kind, secret_id, id)
        .await?
        .ok_or_else(|| WriteError::NotFound("grant disappeared".to_owned()))?;
    transaction.commit().await?;
    Ok((grant, created))
}

async fn load_grant(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    secret_id: i64,
    id: i64,
) -> std::result::Result<Option<GrantRow>, WriteError> {
    let row: Option<GrantTableRow> = sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} {GRANT_FROM} \
         WHERE g.id = $1 AND g.{} = $2 AND g.role_id IS NOT NULL",
        kind.grant_column()
    ))
    .bind(id)
    .bind(secret_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(row.map(GrantRow::from))
}

pub async fn delete_role_grant(
    pool: &PgPool,
    kind: SecretKind,
    secret_id: i64,
    grant_id: i64,
) -> std::result::Result<(), WriteError> {
    let mut transaction = pool.begin().await?;
    let row: Option<GranteeRow> = sqlx::query_as(&format!(
        "SELECT principal_id, role_id FROM grants WHERE id = $1 AND {} = $2 \
         AND role_id IS NOT NULL FOR UPDATE",
        kind.grant_column()
    ))
    .bind(grant_id)
    .bind(secret_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = row else {
        return Err(WriteError::NotFound("Grant not found".to_owned()));
    };
    invalidate::invalidate(
        &mut transaction,
        Scope::Grantee {
            principal_id: row.principal_id,
            role_id: row.role_id,
        },
    )
    .await?;
    sqlx::query("DELETE FROM grants WHERE id = $1")
        .bind(grant_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}
