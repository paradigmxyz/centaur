//! Write paths for grantable secrets.
//!
//! Every kind is written by one pipeline. The kind's API layer builds one
//! [`Document`] — a typed [`ParentRow`] for the columns the kind stores, its
//! sources and rules, and the identity — so the writer only does what every
//! kind does:
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

use serde_json::Value;
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
    /// The parent row's typed columns, without `foreign_id` and `created_by_id`,
    /// which the writer adds.
    pub parent: ParentRow,
    pub sources: Vec<SourceDraft>,
    pub rules: Vec<RuleDocument>,
    /// The inline SSH key parsing error, reported on `private_key`.
    pub private_key_error: Option<String>,
}

/// One credential source of the document, with the role columns the owning
/// model requires.
#[derive(Clone, Debug, PartialEq)]
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

/// A unique violation is retried once for an upsert addressed by foreign id.
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
    let unchanged =
        existing.is_some_and(|row| equivalent(row, &pending.existing_plain, &pending.document));
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

/// Binds a typed parent field as its column type. A JSON `null` is a SQL NULL.
trait BindField {
    fn bind_field<'q>(
        self,
        query: Query<'q, Postgres, PgArguments>,
    ) -> Query<'q, Postgres, PgArguments>;
}

impl BindField for Option<String> {
    fn bind_field<'q>(
        self,
        query: Query<'q, Postgres, PgArguments>,
    ) -> Query<'q, Postgres, PgArguments> {
        query.bind(self)
    }
}

impl BindField for Value {
    fn bind_field<'q>(
        self,
        query: Query<'q, Postgres, PgArguments>,
    ) -> Query<'q, Postgres, PgArguments> {
        query.bind((!self.is_null()).then_some(self))
    }
}

impl BindField for bool {
    fn bind_field<'q>(
        self,
        query: Query<'q, Postgres, PgArguments>,
    ) -> Query<'q, Postgres, PgArguments> {
        query.bind(self)
    }
}

/// The parent columns of every kind. Each field is named once: the macro derives
/// the INSERT/UPDATE column list with `stringify!` and binds the fields in the
/// same order, so a column cannot be misspelled relative to its value.
macro_rules! parent_rows {
    (
        $(
            $variant:ident => $struct:ident {
                $( $field:ident: $ty:ty ),* $(,)?
            }
        ),* $(,)?
    ) => {
        #[derive(Clone, Debug, PartialEq)]
        pub(super) enum ParentRow {
            $( $variant($struct), )*
        }

        $(
            #[derive(Clone, Debug, PartialEq)]
            pub(super) struct $struct {
                $( pub(super) $field: $ty, )*
            }

            impl $struct {
                const COLUMNS: &'static [&'static str] = &[$(stringify!($field)),*];

                fn bind<'q>(
                    self,
                    mut query: Query<'q, Postgres, PgArguments>,
                ) -> Query<'q, Postgres, PgArguments> {
                    $( query = BindField::bind_field(self.$field, query); )*
                    query
                }
            }
        )*

        impl ParentRow {
            pub(super) fn columns(&self) -> &'static [&'static str] {
                match self {
                    $( Self::$variant(_) => $struct::COLUMNS, )*
                }
            }

            pub(super) fn bind<'q>(
                self,
                query: Query<'q, Postgres, PgArguments>,
            ) -> Query<'q, Postgres, PgArguments> {
                match self {
                    $( Self::$variant(row) => row.bind(query), )*
                }
            }
        }
    };
}

