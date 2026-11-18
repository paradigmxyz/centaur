//! API clients: machine callers of the api-rs session API (admin only,
//! mounted while `CENTAUR_CONSOLE_API_CLIENTS_ENABLED` is on).
//!
//! Every write is a targeted statement on the columns it changes, so a
//! rename can never undo a concurrent rotation or disablement. Read-modify-
//! write sequences on a client's principal links lock the client row.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_auth::{bearer::hash_token, random};
use centaur_console_db::{decode_oid, oid};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::ConsoleCaller,
    extract::{Data, Path, data},
};

const SECRET_PREFIX: &str = "acs_";
const SCOPES: [&str; 2] = ["sessions:read", "sessions:write"];

#[derive(sqlx::FromRow)]
struct ClientRow {
    id: i64,
    name: String,
    scopes: Value,
    disabled_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const CLIENT_COLUMNS: &str = "id, name, scopes, disabled_at AT TIME ZONE 'UTC' AS disabled_at, \
     created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at";

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: i64,
    api_client_id: i64,
    default: bool,
    principal_oid: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    kind: String,
}

const LINK_SELECT: &str = "SELECT l.id, l.api_client_id, l.\"default\" AS \"default\", \
     p.id AS principal_oid, p.foreign_id, p.name, p.kind \
     FROM api_client_principals l JOIN principals p ON p.id = l.principal_id";

fn link_payload(link: &LinkRow) -> Value {
    json!({
        "id": oid("aclp", link.id),
        "default": link.default,
        "principal": {
            "id": oid("prn", link.principal_oid),
            "foreign_id": link.foreign_id,
            "name": link.name,
            "kind": link.kind,
        },
    })
}

fn client_payload(client: &ClientRow, links: &[&LinkRow]) -> Value {
    json!({
        "id": oid("acl", client.id),
        "name": client.name,
        "scopes": client.scopes,
        "disabled": client.disabled_at.is_some(),
        "disabled_at": client.disabled_at,
        "principals_count": links.len(),
        "principals": links.iter().map(|link| link_payload(link)).collect::<Vec<_>>(),
        "created_at": client.created_at,
        "updated_at": client.updated_at,
    })
}

fn client_id(id: &str) -> ApiResult<i64> {
    decode_oid("acl", id).ok_or_else(ApiError::not_found)
}

