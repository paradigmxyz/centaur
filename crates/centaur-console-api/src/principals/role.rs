//! Roles: list, show, lookup, create, upsert (`PUT`/`PATCH` by oid or
//! `foreign_id`), and destroy.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::{decode_oid, sync_config::bump_sync_config};
use serde::Deserialize;
use serde_json::{Map, Value};
use sqlx::{PgConnection, Postgres, QueryBuilder};

use super::{
    Actor, Grantee, LabelFilter, check_foreign_id, like_pattern, maybe,
    payload::{self, ROLE_COLUMNS, RoleRow},
    permissions::{self, PermissionInput},
    principal::principal_id,
};
use crate::{
    ApiError, ApiResult, AppState,
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};

#[derive(Default, Deserialize)]
pub struct RoleInput {
    /// Create only; an upsert takes the foreign id from the path.
    pub foreign_id: Option<String>,
    #[serde(default, deserialize_with = "maybe")]
    pub name: Option<Option<String>>,
    pub labels: Option<Map<String, Value>>,
    /// Replaces the role's permissions when present.
    pub slack_channel_permissions: Option<Vec<PermissionInput>>,
}

#[derive(Default, Deserialize)]
pub struct ListQuery {
    q: Option<String>,
    /// Console role picker: hide the roles this principal already has.
    exclude_principal: Option<String>,
}

pub fn role_id(oid: &str) -> ApiResult<i64> {
    decode_oid("role", oid).ok_or_else(ApiError::not_found)
}

/// `GET /roles[?q=&exclude_principal=&labels[k]=v&page=&limit=]`.
pub async fn index(
    State(state): State<AppState>,
    Query(filter): Query<ListQuery>,
    Query(page): Query<PageParams>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> ApiResult {
    let excluded = match filter
        .exclude_principal
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        Some(oid) => {
            let id = principal_id(oid)?;
            let found: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM principals WHERE id = $1)")
                    .bind(id)
                    .fetch_one(&state.db)
                    .await?;
            if !found {
                return Err(ApiError::not_found());
            }
            Some(id)
        }
        None => None,
    };
    let page = page.resolve();
    let pattern = filter.q.as_deref().and_then(like_pattern);
    let labels = LabelFilter::from_pairs(pairs).as_json();
    let filters = |query: &mut QueryBuilder<Postgres>| {
        query.push(" WHERE TRUE");
        if let Some(id) = excluded {
            query.push(" AND id NOT IN (SELECT role_id FROM principal_roles WHERE principal_id = ");
            query.push_bind(id).push(")");
        }
        if let Some(pattern) = &pattern {
            query.push(" AND (name ILIKE ").push_bind(pattern.clone());
            query
                .push(" OR foreign_id ILIKE ")
                .push_bind(pattern.clone());
            query.push(")");
        }
        if let Some(labels) = &labels {
            query.push(" AND labels @> ").push_bind(labels.clone());
        }
    };
    let mut count = QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM roles");
    filters(&mut count);
    let total: i64 = count.build_query_scalar().fetch_one(&state.db).await?;
    let mut rows = QueryBuilder::<Postgres>::new(format!("SELECT {ROLE_COLUMNS} FROM roles"));
    filters(&mut rows);
    rows.push(" ORDER BY created_at ASC, id ASC LIMIT ")
        .push_bind(page.limit());
    rows.push(" OFFSET ").push_bind(page.offset());
    let rows: Vec<RoleRow> = rows.build_query_as().fetch_all(&state.db).await?;
    let data = payload::roles_json(&state.db, rows).await?;
    Ok(axum::Json(serde_json::json!({"data": data, "meta": page.meta(total)})).into_response())
}

async fn render(state: &AppState, row: Option<RoleRow>) -> ApiResult {
    let row = row.ok_or_else(ApiError::not_found)?;
    let mut rendered = payload::roles_json(&state.db, vec![row]).await?;
    Ok(data(rendered.remove(0)).into_response())
}

