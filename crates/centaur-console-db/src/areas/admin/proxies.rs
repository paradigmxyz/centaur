//! Proxies: the bearer credentials agents receive instead of an upstream secret.
//! Each proxy names the principal it serves and, optionally, the principal that
//! requested it.

use sqlx::PgPool;
use time::PrimitiveDateTime;

use crate::Result;
use crate::model::{self, Proxy as ProxyModel};

/// The columns a proxy payload renders. The token hash is stored but never
/// returned, and the requester principal is referenced by its id alone.
pub const PROXY_COLUMNS: &str = "\
    id, name, labels, principal_id, principal_assigned_at, \
    requester_principal_id, requester_principal_assigned_at, created_at, updated_at";

pub const PROXY_SELECT: &str = "\
    SELECT id, name, labels, principal_id, principal_assigned_at, \
    requester_principal_id, requester_principal_assigned_at, created_at, updated_at";

/// The list filter: `principal_id` and `labels @> $2`, in the order the bearer
/// index binds them.
pub const PROXY_LIST_FROM: &str = "\
    FROM proxies \
    WHERE ($1::bigint IS NULL OR principal_id = $1) \
      AND ($2::jsonb IS NULL OR labels @> $2::jsonb)";

pub const PROXY_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

/// A proxy row, without its token hash: the API only ever shows the hash's
/// presence, and issue-time plaintext tokens never come back from the database.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ProxyRow {
    pub id: i64,
    pub name: String,
    #[sqlx(json)]
    pub labels: serde_json::Value,
    pub principal_id: Option<i64>,
    pub principal_assigned_at: Option<PrimitiveDateTime>,
    pub requester_principal_id: Option<i64>,
    pub requester_principal_assigned_at: Option<PrimitiveDateTime>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

pub async fn admin_proxy(pool: &PgPool, id: i64) -> Result<Option<ProxyRow>> {
    Ok(
        sqlx::query_as(&format!("{PROXY_SELECT} FROM proxies WHERE id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// The changes a proxy update asks for. `None` leaves a column alone;
/// `Some(None)` clears an optional principal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyChange {
    pub name: Option<String>,
    pub labels: Option<serde_json::Value>,
    pub principal_id: Option<Option<i64>>,
    pub requester_principal_id: Option<Option<i64>>,
}

/// Insert a proxy whose assignment times are stamped by the database, in the
/// same statement, so this stays a plain insert.
pub async fn create_proxy(
    pool: &PgPool,
    name: &str,
    labels: &serde_json::Value,
    token_hash: &str,
    principal_id: Option<i64>,
    requester_principal_id: Option<i64>,
) -> Result<ProxyRow> {
    let sql = format!(
        "INSERT INTO proxies \
            (name, labels, bearer_token_hash, principal_id, principal_assigned_at, \
             requester_principal_id, requester_principal_assigned_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, CASE WHEN $4::bigint IS NULL THEN NULL ELSE now() END, $5, \
                 CASE WHEN $5::bigint IS NULL THEN NULL ELSE now() END, now(), now()) \
         RETURNING {PROXY_COLUMNS}"
    );
    Ok(sqlx::query_as(&sql)
        .bind(name)
        .bind(sqlx::types::Json(labels.clone()))
        .bind(token_hash)
        .bind(principal_id)
        .bind(requester_principal_id)
        .fetch_one(pool)
        .await?)
}

/// Apply `change`. An update that changes nothing leaves `updated_at` alone.
///
/// The assignment timestamps are stamped (or cleared) by the database only when
/// their principal changes, which a column-value update cannot express.
pub async fn update_proxy(
    pool: &PgPool,
    id: i64,
    change: &ProxyChange,
) -> Result<Option<ProxyRow>> {
    let Some(current) = admin_proxy(pool, id).await? else {
        return Ok(None);
    };
    let principal_id = change.principal_id.unwrap_or(current.principal_id);
    let requester_principal_id = change
        .requester_principal_id
        .unwrap_or(current.requester_principal_id);
    let name = change.name.as_deref().unwrap_or(&current.name);
    let labels = change.labels.as_ref().unwrap_or(&current.labels);
    let principal_changed = principal_id != current.principal_id;
    let requester_changed = requester_principal_id != current.requester_principal_id;
    if !principal_changed && !requester_changed && name == current.name && labels == &current.labels
    {
        return Ok(Some(current));
    }
    sqlx::query(
        "UPDATE proxies SET name = $2, labels = $3, \
             principal_id = $4, \
             principal_assigned_at = CASE WHEN $5 THEN \
                 CASE WHEN $4::bigint IS NULL THEN NULL ELSE now() END \
                 ELSE principal_assigned_at END, \
             requester_principal_id = $6, \
             requester_principal_assigned_at = CASE WHEN $7 THEN \
                 CASE WHEN $6::bigint IS NULL THEN NULL ELSE now() END \
                 ELSE requester_principal_assigned_at END, \
             updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(sqlx::types::Json(labels.clone()))
    .bind(principal_id)
    .bind(principal_changed)
    .bind(requester_principal_id)
    .bind(requester_changed)
    .execute(pool)
    .await?;
    admin_proxy(pool, id).await
}

pub async fn delete_proxy(pool: &PgPool, id: i64) -> Result<()> {
    model::delete::<ProxyModel, _>(pool, id).await?;
    Ok(())
}
