//! API clients: machine callers of the api-rs session API. The plaintext secret
//! is issued here and only its SHA-256 digest is stored.

use sqlx::PgPool;
use sqlx::types::Json;
use time::PrimitiveDateTime;

use crate::Result;
use crate::model::{
    self, ApiClient as ApiClientModel, ApiClientPrincipal as ApiClientPrincipalModel, ConsoleModel,
    Principal as PrincipalModel,
};
use crate::write;

const CLIENT_COLUMNS: &str = "id, name, scopes, disabled_at, created_at, updated_at";

/// An API client row without its principal links.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ApiClientRow {
    pub id: i64,
    pub name: String,
    #[sqlx(json)]
    pub scopes: Vec<String>,
    pub disabled_at: Option<PrimitiveDateTime>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

impl ApiClientRow {
    pub fn disabled(&self) -> bool {
        self.disabled_at.is_some()
    }
}

/// A principal attached to an API client, as the links payload shows it.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ApiClientPrincipalRef {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub kind: String,
}

/// One `api_client_principals` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiClientLink {
    pub id: i64,
    pub default: bool,
    pub principal: ApiClientPrincipalRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiClientWithLinks {
    pub client: ApiClientRow,
    pub links: Vec<ApiClientLink>,
}

/// The joined link projection: the link's own columns plus the attached
/// principal's, which become [`ApiClientLink::principal`].
#[derive(sqlx::FromRow)]
struct LinkRecord {
    id: i64,
    api_client_id: i64,
    #[sqlx(rename = "default")]
    default: bool,
    principal_id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    kind: String,
}

impl LinkRecord {
    fn into_link(self) -> ApiClientLink {
        ApiClientLink {
            id: self.id,
            default: self.default,
            principal: ApiClientPrincipalRef {
                id: self.principal_id,
                foreign_id: self.foreign_id,
                name: self.name,
                kind: self.kind,
            },
        }
    }
}

/// Every API client in id order, with its links in link-id order.
pub async fn api_clients(pool: &PgPool) -> Result<Vec<ApiClientWithLinks>> {
    let clients: Vec<ApiClientRow> = sqlx::query_as(&format!(
        "SELECT {CLIENT_COLUMNS} FROM {} ORDER BY id ASC",
        ApiClientModel::TABLE
    ))
    .fetch_all(pool)
    .await?;
    let ids = clients.iter().map(|client| client.id).collect::<Vec<_>>();
    let mut links = api_client_links(pool, &ids).await?;
    Ok(clients
        .into_iter()
        .map(|client| ApiClientWithLinks {
            links: links.remove(&client.id).unwrap_or_default(),
            client,
        })
        .collect())
}

