//! A user's bearer API keys. Keys are scoped to their owner, soft-deleted, and
//! only ever stored as a SHA-256 token hash.
//!
//! This resource is the Console abstractions' reference slice: the row derives
//! [`sqlx::FromRow`] and is read with `query_as`, the table descriptor is
//! [`crate::model::ApiKey`], the insert comes from [`crate::write`], and the
//! list SQL is what `console::http::list` composes.

use sqlx::PgPool;
use time::PrimitiveDateTime;

use crate::Result;
use crate::model::{self, ApiKey as ApiKeyModel, ConsoleModel};
use crate::write;

/// The columns every key payload renders.
pub const KEY_COLUMNS: &str = "id, name, created_at, updated_at";

/// The list projection. `console::http::list` appends the same `WHERE`, the
/// ordering, and `LIMIT`/`OFFSET` to this and to its `COUNT`.
pub const KEY_LIST_SELECT: &str = "SELECT id, name, created_at, updated_at";
pub const KEY_LIST_FROM: &str = "FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL";
pub const KEY_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

/// An API key as the bearer `/api/v1/api_keys` resource renders it. The token
/// itself is never stored, so it is not part of this row.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: i64,
    pub name: String,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

/// The owner's key whose id is `id`. Another user's key is not found.
pub async fn api_key(pool: &PgPool, user_id: i64, id: i64) -> Result<Option<ApiKeyRow>> {
    let row = sqlx::query_as(&format!(
        "SELECT {KEY_COLUMNS} FROM {} \
         WHERE id = $1 AND user_id = $2 AND deleted_at IS NULL",
        ApiKeyModel::TABLE
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn create_api_key(
    pool: &PgPool,
    user_id: i64,
    name: &str,
    token_hash: &str,
) -> Result<ApiKeyRow> {
    let sql = write::insert_returning_sql(
        ApiKeyModel::TABLE,
        &["user_id", "name", "token_hash"],
        KEY_COLUMNS,
    );
    let row = sqlx::query_as(&sql)
        .bind(user_id)
        .bind(name)
        .bind(token_hash)
        .fetch_one(pool)
        .await?;
    Ok(row)
}

/// Keep the row for auditing, drop it from every scope. A soft delete writes the
/// column directly, so `updated_at` keeps its value.
pub async fn soft_delete_api_key(pool: &PgPool, id: i64) -> Result<()> {
    model::delete::<ApiKeyModel, _>(pool, id).await?;
    Ok(())
}
