//! Slack channel permissions of a principal or a role: upsert one,
//! replace all, delete one. A permission needs a valid channel id and at
//! least one flag on; channel ids are stored trimmed and upper-cased.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::decode_oid;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::PgConnection;

use super::{
    FieldErrors, Grantee,
    payload::{PermissionRow, merge_permissions},
};
use crate::{
    ApiError, ApiResult, AppState,
    extract::{Data, Path, data},
};

/// `[CDG]` followed by at least eight upper-case letters or digits.
pub fn valid_channel_id(value: &str) -> bool {
    value.len() >= 9
        && value.starts_with(['C', 'D', 'G'])
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn normalize(channel_id: &str) -> String {
    channel_id.trim().to_uppercase()
}

fn is_direct_message(channel_id: &str) -> bool {
    channel_id.starts_with('D')
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PermissionInput {
    #[serde(default)]
    pub channel_id: String,
    pub upload_enabled: Option<bool>,
    pub download_enabled: Option<bool>,
    pub history_enabled: Option<bool>,
}

#[derive(Deserialize)]
pub struct ReplaceInput {
    #[serde(default)]
    slack_channel_permissions: Vec<PermissionInput>,
}

fn validate(rows: &[PermissionRow]) -> ApiResult<()> {
    let mut errors = FieldErrors::new();
    for row in rows {
        if row.channel_id.is_empty() {
            errors.add("channel_id", "can't be blank");
        } else if !valid_channel_id(&row.channel_id) {
            errors.add("channel_id", "is not a valid Slack channel ID");
        }
        if !(row.upload_enabled || row.download_enabled || row.history_enabled) {
            errors.add("base", "Select at least one Slack permission");
        }
    }
    errors.finish()
}

async fn current(
    conn: &mut PgConnection,
    owner: Grantee,
) -> Result<Vec<PermissionRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT channel_id, upload_enabled, download_enabled, history_enabled \
         FROM slack_channel_permissions WHERE {} = $1 ORDER BY channel_id COLLATE \"C\", id",
        owner.column()
    ))
    .bind(owner.id())
    .fetch_all(conn)
    .await
}

/// Replaces the owner's permissions with `rows` (duplicate channels merge,
/// flags OR-ed). A principal keeps its direct-message permissions: they
/// come from Slack, not from this API. Nothing is written when nothing
/// changes.
pub async fn replace_in(
    tx: &mut PgConnection,
    owner: Grantee,
    rows: &[PermissionInput],
    now: DateTime<Utc>,
) -> ApiResult<()> {
    let existing = current(tx, owner).await?;
    // Normalize on entry: validation and the direct-message filter must see
    // the id as it is stored, not as the client spelled it.
    let mut wanted = merge_permissions(
        &rows
            .iter()
            .map(|row| PermissionRow {
                channel_id: normalize(&row.channel_id),
                upload_enabled: row.upload_enabled.unwrap_or(false),
                download_enabled: row.download_enabled.unwrap_or(false),
                history_enabled: row.history_enabled.unwrap_or(false),
            })
            .collect::<Vec<_>>(),
    );
    if matches!(owner, Grantee::Principal(_)) {
        wanted.retain(|row| !is_direct_message(&row.channel_id));
        wanted.extend(
            existing
                .iter()
                .filter(|row| is_direct_message(&row.channel_id))
                .cloned(),
        );
        wanted = merge_permissions(&wanted);
    }
    validate(&wanted)?;
    if existing == wanted {
        return Ok(());
    }
    sqlx::query(&format!(
        "DELETE FROM slack_channel_permissions WHERE {} = $1",
        owner.column()
    ))
    .bind(owner.id())
    .execute(&mut *tx)
    .await?;
    for row in &wanted {
        insert(tx, owner, row, now).await?;
    }
    owner.bump(tx).await
}

