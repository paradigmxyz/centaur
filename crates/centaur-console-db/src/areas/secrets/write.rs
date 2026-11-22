//! Write paths for grantable secrets.
//!
//! The three APIs share one document (see [`Document`]) but differ in how it is
//! produced and which nested records are allowed:
//!
//! - bearer `PUT/PATCH` is a full replacement (no retained inline secret);
//! - the console SPA keeps an inline value it omits.
//!
//! Every write validates the whole document first, then writes the parent and
//! its nested records in one transaction. Nothing is written for a rejected
//! document, and the reported errors do not depend on save order: the parent's
//! errors come first and each nested error is reported under its path key
//! (`source.config`, `rules[0].cidr`).
//!
//! The four bearer-only kinds' documents live in [`super::more`]; this module
//! owns the shared entry points ([`write_secret`], [`delete_secret`]) and the
//! secret-scoped role grants.

use serde_json::{Map, Value, json};
use sqlx::{PgPool, Postgres, Transaction};

use super::document::encrypt;
use super::{
    Document, DocumentFields, Errors, ForeignIdValue, GRANT_COLUMNS, GRANT_FROM, GrantRow,
    GrantTableRow, RuleDocument, SecretDocumentPayload, SecretFields, SecretKind, SecretRow,
    SourceDocument, SourceRow, WriteDocument, WriteError, WriteOptions, WriteTarget, decrypt,
    document::value_to_string, load_secret,
};
use crate::invalidate::{self, Scope, SecretChanges};
use crate::model::{self, ConsoleModel};
use crate::write;

