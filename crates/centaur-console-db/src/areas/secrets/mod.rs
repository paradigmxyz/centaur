//! Data access for Console's grantable secrets area.
//!
//! This module is the Rust home of the shared secret plumbing: the kind table,
//! record loading, the cross-kind console list, label-filtered bearer lists,
//! writes with the console/bearer/sandbox differences layered on top, and the
//! secret-scoped role grants. The API layer (`console::areas::secrets`) only
//! parses HTTP and renders these records.
//!
//! All eight kinds live here. The four the Console SPA edits (`static`,
//! `pg_dsn`, `gcp_auth`, `gcp_id_token`) are written from a document built by
//! [`SecretDocumentPayload`]; the four bearer-only kinds (`aws_auth`,
//! `oauth_token`, `hmac`, `ssh_key`) have their own typed request in [`more`].
//! [`WriteDocument`] is the entry point that accepts either, so the HTTP layer
//! has one write call per route.

mod document;
mod more;
mod write;

use std::collections::BTreeMap;

use serde_json::Value;
use sqlx::{PgConnection, PgPool, Postgres};
use time::PrimitiveDateTime;

use crate::Result;
use crate::model::{self, ConsoleModel};

pub use document::{
    Document, DocumentFields, Errors, ForeignIdValue, ParamValue, RuleDocument, RulePayload,
    Scalar, ScalarMap, SecretDocumentPayload, SettingPayload, SourceDocument, SourcePayload,
    WriteError, WriteOptions, WriteTarget, decrypt,
};
pub(crate) use document::{blank, encrypt, value_to_string};
pub use more::{
    AwsAuthRequest, HeaderInput, HmacRequest, OauthTokenRequest, RuleInput, SourceInput,
    SshKeyRequest,
};
pub use write::{WriteResult, create_role_grant, delete_role_grant, delete_secret, write_secret};

/// The kinds this crate stores, with the model descriptor each kind's table is
/// addressed by. One list defines the enum, its canonical order, and every
/// descriptor-derived value, so a kind cannot name a table the model table does
/// not describe.
macro_rules! secret_kinds {
    ($macro:ident) => {
        $macro! {
            (Static, StaticSecret, "static", "Static"),
            (GcpAuth, GcpAuthSecret, "gcp_auth", "GCP Auth"),
            (GcpIdToken, GcpIdTokenSecret, "gcp_id_token", "GCP ID Token"),
            (AwsAuth, AwsAuthSecret, "aws_auth", "AWS Auth"),
            (OauthToken, OauthTokenSecret, "oauth_token", "OAuth Token"),
            (PgDsn, PgDsnSecret, "pg_dsn", "Postgres DSN"),
            (Hmac, HmacSecret, "hmac", "HMAC"),
            (SshKey, SshKeySecret, "ssh_key", "SSH key"),
        }
    };
}

macro_rules! define_secret_kinds {
    ($(($variant:ident, $marker:ident, $slug:literal, $label:literal)),* $(,)?) => {
        /// The grantable secret kinds in Console's canonical order.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
        pub enum SecretKind {
            $($variant,)*
        }

        impl SecretKind {
            pub const ALL: [Self; 8] = [$(Self::$variant,)*];

            pub fn parse(slug: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|kind| kind.slug() == slug)
            }

            pub fn slug(self) -> &'static str {
                match self {
                    $(Self::$variant => $slug,)*
                }
            }

            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)*
                }
            }

            /// The model name, which is what a missing row renders.
            pub fn model_name(self) -> &'static str {
                match self {
                    $(Self::$variant => <model::$marker as ConsoleModel>::NAME,)*
                }
            }

            pub fn oid_prefix(self) -> &'static str {
                match self {
                    $(Self::$variant => <model::$marker as ConsoleModel>::OID_PREFIX,)*
                }
            }

            pub fn table(self) -> &'static str {
                match self {
                    $(Self::$variant => <model::$marker as ConsoleModel>::TABLE,)*
                }
            }

            pub fn oid(self, id: i64) -> String {
                match self {
                    $(Self::$variant => <model::$marker as ConsoleModel>::oid(id),)*
                }
            }

            pub fn decode_oid(self, value: &str) -> Option<i64> {
                match self {
                    $(Self::$variant => <model::$marker as ConsoleModel>::decode_oid(value),)*
                }
            }

            /// The uniform 404 for a missing row of this kind.
            pub fn not_found(self) -> crate::Error {
                match self {
                    $(Self::$variant => <model::$marker as ConsoleModel>::not_found(),)*
                }
            }
        }
    };
}

