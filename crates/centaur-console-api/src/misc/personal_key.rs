//! The caller's personal SSH key (`/api/v1/console/me/ssh_key`): an
//! `ssh_key_secrets` row owned by the user, whose private key is one inline
//! (`control_plane`) secret source. The private key is write-only: no
//! response, error, or log contains it.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use centaur_console_db::{
    keys, oid, personal_ssh_keys,
    redact::{self, Redacted},
    sync_config::{bump_sync_config, grantee_ids},
};
use chrono::{DateTime, Utc};
use http::{HeaderValue, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use ssh_key::{HashAlg, PublicKey};

use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::ConsoleCaller,
    extract::{Data, data},
};

pub const DEFAULT_NAME: &str = "My SSH key";
const MAX_NAME_CHARS: usize = 100;

fn no_store(response: ApiResult) -> Response {
    let mut response = response.unwrap_or_else(IntoResponse::into_response);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(sqlx::FromRow)]
struct Key {
    id: i64,
    name: String,
    public_key: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const KEY_SELECT: &str = "SELECT id, name, public_key, \
    created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at \
    FROM ssh_key_secrets WHERE owner_id = $1";

#[derive(sqlx::FromRow)]
struct GrantRow {
    id: i64,
    principal_id: Option<i64>,
    principal_name: Option<String>,
    principal_foreign_id: Option<String>,
    principal_kind: Option<String>,
    role_id: Option<i64>,
    role_name: Option<String>,
    role_foreign_id: Option<String>,
}

fn label(name: Option<String>, foreign_id: Option<String>, oid: &str) -> String {
    let present = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    present(name)
        .or_else(|| present(foreign_id))
        .unwrap_or_else(|| oid.to_owned())
}

fn fingerprint(public_key: &str) -> Option<String> {
    PublicKey::from_openssh(public_key)
        .ok()
        .map(|key| key.fingerprint(HashAlg::Sha256).to_string())
}

async fn payload(state: &AppState, key: Option<Key>) -> ApiResult<Value> {
    let Some(key) = key else {
        return Ok(json!({
            "id": null, "saved": false, "name": DEFAULT_NAME, "has_private_key": false,
            "fingerprint": null, "public_key": null, "created_at": null, "updated_at": null,
            "grants": [],
        }));
    };
    let has_private_key: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM secret_sources WHERE ssh_key_secret_id = $1)",
    )
    .bind(key.id)
    .fetch_one(&state.db)
    .await?;
    let grants: Vec<GrantRow> = sqlx::query_as(
        "SELECT g.id, p.id AS principal_id, p.name AS principal_name, \
                p.foreign_id AS principal_foreign_id, p.kind AS principal_kind, \
                r.id AS role_id, r.name AS role_name, r.foreign_id AS role_foreign_id \
         FROM grants g LEFT JOIN principals p ON p.id = g.principal_id \
         LEFT JOIN roles r ON r.id = g.role_id \
         WHERE g.ssh_key_secret_id = $1 ORDER BY g.id",
    )
    .bind(key.id)
    .fetch_all(&state.db)
    .await?;
    let grants: Vec<Value> = grants
        .into_iter()
        .filter_map(|grant| {
            let (grantee_type, grantee_id, label) = if let Some(id) = grant.principal_id {
                let grantee = oid("prn", id);
                let text = label(grant.principal_name, grant.principal_foreign_id, &grantee);
                ("principal", grantee, text)
            } else {
                let id = grant.role_id?;
                let grantee = oid("role", id);
                let text = label(grant.role_name, grant.role_foreign_id, &grantee);
                ("role", grantee, text)
            };
            Some(json!({
                "id": oid("grant", grant.id),
                "grantee_type": grantee_type,
                "grantee_id": grantee_id,
                "label": label,
                "principal_kind": grant.principal_kind,
            }))
        })
        .collect();
    Ok(json!({
        "id": oid("sks", key.id),
        "saved": true,
        "name": key.name,
        "has_private_key": has_private_key,
        "fingerprint": fingerprint(&key.public_key),
        "public_key": key.public_key,
        "created_at": key.created_at,
        "updated_at": key.updated_at,
        "grants": grants,
    }))
}

pub async fn show(State(state): State<AppState>, ConsoleCaller(user): ConsoleCaller) -> Response {
    no_store(
        async {
            let key = sqlx::query_as(KEY_SELECT)
                .bind(user.id)
                .fetch_optional(&state.db)
                .await?;
            Ok(data(payload(&state, key).await?).into_response())
        }
        .await,
    )
}

#[derive(Deserialize, Default)]
pub struct Input {
    name: Option<String>,
    private_key: Option<String>,
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Input")
            .field("name", &self.name)
            .field("private_key", &redact::option(&self.private_key))
            .finish()
    }
}

