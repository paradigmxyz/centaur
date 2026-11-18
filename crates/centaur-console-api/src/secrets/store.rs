//! Reading secrets: a row with its sources and rules, and the JSON the
//! per-kind resources render. Sources render only `source_type` and
//! `config`, never a value.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;

use super::{
    kind::Kind,
    parts::{CREDENTIAL_FIELD, ENDPOINT_HEADER, Rule, StoredSource, load_rules, load_sources},
};
use crate::{ApiError, pagination::Page};

/// A secret's own row: every column as JSON (`doc`) and its timestamps.
#[derive(sqlx::FromRow)]
pub struct ParentRow {
    pub id: i64,
    pub doc: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ParentRow {
    pub fn column(&self, name: &str) -> &Value {
        self.doc.get(name).unwrap_or(&Value::Null)
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        self.column(name).as_str()
    }

    /// The document columns of `kind`, as a request would carry them.
    pub fn document(&self, kind: Kind) -> Map<String, Value> {
        kind.document_columns()
            .iter()
            .map(|column| ((*column).to_owned(), self.column(column).clone()))
            .collect()
    }
}

fn select(kind: Kind, tail: &str) -> String {
    format!(
        "SELECT t.id, to_jsonb(t) AS doc, t.created_at AT TIME ZONE 'UTC' AS created_at, \
         t.updated_at AT TIME ZONE 'UTC' AS updated_at FROM {} t {tail}",
        kind.table()
    )
}

pub async fn find(
    conn: &mut PgConnection,
    kind: Kind,
    id: i64,
) -> Result<Option<ParentRow>, ApiError> {
    Ok(sqlx::query_as(&select(kind, "WHERE t.id = $1"))
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

/// The row under `FOR UPDATE`: writers of one secret take turns.
pub async fn lock(
    conn: &mut PgConnection,
    kind: Kind,
    id: i64,
) -> Result<Option<ParentRow>, ApiError> {
    Ok(sqlx::query_as(&select(kind, "WHERE t.id = $1 FOR UPDATE"))
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

pub async fn find_many(
    conn: &mut PgConnection,
    kind: Kind,
    ids: &[i64],
) -> Result<Vec<ParentRow>, ApiError> {
    Ok(sqlx::query_as(&select(kind, "WHERE t.id = ANY($1)"))
        .bind(ids)
        .fetch_all(conn)
        .await?)
}

pub async fn lock_by_foreign_id(
    conn: &mut PgConnection,
    kind: Kind,
    foreign_id: &str,
) -> Result<Option<ParentRow>, ApiError> {
    Ok(
        sqlx::query_as(&select(kind, "WHERE t.foreign_id = $1 FOR UPDATE"))
            .bind(foreign_id)
            .fetch_optional(conn)
            .await?,
    )
}

pub async fn find_by_foreign_id(
    conn: &mut PgConnection,
    kind: Kind,
    foreign_id: &str,
) -> Result<Option<ParentRow>, ApiError> {
    Ok(sqlx::query_as(&select(kind, "WHERE t.foreign_id = $1"))
        .bind(foreign_id)
        .fetch_optional(conn)
        .await?)
}

/// A secret with what it owns.
pub struct Loaded {
    pub kind: Kind,
    pub row: ParentRow,
    pub sources: Vec<StoredSource>,
    pub rules: Vec<Rule>,
}

/// Attaches sources and rules to `rows` with one query each.
pub async fn load_many(
    conn: &mut PgConnection,
    kind: Kind,
    rows: Vec<ParentRow>,
) -> Result<Vec<Loaded>, ApiError> {
    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let mut sources = load_sources(conn, kind, &ids).await?;
    let mut rules = load_rules(conn, kind, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let (mine, rest): (Vec<_>, Vec<_>) = sources
                .drain(..)
                .partition(|source| source.owner_id == row.id);
            sources = rest;
            let (own_rules, rest): (Vec<_>, Vec<_>) =
                rules.drain(..).partition(|rule| rule.owner_id == row.id);
            rules = rest;
            Loaded {
                kind,
                row,
                sources: mine,
                rules: own_rules.into_iter().map(|rule| rule.rule).collect(),
            }
        })
        .collect())
}

pub async fn load(conn: &mut PgConnection, kind: Kind, row: ParentRow) -> Result<Loaded, ApiError> {
    let mut loaded = load_many(conn, kind, vec![row]).await?;
    Ok(loaded.remove(0))
}

/// The secret with this oid; an unknown or malformed oid is a 404.
pub async fn load_oid(conn: &mut PgConnection, kind: Kind, oid: &str) -> Result<Loaded, ApiError> {
    let id = kind.decode_oid(oid).ok_or_else(ApiError::not_found)?;
    let row = find(conn, kind, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    load(conn, kind, row).await
}

pub async fn load_foreign_id(
    conn: &mut PgConnection,
    kind: Kind,
    foreign_id: &str,
) -> Result<Loaded, ApiError> {
    let row = find_by_foreign_id(conn, kind, foreign_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    load(conn, kind, row).await
}

/// One page of the kind's secrets whose labels contain `labels`, oldest
/// first, and the number of matches.
pub async fn list(
    conn: &mut PgConnection,
    kind: Kind,
    labels: &Map<String, Value>,
    page: Page,
) -> Result<(Vec<Loaded>, i64), ApiError> {
    let filter = (!labels.is_empty()).then(|| Value::Object(labels.clone()));
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {} WHERE ($1::jsonb IS NULL OR labels @> $1)",
        kind.table()
    ))
    .bind(&filter)
    .fetch_one(&mut *conn)
    .await?;
    let rows = sqlx::query_as(&select(
        kind,
        "WHERE ($1::jsonb IS NULL OR t.labels @> $1) ORDER BY t.created_at, t.id LIMIT $2 OFFSET $3",
    ))
    .bind(&filter)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&mut *conn)
    .await?;
    Ok((load_many(conn, kind, rows).await?, total))
}

fn by_role<'a>(sources: impl Iterator<Item = &'a StoredSource>) -> Value {
    Value::Object(
        sources
            .map(|source| {
                (
                    source.role().unwrap_or_default().to_owned(),
                    source.payload(),
                )
            })
            .collect(),
    )
}

impl Loaded {
    pub fn id(&self) -> i64 {
        self.row.id
    }

