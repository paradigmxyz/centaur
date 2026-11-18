//! The bearer API's machine credentials: the caller's own API keys and the
//! sandbox proxies.

use axum::{
    extract::{RawQuery, State},
    response::IntoResponse,
};
use centaur_console_auth::bearer::hash_token;
use centaur_console_db::{decode_oid, oid};
use chrono::{DateTime, Utc};
use http::StatusCode;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};
use sqlx::PgPool;

use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::BearerCaller,
    extract::{Data, Path, Query, data},
    pagination::{Meta, PageParams},
};

fn list<T: serde::Serialize>(rows: &[T], meta: Meta) -> axum::response::Response {
    axum::Json(json!({ "data": rows, "meta": meta })).into_response()
}

// --- API keys -----------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: i64,
    name: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const KEY_COLUMNS: &str = "id, name, created_at AT TIME ZONE 'UTC' AS created_at, \
    updated_at AT TIME ZONE 'UTC' AS updated_at";

fn key_json(key: &KeyRow, token: Option<&str>) -> Value {
    let mut body = json!({
        "id": oid("ak", key.id),
        "name": key.name,
        "created_at": key.created_at,
        "updated_at": key.updated_at,
    });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    body
}

pub async fn api_keys_index(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let page = page.resolve();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_keys WHERE deleted_at IS NULL AND user_id = $1",
    )
    .bind(auth.user.id)
    .fetch_one(&state.db)
    .await?;
    let keys: Vec<KeyRow> = sqlx::query_as(&format!(
        "SELECT {KEY_COLUMNS} FROM api_keys WHERE deleted_at IS NULL AND user_id = $1 \
         ORDER BY created_at ASC, id ASC LIMIT $2 OFFSET $3"
    ))
    .bind(auth.user.id)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    let rows: Vec<Value> = keys.iter().map(|key| key_json(key, None)).collect();
    Ok(list(&rows, page.meta(total)))
}

async fn find_key(db: &PgPool, user_id: i64, id: &str) -> ApiResult<KeyRow> {
    let id = decode_oid("ak", id).ok_or_else(ApiError::not_found)?;
    sqlx::query_as(&format!(
        "SELECT {KEY_COLUMNS} FROM api_keys WHERE deleted_at IS NULL AND user_id = $1 AND id = $2"
    ))
    .bind(user_id)
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(ApiError::not_found)
}

pub async fn api_keys_show(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let key = find_key(&state.db, auth.user.id, &id).await?;
    Ok(data(key_json(&key, None)).into_response())
}

#[derive(Deserialize)]
pub struct NewKey {
    name: Option<String>,
}

/// A new random secret with `prefix`; its plaintext is only ever returned
/// to the creator.
fn new_token(prefix: &str) -> String {
    format!("{prefix}{}", centaur_console_auth::random::hex32())
}

pub async fn api_keys_create(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Data(input): Data<NewKey>,
) -> ApiResult {
    let name = input.name.as_deref().unwrap_or_default().trim();
    if name.is_empty() {
        return Err(ApiError::field("name", "can't be blank"));
    }
    let token = new_token("iak_");
    let now = state.now().naive_utc();
    let key: KeyRow = sqlx::query_as(&format!(
        "INSERT INTO api_keys (name, token_hash, user_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4) RETURNING {KEY_COLUMNS}"
    ))
    .bind(name)
    .bind(hash_token(&token))
    .bind(auth.user.id)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, data(key_json(&key, Some(&token)))).into_response())
}