async fn load(state: &AppState, id: i64) -> ApiResult<(ClientRow, Vec<LinkRow>)> {
    let client: ClientRow = sqlx::query_as(&format!(
        "SELECT {CLIENT_COLUMNS} FROM api_clients WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let links = sqlx::query_as(&format!(
        "{LINK_SELECT} WHERE l.api_client_id = $1 ORDER BY l.id"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok((client, links))
}

async fn render(state: &AppState, id: i64) -> ApiResult {
    let (client, links) = load(state, id).await?;
    Ok(data(client_payload(&client, &links.iter().collect::<Vec<_>>())).into_response())
}

/// The response that carries a newly issued secret: shown once, never cached.
async fn render_with_secret(
    state: &AppState,
    id: i64,
    secret: &str,
    status: StatusCode,
) -> ApiResult {
    let (client, links) = load(state, id).await?;
    let mut payload = client_payload(&client, &links.iter().collect::<Vec<_>>());
    payload["secret"] = json!(secret);
    Ok((
        status,
        [(http::header::CACHE_CONTROL, "no-store")],
        data(payload),
    )
        .into_response())
}

fn issue_secret() -> String {
    format!("{SECRET_PREFIX}{}", random::hex32())
}

fn validate_name(errors: &mut FieldErrors, name: &str) {
    if name.trim().is_empty() {
        errors.add("name", "can't be blank");
    }
}

fn validate_scopes(errors: &mut FieldErrors, scopes: &[String]) {
    if scopes.is_empty() {
        errors.add("scopes", "must include at least one scope");
    }
    let unsupported: Vec<&str> = scopes
        .iter()
        .map(String::as_str)
        .filter(|scope| !SCOPES.contains(scope))
        .collect();
    if !unsupported.is_empty() {
        errors.add(
            "scopes",
            format!("contains unsupported values: {}", unsupported.join(", ")),
        );
    }
}

fn clean_scopes(scopes: Vec<String>) -> Vec<String> {
    scopes
        .into_iter()
        .filter(|scope| !scope.trim().is_empty())
        .collect()
}

async fn find_principal(
    conn: &mut PgConnection,
    reference: &str,
) -> Result<Option<i64>, sqlx::Error> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    if let Some(id) = decode_oid("prn", reference) {
        let found: Option<i64> = sqlx::query_scalar("SELECT id FROM principals WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
        if found.is_some() {
            return Ok(found);
        }
    }
    sqlx::query_scalar("SELECT id FROM principals WHERE foreign_id = $1 LIMIT 1")
        .bind(reference)
        .fetch_optional(conn)
        .await
}

pub async fn index(State(state): State<AppState>, _: ConsoleCaller) -> ApiResult {
    let clients: Vec<ClientRow> = sqlx::query_as(&format!(
        "SELECT {CLIENT_COLUMNS} FROM api_clients ORDER BY id"
    ))
    .fetch_all(&state.db)
    .await?;
    let links: Vec<LinkRow> = sqlx::query_as(&format!("{LINK_SELECT} ORDER BY l.id"))
        .fetch_all(&state.db)
        .await?;
    let rows: Vec<Value> = clients
        .iter()
        .map(|client| {
            let own: Vec<&LinkRow> = links
                .iter()
                .filter(|link| link.api_client_id == client.id)
                .collect();
            client_payload(client, &own)
        })
        .collect();
    Ok(axum::Json(json!({"data": rows, "meta": {"total": rows.len()}})).into_response())
}

pub async fn show(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    render(&state, client_id(&id)?).await
}

#[derive(Deserialize)]
pub struct NewClient {
    #[serde(default)]
    name: String,
    #[serde(default)]
    scopes: Vec<String>,
    /// A principal to attach (by oid or foreign id).
    principal: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Data(input): Data<NewClient>,
) -> ApiResult {
    let name = input.name.trim().to_owned();
    let scopes = clean_scopes(input.scopes);
    let mut errors = FieldErrors::new();
    validate_name(&mut errors, &name);
    validate_scopes(&mut errors, &scopes);
    errors.finish()?;
    let reference = input
        .principal
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();

    let mut tx = state.db.begin().await?;
    let principal = find_principal(&mut tx, reference).await?;
    if !reference.is_empty() && principal.is_none() {
        return Err(ApiError::field(
            "base",
            "No principal matches that OID or foreign ID.",
        ));
    }
    let secret = issue_secret();
    let now = state.now().naive_utc();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO api_clients (name, secret_digest, scopes, created_by_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
    )
    .bind(&name)
    .bind(hash_token(&secret))
    .bind(json!(scopes))
    .bind(user.id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(principal_id) = principal {
        sqlx::query(
            "INSERT INTO api_client_principals (api_client_id, principal_id, \"default\", created_at, updated_at) \
             VALUES ($1, $2, TRUE, $3, $3)",
        )
        .bind(id)
        .bind(principal_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    render_with_secret(&state, id, &secret, StatusCode::CREATED).await
}

#[derive(Default, Deserialize)]
pub struct ClientEdit {
    name: Option<String>,
    scopes: Option<Vec<String>>,
}

/// Renames and rescopes. Only the columns the request names are written.
pub async fn update(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
    Data(input): Data<ClientEdit>,
) -> ApiResult {
    let id = client_id(&id)?;
    let name = input.name.map(|name| name.trim().to_owned());
    let scopes = input.scopes.map(clean_scopes);
    let mut errors = FieldErrors::new();
    if let Some(name) = &name {
        validate_name(&mut errors, name);
    }
    if let Some(scopes) = &scopes {
        validate_scopes(&mut errors, scopes);
    }
    errors.finish()?;
    if name.is_some() || scopes.is_some() {
        sqlx::query(
            "UPDATE api_clients SET name = COALESCE($2, name), scopes = COALESCE($3, scopes), \
             updated_at = $4 WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(scopes.map(|scopes| json!(scopes)))
        .bind(state.now().naive_utc())
        .execute(&state.db)
        .await?;
    }
    render(&state, id).await
}

pub async fn destroy(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    // The links go with the client (`ON DELETE CASCADE`).
    let deleted = sqlx::query("DELETE FROM api_clients WHERE id = $1")
        .bind(client_id(&id)?)
        .execute(&state.db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Issues a new secret; the old one stops working.
pub async fn rotate_secret(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let id = client_id(&id)?;
    let secret = issue_secret();
    let rotated =
        sqlx::query("UPDATE api_clients SET secret_digest = $2, updated_at = $3 WHERE id = $1")
            .bind(id)
            .bind(hash_token(&secret))
            .bind(state.now().naive_utc())
            .execute(&state.db)
            .await?;
    if rotated.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    render_with_secret(&state, id, &secret, StatusCode::OK).await
}

pub async fn disable(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let id = client_id(&id)?;
    sqlx::query(
        "UPDATE api_clients SET disabled_at = $2, updated_at = $2 \
         WHERE id = $1 AND disabled_at IS NULL",
    )
    .bind(id)
    .bind(state.now().naive_utc())
    .execute(&state.db)
    .await?;
    render(&state, id).await
}

pub async fn enable(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let id = client_id(&id)?;
    sqlx::query(
        "UPDATE api_clients SET disabled_at = NULL, updated_at = $2 \
         WHERE id = $1 AND disabled_at IS NOT NULL",
    )
    .bind(id)
    .bind(state.now().naive_utc())
    .execute(&state.db)
    .await?;
    render(&state, id).await
}

#[derive(Deserialize)]
pub struct Attach {
    #[serde(default)]
    principal: String,
}

async fn link(
    conn: &mut PgConnection,
    client: i64,
    principal: i64,
) -> Result<LinkRow, sqlx::Error> {
    sqlx::query_as(&format!(
        "{LINK_SELECT} WHERE l.api_client_id = $1 AND l.principal_id = $2"
    ))
    .bind(client)
    .bind(principal)
    .fetch_one(conn)
    .await
}

/// Locks the client row (serializing changes to its links); 404 when gone.
async fn lock_client(conn: &mut PgConnection, id: i64) -> ApiResult<()> {
    sqlx::query_scalar::<_, i64>("SELECT id FROM api_clients WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .map(drop)
        .ok_or_else(ApiError::not_found)
}

/// Attaches a principal (by oid or foreign id). The first link becomes the
/// default. Attaching again is a no-op that answers with the link.
pub async fn attach_principal(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path(id): Path<String>,
    Data(input): Data<Attach>,
) -> ApiResult {
    let id = client_id(&id)?;
    let mut tx = state.db.begin().await?;
    lock_client(&mut tx, id).await?;
    let Some(principal) = find_principal(&mut tx, &input.principal).await? else {
        return Err(ApiError::field(
            "base",
            "No principal matches that OID or foreign ID.",
        ));
    };
    let now = state.now().naive_utc();
    sqlx::query(
        "INSERT INTO api_client_principals (api_client_id, principal_id, \"default\", created_at, updated_at) \
         VALUES ($1, $2, NOT EXISTS (SELECT 1 FROM api_client_principals \
                  WHERE api_client_id = $1 AND \"default\"), $3, $3) \
         ON CONFLICT (api_client_id, principal_id) DO NOTHING",
    )
    .bind(id)
    .bind(principal)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // An existing link becomes the default when the client has none.
    sqlx::query(
        "UPDATE api_client_principals SET \"default\" = TRUE, updated_at = $3 \
         WHERE api_client_id = $1 AND principal_id = $2 AND NOT \"default\" \
           AND NOT EXISTS (SELECT 1 FROM api_client_principals \
                           WHERE api_client_id = $1 AND \"default\")",
    )
    .bind(id)
    .bind(principal)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let link = link(&mut tx, id, principal).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, data(link_payload(&link))).into_response())
}

fn link_id(id: &str) -> ApiResult<i64> {
    decode_oid("aclp", id).ok_or_else(ApiError::not_found)
}

/// Removes a link. Serialized on the client row with attach and make-default,
/// so a detach cannot delete the link another request is deciding the default
/// for.
pub async fn detach_principal(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path((id, link_id_param)): Path<(String, String)>,
) -> ApiResult {
    let (id, link_id) = (client_id(&id)?, link_id(&link_id_param)?);
    let mut tx = state.db.begin().await?;
    lock_client(&mut tx, id).await?;
    let deleted =
        sqlx::query("DELETE FROM api_client_principals WHERE id = $1 AND api_client_id = $2")
            .bind(link_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn make_default_principal(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Path((id, link_id_param)): Path<(String, String)>,
) -> ApiResult {
    let (client, link_id) = (client_id(&id)?, link_id(&link_id_param)?);
    let mut tx = state.db.begin().await?;
    lock_client(&mut tx, client).await?;
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM api_client_principals WHERE id = $1 AND api_client_id = $2",
    )
    .bind(link_id)
    .bind(client)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Err(ApiError::not_found());
    }
    sqlx::query(
        "UPDATE api_client_principals SET \"default\" = FALSE \
         WHERE api_client_id = $1 AND \"default\" AND id <> $2",
    )
    .bind(client)
    .bind(link_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE api_client_principals SET \"default\" = TRUE, updated_at = $2 \
         WHERE id = $1 AND NOT \"default\"",
    )
    .bind(link_id)
    .bind(state.now().naive_utc())
    .execute(&mut *tx)
    .await?;
    let row: LinkRow = sqlx::query_as(&format!("{LINK_SELECT} WHERE l.id = $1"))
        .bind(link_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(data(link_payload(&row)).into_response())
}