/// The descriptor-backed row lookup. The marker is chosen at runtime from
/// [`SecretKind`] and the generic helper selects with `SELECT *`.
macro_rules! define_secret_lookup {
    ($(($variant:ident, $marker:ident, $slug:literal, $label:literal)),* $(,)?) => {
        async fn find_table_row<'e, E>(
            executor: E,
            kind: SecretKind,
            lookup: Lookup<'_>,
        ) -> Result<Option<SecretTableRow>>
        where
            E: sqlx::Executor<'e, Database = Postgres>,
        {
            match kind {
                $(SecretKind::$variant => lookup_row::<model::$marker, _>(executor, lookup).await,)*
            }
        }
    };
}

secret_kinds!(define_secret_kinds);
secret_kinds!(define_secret_lookup);

impl SecretKind {
    /// The `grants` column naming this kind.
    pub fn grant_column(self) -> &'static str {
        match self {
            Self::Static => "static_secret_id",
            Self::GcpAuth => "gcp_auth_secret_id",
            Self::GcpIdToken => "gcp_id_token_secret_id",
            Self::AwsAuth => "aws_auth_secret_id",
            Self::OauthToken => "oauth_token_secret_id",
            Self::PgDsn => "pg_dsn_secret_id",
            Self::Hmac => "hmac_secret_id",
            Self::SshKey => "ssh_key_secret_id",
        }
    }

    /// The `secret_sources` column naming this kind, when it owns a source.
    pub fn source_owner_column(self) -> Option<&'static str> {
        match self {
            Self::Static => Some("static_secret_id"),
            Self::GcpAuth => Some("gcp_auth_secret_id"),
            Self::GcpIdToken => Some("gcp_id_token_secret_id"),
            Self::AwsAuth => Some("aws_auth_secret_id"),
            Self::OauthToken => Some("oauth_token_secret_id"),
            Self::PgDsn => Some("pg_dsn_secret_id"),
            Self::Hmac => Some("hmac_secret_id"),
            Self::SshKey => Some("ssh_key_secret_id"),
        }
    }

    /// The `request_rules` column naming this kind, when it owns rules.
    pub fn rule_owner_column(self) -> Option<&'static str> {
        match self {
            Self::Static => Some("static_secret_id"),
            Self::GcpAuth => Some("gcp_auth_secret_id"),
            Self::GcpIdToken => Some("gcp_id_token_secret_id"),
            Self::AwsAuth => Some("aws_auth_secret_id"),
            Self::OauthToken => Some("oauth_token_secret_id"),
            Self::Hmac => Some("hmac_secret_id"),
            Self::PgDsn => None,
            Self::SshKey => None,
        }
    }

    /// Whether the console form edits this kind (`SecretKinds[:form]`).
    pub fn editable(self) -> bool {
        matches!(
            self,
            Self::Static | Self::GcpAuth | Self::GcpIdToken | Self::PgDsn
        )
    }
}

/// A typed write for one kind: the console/bearer document or the bearer-only
/// request. Both `From` impls let an HTTP route declare its own body type and
/// hand it to [`write_secret`] unchanged.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // One boxed body per request is the same cost as the payload itself.
pub enum WriteDocument {
    /// The shared `{"data": {...}}` document of the four editable kinds.
    Console(SecretDocumentPayload),
    /// The typed request of the four bearer-only kinds.
    Bearer(BearerRequest),
}

/// The typed request of one bearer-only secret kind.
#[derive(Clone, Debug)]
pub enum BearerRequest {
    AwsAuth(AwsAuthRequest),
    OauthToken(OauthTokenRequest),
    Hmac(HmacRequest),
    SshKey(SshKeyRequest),
}