pub async fn api_keys_destroy(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let key = find_key(&state.db, auth.user.id, &id).await?;
    if key.id == auth.api_key_id {
        return Err(ApiError::Unprocessable {
            message: "cannot revoke the API key used for this request".into(),
            reason: None,
        });
    }
    sqlx::query("UPDATE api_keys SET deleted_at = $2 WHERE id = $1 AND deleted_at IS NULL")
        .bind(key.id)
        .bind(state.now().naive_utc())
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- proxies ------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ProxyRow {
    id: i64,
    name: String,
    principal_id: Option<i64>,
    requester_principal_id: Option<i64>,
    labels: Value,
    principal_assigned_at: Option<DateTime<Utc>>,
    requester_principal_assigned_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const PROXY_COLUMNS: &str = "id, name, principal_id, requester_principal_id, labels, \
    principal_assigned_at AT TIME ZONE 'UTC' AS principal_assigned_at, \
    requester_principal_assigned_at AT TIME ZONE 'UTC' AS requester_principal_assigned_at, \
    created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at";

fn proxy_json(proxy: &ProxyRow, token: Option<&str>) -> Value {
    let mut body = json!({
        "id": oid("prx", proxy.id),
        "name": proxy.name,
        "principal_id": proxy.principal_id.map(|id| oid("prn", id)),
        "requester_principal_id": proxy.requester_principal_id.map(|id| oid("prn", id)),
        "status": if proxy.principal_id.is_some() { "assigned" } else { "unassigned" },
        "labels": proxy.labels,
        "principal_assigned_at": proxy.principal_assigned_at,
        "requester_principal_assigned_at": proxy.requester_principal_assigned_at,
        "created_at": proxy.created_at,
        "updated_at": proxy.updated_at,
    });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    body
}

/// The id of the principal `oid` names; unknown is a 404.
async fn principal(db: &PgPool, oid: &str) -> ApiResult<i64> {
    let id = decode_oid("prn", oid.trim()).ok_or_else(ApiError::not_found)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM principals WHERE id = $1)")
        .bind(id)
        .fetch_one(db)
        .await?;
    if exists {
        Ok(id)
    } else {
        Err(ApiError::not_found())
    }
}

/// A blank oid means none.
async fn optional_principal(db: &PgPool, oid: Option<&str>) -> ApiResult<Option<i64>> {
    match oid.map(str::trim).filter(|oid| !oid.is_empty()) {
        Some(oid) => principal(db, oid).await.map(Some),
        None => Ok(None),
    }
}

/// `principal_id`, `labels[key]=value` filters from the query string.
fn proxy_filter(raw: Option<&str>) -> (Option<String>, Map<String, Value>) {
    let mut principal = None;
    let mut labels = Map::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if key == "principal_id" {
            principal = Some(value.into_owned()).filter(|id| !id.trim().is_empty());
        } else if let Some(label) = key
            .strip_prefix("labels[")
            .and_then(|k| k.strip_suffix(']'))
        {
            labels.insert(label.to_owned(), Value::String(value.into_owned()));
        }
    }
    (principal, labels)
}

pub async fn proxies_index(
    State(state): State<AppState>,
    BearerCaller(_auth): BearerCaller,
    RawQuery(raw): RawQuery,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let (principal_oid, labels) = proxy_filter(raw.as_deref());
    let principal_id = optional_principal(&state.db, principal_oid.as_deref()).await?;
    let labels = (!labels.is_empty()).then_some(Value::Object(labels));
    let page = page.resolve();
    const FILTER: &str =
        "($1::bigint IS NULL OR principal_id = $1) AND ($2::jsonb IS NULL OR labels @> $2)";
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM proxies WHERE {FILTER}"))
        .bind(principal_id)
        .bind(&labels)
        .fetch_one(&state.db)
        .await?;
    let proxies: Vec<ProxyRow> = sqlx::query_as(&format!(
        "SELECT {PROXY_COLUMNS} FROM proxies WHERE {FILTER} \
         ORDER BY created_at ASC, id ASC LIMIT $3 OFFSET $4"
    ))
    .bind(principal_id)
    .bind(&labels)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    let rows: Vec<Value> = proxies.iter().map(|p| proxy_json(p, None)).collect();
    Ok(list(&rows, page.meta(total)))
}

