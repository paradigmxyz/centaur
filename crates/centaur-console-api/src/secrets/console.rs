//! The console's cross-kind secret list and detail, and the role grants of
//! a secret.

use std::collections::HashMap;

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::sync_config::{bump_sync_config, role_member_ids};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, Row};

use super::{
    kind::{ALL, Kind},
    store::{self, Loaded},
    view,
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::ConsoleCaller,
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};

#[derive(Deserialize, Default)]
pub struct ListQuery {
    kind: Option<String>,
    q: Option<String>,
    /// `principal:<oid>` or `role:<oid>`: leave out what they already hold.
    exclude_grantee: Option<String>,
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// An ILIKE pattern matching `query` anywhere, wildcards escaped.
pub fn like_pattern(query: &str) -> String {
    let mut pattern = String::from("%");
    for character in query.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// The secrets a principal or role holds, per kind.
async fn granted_ids(
    conn: &mut PgConnection,
    grantee: &str,
) -> Result<HashMap<Kind, Vec<i64>>, ApiError> {
    let bad =
        || ApiError::BadRequest("exclude_grantee must be principal:<oid> or role:<oid>".into());
    let (kind, oid) = grantee.split_once(':').ok_or_else(bad)?;
    let (table, column, prefix) = match kind {
        "principal" => ("principals", "principal_id", "prn"),
        "role" => ("roles", "role_id", "role"),
        _ => return Err(bad()),
    };
    let id = centaur_console_db::decode_oid(prefix, oid).ok_or_else(ApiError::not_found)?;
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let columns: Vec<&str> = ALL.iter().map(|kind| kind.owner_column()).collect();
    let rows = sqlx::query(&format!(
        "SELECT {} FROM grants WHERE {column} = $1",
        columns.join(", ")
    ))
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: HashMap<Kind, Vec<i64>> = HashMap::new();
    for row in &rows {
        for kind in ALL {
            if let Some(id) = row.try_get::<Option<i64>, _>(kind.owner_column())? {
                out.entry(kind).or_default().push(id);
            }
        }
    }
    Ok(out)
}

/// The kinds' rows as one list: `(slug, id)` selects unioned.
fn union_sql(kinds: &[Kind]) -> String {
    let selects: Vec<String> = kinds
        .iter()
        .enumerate()
        .map(|(position, kind)| {
            format!(
                "SELECT '{slug}' AS kind, {position} AS position, id, \
                 lower(coalesce(name, '')) AS sort_name, created_at FROM {table} \
                 WHERE ($1::text IS NULL OR name ILIKE $1 OR foreign_id ILIKE $1) \
                 AND NOT (id = ANY(${exclude}))",
                slug = kind.slug(),
                table = kind.table(),
                exclude = position + 2,
            )
        })
        .collect();
    selects.join(" UNION ALL ")
}

fn summary(loaded: &Loaded, managed_by: Value) -> Map<String, Value> {
    let row = &loaded.row;
    let Value::Object(map) = json!({
        "kind": loaded.kind.slug(),
        "kind_label": loaded.kind.label(),
        "id": loaded.oid(),
        "foreign_id": row.column("foreign_id"),
        "name": row.column("name"),
        "grant_key": loaded.kind.owner_column(),
        "description": row.column("description"),
        "labels": row.column("labels"),
        "source_types": view::source_types(loaded),
        "managed_by": managed_by,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    }) else {
        unreachable!("a JSON object");
    };
    map
}

/// `GET /secrets`: every kind's secrets by name.
pub async fn index(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Query(filter): Query<ListQuery>,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let page = page.resolve();
    let kinds: Vec<Kind> = match nonblank(filter.kind.as_deref()) {
        None => ALL.to_vec(),
        Some(slug) => vec![Kind::from_slug(slug).ok_or_else(|| {
            let slugs: Vec<&str> = ALL.iter().map(|kind| kind.slug()).collect();
            ApiError::BadRequest(format!("kind must be one of {}", slugs.join(", ")))
        })?],
    };
    let pattern = nonblank(filter.q.as_deref()).map(like_pattern);
    let mut conn = state.db.acquire().await?;
    let excluded = match nonblank(filter.exclude_grantee.as_deref()) {
        Some(grantee) => granted_ids(&mut conn, grantee).await?,
        None => HashMap::new(),
    };
    let union = union_sql(&kinds);

    let count_sql = format!("SELECT COUNT(*) FROM ({union}) secrets");
    let mut count = sqlx::query_scalar(&count_sql).bind(&pattern);
    for kind in &kinds {
        count = count.bind(excluded.get(kind).cloned().unwrap_or_default());
    }
    let total: i64 = count.fetch_one(&mut *conn).await?;

    let limit_at = kinds.len() + 2;
    let page_sql = format!(
        "SELECT kind, id FROM ({union}) secrets \
         ORDER BY sort_name COLLATE \"C\", created_at, id, position LIMIT ${limit_at} OFFSET ${}",
        limit_at + 1
    );
    let mut ids = sqlx::query_as::<_, (String, i64)>(&page_sql).bind(&pattern);
    for kind in &kinds {
        ids = ids.bind(excluded.get(kind).cloned().unwrap_or_default());
    }
    let ids = ids
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&mut *conn)
        .await?;

    let mut loaded: HashMap<(Kind, i64), Loaded> = HashMap::new();
    for kind in &kinds {
        let wanted: Vec<i64> = ids
            .iter()
            .filter(|(slug, _)| slug == kind.slug())
            .map(|(_, id)| *id)
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let rows = store::find_many(&mut conn, *kind, &wanted).await?;
        for secret in store::load_many(&mut conn, *kind, rows).await? {
            loaded.insert((*kind, secret.id()), secret);
        }
    }
    let mut rows = Vec::with_capacity(ids.len());
    for (slug, id) in ids {
        let Some(kind) = Kind::from_slug(&slug) else {
            continue;
        };
        if let Some(secret) = loaded.get(&(kind, id)) {
            let managed_by = view::managed_by(&mut conn, secret).await?;
            rows.push(Value::Object(summary(secret, managed_by)));
        }
    }
    Ok(axum::Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
}

/// `GET /secrets/{kind}/{id}`.
pub async fn show(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult {
    let kind = Kind::from_slug(&kind).ok_or_else(ApiError::not_found)?;
    let mut conn = state.db.acquire().await?;
    let secret = store::load_oid(&mut conn, kind, &id).await?;
    let managed_by = view::managed_by(&mut conn, &secret).await?;
    let mut body = summary(&secret, managed_by);
    body.insert("editable".into(), view::editable(kind).into());
    body.insert("injection".into(), view::injection(&secret).into());
    body.insert("source_segments".into(), view::source_segments(&secret));
    body.insert("configuration".into(), view::configuration(&secret));
    body.insert("signed_headers".into(), view::signed_headers(&secret));
    body.insert("rules".into(), view::rules(&secret));
    Ok(data(Value::Object(body)).into_response())
}

// --- role grants ----------------------------------------------------------

#[derive(sqlx::FromRow)]
struct RoleGrantRow {
    id: i64,
    priority: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    role_id: i64,
    role_foreign_id: Option<String>,
    role_name: Option<String>,
}

const SELECT_ROLE_GRANTS: &str = "SELECT g.id, g.priority, \
    g.created_at AT TIME ZONE 'UTC' AS created_at, g.updated_at AT TIME ZONE 'UTC' AS updated_at, \
    r.id AS role_id, r.foreign_id AS role_foreign_id, r.name AS role_name \
    FROM grants g JOIN roles r ON r.id = g.role_id";

fn role_summary(id: i64, foreign_id: &Option<String>, name: &Option<String>) -> Value {
    json!({"id": centaur_console_db::oid("role", id), "foreign_id": foreign_id, "name": name})
}

fn grant_payload(grant: &RoleGrantRow, secret: &Loaded) -> Value {
    json!({
        "id": centaur_console_db::oid("grant", grant.id),
        "role_id": centaur_console_db::oid("role", grant.role_id),
        "role": role_summary(grant.role_id, &grant.role_foreign_id, &grant.role_name),
        "grantable": {
            "kind": secret.kind.slug(),
            "kind_label": secret.kind.label(),
            "id": secret.oid(),
            "foreign_id": secret.row.column("foreign_id"),
            "name": secret.row.column("name"),
        },
        "priority": grant.priority,
        "created_at": grant.created_at,
        "updated_at": grant.updated_at,
    })
}

async fn secret_of(conn: &mut PgConnection, kind: &str, id: &str) -> Result<Loaded, ApiError> {
    let kind = Kind::from_slug(kind).ok_or_else(ApiError::not_found)?;
    store::load_oid(conn, kind, id).await
}

/// `GET /secrets/{kind}/{id}/role_grants`.
pub async fn role_grants(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult {
    let mut conn = state.db.acquire().await?;
    let secret = secret_of(&mut conn, &kind, &id).await?;
    let grants: Vec<RoleGrantRow> = sqlx::query_as(&format!(
        "{SELECT_ROLE_GRANTS} WHERE g.{} = $1 ORDER BY g.id",
        secret.kind.owner_column()
    ))
    .bind(secret.id())
    .fetch_all(&mut *conn)
    .await?;
    let granted: Vec<i64> = grants.iter().map(|grant| grant.role_id).collect();
    let assignable: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, foreign_id, name FROM roles WHERE NOT (id = ANY($1)) ORDER BY id",
    )
    .bind(&granted)
    .fetch_all(&mut *conn)
    .await?;
    Ok(axum::Json(json!({
        "data": grants.iter().map(|grant| grant_payload(grant, &secret)).collect::<Vec<_>>(),
        "assignable_roles": assignable
            .iter()
            .map(|(id, foreign_id, name)| role_summary(*id, foreign_id, name))
            .collect::<Vec<_>>(),
    }))
    .into_response())
}

#[derive(Deserialize)]
pub struct RoleGrantIn {
    role_id: Option<String>,
}

/// `POST /secrets/{kind}/{id}/role_grants`: idempotent (200 when the role
/// already holds the secret).
pub async fn create_role_grant(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path((kind, id)): Path<(String, String)>,
    Data(input): Data<RoleGrantIn>,
) -> ApiResult {
    let role_oid = input
        .role_id
        .filter(|role| !role.trim().is_empty())
        .ok_or_else(|| ApiError::BadRequest("role_id is required".into()))?;
    let mut conn = state.db.acquire().await?;
    let secret = secret_of(&mut conn, &kind, &id).await?;
    let role_id =
        centaur_console_db::decode_oid("role", &role_oid).ok_or_else(ApiError::not_found)?;
    let role_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM roles WHERE id = $1)")
        .bind(role_id)
        .fetch_one(&mut *conn)
        .await?;
    if !role_exists {
        return Err(ApiError::not_found());
    }
    drop(conn);

    let owner = secret.kind.owner_column();
    let mut tx = state.db.begin().await?;
    let inserted: Option<i64> = sqlx::query_scalar(&format!(
        "INSERT INTO grants (role_id, {owner}, priority, created_by_id, created_at, updated_at) \
         VALUES ($1, $2, 0, $3, $4, $4) ON CONFLICT DO NOTHING RETURNING id"
    ))
    .bind(role_id)
    .bind(secret.id())
    .bind(user.id)
    .bind(state.now().naive_utc())
    .fetch_optional(&mut *tx)
    .await?;
    if inserted.is_some() {
        let members = role_member_ids(&mut tx, role_id).await?;
        bump_sync_config(&mut tx, &members).await?;
    }
    tx.commit().await?;

    let grant: RoleGrantRow = sqlx::query_as(&format!(
        "{SELECT_ROLE_GRANTS} WHERE g.role_id = $1 AND g.{owner} = $2"
    ))
    .bind(role_id)
    .bind(secret.id())
    .fetch_one(&state.db)
    .await?;
    let status = if inserted.is_some() {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, data(grant_payload(&grant, &secret))).into_response())
}

/// `DELETE /secrets/{kind}/{id}/role_grants/{grant}`.
pub async fn delete_role_grant(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Path((kind, id, grant)): Path<(String, String, String)>,
) -> ApiResult {
    let mut conn = state.db.acquire().await?;
    let secret = secret_of(&mut conn, &kind, &id).await?;
    drop(conn);
    let grant_id =
        centaur_console_db::decode_oid("grant", &grant).ok_or_else(ApiError::not_found)?;
    let mut tx = state.db.begin().await?;
    let role_id: Option<i64> = sqlx::query_scalar(&format!(
        "DELETE FROM grants WHERE id = $1 AND {} = $2 AND role_id IS NOT NULL RETURNING role_id",
        secret.kind.owner_column()
    ))
    .bind(grant_id)
    .bind(secret.id())
    .fetch_optional(&mut *tx)
    .await?;
    let role_id = role_id.ok_or_else(ApiError::not_found)?;
    let members = role_member_ids(&mut tx, role_id).await?;
    bump_sync_config(&mut tx, &members).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