parent_rows! {
    Static => StaticParent {
        name: Option<String>,
        description: Option<String>,
        labels: Value,
        kind: Option<String>,
        inject_config: Value,
        replace_config: Value,
    },
    PgDsn => PgDsnParent {
        name: Option<String>,
        description: Option<String>,
        database: Option<String>,
        role: Option<String>,
        labels: Value,
        settings: Value,
    },
    GcpAuth => GcpAuthParent {
        name: Option<String>,
        description: Option<String>,
        subject: Option<String>,
        labels: Value,
        credentials_provider: Value,
        scopes: Value,
    },
    GcpIdToken => GcpIdTokenParent {
        name: Option<String>,
        description: Option<String>,
        audience: Option<String>,
        header: Option<String>,
        labels: Value,
    },
    AwsAuth => AwsAuthParent {
        name: Option<String>,
        description: Option<String>,
        labels: Value,
        allowed_regions: Value,
        allowed_services: Value,
    },
    OauthToken => OauthTokenParent {
        name: Option<String>,
        description: Option<String>,
        labels: Value,
        grant: Option<String>,
        token_endpoint: Option<String>,
        audience: Option<String>,
        header: Option<String>,
        value_prefix: Option<String>,
        scopes: Value,
    },
    Hmac => HmacParent {
        name: Option<String>,
        description: Option<String>,
        labels: Value,
        timestamp_format: Option<String>,
        signature_algorithm: Option<String>,
        signature_key_encoding: Option<String>,
        signature_output_encoding: Option<String>,
        signature_message: Option<String>,
        allow_chunked_body: bool,
        headers: Value,
    },
    SshKey => SshKeyParent {
        name: Option<String>,
        description: Option<String>,
        labels: Value,
        public_key: Option<String>,
    },
}

impl ParentRow {
    /// The same struct built from a stored row, for the `PartialEq` unchanged
    /// check. Both sides name each field once, so they cannot drift.
    pub(super) fn from_secret_row(row: &SecretRow) -> Self {
        let name = row.name.clone();
        let description = row.description.clone();
        let labels = row.labels.clone();
        match &row.fields {
            SecretFields::Static {
                kind,
                inject_config,
                replace_config,
            } => Self::Static(StaticParent {
                name,
                description,
                labels,
                kind: Some(kind.clone()),
                inject_config: inject_config.clone(),
                replace_config: replace_config.clone(),
            }),
            SecretFields::PgDsn {
                database,
                role,
                settings,
            } => Self::PgDsn(PgDsnParent {
                name,
                description,
                database: Some(database.clone()),
                role: role.clone(),
                labels,
                settings: settings.clone(),
            }),
            SecretFields::GcpAuth {
                credentials_provider,
                subject,
                scopes,
            } => Self::GcpAuth(GcpAuthParent {
                name,
                description,
                subject: subject.clone(),
                labels,
                credentials_provider: credentials_provider.clone(),
                scopes: scopes.clone(),
            }),
            SecretFields::GcpIdToken { audience, header } => Self::GcpIdToken(GcpIdTokenParent {
                name,
                description,
                audience: Some(audience.clone()),
                header: header.clone(),
                labels,
            }),
            SecretFields::AwsAuth {
                allowed_regions,
                allowed_services,
            } => Self::AwsAuth(AwsAuthParent {
                name,
                description,
                labels,
                allowed_regions: allowed_regions.clone(),
                allowed_services: allowed_services.clone(),
            }),
            SecretFields::OauthToken {
                grant,
                token_endpoint,
                audience,
                scopes,
                header,
                value_prefix,
            } => Self::OauthToken(OauthTokenParent {
                name,
                description,
                labels,
                grant: Some(grant.clone()),
                token_endpoint: Some(token_endpoint.clone()),
                audience: audience.clone(),
                header: header.clone(),
                value_prefix: value_prefix.clone(),
                scopes: scopes.clone(),
            }),
            SecretFields::Hmac {
                timestamp_format,
                signature_algorithm,
                signature_key_encoding,
                signature_output_encoding,
                signature_message,
                headers,
                allow_chunked_body,
            } => Self::Hmac(HmacParent {
                name,
                description,
                labels,
                timestamp_format: Some(timestamp_format.clone()),
                signature_algorithm: Some(signature_algorithm.clone()),
                signature_key_encoding: Some(signature_key_encoding.clone()),
                signature_output_encoding: Some(signature_output_encoding.clone()),
                signature_message: Some(signature_message.clone()),
                allow_chunked_body: *allow_chunked_body,
                headers: headers.clone(),
            }),
            SecretFields::SshKey { public_key } => Self::SshKey(SshKeyParent {
                name,
                description,
                labels,
                public_key: Some(public_key.clone()),
            }),
        }
    }
}