impl BearerRequest {
    pub fn kind(&self) -> SecretKind {
        match self {
            Self::AwsAuth(_) => SecretKind::AwsAuth,
            Self::OauthToken(_) => SecretKind::OauthToken,
            Self::Hmac(_) => SecretKind::Hmac,
            Self::SshKey(_) => SecretKind::SshKey,
        }
    }
}

impl From<SecretDocumentPayload> for WriteDocument {
    fn from(payload: SecretDocumentPayload) -> Self {
        Self::Console(payload)
    }
}

impl From<AwsAuthRequest> for WriteDocument {
    fn from(request: AwsAuthRequest) -> Self {
        Self::Bearer(BearerRequest::AwsAuth(request))
    }
}

impl From<OauthTokenRequest> for WriteDocument {
    fn from(request: OauthTokenRequest) -> Self {
        Self::Bearer(BearerRequest::OauthToken(request))
    }
}

impl From<HmacRequest> for WriteDocument {
    fn from(request: HmacRequest) -> Self {
        Self::Bearer(BearerRequest::Hmac(request))
    }
}

impl From<SshKeyRequest> for WriteDocument {
    fn from(request: SshKeyRequest) -> Self {
        Self::Bearer(BearerRequest::SshKey(request))
    }
}

/// A grantee: a principal or a role, addressed by one column. Grants, Slack
/// channel permissions, and the console's `exclude_grantee` filter all name
/// their subject this way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Grantee {
    Principal(i64),
    Role(i64),
}

impl Grantee {
    /// The column naming this grantee's row.
    pub fn column(self) -> &'static str {
        match self {
            Self::Principal(_) => "principal_id",
            Self::Role(_) => "role_id",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Self::Principal(id) | Self::Role(id) => id,
        }
    }

    pub fn principal_id(self) -> Option<i64> {
        match self {
            Self::Principal(id) => Some(id),
            Self::Role(_) => None,
        }
    }

    pub fn role_id(self) -> Option<i64> {
        match self {
            Self::Role(id) => Some(id),
            Self::Principal(_) => None,
        }
    }

    /// `<table> WHERE <owner column> = $1`: the prefix every owner-scoped query
    /// shares, so the SQL stays one string per query.
    pub fn owner_sql(self, table: &str) -> String {
        format!("{table} WHERE {} = $1", self.column())
    }
}

/// One stored source. `secret` is the ciphertext the `secret` column holds.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct SourceRow {
    pub id: i64,
    pub source_type: String,
    pub config: Value,
    pub secret: Option<String>,
    pub role: Option<String>,
    pub role_kind: Option<String>,
    pub broker_credential_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct RuleRow {
    pub id: i64,
    pub host: Option<String>,
    pub cidr: Option<String>,
    pub position: i32,
    pub http_methods: Value,
    pub paths: Value,
}