async fn find_proxy(db: &PgPool, id: &str) -> ApiResult<ProxyRow> {
    let id = decode_oid("prx", id).ok_or_else(ApiError::not_found)?;
    sqlx::query_as(&format!(
        "SELECT {PROXY_COLUMNS} FROM proxies WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(ApiError::not_found)
}

pub async fn proxies_show(
    State(state): State<AppState>,
    BearerCaller(_auth): BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let proxy = find_proxy(&state.db, &id).await?;
    Ok(data(proxy_json(&proxy, None)).into_response())
}

#[derive(Deserialize)]
pub struct NewProxy {
    name: Option<String>,
    principal_id: Option<String>,
    requester_principal_id: Option<String>,
    labels: Option<Map<String, Value>>,
}

pub async fn proxies_create(
    State(state): State<AppState>,
    BearerCaller(_auth): BearerCaller,
    Data(input): Data<NewProxy>,
) -> ApiResult {
    let name = input.name.as_deref().unwrap_or_default().trim();
    let labels = input.labels.unwrap_or_default();
    let mut errors = FieldErrors::new();
    if name.is_empty() {
        errors.add("name", "can't be blank");
    }
    if labels.values().any(|value| !value.is_string()) {
        errors.add("labels", "values must be strings");
    }
    let principal_id = optional_principal(&state.db, input.principal_id.as_deref()).await?;
    let requester_id =
        optional_principal(&state.db, input.requester_principal_id.as_deref()).await?;
    errors.finish()?;
    let token = new_token("iprx_");
    let now = state.now().naive_utc();
    let proxy: ProxyRow = sqlx::query_as(&format!(
        "INSERT INTO proxies (name, bearer_token_hash, labels, principal_id, requester_principal_id, \
            principal_assigned_at, requester_principal_assigned_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $4::bigint IS NOT NULL THEN $6 END, \
                 CASE WHEN $5::bigint IS NOT NULL THEN $6 END, $6, $6) RETURNING {PROXY_COLUMNS}"
    ))
    .bind(name)
    .bind(hash_token(&token))
    .bind(Value::Object(labels))
    .bind(principal_id)
    .bind(requester_id)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, data(proxy_json(&proxy, Some(&token)))).into_response())
}

/// `Some(None)` is an explicit `null` (clear it); `None` is absent (keep).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
pub struct ProxyPatch {
    name: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    principal_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    requester_principal_id: Option<Option<String>>,
    labels: Option<Map<String, Value>>,
}

pub async fn proxies_update(
    State(state): State<AppState>,
    BearerCaller(_auth): BearerCaller,
    Path(id): Path<String>,
    Data(patch): Data<ProxyPatch>,
) -> ApiResult {
    let proxy = find_proxy(&state.db, &id).await?;
    let mut errors = FieldErrors::new();
    let name = patch.name.as_deref().map(str::trim);
    if name == Some("") {
        errors.add("name", "can't be blank");
    }
    if let Some(labels) = &patch.labels
        && labels.values().any(|value| !value.is_string())
    {
        errors.add("labels", "values must be strings");
    }
    let principal = match &patch.principal_id {
        Some(oid) => Some(optional_principal(&state.db, oid.as_deref()).await?),
        None => None,
    };
    let requester = match &patch.requester_principal_id {
        Some(oid) => Some(optional_principal(&state.db, oid.as_deref()).await?),
        None => None,
    };
    errors.finish()?;
    // Only the submitted columns change; the assignment time follows a
    // changed assignment.
    sqlx::query(
        "UPDATE proxies SET name = COALESCE($2, name), labels = COALESCE($3, labels), \
            principal_id = CASE WHEN $4 THEN $5 ELSE principal_id END, \
            principal_assigned_at = CASE WHEN $4 AND principal_id IS DISTINCT FROM $5 \
                THEN CASE WHEN $5::bigint IS NULL THEN NULL ELSE $8 END ELSE principal_assigned_at END, \
            requester_principal_id = CASE WHEN $6 THEN $7 ELSE requester_principal_id END, \
            requester_principal_assigned_at = CASE WHEN $6 AND requester_principal_id IS DISTINCT FROM $7 \
                THEN CASE WHEN $7::bigint IS NULL THEN NULL ELSE $8 END ELSE requester_principal_assigned_at END, \
            updated_at = $8 WHERE id = $1",
    )
    .bind(proxy.id)
    .bind(name)
    .bind(patch.labels.map(Value::Object))
    .bind(principal.is_some())
    .bind(principal.flatten())
    .bind(requester.is_some())
    .bind(requester.flatten())
    .bind(state.now().naive_utc())
    .execute(&state.db)
    .await?;
    let proxy = find_proxy(&state.db, &id).await?;
    Ok(data(proxy_json(&proxy, None)).into_response())
}

pub async fn proxies_destroy(
    State(state): State<AppState>,
    BearerCaller(_auth): BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let proxy = find_proxy(&state.db, &id).await?;
    sqlx::query("DELETE FROM proxies WHERE id = $1")
        .bind(proxy.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