    pub fn oid(&self) -> String {
        self.kind.oid(self.row.id)
    }

    /// The only source of a kind that has exactly one.
    pub fn source(&self) -> Option<&StoredSource> {
        self.sources.first()
    }

    pub fn rule_payloads(&self) -> Vec<Value> {
        self.rules
            .iter()
            .enumerate()
            .map(|(position, rule)| rule.payload(position))
            .collect()
    }

    /// The resource the bearer, console, and sandbox APIs render.
    pub fn payload(&self) -> Value {
        let row = &self.row;
        let mut out = Map::new();
        let mut put = |key: &str, value: Value| {
            out.insert(key.to_owned(), value);
        };
        put("id", self.oid().into());
        let columns: &[&str] = match self.kind {
            Kind::Static => &["kind", "labels", "inject_config", "replace_config"],
            Kind::GcpAuth => &["labels", "credentials_provider", "subject", "scopes"],
            Kind::GcpIdToken => &["labels", "audience", "header"],
            Kind::AwsAuth => &["labels", "allowed_regions", "allowed_services"],
            Kind::OauthToken => &[
                "labels",
                "grant",
                "token_endpoint",
                "audience",
                "scopes",
                "header",
                "value_prefix",
            ],
            Kind::PgDsn => &["labels", "database", "role", "settings"],
            Kind::Hmac => &[
                "labels",
                "timestamp_format",
                "signature_algorithm",
                "signature_key_encoding",
                "signature_output_encoding",
                "signature_message",
                "allow_chunked_body",
                "headers",
            ],
            Kind::SshKey => &["labels", "public_key"],
        };
        for column in ["foreign_id", "name", "description"]
            .iter()
            .chain(columns.iter())
        {
            put(column, row.column(column).clone());
        }
        let one = || self.source().map_or(Value::Null, StoredSource::payload);
        match self.kind {
            Kind::Static => put("source", one()),
            Kind::GcpAuth | Kind::GcpIdToken => put("keyfile", one()),
            Kind::PgDsn => put("dsn", one()),
            Kind::SshKey => {
                put("personal", (!row.column("owner_id").is_null()).into());
                put("private_key", one());
            }
            Kind::AwsAuth => {
                for role in ["access_key_id", "secret_access_key", "session_token"] {
                    let source = self
                        .sources
                        .iter()
                        .find(|source| source.role() == Some(role));
                    put(role, source.map_or(Value::Null, StoredSource::payload));
                }
            }
            Kind::OauthToken => {
                let with = |kind: &'static str| {
                    by_role(
                        self.sources
                            .iter()
                            .filter(move |s| s.role_kind() == Some(kind)),
                    )
                };
                put("credentials", with(CREDENTIAL_FIELD));
                put("token_endpoint_headers", with(ENDPOINT_HEADER));
            }
            Kind::Hmac => put("credentials", by_role(self.sources.iter())),
        }
        if self.kind.has_rules() {
            put("rules", self.rule_payloads().into());
        }
        put("created_at", json!(row.created_at));
        put("updated_at", json!(row.updated_at));
        Value::Object(out)
    }

    /// The document as a write would carry it, inline values included: the
    /// base a partial update merges over. Never rendered.
    pub fn document(&self, encryptor: &centaur_console_db::Encryptor) -> Result<Value, ApiError> {
        let mut doc = self.row.document(self.kind);
        doc.remove("foreign_id");
        if self.kind.has_rules() {
            let rules = self.rules.iter().map(|rule| {
                json!({"host": rule.host, "cidr": rule.cidr,
                       "http_methods": rule.http_methods, "paths": rule.paths})
            });
            doc.insert("rules".into(), rules.collect());
        }
        if let (Some(key), Some(source)) = (self.kind.source_key(), self.source()) {
            let source = source.clone().decrypt(encryptor)?;
            doc.insert(
                key.into(),
                json!({"source_type": source.source_type, "config": source.config,
                       "secret": source.secret}),
            );
        }
        Ok(Value::Object(doc))
    }
}