/// Inserts a permission; `false` when the owner already has the channel.
async fn insert(
    conn: &mut PgConnection,
    owner: Grantee,
    row: &PermissionRow,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let inserted = sqlx::query(&format!(
        "INSERT INTO slack_channel_permissions ({}, channel_id, upload_enabled, \
         download_enabled, history_enabled, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) ON CONFLICT DO NOTHING",
        owner.column()
    ))
    .bind(owner.id())
    .bind(&row.channel_id)
    .bind(row.upload_enabled)
    .bind(row.download_enabled)
    .bind(row.history_enabled)
    .bind(now.naive_utc())
    .execute(conn)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

/// Creates the permission (every flag on unless given) or updates the
/// given flags of the existing one. `true` when it was created.
pub async fn upsert_in(
    tx: &mut PgConnection,
    owner: Grantee,
    input: &PermissionInput,
    now: DateTime<Utc>,
) -> ApiResult<(PermissionRow, bool)> {
    let channel_id = normalize(&input.channel_id);
    let mut attempts = 0;
    loop {
        attempts += 1;
        let existing: Option<PermissionRow> = sqlx::query_as(&format!(
            "SELECT channel_id, upload_enabled, download_enabled, history_enabled \
             FROM slack_channel_permissions WHERE {} = $1 AND channel_id = $2 FOR UPDATE",
            owner.column()
        ))
        .bind(owner.id())
        .bind(&channel_id)
        .fetch_optional(&mut *tx)
        .await?;
        let base = existing.clone().unwrap_or(PermissionRow {
            channel_id: channel_id.clone(),
            upload_enabled: true,
            download_enabled: true,
            history_enabled: true,
        });
        let row = PermissionRow {
            channel_id: channel_id.clone(),
            upload_enabled: input.upload_enabled.unwrap_or(base.upload_enabled),
            download_enabled: input.download_enabled.unwrap_or(base.download_enabled),
            history_enabled: input.history_enabled.unwrap_or(base.history_enabled),
        };
        validate(std::slice::from_ref(&row))?;
        match existing {
            Some(existing) => {
                if existing != row {
                    sqlx::query(&format!(
                        "UPDATE slack_channel_permissions SET upload_enabled = $3, \
                         download_enabled = $4, history_enabled = $5, updated_at = $6 \
                         WHERE {} = $1 AND channel_id = $2",
                        owner.column()
                    ))
                    .bind(owner.id())
                    .bind(&channel_id)
                    .bind(row.upload_enabled)
                    .bind(row.download_enabled)
                    .bind(row.history_enabled)
                    .bind(now.naive_utc())
                    .execute(&mut *tx)
                    .await?;
                    owner.bump(tx).await?;
                }
                return Ok((row, false));
            }
            None if insert(tx, owner, &row, now).await? => {
                owner.bump(tx).await?;
                return Ok((row, true));
            }
            // A concurrent request created it first: update that one.
            None if attempts < 2 => {}
            None => return Err(ApiError::internal("permission insert conflicted twice")),
        }
    }
}

// --- handlers ---------------------------------------------------------------

async fn resolve(state: &AppState, role: bool, oid: &str) -> ApiResult<Grantee> {
    let (prefix, table) = if role {
        ("role", "roles")
    } else {
        ("prn", "principals")
    };
    let id = decode_oid(prefix, oid).ok_or_else(ApiError::not_found)?;
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    Ok(if role {
        Grantee::Role(id)
    } else {
        Grantee::Principal(id)
    })
}

async fn upsert(state: AppState, owner: Grantee, input: PermissionInput) -> ApiResult {
    let mut tx = state.db.begin().await?;
    let (row, created) = upsert_in(&mut tx, owner, &input, state.now()).await?;
    tx.commit().await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, data(row)).into_response())
}

async fn replace(state: AppState, owner: Grantee, input: ReplaceInput) -> ApiResult {
    let mut tx = state.db.begin().await?;
    replace_in(
        &mut tx,
        owner,
        &input.slack_channel_permissions,
        state.now(),
    )
    .await?;
    let rows = current(&mut tx, owner).await?;
    tx.commit().await?;
    Ok(data(rows).into_response())
}

async fn destroy(state: AppState, owner: Grantee, channel_id: &str) -> ApiResult {
    let mut tx = state.db.begin().await?;
    let deleted = sqlx::query(&format!(
        "DELETE FROM slack_channel_permissions WHERE {} = $1 AND channel_id = $2",
        owner.column()
    ))
    .bind(owner.id())
    .bind(normalize(channel_id))
    .execute(&mut *tx)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    owner.bump(&mut tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn principal_upsert(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Data(input): Data<PermissionInput>,
) -> ApiResult {
    let owner = resolve(&state, false, &id).await?;
    upsert(state, owner, input).await
}

pub async fn role_upsert(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Data(input): Data<PermissionInput>,
) -> ApiResult {
    let owner = resolve(&state, true, &id).await?;
    upsert(state, owner, input).await
}

pub async fn principal_replace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Data(input): Data<ReplaceInput>,
) -> ApiResult {
    let owner = resolve(&state, false, &id).await?;
    replace(state, owner, input).await
}

pub async fn role_replace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Data(input): Data<ReplaceInput>,
) -> ApiResult {
    let owner = resolve(&state, true, &id).await?;
    replace(state, owner, input).await
}

pub async fn principal_destroy(
    State(state): State<AppState>,
    Path((id, channel_id)): Path<(String, String)>,
) -> ApiResult {
    let owner = resolve(&state, false, &id).await?;
    destroy(state, owner, &channel_id).await
}

pub async fn role_destroy(
    State(state): State<AppState>,
    Path((id, channel_id)): Path<(String, String)>,
) -> ApiResult {
    let owner = resolve(&state, true, &id).await?;
    destroy(state, owner, &channel_id).await
}