/// The API client whose id is `id`, or `None` when it does not exist.
pub async fn api_client(pool: &PgPool, id: i64) -> Result<Option<ApiClientWithLinks>> {
    let client: Option<ApiClientRow> = sqlx::query_as(&format!(
        "SELECT {CLIENT_COLUMNS} FROM {} WHERE id = $1",
        ApiClientModel::TABLE
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(client) = client else {
        return Ok(None);
    };
    let links = api_client_links(pool, &[client.id])
        .await?
        .remove(&client.id)
        .unwrap_or_default();
    Ok(Some(ApiClientWithLinks { client, links }))
}

/// Insert an API client and optionally attach its first principal, which
/// becomes the default, in one transaction.
pub async fn create_api_client(
    pool: &PgPool,
    name: &str,
    scopes: &[String],
    created_by: i64,
    secret_digest: &str,
    principal_id: Option<i64>,
) -> Result<i64> {
    let mut transaction = pool.begin().await?;
    let id: i64 = sqlx::query_scalar(&write::insert_returning_sql(
        ApiClientModel::TABLE,
        &["name", "secret_digest", "scopes", "created_by_id"],
        "id",
    ))
    .bind(name)
    .bind(secret_digest)
    .bind(Json(scopes))
    .bind(created_by)
    .fetch_one(&mut *transaction)
    .await?;
    if let Some(principal_id) = principal_id {
        // A brand-new client has no default link, so the first one is it.
        sqlx::query(&write::insert_sql(
            ApiClientPrincipalModel::TABLE,
            &["api_client_id", "principal_id", "default"],
        ))
        .bind(id)
        .bind(principal_id)
        .bind(true)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(id)
}

/// The stored row takes the attributes the request carried and keeps the rest.
pub async fn update_api_client(
    pool: &PgPool,
    id: i64,
    name: &str,
    scopes: &[String],
) -> Result<()> {
    let sql = write::update_sql(ApiClientModel::TABLE, &["name", "scopes"]);
    sqlx::query(&sql)
        .bind(name)
        .bind(Json(scopes))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace the digest and bump `updated_at`.
pub async fn rotate_api_client_secret(pool: &PgPool, id: i64, secret_digest: &str) -> Result<()> {
    let sql = write::update_sql(ApiClientModel::TABLE, &["secret_digest"]);
    sqlx::query(&sql)
        .bind(secret_digest)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Disabling and enabling stamp or clear `disabled_at`; a no-op value leaves the
/// row (and its `updated_at`) untouched.
///
/// The `disabled_at IS NULL` / `IS NOT NULL` guard is what makes a repeat call a
/// no-op, so this stays a plain statement rather than a bound-value update.
pub async fn set_api_client_disabled(pool: &PgPool, id: i64, disabled: bool) -> Result<()> {
    let sql = if disabled {
        "UPDATE api_clients SET disabled_at = now(), updated_at = now() \
         WHERE id = $1 AND disabled_at IS NULL"
    } else {
        "UPDATE api_clients SET disabled_at = NULL, updated_at = now() \
         WHERE id = $1 AND disabled_at IS NOT NULL"
    };
    sqlx::query(sql).bind(id).execute(pool).await?;
    Ok(())
}

/// The link `id` of `api_client_id`, or `None` when it belongs to another client
/// or does not exist.
pub async fn find_api_client_link(
    pool: &PgPool,
    api_client_id: i64,
    id: i64,
) -> Result<Option<ApiClientLink>> {
    let record: Option<LinkRecord> = sqlx::query_as(
        "SELECT l.id, l.api_client_id, l.\"default\", p.id AS principal_id, p.foreign_id, \
                p.name, p.kind \
         FROM api_client_principals l JOIN principals p ON p.id = l.principal_id \
         WHERE l.id = $1 AND l.api_client_id = $2",
    )
    .bind(id)
    .bind(api_client_id)
    .fetch_optional(pool)
    .await?;
    Ok(record.map(LinkRecord::into_link))
}

/// Attach `principal_id`, making it the default when the client has none.
/// Re-attaching an attached principal keeps it.
pub async fn attach_api_client_principal(
    pool: &PgPool,
    api_client_id: i64,
    principal_id: i64,
) -> Result<ApiClientLink> {
    let mut transaction = pool.begin().await?;
    let existing: Option<(i64, bool)> = sqlx::query_as(
        "SELECT id, \"default\" FROM api_client_principals \
         WHERE api_client_id = $1 AND principal_id = $2",
    )
    .bind(api_client_id)
    .bind(principal_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let has_default: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM api_client_principals \
         WHERE api_client_id = $1 AND \"default\")",
    )
    .bind(api_client_id)
    .fetch_one(&mut *transaction)
    .await?;
    let id = match existing {
        Some((id, default)) => {
            if !default && !has_default {
                sqlx::query(&write::update_sql(
                    ApiClientPrincipalModel::TABLE,
                    &["default"],
                ))
                .bind(true)
                .bind(id)
                .execute(&mut *transaction)
                .await?;
            }
            id
        }
        None => {
            sqlx::query_scalar(&write::insert_returning_sql(
                ApiClientPrincipalModel::TABLE,
                &["api_client_id", "principal_id", "default"],
                "id",
            ))
            .bind(api_client_id)
            .bind(principal_id)
            .bind(!has_default)
            .fetch_one(&mut *transaction)
            .await?
        }
    };
    transaction.commit().await?;
    find_api_client_link(pool, api_client_id, id)
        .await?
        .ok_or_else(|| {
            crate::Error::InvalidData("api client link vanished after insert".to_owned())
        })
}

/// Exactly one link is the default.
pub async fn make_default_api_client_principal(
    pool: &PgPool,
    api_client_id: i64,
    id: i64,
) -> Result<Option<ApiClientLink>> {
    let mut transaction = pool.begin().await?;
    // A set-style update skips timestamps.
    sqlx::query(
        "UPDATE api_client_principals SET \"default\" = false \
         WHERE api_client_id = $1 AND \"default\" AND id <> $2",
    )
    .bind(api_client_id)
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(&write::update_changed_sql(
        ApiClientPrincipalModel::TABLE,
        &["default"],
    ))
    .bind(true)
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    find_api_client_link(pool, api_client_id, id).await
}

/// Delete one link; the client keeps the rest.
pub async fn detach_api_client_principal(pool: &PgPool, api_client_id: i64, id: i64) -> Result<()> {
    sqlx::query("DELETE FROM api_client_principals WHERE id = $1 AND api_client_id = $2")
        .bind(id)
        .bind(api_client_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete the client; its `api_client_principals` rows cascade.
pub async fn delete_api_client(pool: &PgPool, id: i64) -> Result<()> {
    model::delete::<ApiClientModel, _>(pool, id).await?;
    Ok(())
}

/// The principal named by the Console API's `data.principal` reference: an oid
/// or a foreign id. A blank reference names no principal.
pub async fn find_api_client_principal(
    pool: &PgPool,
    reference: &str,
) -> Result<Option<ApiClientPrincipalRef>> {
    if reference.is_empty() {
        return Ok(None);
    }
    model::find_by_ident::<PrincipalModel, ApiClientPrincipalRef, _>(pool, reference).await
}

async fn api_client_links(
    pool: &PgPool,
    api_client_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<ApiClientLink>>> {
    if api_client_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let records: Vec<LinkRecord> = sqlx::query_as(
        "SELECT l.id, l.api_client_id, l.\"default\", p.id AS principal_id, p.foreign_id, \
                p.name, p.kind \
         FROM api_client_principals l JOIN principals p ON p.id = l.principal_id \
         WHERE l.api_client_id = ANY($1) ORDER BY l.id ASC",
    )
    .bind(api_client_ids)
    .fetch_all(pool)
    .await?;
    let mut links: std::collections::HashMap<i64, Vec<ApiClientLink>> =
        std::collections::HashMap::new();
    for record in records {
        let api_client_id = record.api_client_id;
        links
            .entry(api_client_id)
            .or_default()
            .push(record.into_link());
    }
    Ok(links)
}