/// A completed write: the reloaded document and whether it was created.
#[derive(Debug)]
pub struct WriteResult {
    pub row: SecretRow,
    pub created: bool,
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
            write_document(pool, kind, &target, &payload, operator_id, options).await
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

async fn write_document(
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
        match write_once(pool, kind, target, payload, operator_id, options).await {
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

async fn write_once(
    pool: &PgPool,
    kind: SecretKind,
    target: &WriteTarget,
    payload: &SecretDocumentPayload,
    operator_id: Option<i64>,
    options: WriteOptions,
) -> std::result::Result<WriteResult, WriteError> {
    let mut transaction = pool.begin().await?;
    let (existing, was_new) = resolve_target(&mut transaction, kind, target).await?;
    let existing_plain = match existing.as_ref().and_then(|row| row.source()) {
        Some(source) => match source.secret.as_deref() {
            Some(ciphertext) => Some(decrypt(ciphertext)?),
            None => None,
        },
        None => None,
    };

    let mut document = Document::from_payload(
        kind,
        payload,
        existing.as_ref(),
        existing_plain.as_deref(),
        options,
    )?;
    if let (WriteTarget::ForeignId(identifier), None) = (target, existing.as_ref())
        && payload.foreign_id.is_none()
    {
        document.foreign_id = ForeignIdValue::Set(Some(identifier.clone()));
    }

    let foreign_id_taken = match &document.foreign_id {
        ForeignIdValue::Set(Some(foreign_id)) if !foreign_id.is_empty() => {
            let excluded = existing.as_ref().map(|row| row.id).unwrap_or_default();
            sqlx::query_scalar::<_, bool>(&format!(
                "SELECT EXISTS (SELECT 1 FROM {} WHERE foreign_id = $1 AND id <> $2)",
                kind.table()
            ))
            .bind(foreign_id)
            .bind(excluded)
            .fetch_one(&mut *transaction)
            .await?
        }
        _ => false,
    };

    // Resolve a token_broker source's credential reference before validation so
    // the source row stores the resolved broker link.
    let broker_id = resolve_broker_reference(&mut transaction, document.source.as_ref()).await?;
    let source_errors = document
        .source
        .as_ref()
        .map(|source| source.errors(broker_id.is_some()));
    let rule_errors: Vec<Errors> = document.rules.iter().map(RuleDocument::errors).collect();

    // Validate the whole document before writing anything. A nested error is
    // reported under the path key the SPA shows it beside; parent errors are
    // never dropped because a nested record also failed.
    let mut errors = document.parent_errors(kind, existing.as_ref(), foreign_id_taken);
    if let Some(source_errors) = source_errors.as_ref() {
        let source_key = source_key(kind);
        for (attribute, messages) in source_errors.entries() {
            for message in messages {
                errors.add(&format!("{source_key}.{attribute}"), message.clone());
            }
        }
    }
    for (index, rule_errors) in rule_errors.iter().enumerate() {
        for (attribute, messages) in rule_errors.entries() {
            for message in messages {
                errors.add(&format!("rules[{index}].{attribute}"), message.clone());
            }
        }
    }
    if !errors.is_empty() {
        transaction.rollback().await?;
        return Err(WriteError::Validation(errors.into_value()));
    }

    if !was_new
        && let Some(existing) = existing.as_ref()
        && equivalent(
            kind,
            existing,
            existing_plain.as_deref(),
            &document,
            broker_id,
        )
    {
        transaction.rollback().await?;
        return Ok(WriteResult {
            row: existing.clone(),
            created: false,
        });
    }

    let (id, parent_changed) = persist_parent(
        &mut transaction,
        kind,
        &document,
        existing.as_ref(),
        operator_id,
    )
    .await?;
    replace_nested(&mut transaction, kind, id, &document, broker_id).await?;
    let changes = SecretChanges::replaced(
        was_new || parent_changed,
        usize::from(
            existing
                .as_ref()
                .map(|row| row.source().is_some())
                .unwrap_or(false),
        ),
        usize::from(document.source.is_some()),
        existing
            .as_ref()
            .map(|row| row.rules.len())
            .unwrap_or_default(),
        document.rules.len(),
    );
    invalidate::invalidate(&mut transaction, Scope::secret_write(kind, id, changes)).await?;
    transaction.commit().await?;
    let row = reload(pool, kind, id).await?;
    Ok(WriteResult {
        row,
        created: was_new,
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

pub(super) fn not_found(kind: SecretKind) -> WriteError {
    WriteError::NotFound(format!("{} not found", kind.model_name()))
}

async fn resolve_broker_reference(
    transaction: &mut Transaction<'_, Postgres>,
    source: Option<&SourceDocument>,
) -> std::result::Result<Option<i64>, WriteError> {
    let Some(source) = source else {
        return Ok(None);
    };
    if source.source_type.as_deref() != Some("token_broker") {
        return Ok(None);
    }
    let Some(reference) = source
        .config
        .get("credential_id")
        .map(value_to_string)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let id = if let Some(id) = model::BrokerCredential::decode_oid(&reference) {
        sqlx::query_scalar::<_, i64>("SELECT id FROM broker_credentials WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await?
    } else {
        sqlx::query_scalar::<_, i64>("SELECT id FROM broker_credentials WHERE foreign_id = $1")
            .bind(&reference)
            .fetch_optional(&mut **transaction)
            .await?
    };
    Ok(id)
}

/// A JSON value bound to a nullable jsonb column: an explicit `null` is `NULL`,
/// like `optional_json_value` before it.
fn optional_json(value: &Value) -> Option<Value> {
    (!value.is_null()).then(|| value.clone())
}

/// Inserts or updates the parent row, returning its id and whether any parent
/// attribute actually changed (Rails' `changes_to_save` for cache bumps).
async fn persist_parent(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    document: &Document,
    existing: Option<&SecretRow>,
    operator_id: Option<i64>,
) -> std::result::Result<(i64, bool), WriteError> {
    let foreign_id = match &document.foreign_id {
        ForeignIdValue::Omitted => existing.and_then(|row| row.foreign_id.clone()),
        ForeignIdValue::Set(value) => value.clone(),
    };
    match (kind, &document.fields) {
        (
            SecretKind::Static,
            DocumentFields::Static {
                kind: static_kind,
                inject_config,
                replace_config,
            },
        ) => {
            let static_kind = static_kind.clone().unwrap_or_else(|| "custom".to_owned());
            if let Some(existing) = existing {
                let changed = json!(existing.name) != json!(document.name)
                    || json!(existing.description) != json!(document.description)
                    || existing.labels != document.labels
                    || matches!(&existing.fields, SecretFields::Static { kind, inject_config: existing_inject, replace_config: existing_replace }
                        if kind != &static_kind
                            || existing_inject != inject_config
                            || existing_replace != replace_config);
                if let ForeignIdValue::Set(_) = document.foreign_id {
                    // foreign_id is included in the update below; include it in
                    // the change check when it actually differs.
                    if existing.foreign_id != foreign_id {
                        return update_parent(
                            transaction,
                            kind,
                            existing.id,
                            document,
                            foreign_id,
                            true,
                        )
                        .await;
                    }
                }
                if !changed {
                    return Ok((existing.id, false));
                }
                return update_parent(transaction, kind, existing.id, document, foreign_id, false)
                    .await;
            }
            let sql = write::insert_returning_sql(
                kind.table(),
                &[
                    "foreign_id",
                    "name",
                    "description",
                    "kind",
                    "labels",
                    "inject_config",
                    "replace_config",
                    "created_by_id",
                ],
                "id",
            );
            let id = sqlx::query_scalar::<_, i64>(&sql)
                .bind(foreign_id)
                .bind(document.name.clone())
                .bind(document.description.clone())
                .bind(static_kind)
                .bind(document.labels.clone())
                .bind(optional_json(inject_config))
                .bind(optional_json(replace_config))
                .bind(operator_id)
                .fetch_one(&mut **transaction)
                .await?;
            Ok((id, true))
        }
        (
            SecretKind::PgDsn,
            DocumentFields::PgDsn {
                database,
                role,
                settings,
            },
        ) => {
            if let Some(existing) = existing {
                let changed = json!(existing.name) != json!(document.name)
                    || json!(existing.description) != json!(document.description)
                    || existing.labels != document.labels
                    || !matches!(&existing.fields, SecretFields::PgDsn { database: existing_database, role: existing_role, settings: existing_settings }
                        if Some(existing_database.as_str()) == database.as_deref()
                            && existing_role == role
                            && existing_settings == settings);
                let foreign_changed = matches!(document.foreign_id, ForeignIdValue::Set(_))
                    && existing.foreign_id != foreign_id;
                if !changed && !foreign_changed {
                    return Ok((existing.id, false));
                }
                return update_parent(
                    transaction,
                    kind,
                    existing.id,
                    document,
                    foreign_id,
                    foreign_changed,
                )
                .await;
            }
            let sql = write::insert_returning_sql(
                kind.table(),
                &[
                    "foreign_id",
                    "name",
                    "description",
                    "database",
                    "role",
                    "labels",
                    "settings",
                    "created_by_id",
                ],
                "id",
            );
            let id = sqlx::query_scalar::<_, i64>(&sql)
                .bind(foreign_id)
                .bind(document.name.clone())
                .bind(document.description.clone())
                .bind(database.clone())
                .bind(role.clone())
                .bind(document.labels.clone())
                .bind(settings.clone())
                .bind(operator_id)
                .fetch_one(&mut **transaction)
                .await?;
            Ok((id, true))
        }
        (
            SecretKind::GcpAuth,
            DocumentFields::GcpAuth {
                credentials_provider,
                subject,
                scopes,
            },
        ) => {
            if let Some(existing) = existing {
                let changed = json!(existing.name) != json!(document.name)
                    || json!(existing.description) != json!(document.description)
                    || existing.labels != document.labels
                    || !matches!(&existing.fields, SecretFields::GcpAuth { credentials_provider: existing_provider, subject: existing_subject, scopes: existing_scopes }
                        if existing_provider == credentials_provider
                            && existing_subject == subject
                            && existing_scopes == scopes);
                let foreign_changed = matches!(document.foreign_id, ForeignIdValue::Set(_))
                    && existing.foreign_id != foreign_id;
                if !changed && !foreign_changed {
                    return Ok((existing.id, false));
                }
                return update_parent(
                    transaction,
                    kind,
                    existing.id,
                    document,
                    foreign_id,
                    foreign_changed,
                )
                .await;
            }
            let sql = write::insert_returning_sql(
                kind.table(),
                &[
                    "foreign_id",
                    "name",
                    "description",
                    "subject",
                    "labels",
                    "credentials_provider",
                    "scopes",
                    "created_by_id",
                ],
                "id",
            );
            let id = sqlx::query_scalar::<_, i64>(&sql)
                .bind(foreign_id)
                .bind(document.name.clone())
                .bind(document.description.clone())
                .bind(subject.clone())
                .bind(document.labels.clone())
                .bind(optional_json(credentials_provider))
                .bind(scopes.clone())
                .bind(operator_id)
                .fetch_one(&mut **transaction)
                .await?;
            Ok((id, true))
        }
        (SecretKind::GcpIdToken, DocumentFields::GcpIdToken { audience, header }) => {
            if let Some(existing) = existing {
                let changed = json!(existing.name) != json!(document.name)
                    || json!(existing.description) != json!(document.description)
                    || existing.labels != document.labels
                    || !matches!(&existing.fields, SecretFields::GcpIdToken { audience: existing_audience, header: existing_header }
                        if Some(existing_audience.as_str()) == audience.as_deref()
                            && existing_header == header);
                let foreign_changed = matches!(document.foreign_id, ForeignIdValue::Set(_))
                    && existing.foreign_id != foreign_id;
                if !changed && !foreign_changed {
                    return Ok((existing.id, false));
                }
                return update_parent(
                    transaction,
                    kind,
                    existing.id,
                    document,
                    foreign_id,
                    foreign_changed,
                )
                .await;
            }
            let sql = write::insert_returning_sql(
                kind.table(),
                &[
                    "foreign_id",
                    "name",
                    "description",
                    "audience",
                    "header",
                    "labels",
                    "created_by_id",
                ],
                "id",
            );
            let id = sqlx::query_scalar::<_, i64>(&sql)
                .bind(foreign_id)
                .bind(document.name.clone())
                .bind(document.description.clone())
                .bind(audience.clone())
                .bind(header.clone())
                .bind(document.labels.clone())
                .bind(operator_id)
                .fetch_one(&mut **transaction)
                .await?;
            Ok((id, true))
        }
        _ => Err(WriteError::BadRequest(format!(
            "{} secrets are not writable through this API",
            kind.label()
        ))),
    }
}

/// Updates the parent row for the four writable kinds. `foreign_id` is written
/// only when the document supplied one (or explicitly cleared it); `changed`
/// says whether to include the foreign id in the SET list.
async fn update_parent(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
    document: &Document,
    foreign_id: Option<String>,
    include_foreign_id: bool,
) -> std::result::Result<(i64, bool), WriteError> {
    let mut columns: Vec<&str> = vec!["name", "description"];
    if include_foreign_id {
        columns.push("foreign_id");
    }
    columns.push("labels");
    match (&document.fields, kind) {
        (DocumentFields::Static { .. }, SecretKind::Static) => {
            columns.extend(["kind", "inject_config", "replace_config"]);
        }
        (DocumentFields::PgDsn { .. }, SecretKind::PgDsn) => {
            columns.extend(["database", "role", "settings"]);
        }
        (DocumentFields::GcpAuth { .. }, SecretKind::GcpAuth) => {
            columns.extend(["subject", "credentials_provider", "scopes"]);
        }
        (DocumentFields::GcpIdToken { .. }, SecretKind::GcpIdToken) => {
            columns.extend(["audience", "header"]);
        }
        _ => {}
    }
    let sql = write::update_sql(kind.table(), &columns);
    let mut query = sqlx::query(&sql)
        .bind(document.name.clone())
        .bind(document.description.clone());
    if include_foreign_id {
        query = query.bind(foreign_id);
    }
    query = query.bind(document.labels.clone());
    query = match (&document.fields, kind) {
        (
            DocumentFields::Static {
                kind: static_kind,
                inject_config,
                replace_config,
            },
            SecretKind::Static,
        ) => query
            .bind(static_kind.clone().unwrap_or_else(|| "custom".to_owned()))
            .bind(optional_json(inject_config))
            .bind(optional_json(replace_config)),
        (
            DocumentFields::PgDsn {
                database,
                role,
                settings,
            },
            SecretKind::PgDsn,
        ) => query
            .bind(database.clone())
            .bind(role.clone())
            .bind(settings.clone()),
        (
            DocumentFields::GcpAuth {
                credentials_provider,
                subject,
                scopes,
            },
            SecretKind::GcpAuth,
        ) => query
            .bind(subject.clone())
            .bind(optional_json(credentials_provider))
            .bind(scopes.clone()),
        (DocumentFields::GcpIdToken { audience, header }, SecretKind::GcpIdToken) => {
            query.bind(audience.clone()).bind(header.clone())
        }
        _ => query,
    };
    query.bind(id).execute(&mut **transaction).await?;
    Ok((id, true))
}

/// Deletes the replaced source/rules and inserts the new ones, keeping each
/// rule's position.
async fn replace_nested(
    transaction: &mut Transaction<'_, Postgres>,
    kind: SecretKind,
    id: i64,
    document: &Document,
    broker_id: Option<i64>,
) -> std::result::Result<(), WriteError> {
    if let Some(owner_column) = kind.source_owner_column() {
        sqlx::query(&format!(
            "DELETE FROM secret_sources WHERE {owner_column} = $1"
        ))
        .bind(id)
        .execute(&mut **transaction)
        .await?;
    }
    if let Some(owner_column) = kind.rule_owner_column() {
        sqlx::query(&format!(
            "DELETE FROM request_rules WHERE {owner_column} = $1"
        ))
        .bind(id)
        .execute(&mut **transaction)
        .await?;
    }

    if let (Some(owner_column), Some(source)) = (kind.source_owner_column(), &document.source) {
        let encrypted = source
            .secret
            .as_deref()
            .map(encrypt)
            .transpose()
            .map_err(WriteError::Internal)?;
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
        sqlx::query(&sql)
            .bind(id)
            .bind(source.source_type.clone())
            .bind(source.config.clone())
            .bind(encrypted)
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .bind(broker_id)
            .execute(&mut **transaction)
            .await?;
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

fn source_equal(
    existing: Option<&SourceRow>,
    existing_plain: Option<&str>,
    document: Option<&SourceDocument>,
    broker_id: Option<i64>,
) -> bool {
    match (existing, document) {
        (None, None) => true,
        (Some(existing), Some(document)) => {
            existing.source_type == document.source_type.clone().unwrap_or_default()
                && existing.config == document.config
                && existing_plain == document.secret.as_deref()
                && existing.role.is_none()
                && existing.role_kind.is_none()
                && existing.broker_credential_id == broker_id
        }
        _ => false,
    }
}

fn rules_equal(existing: &[super::RuleRow], document: &[RuleDocument]) -> bool {
    if existing.len() != document.len() {
        return false;
    }
    let mut existing: Vec<&super::RuleRow> = existing.iter().collect();
    existing.sort_by_key(|rule| rule.position);
    let mut document: Vec<&RuleDocument> = document.iter().collect();
    document.sort_by_key(|rule| rule.position);
    existing.iter().zip(document).all(|(existing, document)| {
        existing.host == document.host
            && existing.cidr == document.cidr
            && existing.position == document.position
            && existing.http_methods == document.http_methods
            && existing.paths == document.paths
    })
}

fn parent_map(kind: SecretKind, document: &Document) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("name".to_owned(), json!(document.name));
    map.insert("description".to_owned(), json!(document.description));
    map.insert("labels".to_owned(), document.labels.clone());
    if let ForeignIdValue::Set(foreign_id) = &document.foreign_id {
        map.insert("foreign_id".to_owned(), json!(foreign_id));
    }
    match (&document.fields, kind) {
        (
            DocumentFields::Static {
                kind: static_kind,
                inject_config,
                replace_config,
            },
            SecretKind::Static,
        ) => {
            map.insert(
                "kind".to_owned(),
                json!(static_kind.clone().unwrap_or_else(|| "custom".to_owned())),
            );
            map.insert("inject_config".to_owned(), inject_config.clone());
            map.insert("replace_config".to_owned(), replace_config.clone());
        }
        (
            DocumentFields::PgDsn {
                database,
                role,
                settings,
            },
            SecretKind::PgDsn,
        ) => {
            map.insert("database".to_owned(), json!(database));
            map.insert("role".to_owned(), json!(role));
            map.insert("settings".to_owned(), settings.clone());
        }
        (
            DocumentFields::GcpAuth {
                credentials_provider,
                subject,
                scopes,
            },
            SecretKind::GcpAuth,
        ) => {
            map.insert(
                "credentials_provider".to_owned(),
                credentials_provider.clone(),
            );
            map.insert("subject".to_owned(), json!(subject));
            map.insert("scopes".to_owned(), scopes.clone());
        }
        (DocumentFields::GcpIdToken { audience, header }, SecretKind::GcpIdToken) => {
            map.insert("audience".to_owned(), json!(audience));
            map.insert("header".to_owned(), json!(header));
        }
        _ => {}
    }
    map
}

fn existing_parent_map(kind: SecretKind, existing: &SecretRow) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("name".to_owned(), json!(existing.name));
    map.insert("description".to_owned(), json!(existing.description));
    map.insert("labels".to_owned(), existing.labels.clone());
    map.insert("foreign_id".to_owned(), json!(existing.foreign_id));
    match (&existing.fields, kind) {
        (
            SecretFields::Static {
                kind: static_kind,
                inject_config,
                replace_config,
            },
            SecretKind::Static,
        ) => {
            map.insert("kind".to_owned(), json!(static_kind));
            map.insert("inject_config".to_owned(), inject_config.clone());
            map.insert("replace_config".to_owned(), replace_config.clone());
        }
        (
            SecretFields::PgDsn {
                database,
                role,
                settings,
            },
            SecretKind::PgDsn,
        ) => {
            map.insert("database".to_owned(), json!(database));
            map.insert("role".to_owned(), json!(role));
            map.insert("settings".to_owned(), settings.clone());
        }
        (
            SecretFields::GcpAuth {
                credentials_provider,
                subject,
                scopes,
            },
            SecretKind::GcpAuth,
        ) => {
            map.insert(
                "credentials_provider".to_owned(),
                credentials_provider.clone(),
            );
            map.insert("subject".to_owned(), json!(subject));
            map.insert("scopes".to_owned(), scopes.clone());
        }
        (SecretFields::GcpIdToken { audience, header }, SecretKind::GcpIdToken) => {
            map.insert("audience".to_owned(), json!(audience));
            map.insert("header".to_owned(), json!(header));
        }
        _ => {}
    }
    map
}

/// `SyncConfigReplacement.equivalent?`: every parent attribute and the nested
/// documents (with inline plaintext) are unchanged.
fn equivalent(
    kind: SecretKind,
    existing: &SecretRow,
    existing_plain: Option<&str>,
    document: &Document,
    broker_id: Option<i64>,
) -> bool {
    let mut parent = parent_map(kind, document);
    if matches!(document.foreign_id, ForeignIdValue::Omitted) {
        parent.remove("foreign_id");
    }
    parent == existing_parent_map(kind, existing)
        && source_equal(
            existing.source(),
            existing_plain,
            document.source.as_ref(),
            broker_id,
        )
        && rules_equal(&existing.rules, &document.rules)
}

fn source_key(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::PgDsn => "dsn",
        SecretKind::GcpAuth | SecretKind::GcpIdToken => "keyfile",
        _ => "source",
    }
}

// ---------------------------------------------------------------------------
// Deletes and secret-scoped role grants
// ---------------------------------------------------------------------------

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
        invalidate::invalidate(&mut transaction, Scope::secret_delete(kind, id)).await?;
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
