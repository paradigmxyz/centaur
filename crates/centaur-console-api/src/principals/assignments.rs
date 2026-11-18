//! A principal's roles: list, assign, unassign.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::sync_config::bump_sync_config;
use serde::Deserialize;

use super::{
    payload::{self, ROLE_COLUMNS, RoleRow},
    principal::principal_id,
    role::role_id,
};
use crate::{
    ApiError, ApiResult, AppState,
    extract::{Data, Path, data},
};

async fn principal(state: &AppState, oid: &str) -> ApiResult<i64> {
    let id = principal_id(oid)?;
    let found: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM principals WHERE id = $1)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    found.then_some(id).ok_or_else(ApiError::not_found)
}

/// `GET /principals/{id}/roles`.
pub async fn index(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = principal(&state, &id).await?;
    let rows: Vec<RoleRow> = sqlx::query_as(&format!(
        "SELECT {ROLE_COLUMNS} FROM roles \
         WHERE id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1) ORDER BY id"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(data(payload::roles_json(&state.db, rows).await?).into_response())
}

#[derive(Deserialize)]
pub struct Assign {
    role_id: String,
}

/// `POST /principals/{id}/roles`: 201 when assigned now, 200 when the
/// principal already had the role.
pub async fn create(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Data(input): Data<Assign>,
) -> ApiResult {
    let principal_id = principal(&state, &id).await?;
    let role_id = role_id(&input.role_id)?;
    let mut tx = state.db.begin().await?;
    let role_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM roles WHERE id = $1)")
        .bind(role_id)
        .fetch_one(&mut *tx)
        .await?;
    if !role_exists {
        return Err(ApiError::not_found());
    }
    let now = state.now().naive_utc();
    let inserted = sqlx::query(
        "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $3) ON CONFLICT DO NOTHING",
    )
    .bind(principal_id)
    .bind(role_id)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        bump_sync_config(&mut tx, &[principal_id]).await?;
    }
    tx.commit().await?;
    let status = if inserted {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, data(payload::role_json(&state.db, role_id).await?)).into_response())
}

/// `DELETE /principals/{id}/roles/{role_id}`.
pub async fn destroy(
    State(state): State<AppState>,
    Path((id, role)): Path<(String, String)>,
) -> ApiResult {
    let principal_id = principal(&state, &id).await?;
    let role_id = role_id(&role)?;
    let mut tx = state.db.begin().await?;
    let removed =
        sqlx::query("DELETE FROM principal_roles WHERE principal_id = $1 AND role_id = $2")
            .bind(principal_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed == 0 {
        return Err(ApiError::not_found());
    }
    bump_sync_config(&mut tx, &[principal_id]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
