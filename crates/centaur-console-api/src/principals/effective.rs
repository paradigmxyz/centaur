//! `effective_config` (what the principal's proxy is served, secrets
//! redacted) and `effective_grants` (every secret the principal reaches,
//! directly or through roles).

use std::collections::HashMap;

use axum::{
    extract::State,
    http::{HeaderValue, header},
    response::IntoResponse,
};
use centaur_console_db::{REDACTED, oid};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{Ref, SECRET_KINDS, principal::principal_id};
use crate::{
    ApiError, ApiResult, AppState,
    env::Env,
    extract::{Path, Query, data},
    pagination::PageParams,
};

/// The hosts that receive the api-server token: `CENTAUR_API_SERVER_PROXY_HOSTS`
/// plus the host of `CENTAUR_API_URL`, lower-cased, deduplicated.
fn api_server_hosts(env: &Env) -> Vec<String> {
    let mut hosts: Vec<String> = env
        .var("CENTAUR_API_SERVER_PROXY_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect();
    hosts.extend(
        env.var("CENTAUR_API_URL")
            .and_then(|url| url::Url::parse(url).ok())
            .and_then(|url| url.host_str().map(str::to_owned)),
    );
    let mut normalized: Vec<String> = Vec::new();
    for host in hosts {
        let host = host.trim().to_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host);
        if !host.is_empty() && !normalized.iter().any(|seen| seen == host) {
            normalized.push(host.to_owned());
        }
    }
    normalized
}

/// The principal's effective config with every secret value redacted:
/// `secrets`, `transforms`, `postgres`, and (when api-server hosts are
/// configured and tokens can be minted) the redacted api-server token.
pub async fn redacted_config(
    db: &PgPool,
    env: &Env,
    jwt_signing_secret: &str,
    principal_id: i64,
) -> Result<Value, ApiError> {
    let config = centaur_console_db::effective_config(db, principal_id)
        .await?
        .unwrap_or_default();
    let mut secrets = config.secrets;
    let hosts = api_server_hosts(env);
    if !jwt_signing_secret.trim().is_empty() && !hosts.is_empty() {
        secrets.push(json!({
            "source": {"type": "control_plane", "value": REDACTED},
            "inject": {"header": "Authorization", "formatter": "Bearer {{ .Value }}"},
            "rules": hosts.iter().map(|host| json!({"host": host})).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({
        "secrets": secrets,
        "transforms": config.transforms,
        "postgres": config.postgres,
    }))
}

async fn render_config(state: &AppState, id: Option<i64>) -> ApiResult {
    let id = id.ok_or_else(ApiError::not_found)?;
    let mut body = redacted_config(
        &state.db,
        &state.config.env,
        &state.config.jwt_signing_secret,
        id,
    )
    .await?;
    body["id"] = oid("prn", id).into();
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        data(body),
    )
        .into_response())
}

/// `GET /principals/{id}/effective_config`.
pub async fn config(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = principal_id(&id)?;
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM principals WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    render_config(&state, found).await
}

/// `GET /principals/lookup/{foreign_id}/effective_config`.
pub async fn lookup_config(
    State(state): State<AppState>,
    Path(foreign_id): Path<String>,
) -> ApiResult {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM principals WHERE foreign_id = $1")
        .bind(foreign_id)
        .fetch_optional(&state.db)
        .await?;
    render_config(&state, found).await
}

/// One `SELECT` per grantable column, over the grants that reach the
/// principal (`$1`): the secret, its priority, and the role it came through
/// (`NULL` for a direct grant).
fn reach_sql() -> String {
    SECRET_KINDS
        .iter()
        .enumerate()
        .map(|(order, kind)| {
            format!(
                "SELECT {order} AS ord, {col} AS sid, priority, role_id FROM grants \
                 WHERE {col} IS NOT NULL AND (principal_id = $1 OR role_id IN \
                 (SELECT role_id FROM principal_roles WHERE principal_id = $1))",
                col = kind.column
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

#[derive(sqlx::FromRow)]
struct Reached {
    ord: i32,
    sid: i64,
    direct: bool,
    role_ids: Vec<i64>,
}

#[derive(sqlx::FromRow)]
struct Named {
    id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
}

/// `GET /principals/{id}/effective_grants`: each reachable secret once,
/// by kind, then by ascending strongest priority, then id.
pub async fn grants(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let id = principal_id(&id)?;
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM principals WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if found.is_none() {
        return Err(ApiError::not_found());
    }
    let page = page.resolve();
    let reach = reach_sql();
    let total: i64 = sqlx::query_scalar(&format!(
        "WITH reach AS ({reach}) SELECT COUNT(*) FROM (SELECT 1 FROM reach GROUP BY ord, sid) g"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let reached: Vec<Reached> = sqlx::query_as(&format!(
        "WITH reach AS ({reach}) \
         SELECT ord, sid, bool_or(role_id IS NULL) AS direct, \
                COALESCE(array_agg(DISTINCT role_id) FILTER (WHERE role_id IS NOT NULL), '{{}}') AS role_ids \
         FROM reach GROUP BY ord, sid ORDER BY ord, MAX(priority), sid LIMIT $2 OFFSET $3"
    ))
    .bind(id)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;

    // Names of the secrets and roles on this page: one query per kind.
    let mut secrets: HashMap<(i32, i64), Named> = HashMap::new();
    for (order, kind) in (0..).zip(&SECRET_KINDS) {
        let ids: Vec<i64> = reached
            .iter()
            .filter(|r| r.ord == order)
            .map(|r| r.sid)
            .collect();
        if ids.is_empty() {
            continue;
        }
        let rows: Vec<Named> = sqlx::query_as(&format!(
            "SELECT id, foreign_id, name FROM {} WHERE id = ANY($1)",
            kind.table
        ))
        .bind(&ids)
        .fetch_all(&state.db)
        .await?;
        secrets.extend(rows.into_iter().map(|row| ((order, row.id), row)));
    }
    let role_ids: Vec<i64> = reached.iter().flat_map(|r| r.role_ids.clone()).collect();
    let roles: HashMap<i64, Named> =
        sqlx::query_as::<_, Named>("SELECT id, foreign_id, name FROM roles WHERE id = ANY($1)")
            .bind(&role_ids)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .map(|row| (row.id, row))
            .collect();

    let entries: Vec<Value> = reached
        .iter()
        .map(|r| {
            let kind = &SECRET_KINDS[r.ord as usize];
            let named = secrets.get(&(r.ord, r.sid));
            json!({
                "id": oid(kind.prefix, r.sid),
                "kind": kind.kind,
                "grant_key": kind.column,
                "foreign_id": named.and_then(|n| n.foreign_id.clone()),
                "name": named.and_then(|n| n.name.clone()),
                "direct": r.direct,
                "roles": r.role_ids.iter().filter_map(|id| roles.get(id)).map(|role| Ref {
                    id: oid("role", role.id),
                    foreign_id: role.foreign_id.clone(),
                    name: role.name.clone(),
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(axum::Json(json!({"data": entries, "meta": page.meta(total)})).into_response())
}