#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct BrokerRow {
    pub id: i64,
    pub oauth_app_id: Option<i64>,
    pub oauth_app_slug: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SecretFields {
    Static {
        kind: String,
        inject_config: Value,
        replace_config: Value,
    },
    PgDsn {
        database: String,
        role: Option<String>,
        settings: Value,
    },
    GcpAuth {
        credentials_provider: Value,
        subject: Option<String>,
        scopes: Value,
    },
    GcpIdToken {
        audience: String,
        header: Option<String>,
    },
    AwsAuth {
        allowed_regions: Value,
        allowed_services: Value,
    },
    OauthToken {
        grant: String,
        token_endpoint: String,
        audience: Option<String>,
        scopes: Value,
        header: Option<String>,
        value_prefix: Option<String>,
    },
    Hmac {
        timestamp_format: String,
        signature_algorithm: String,
        signature_key_encoding: String,
        signature_output_encoding: String,
        signature_message: String,
        headers: Value,
        allow_chunked_body: bool,
    },
    SshKey {
        public_key: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SecretRow {
    pub kind: SecretKind,
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub labels: Value,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
    pub created_by_id: Option<i64>,
    pub broker_credential_id: Option<i64>,
    /// Only `ssh_key_secrets` has one: a personal key's owner.
    pub owner_id: Option<i64>,
    pub fields: SecretFields,
    pub sources: Vec<SourceRow>,
    pub rules: Vec<RuleRow>,
    pub broker: Option<BrokerRow>,
}

impl SecretRow {
    pub fn oid(&self) -> String {
        self.kind.oid(self.id)
    }

    /// The document source a single-source kind owns, if any.
    pub fn source(&self) -> Option<&SourceRow> {
        match self.kind {
            SecretKind::Static
            | SecretKind::GcpAuth
            | SecretKind::GcpIdToken
            | SecretKind::PgDsn
            | SecretKind::SshKey => self.sources.first(),
            SecretKind::AwsAuth | SecretKind::OauthToken | SecretKind::Hmac => None,
        }
    }
}

/// One secret table's `SELECT *` projection. The union of all eight tables'
/// columns is selected by name; `#[sqlx(default)]` supplies the columns a given
/// table does not have, so one row type covers every kind.
#[derive(Debug, sqlx::FromRow)]
struct SecretTableRow {
    id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    labels: Value,
    created_at: PrimitiveDateTime,
    updated_at: PrimitiveDateTime,
    created_by_id: Option<i64>,
    // static_secrets
    #[sqlx(default)]
    broker_credential_id: Option<i64>,
    #[sqlx(default)]
    kind: Option<String>,
    #[sqlx(default)]
    inject_config: Option<Value>,
    #[sqlx(default)]
    replace_config: Option<Value>,
    // pg_dsn_secrets
    #[sqlx(default)]
    database: Option<String>,
    #[sqlx(default)]
    role: Option<String>,
    #[sqlx(default)]
    settings: Value,
    // gcp_auth_secrets / oauth_token_secrets
    #[sqlx(default)]
    credentials_provider: Option<Value>,
    #[sqlx(default)]
    subject: Option<String>,
    #[sqlx(default)]
    scopes: Value,
    // gcp_id_token_secrets
    #[sqlx(default)]
    audience: Option<String>,
    #[sqlx(default)]
    header: Option<String>,
    // aws_auth_secrets
    #[sqlx(default)]
    allowed_regions: Value,
    #[sqlx(default)]
    allowed_services: Value,
    // oauth_token_secrets
    #[sqlx(default)]
    grant: Option<String>,
    #[sqlx(default)]
    token_endpoint: Option<String>,
    #[sqlx(default)]
    value_prefix: Option<String>,
    // hmac_secrets
    #[sqlx(default)]
    timestamp_format: Option<String>,
    #[sqlx(default)]
    signature_algorithm: Option<String>,
    #[sqlx(default)]
    signature_key_encoding: Option<String>,
    #[sqlx(default)]
    signature_output_encoding: Option<String>,
    #[sqlx(default)]
    signature_message: Option<String>,
    #[sqlx(default)]
    allow_chunked_body: Option<bool>,
    #[sqlx(default)]
    headers: Value,
    // ssh_key_secrets
    #[sqlx(default)]
    public_key: Option<String>,
    #[sqlx(default)]
    owner_id: Option<i64>,
}

impl SecretTableRow {
    fn fields(&self, kind: SecretKind) -> SecretFields {
        match kind {
            SecretKind::Static => SecretFields::Static {
                kind: self.kind.clone().unwrap_or_default(),
                inject_config: self.inject_config.clone().unwrap_or(Value::Null),
                replace_config: self.replace_config.clone().unwrap_or(Value::Null),
            },
            SecretKind::PgDsn => SecretFields::PgDsn {
                database: self.database.clone().unwrap_or_default(),
                role: self.role.clone(),
                settings: self.settings.clone(),
            },
            SecretKind::GcpAuth => SecretFields::GcpAuth {
                credentials_provider: self.credentials_provider.clone().unwrap_or(Value::Null),
                subject: self.subject.clone(),
                scopes: self.scopes.clone(),
            },
            SecretKind::GcpIdToken => SecretFields::GcpIdToken {
                audience: self.audience.clone().unwrap_or_default(),
                header: self.header.clone(),
            },
            SecretKind::AwsAuth => SecretFields::AwsAuth {
                allowed_regions: self.allowed_regions.clone(),
                allowed_services: self.allowed_services.clone(),
            },
            SecretKind::OauthToken => SecretFields::OauthToken {
                grant: self.grant.clone().unwrap_or_default(),
                token_endpoint: self.token_endpoint.clone().unwrap_or_default(),
                audience: self.audience.clone(),
                scopes: self.scopes.clone(),
                header: self.header.clone(),
                value_prefix: self.value_prefix.clone(),
            },
            SecretKind::Hmac => SecretFields::Hmac {
                timestamp_format: self.timestamp_format.clone().unwrap_or_default(),
                signature_algorithm: self.signature_algorithm.clone().unwrap_or_default(),
                signature_key_encoding: self.signature_key_encoding.clone().unwrap_or_default(),
                signature_output_encoding: self
                    .signature_output_encoding
                    .clone()
                    .unwrap_or_default(),
                signature_message: self.signature_message.clone().unwrap_or_default(),
                headers: self.headers.clone(),
                allow_chunked_body: self.allow_chunked_body.unwrap_or(false),
            },
            SecretKind::SshKey => SecretFields::SshKey {
                public_key: self.public_key.clone().unwrap_or_default(),
            },
        }
    }
}

/// One page of the cross-kind console secret list.
#[derive(Clone, Debug, PartialEq)]
pub struct SecretPage {
    pub rows: Vec<SecretRow>,
    pub total: i64,
}

/// Filters for the console cross-kind list.
#[derive(Clone, Debug, Default)]
pub struct SecretFilter {
    pub kind: Option<SecretKind>,
    pub query: String,
    pub exclude: Option<Grantee>,
    pub page: i64,
    pub limit: i64,
}

/// The columns the console list sorts on.
#[derive(Debug, sqlx::FromRow)]
struct SortRow {
    id: i64,
    sort_name: String,
    created_at: PrimitiveDateTime,
}

pub async fn list_secrets(pool: &PgPool, filter: &SecretFilter) -> Result<SecretPage> {
    let kinds: Vec<SecretKind> = filter
        .kind
        .map(|kind| vec![kind])
        .unwrap_or_else(|| SecretKind::ALL.to_vec());
    let mut rows = Vec::new();
    for kind in kinds {
        let mut sql = format!(
            "SELECT id, lower(coalesce(name, '')) AS sort_name, created_at FROM {}",
            kind.table()
        );
        let mut conditions = Vec::new();
        let mut next = 1;
        if !filter.query.is_empty() {
            conditions.push(format!("(name ILIKE ${next} OR foreign_id ILIKE ${next})"));
            next += 1;
        }
        if let Some(grantee) = filter.exclude {
            let column = match grantee {
                Grantee::Principal(_) => "principal_id",
                Grantee::Role(_) => "role_id",
            };
            conditions.push(format!(
                "NOT EXISTS (SELECT 1 FROM grants WHERE grants.{column} = ${next} \
                 AND grants.{} = {}.id)",
                kind.grant_column(),
                kind.table()
            ));
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        let mut query = sqlx::query_as::<_, SortRow>(&sql);
        if !filter.query.is_empty() {
            query = query.bind(like_pattern(&filter.query));
        }
        if let Some(grantee) = filter.exclude {
            query = query.bind(match grantee {
                Grantee::Principal(id) | Grantee::Role(id) => id,
            });
        }
        for row in query.fetch_all(pool).await? {
            rows.push((kind, row));
        }
    }
    let total = rows.len() as i64;
    rows.sort_by(|(left_kind, left), (right_kind, right)| {
        left.sort_name
            .cmp(&right.sort_name)
            .then(left.created_at.cmp(&right.created_at))
            .then(left.id.cmp(&right.id))
            .then(kind_position(*left_kind).cmp(&kind_position(*right_kind)))
    });
    let offset = (filter.page.max(1) - 1) * filter.limit.max(1);
    let selected: Vec<(SecretKind, i64)> = rows
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(usize::try_from(filter.limit.max(1)).unwrap_or(usize::MAX))
        .map(|(kind, row)| (kind, row.id))
        .collect();

    let mut records = Vec::with_capacity(selected.len());
    let mut connection = pool.acquire().await?;
    for (kind, id) in selected {
        if let Some(row) = load_secret(&mut connection, kind, id).await? {
            records.push(row);
        }
    }
    Ok(SecretPage {
        rows: records,
        total,
    })
}

fn kind_position(kind: SecretKind) -> usize {
    SecretKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(0)
}

/// An ILIKE pattern that escapes `\`, `%`, and `_` in `query`.
fn like_pattern(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len() + 2);
    escaped.push('%');
    for character in query.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
}

/// The `labels @> $1::jsonb` value of one label filter.
fn labels_containment(labels: &BTreeMap<String, String>) -> Value {
    Value::Object(
        labels
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

/// One page of a single-kind bearer list, filtered by label containment. The
/// table is chosen at runtime, so the count-and-page pair cannot use the
/// `http::list` `ListSource`; the caller renders it through
/// `http::list_response`.
pub async fn list_kind_secrets(
    pool: &PgPool,
    kind: SecretKind,
    labels: &BTreeMap<String, String>,
    page: i64,
    limit: i64,
) -> Result<SecretPage> {
    let table = kind.table();
    let filter = (!labels.is_empty()).then(|| labels_containment(labels));
    let where_clause = if filter.is_some() {
        " WHERE labels @> $1::jsonb"
    } else {
        ""
    };
    let offset = (page.max(1) - 1) * limit.max(1);

    let page_sql = format!(
        "SELECT id FROM {table}{where_clause} ORDER BY created_at ASC, id ASC LIMIT ${} OFFSET ${}",
        if filter.is_some() { 2 } else { 1 },
        if filter.is_some() { 3 } else { 2 }
    );
    let mut shown = sqlx::query_scalar::<_, i64>(&page_sql);
    if let Some(filter) = &filter {
        shown = shown.bind(filter.clone());
    }
    let ids = shown
        .bind(limit.max(1))
        .bind(offset)
        .fetch_all(pool)
        .await?;

    let count_sql = format!("SELECT count(*) FROM {table}{where_clause}");
    let mut counted = sqlx::query_scalar::<_, i64>(&count_sql);
    if let Some(filter) = &filter {
        counted = counted.bind(filter.clone());
    }
    let total = counted.fetch_one(pool).await?;

    let mut connection = pool.acquire().await?;
    let mut rows = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(row) = load_secret(&mut connection, kind, id).await? {
            rows.push(row);
        }
    }
    Ok(SecretPage { rows, total })
}

/// The row a path segment names for `kind`: an opaque id, else a foreign id.
enum Lookup<'a> {
    Id(i64),
    Oid(&'a str),
    ForeignId(&'a str),
}

async fn lookup_row<'e, M, E>(executor: E, lookup: Lookup<'_>) -> Result<Option<SecretTableRow>>
where
    M: ConsoleModel,
    E: sqlx::Executor<'e, Database = Postgres>,
{
    match lookup {
        Lookup::Id(id) => model::find_by_id::<M, SecretTableRow, _>(executor, id).await,
        Lookup::Oid(oid) => model::find_by_oid::<M, SecretTableRow, _>(executor, oid).await,
        Lookup::ForeignId(foreign_id) => {
            model::find_by_foreign_id::<M, SecretTableRow, _>(executor, foreign_id).await
        }
    }
}

/// The secret an opaque id names, loaded with its sources, rules, and broker.
pub async fn find_secret(pool: &PgPool, kind: SecretKind, oid: &str) -> Result<Option<SecretRow>> {
    lookup_secret(pool, kind, Lookup::Oid(oid)).await
}

/// The secret a globally unique foreign id names.
pub async fn find_secret_by_foreign_id(
    pool: &PgPool,
    kind: SecretKind,
    foreign_id: &str,
) -> Result<Option<SecretRow>> {
    lookup_secret(pool, kind, Lookup::ForeignId(foreign_id)).await
}

async fn lookup_secret(
    pool: &PgPool,
    kind: SecretKind,
    lookup: Lookup<'_>,
) -> Result<Option<SecretRow>> {
    let Some(row) = find_table_row(pool, kind, lookup).await? else {
        return Ok(None);
    };
    let mut connection = pool.acquire().await?;
    Ok(Some(hydrate(&mut connection, kind, row).await?))
}

pub(crate) async fn load_secret(
    connection: &mut PgConnection,
    kind: SecretKind,
    id: i64,
) -> Result<Option<SecretRow>> {
    let Some(row) = find_table_row(&mut *connection, kind, Lookup::Id(id)).await? else {
        return Ok(None);
    };
    Ok(Some(hydrate(connection, kind, row).await?))
}

/// The parent row plus its nested associations, the document every payload and
/// write comparison reads.
async fn hydrate(
    connection: &mut PgConnection,
    kind: SecretKind,
    row: SecretTableRow,
) -> Result<SecretRow> {
    let id = row.id;
    let mut secret = SecretRow {
        kind,
        id,
        foreign_id: row.foreign_id.clone(),
        name: row.name.clone(),
        description: row.description.clone(),
        labels: row.labels.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        created_by_id: row.created_by_id,
        broker_credential_id: row.broker_credential_id,
        owner_id: row.owner_id,
        fields: row.fields(kind),
        sources: Vec::new(),
        rules: Vec::new(),
        broker: None,
    };
    secret.sources = load_sources(connection, kind, &[id])
        .await?
        .remove(&id)
        .unwrap_or_default();
    secret.rules = load_rules(connection, kind, &[id])
        .await?
        .remove(&id)
        .unwrap_or_default();
    if let Some(broker_id) = secret.broker_credential_id {
        secret.broker = load_broker(connection, broker_id).await?;
    }
    Ok(secret)
}

pub(crate) async fn load_sources(
    connection: &mut PgConnection,
    kind: SecretKind,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<SourceRow>>> {
    let Some(owner_column) = kind.source_owner_column() else {
        return Ok(Default::default());
    };
    let rows: Vec<OwnedSourceRow> = sqlx::query_as(&format!(
        "SELECT {owner_column} AS owner_id, id, source_type, config, secret, role, role_kind, \
         broker_credential_id FROM secret_sources WHERE {owner_column} = ANY($1) ORDER BY id"
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await?;
    Ok(group_by_owner(
        rows.into_iter()
            .map(|row| (row.owner_id, row.source))
            .collect(),
    ))
}

pub(crate) async fn load_rules(
    connection: &mut PgConnection,
    kind: SecretKind,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<RuleRow>>> {
    let Some(owner_column) = kind.rule_owner_column() else {
        return Ok(Default::default());
    };
    let rows: Vec<OwnedRuleRow> = sqlx::query_as(&format!(
        "SELECT {owner_column} AS owner_id, id, host, cidr, position, http_methods, paths \
         FROM request_rules WHERE {owner_column} = ANY($1) ORDER BY position, id"
    ))
    .bind(ids)
    .fetch_all(&mut *connection)
    .await?;
    Ok(group_by_owner(
        rows.into_iter()
            .map(|row| (row.owner_id, row.rule))
            .collect(),
    ))
}

/// One nested source with the owner column the batch query selects beside it.
#[derive(Debug, sqlx::FromRow)]
struct OwnedSourceRow {
    owner_id: i64,
    #[sqlx(flatten)]
    source: SourceRow,
}

/// One nested rule with the owner column the batch query selects beside it.
#[derive(Debug, sqlx::FromRow)]
struct OwnedRuleRow {
    owner_id: i64,
    #[sqlx(flatten)]
    rule: RuleRow,
}

fn group_by_owner<T>(rows: Vec<(i64, T)>) -> std::collections::HashMap<i64, Vec<T>> {
    let mut grouped: std::collections::HashMap<i64, Vec<T>> = Default::default();
    for (owner_id, row) in rows {
        grouped.entry(owner_id).or_default().push(row);
    }
    grouped
}

async fn load_broker(connection: &mut PgConnection, id: i64) -> Result<Option<BrokerRow>> {
    Ok(sqlx::query_as(
        "SELECT bc.id, bc.oauth_app_id, oa.slug AS oauth_app_slug \
         FROM broker_credentials bc LEFT JOIN oauth_apps oa ON oa.id = bc.oauth_app_id \
         WHERE bc.id = $1",
    )
    .bind(id)
    .fetch_optional(connection)
    .await?)
}

#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct RoleSummary {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
}

impl RoleSummary {
    pub fn oid(&self) -> String {
        model::Role::oid(self.id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrantRow {
    pub id: i64,
    pub role_id: i64,
    pub priority: i32,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
    pub role: RoleSummary,
}

/// The grant projection the secret-scoped routes select: the grant row and the
/// joined role's summary.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct GrantTableRow {
    pub(crate) id: i64,
    pub(crate) role_id: i64,
    pub(crate) priority: i32,
    pub(crate) created_at: PrimitiveDateTime,
    pub(crate) updated_at: PrimitiveDateTime,
    pub(crate) role_foreign_id: Option<String>,
    pub(crate) role_name: Option<String>,
}

impl From<GrantTableRow> for GrantRow {
    fn from(row: GrantTableRow) -> Self {
        Self {
            id: row.id,
            role_id: row.role_id,
            priority: row.priority,
            created_at: row.created_at,
            updated_at: row.updated_at,
            role: RoleSummary {
                id: row.role_id,
                foreign_id: row.role_foreign_id,
                name: row.role_name,
            },
        }
    }
}

pub(crate) const GRANT_COLUMNS: &str = "g.id, g.role_id, g.priority, g.created_at, \
     g.updated_at, r.foreign_id AS role_foreign_id, r.name AS role_name";

/// The join every grant projection uses: a grant with a grantee role.
pub(crate) const GRANT_FROM: &str = "FROM grants g JOIN roles r ON r.id = g.role_id";

pub async fn find_role(pool: &PgPool, oid: &str) -> Result<Option<RoleSummary>> {
    model::find_by_oid::<model::Role, RoleSummary, _>(pool, oid).await
}

pub async fn role_grants(
    pool: &PgPool,
    kind: SecretKind,
    secret_id: i64,
) -> Result<(Vec<GrantRow>, Vec<RoleSummary>)> {
    let grants: Vec<GrantTableRow> = sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} {GRANT_FROM} \
         WHERE g.{} = $1 AND g.role_id IS NOT NULL ORDER BY g.id",
        kind.grant_column()
    ))
    .bind(secret_id)
    .fetch_all(pool)
    .await?;
    let granted: Vec<i64> = grants.iter().map(|grant| grant.role_id).collect();
    let assignable: Vec<RoleSummary> = sqlx::query_as(
        "SELECT id, foreign_id, name FROM roles WHERE NOT (id = ANY($1)) ORDER BY id",
    )
    .bind(&granted)
    .fetch_all(pool)
    .await?;
    Ok((grants.into_iter().map(GrantRow::from).collect(), assignable))
}

pub async fn find_role_grant(
    pool: &PgPool,
    kind: SecretKind,
    secret_id: i64,
    oid: &str,
) -> Result<Option<GrantRow>> {
    let Some(id) = model::Grant::decode_oid(oid) else {
        return Ok(None);
    };
    let row: Option<GrantTableRow> = sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} {GRANT_FROM} \
         WHERE g.id = $1 AND g.{} = $2 AND g.role_id IS NOT NULL",
        kind.grant_column()
    ))
    .bind(id)
    .bind(secret_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(GrantRow::from))
}

#[cfg(test)]
mod tests {
    use super::SecretKind;

    #[test]
    fn the_kind_table_is_canonical_and_complete() {
        assert_eq!(SecretKind::ALL.len(), 8);
        for kind in SecretKind::ALL {
            assert_eq!(SecretKind::parse(kind.slug()), Some(kind));
            assert_eq!(
                kind.not_found().to_string(),
                format!("{} not found", kind.model_name())
            );
            let oid = kind.oid(42);
            assert_eq!(kind.decode_oid(&oid), Some(42));
            assert!(oid.starts_with(&format!("{}_", kind.oid_prefix())));
        }
        let slugs: Vec<&str> = SecretKind::ALL.iter().map(|kind| kind.slug()).collect();
        assert_eq!(
            slugs,
            [
                "static",
                "gcp_auth",
                "gcp_id_token",
                "aws_auth",
                "oauth_token",
                "pg_dsn",
                "hmac",
                "ssh_key",
            ]
        );
    }

    #[test]
    fn oids_decode_only_for_their_own_kind() {
        let oid = SecretKind::Static.oid(7);
        assert_eq!(SecretKind::PgDsn.decode_oid(&oid), None);
        assert_eq!(SecretKind::Static.decode_oid("nope"), None);
    }
}