pub async fn show(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = role_id(&id)?;
    let row = sqlx::query_as(&format!("SELECT {ROLE_COLUMNS} FROM roles WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    render(&state, row).await
}

pub async fn lookup(State(state): State<AppState>, Path(foreign_id): Path<String>) -> ApiResult {
    let row = sqlx::query_as(&format!(
        "SELECT {ROLE_COLUMNS} FROM roles WHERE foreign_id = $1"
    ))
    .bind(foreign_id)
    .fetch_optional(&state.db)
    .await?;
    render(&state, row).await
}

#[derive(Clone, Copy)]
enum Target<'a> {
    New(Option<&'a str>),
    Id(i64),
    ForeignId(&'a str),
}

#[derive(sqlx::FromRow)]
struct Current {
    id: i64,
    name: Option<String>,
    labels: Value,
}

async fn load(tx: &mut PgConnection, target: Target<'_>) -> Result<Option<Current>, sqlx::Error> {
    let select = "SELECT id, name, labels FROM roles";
    match target {
        Target::Id(id) => {
            sqlx::query_as(&format!("{select} WHERE id = $1 FOR UPDATE"))
                .bind(id)
                .fetch_optional(tx)
                .await
        }
        Target::ForeignId(foreign_id) => {
            sqlx::query_as(&format!("{select} WHERE foreign_id = $1 FOR UPDATE"))
                .bind(foreign_id)
                .fetch_optional(tx)
                .await
        }
        Target::New(_) => Ok(None),
    }
}

/// Applies `input` to a new role or the one `target` names; returns its id
/// and whether it was created.
async fn write(
    state: &AppState,
    actor: i64,
    target: Target<'_>,
    input: &RoleInput,
) -> ApiResult<(i64, bool)> {
    let now = state.now();
    let mut tx = state.db.begin().await?;
    let mut attempts = 0;
    let (id, created) = loop {
        attempts += 1;
        let existing = load(&mut tx, target).await?;
        if let (Target::Id(_), None) = (target, &existing) {
            return Err(ApiError::not_found());
        }
        if let Some(current) = existing {
            let name = input.name.clone().unwrap_or(current.name.clone());
            let labels = input
                .labels
                .clone()
                .map_or(current.labels.clone(), Value::Object);
            let mut update = QueryBuilder::<Postgres>::new("UPDATE roles SET updated_at = ");
            update.push_bind(now.naive_utc());
            let (name_changed, labels_changed) = (name != current.name, labels != current.labels);
            if name_changed {
                update.push(", name = ").push_bind(name);
            }
            if labels_changed {
                update.push(", labels = ").push_bind(labels);
            }
            if name_changed || labels_changed {
                update.push(" WHERE id = ").push_bind(current.id);
                update.build().execute(&mut *tx).await?;
            }
            break (current.id, false);
        }
        let foreign_id = match target {
            Target::New(foreign_id) => foreign_id,
            Target::ForeignId(foreign_id) => Some(foreign_id),
            Target::Id(_) => unreachable!("an oid target resolves or is a 404"),
        };
        let mut errors = crate::FieldErrors::new();
        check_foreign_id(&mut errors, foreign_id, "role");
        errors.finish()?;
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO roles (foreign_id, name, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT (foreign_id) DO NOTHING RETURNING id",
        )
        .bind(foreign_id)
        .bind(input.name.clone().flatten())
        .bind(Value::Object(input.labels.clone().unwrap_or_default()))
        .bind(actor)
        .bind(now.naive_utc())
        .fetch_optional(&mut *tx)
        .await?;
        match inserted {
            Some(id) => break (id, true),
            None if matches!(target, Target::ForeignId(_)) && attempts < 2 => {}
            None => return Err(ApiError::field("foreign_id", "has already been taken")),
        }
    };
    if let Some(rows) = &input.slack_channel_permissions {
        permissions::replace_in(&mut tx, Grantee::Role(id), rows, now).await?;
    }
    tx.commit().await?;
    Ok((id, created))
}

async fn respond(state: &AppState, id: i64, created: bool) -> ApiResult {
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, data(payload::role_json(&state.db, id).await?)).into_response())
}

/// `POST /roles`.
pub async fn create(
    State(state): State<AppState>,
    Actor(actor): Actor,
    Data(input): Data<RoleInput>,
) -> ApiResult {
    let (id, created) = write(
        &state,
        actor,
        Target::New(input.foreign_id.as_deref()),
        &input,
    )
    .await?;
    respond(&state, id, created).await
}

/// `PUT|PATCH /roles/{id}`: an oid (`role_...`, must exist) or a
/// `foreign_id`, created when unknown.
pub async fn upsert(
    State(state): State<AppState>,
    Actor(actor): Actor,
    Path(id): Path<String>,
    Data(input): Data<RoleInput>,
) -> ApiResult {
    let target = if id.starts_with("role_") {
        Target::Id(role_id(&id)?)
    } else {
        Target::ForeignId(&id)
    };
    let (id, created) = write(&state, actor, target, &input).await?;
    respond(&state, id, created).await
}

/// `DELETE /roles/{id}`: its grants, assignments, and permissions go with
/// it; the principals that held it get a fresh config.
pub async fn destroy(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = role_id(&id)?;
    let mut tx = state.db.begin().await?;
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM roles WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    if found.is_none() {
        return Err(ApiError::not_found());
    }
    sqlx::query("DELETE FROM grants WHERE role_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let members: Vec<i64> =
        sqlx::query_scalar("DELETE FROM principal_roles WHERE role_id = $1 RETURNING principal_id")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM slack_channel_permissions WHERE role_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM roles WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    bump_sync_config(&mut tx, &members).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