/// Inserts or updates the parent row from its typed columns. The Console SPA
/// leaves an unchanged row alone; the bearer API rewrites it, and writes a
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
        columns.extend(document.parent.columns());
        columns.push("created_by_id");
        let sql = write::insert_returning_sql(kind.table(), &columns, "id");
        let row = document
            .parent
            .clone()
            .bind(sqlx::query(&sql).bind(foreign_id))
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
        if !foreign_changed && parent_matches(&document.parent, existing) {
            return Ok(existing.id);
        }
        let mut columns: Vec<&str> = document.parent.columns().to_vec();
        if foreign_changed {
            columns.push("foreign_id");
        }
        let sql = write::update_sql(kind.table(), &columns);
        let mut query = document.parent.clone().bind(sqlx::query(&sql));
        if foreign_changed {
            query = query.bind(foreign_id);
        }
        query.bind(existing.id).execute(&mut **transaction).await?;
        return Ok(existing.id);
    }

    let columns: Vec<&str> = document.parent.columns().to_vec();
    let sql = write::update_sql(kind.table(), &columns);
    document
        .parent
        .clone()
        .bind(sqlx::query(&sql))
        .bind(existing.id)
        .execute(&mut **transaction)
        .await?;
    if let ForeignIdValue::Set(foreign_id) = &document.foreign_id {
        // The identity changes without a second `updated_at` bump.
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

/// Whether the existing row and the document are equivalent: the identity (when
/// the document sets one), the parent attributes, the sources with their inline
/// plaintext, and the rules are unchanged. Nested records are compared as sorted
/// documents, so the order they were stored in does not matter.
fn equivalent(
    existing: &SecretRow,
    existing_plain: &[Option<String>],
    document: &Document,
) -> bool {
    if let ForeignIdValue::Set(foreign_id) = &document.foreign_id
        && existing.foreign_id != *foreign_id
    {
        return false;
    }
    parent_matches(&document.parent, existing)
        && sources_match(&existing.sources, existing_plain, &document.sources)
        && rules_match(&existing.rules, &document.rules)
}

/// Every parent column the kind stores equals the stored value.
fn parent_matches(parent: &ParentRow, existing: &SecretRow) -> bool {
    *parent == ParentRow::from_secret_row(existing)
}

fn sources_match(
    existing: &[super::SourceRow],
    existing_plain: &[Option<String>],
    sources: &[SourceDraft],
) -> bool {
    if existing.len() != sources.len() {
        return false;
    }
    let mut stored: Vec<SourceDraft> = existing
        .iter()
        .zip(existing_plain)
        .map(|(source, plain)| SourceDraft {
            role: source.role.clone(),
            role_kind: source.role_kind.clone(),
            source_type: Some(source.source_type.clone()),
            secret: plain.clone(),
            config: source.config.clone(),
            broker_credential_id: source.broker_credential_id,
            broker_resolved: true,
        })
        .collect();
    // The stored bind turns an absent source type into the empty string, so
    // normalize the draft the same way. `broker_resolved` is a validation
    // side effect, not a stored column, so it does not participate.
    let mut draft: Vec<SourceDraft> = sources
        .iter()
        .cloned()
        .map(|mut source| {
            source.source_type = Some(source.source_type.unwrap_or_default());
            source.broker_resolved = true;
            source
        })
        .collect();
    stored.sort_by(|left, right| left.role.cmp(&right.role));
    draft.sort_by(|left, right| left.role.cmp(&right.role));
    stored == draft
}

fn rules_match(existing: &[RuleRow], rules: &[RuleDocument]) -> bool {
    if existing.len() != rules.len() {
        return false;
    }
    let mut stored: Vec<RuleDocument> = existing
        .iter()
        .map(|rule| RuleDocument {
            host: rule.host.clone(),
            cidr: rule.cidr.clone(),
            position: rule.position,
            http_methods: rule.http_methods.clone(),
            paths: rule.paths.clone(),
        })
        .collect();
    let mut draft = rules.to_vec();
    stored.sort_by_key(|rule| rule.position);
    draft.sort_by_key(|rule| rule.position);
    stored == draft
}

/// Deletes a secret, cascading to its sources, rules, and grants. The parent's
/// bump is deferred to commit, where the secret's grants are gone and the bump
/// reaches no principal, so the destroyed grants' own bumps are what reach
/// principals.
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