pub async fn save(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Data(input): Data<Input>,
) -> Response {
    no_store(save_key(&state, user.id, input).await)
}

async fn save_key(state: &AppState, user_id: i64, input: Input) -> ApiResult {
    let existing: Option<Key> = sqlx::query_as(KEY_SELECT)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
    let name = match input.name.as_deref() {
        Some(name) => name.trim().to_owned(),
        None => existing
            .as_ref()
            .map_or_else(|| DEFAULT_NAME.to_owned(), |key| key.name.clone()),
    };
    let mut errors = FieldErrors::new();
    if name.is_empty() {
        errors.add("name", "can't be blank");
    } else if name.chars().count() > MAX_NAME_CHARS {
        errors.add("name", "is too long (maximum is 100 characters)");
    }
    let private_key = input
        .private_key
        .as_deref()
        .filter(|key| !key.trim().is_empty());
    let info = match private_key {
        Some(private_key) => match keys::inspect(private_key) {
            Ok(info) => Some(info),
            Err(error) => {
                errors.add("private_key", error.to_string());
                None
            }
        },
        None if existing.is_none() => {
            errors.add("private_key", "can't be blank");
            None
        }
        None => None,
    };
    errors.finish()?;

    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    let key_id = match &existing {
        Some(key) => {
            sqlx::query(
                "UPDATE ssh_key_secrets SET name = $2, public_key = COALESCE($3, public_key), \
                 updated_at = $4 WHERE id = $1",
            )
            .bind(key.id)
            .bind(&name)
            .bind(info.as_ref().map(|info| info.public_key.as_str()))
            .bind(now)
            .execute(&mut *tx)
            .await?;
            key.id
        }
        None => {
            let public_key = info.as_ref().map(|info| info.public_key.as_str());
            sqlx::query_scalar(
                "INSERT INTO ssh_key_secrets (name, public_key, owner_id, created_by_id, \
                 created_at, updated_at) VALUES ($1, $2, $3, $3, $4, $4) RETURNING id",
            )
            .bind(&name)
            .bind(public_key)
            .bind(user_id)
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::unique(e, "owner_id", "has already been taken"))?
        }
    };
    if let Some(private_key) = private_key {
        let secret = state.encryptor.encrypt(&keys::normalize(private_key))?;
        sqlx::query("DELETE FROM secret_sources WHERE ssh_key_secret_id = $1")
            .bind(key_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO secret_sources (source_type, secret, config, ssh_key_secret_id, \
             created_at, updated_at) VALUES ('control_plane', $1, '{}'::jsonb, $2, $3, $3)",
        )
        .bind(secret)
        .bind(key_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    let affected = grantee_ids(&mut tx, "ssh_key_secret_id", key_id).await?;
    bump_sync_config(&mut tx, &affected).await?;
    tx.commit().await?;
    // Also repairs grants an earlier enrollment missed.
    personal_ssh_keys::apply_for_user(&state.db, user_id, state.now()).await?;
    let key = sqlx::query_as(KEY_SELECT)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(data(payload(state, key).await?).into_response())
}

/// Removes the key, its source, and every grant of it.
pub async fn destroy(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
) -> Response {
    no_store(
        async {
            let mut tx = state.db.begin().await?;
            let key_id: Option<i64> =
                sqlx::query_scalar("SELECT id FROM ssh_key_secrets WHERE owner_id = $1 FOR UPDATE")
                    .bind(user.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(key_id) = key_id {
                // Resolve the grantees while the grants still exist.
                let affected = grantee_ids(&mut tx, "ssh_key_secret_id", key_id).await?;
                delete_key(&mut tx, key_id).await?;
                bump_sync_config(&mut tx, &affected).await?;
            }
            tx.commit().await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        .await,
    )
}

async fn delete_key(tx: &mut PgConnection, key_id: i64) -> ApiResult<()> {
    for sql in [
        "DELETE FROM secret_sources WHERE ssh_key_secret_id = $1",
        "DELETE FROM grants WHERE ssh_key_secret_id = $1",
        "DELETE FROM ssh_key_secrets WHERE id = $1",
    ] {
        sqlx::query(sql).bind(key_id).execute(&mut *tx).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct Candidate {
    private_key: Option<String>,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("private_key", &Redacted)
            .finish()
    }
}

/// Checks a pasted private key without saving it.
pub async fn validate(
    ConsoleCaller(_user): ConsoleCaller,
    Data(candidate): Data<Candidate>,
) -> Response {
    no_store(
        match keys::inspect(candidate.private_key.as_deref().unwrap_or_default()) {
            Ok(info) => Ok(data(json!({
                "valid": true,
                "message": "Valid private key. Ready to save.",
                "fingerprint": info.fingerprint,
            }))
            .into_response()),
            Err(error) => Err(ApiError::Unprocessable {
                message: error.to_string(),
                reason: None,
            }),
        },
    )
}
