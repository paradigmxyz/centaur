//! Grants of a secret to a principal or a role: create, show, destroy, and
//! the grants of one grantee.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::decode_oid;
use serde::Deserialize;
use serde_json::Value;

use super::Actor;
use super::{
    FieldErrors, Grantee, SECRET_KINDS,
    payload::{self, GRANT_COLUMNS, GrantRow},
    principal::principal_id,
    role::role_id,
};
use crate::{
    ApiError, ApiResult, AppState,
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};

/// Direct grants outrank role grants when both reach a secret.
const DIRECT_PRIORITY: i32 = 100;
const ROLE_PRIORITY: i32 = 0;

/// `{ "principal_id" | "role_id": oid, "<kind>_secret_id": oid }`.
#[derive(Deserialize)]
pub struct NewGrant {
    principal_id: Option<String>,
    role_id: Option<String>,
    #[serde(flatten)]
    grantable: serde_json::Map<String, Value>,
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

async fn exists(state: &AppState, table: &str, id: i64) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?)
}

/// `POST /grants`: 201 for a new grant, 200 when it already existed.
pub async fn create(
    State(state): State<AppState>,
    Actor(actor): Actor,
    Data(input): Data<NewGrant>,
) -> ApiResult {
    let mut errors = FieldErrors::new();
    let grantee_oid = present(&input.principal_id)
        .map(|oid| ("principal_id", oid))
        .or_else(|| present(&input.role_id).map(|oid| ("role_id", oid)));
    if grantee_oid.is_none() {
        errors.add("base", "must reference one of principal_id, role_id");
    }
    let secret = SECRET_KINDS.iter().find_map(|kind| {
        let oid = input.grantable.get(kind.column)?.as_str()?;
        (!oid.trim().is_empty()).then_some((kind, oid))
    });
    if secret.is_none() {
        let columns: Vec<&str> = SECRET_KINDS.iter().map(|kind| kind.column).collect();
        errors.add(
            "base",
            format!("must reference one of {}", columns.join(", ")),
        );
    }
    errors.finish()?;
    let ((grantee_column, grantee_oid), (kind, secret_oid)) =
        (grantee_oid.expect("checked"), secret.expect("checked"));
    let grantee = if grantee_column == "principal_id" {
        let id = principal_id(grantee_oid)?;
        exists(&state, "principals", id)
            .await?
            .then_some(Grantee::Principal(id))
    } else {
        let id = role_id(grantee_oid)?;
        exists(&state, "roles", id)
            .await?
            .then_some(Grantee::Role(id))
    }
    .ok_or_else(ApiError::not_found)?;
    let secret_id = decode_oid(kind.prefix, secret_oid).ok_or_else(ApiError::not_found)?;
    if !exists(&state, kind.table, secret_id).await? {
        return Err(ApiError::not_found());
    }

    let priority = match grantee {
        Grantee::Principal(_) => DIRECT_PRIORITY,
        Grantee::Role(_) => ROLE_PRIORITY,
    };
    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    let id: Option<i64> = sqlx::query_scalar(&format!(
        "INSERT INTO grants ({}, {}, priority, created_by_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT DO NOTHING RETURNING id",
        grantee.column(),
        kind.column
    ))
    .bind(grantee.id())
    .bind(secret_id)
    .bind(priority)
    .bind(actor)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    if id.is_some() {
        grantee.bump(&mut tx).await?;
    }
    tx.commit().await?;
    let (status, id) = match id {
        Some(id) => (StatusCode::CREATED, id),
        None => (
            StatusCode::OK,
            sqlx::query_scalar(&format!(
                "SELECT id FROM grants WHERE {} = $1 AND {} = $2",
                grantee.column(),
                kind.column
            ))
            .bind(grantee.id())
            .bind(secret_id)
            .fetch_one(&state.db)
            .await?,
        ),
    };
    Ok((status, data(payload::grant_json(&state.db, id).await?)).into_response())
}

fn grant_id(oid: &str) -> ApiResult<i64> {
    decode_oid("grant", oid).ok_or_else(ApiError::not_found)
}

/// `GET /grants/{id}`.
pub async fn show(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = grant_id(&id)?;
    let row: GrantRow =
        sqlx::query_as(&format!("SELECT {GRANT_COLUMNS} FROM grants WHERE id = $1"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(ApiError::not_found)?;
    let mut rendered = payload::grants_json(&state.db, vec![row]).await?;
    Ok(data(rendered.remove(0)).into_response())
}

/// `DELETE /grants/{id}`.
pub async fn destroy(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = grant_id(&id)?;
    let mut tx = state.db.begin().await?;
    let owner: Option<(Option<i64>, Option<i64>)> =
        sqlx::query_as("DELETE FROM grants WHERE id = $1 RETURNING principal_id, role_id")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((principal, role)) = owner else {
        return Err(ApiError::not_found());
    };
    match (principal, role) {
        (Some(id), _) => Grantee::Principal(id).bump(&mut tx).await?,
        (None, Some(id)) => Grantee::Role(id).bump(&mut tx).await?,
        (None, None) => {}
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn grantee_grants(state: AppState, grantee: Grantee, page: PageParams) -> ApiResult {
    let page = page.resolve();
    let column = grantee.column();
    let total: i64 =
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM grants WHERE {column} = $1"))
            .bind(grantee.id())
            .fetch_one(&state.db)
            .await?;
    let rows: Vec<GrantRow> = sqlx::query_as(&format!(
        "SELECT {GRANT_COLUMNS} FROM grants WHERE {column} = $1 \
         ORDER BY created_at ASC, id ASC LIMIT $2 OFFSET $3"
    ))
    .bind(grantee.id())
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    let data = payload::grants_json(&state.db, rows).await?;
    Ok(axum::Json(serde_json::json!({"data": data, "meta": page.meta(total)})).into_response())
}

/// `GET /principals/{id}/grants`.
pub async fn principal_index(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let id = principal_id(&id)?;
    if !exists(&state, "principals", id).await? {
        return Err(ApiError::not_found());
    }
    grantee_grants(state, Grantee::Principal(id), page).await
}

/// `GET /roles/{id}/grants`.
pub async fn role_index(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let id = role_id(&id)?;
    if !exists(&state, "roles", id).await? {
        return Err(ApiError::not_found());
    }
    grantee_grants(state, Grantee::Role(id), page).await
}
