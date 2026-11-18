//! Creating, replacing, and deleting secrets.
//!
//! A write replaces the secret's document: columns the request leaves out
//! take their defaults (except `foreign_id`, and a static secret's `kind`,
//! which keep their value), and sources and rules are replaced as a set.
//! Everything runs in one transaction that first locks the secret's row, so
//! what a metadata-only edit keeps (the stored inline value) is read after a
//! concurrent rotation has committed, never before. Only columns that
//! differ are written, and a document equal to what is stored writes
//! nothing and moves no proxy configuration.

use axum::http::StatusCode;
use centaur_console_db::sync_config::{bump_sync_config, grantee_ids};
use serde_json::Value;
use sqlx::PgConnection;

use super::{
    draft::{self, Draft},
    kind::Kind,
    parts::{self, Source, sorted},
    store,
};
use crate::{ApiError, ApiResult, AppState, error::is_unique_violation};

/// Who is writing; the sandbox may only reference external stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Bearer,
    Console,
    SandboxAdmin,
}

/// Which secret a write is about.
#[derive(Clone, Copy, Debug)]
pub enum Target<'a> {
    Create,
    /// An existing secret, by oid.
    Oid(&'a str),
    /// An oid, or else a foreign id (created when none matches).
    Upsert(&'a str),
}

pub struct Saved {
    pub id: i64,
    pub created: bool,
}

impl Saved {
    pub fn status(&self) -> StatusCode {
        if self.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        }
    }
}

enum Outcome {
    Saved(Saved),
    /// An upsert lost a race to create the same foreign id.
    LostRace,
}

pub async fn save(
    state: &AppState,
    kind: Kind,
    flavor: Flavor,
    created_by: i64,
    target: Target<'_>,
    body: Value,
) -> ApiResult<Saved> {
    if flavor == Flavor::SandboxAdmin {
        forbid_sandbox_sources(kind, &body)?;
    }
    // A lost race becomes an update on the second attempt.
    for _ in 0..2 {
        match attempt(state, kind, flavor, created_by, target, body.clone()).await? {
            Outcome::Saved(saved) => return Ok(saved),
            Outcome::LostRace => {}
        }
    }
    Err(ApiError::Conflict(
        "the secret was changed concurrently".into(),
    ))
}

/// A sandbox acts for a chat user: it may not create inline values or
/// borrow a broker credential.
fn forbid_sandbox_sources(kind: Kind, body: &Value) -> ApiResult<()> {
    let Some(key) = kind.source_key() else {
        return Ok(());
    };
    match body
        .get(key)
        .and_then(|source| source.get("source_type"))
        .and_then(Value::as_str)
    {
        Some(source_type @ ("control_plane" | "token_broker")) => {
            Err(ApiError::Forbidden(format!(
                "{source_type} sources cannot be set from a sandbox; reference an external store"
            )))
        }
        _ => Ok(()),
    }
}

async fn attempt(
    state: &AppState,
    kind: Kind,
    flavor: Flavor,
    created_by: i64,
    target: Target<'_>,
    mut body: Value,
) -> ApiResult<Outcome> {
    let mut tx = state.db.begin().await?;
    let existing = match target {
        Target::Create => None,
        Target::Oid(oid) => Some(lock_oid(&mut tx, kind, oid).await?),
        Target::Upsert(identifier)
            if identifier.starts_with(&format!("{}_", kind.oid_prefix())) =>
        {
            Some(lock_oid(&mut tx, kind, identifier).await?)
        }
        Target::Upsert(foreign_id) => {
            let found = store::lock_by_foreign_id(&mut tx, kind, foreign_id).await?;
            if let Some(map) = body.as_object_mut() {
                if found.is_some() {
                    // The write is addressed by it: it is not what changes.
                    map.remove("foreign_id");
                } else if map.get("foreign_id").is_none_or(Value::is_null) {
                    map.insert("foreign_id".into(), foreign_id.into());
                }
            }
            found
        }
    };
    if kind == Kind::SshKey
        && existing
            .as_ref()
            .is_some_and(|row| !row.column("owner_id").is_null())
    {
        return Err(ApiError::Forbidden(
            "personal SSH keys are managed by their owner".into(),
        ));
    }

    // What is stored, read under the row lock.
    let existing_id = existing.as_ref().map(|row| row.id);
    let mut current_columns = None;
    let mut current_sources = Vec::new();
    let mut current_rules = Vec::new();
    let mut broker_credential_id = None;
    if let Some(row) = existing {
        let loaded = store::load(&mut tx, kind, row).await?;
        if flavor == Flavor::SandboxAdmin {
            if loaded.sources.iter().any(|source| source.is_token_broker()) {
                return Err(ApiError::Forbidden(
                    "OAuth credential secrets cannot be edited from a sandbox".into(),
                ));
            }
            // A sandbox sends the fields it changes; the rest stays.
            merge_over(&mut body, loaded.document(&state.encryptor)?);
        }
        broker_credential_id = loaded.row.column("broker_credential_id").as_i64();
        current_rules = loaded.rules;
        for source in loaded.sources {
            current_sources.push(source.decrypt(&state.encryptor)?);
        }
        current_columns = loaded.row.doc.as_object().cloned();
    }

    let mut draft = draft::parse(kind, &body, current_columns.as_ref())?;
    keep_inline_value(&mut draft, &current_sources);
    for source in &mut draft.sources {
        source.resolve_broker(&mut tx).await?;
    }
    draft.apply_profile();
    draft.derive_ssh_public_key();
    draft.validate();
    std::mem::take(&mut draft.errors).finish()?;

    let now = state.now();
    let Some(id) = existing_id else {
        let id = match insert_row(&mut tx, &draft, created_by, now).await {
            Ok(id) => id,
            Err(error) if is_unique_violation(&error) && matches!(target, Target::Upsert(_)) => {
                return Ok(Outcome::LostRace);
            }
            Err(error) => {
                return Err(ApiError::unique(
                    error,
                    "foreign_id",
                    "has already been taken",
                ));
            }
        };
        parts::replace_sources(&mut tx, kind, id, &draft.sources, &state.encryptor, now).await?;
        parts::replace_rules(&mut tx, kind, id, &draft.rules, now).await?;
        tx.commit().await?;
        return Ok(Outcome::Saved(Saved { id, created: true }));
    };

    let columns = current_columns.unwrap_or_default();
    let changed: Vec<&str> = kind
        .document_columns()
        .iter()
        .copied()
        .filter(|column| columns.get(*column) != draft.columns.get(*column))
        .collect();
    let sources_changed = sorted(draft.sources.clone()) != sorted(current_sources);
    let rules_changed = draft.rules != current_rules;
    if changed.is_empty() && !sources_changed && !rules_changed {
        return Ok(Outcome::Saved(Saved { id, created: false }));
    }

    update_row(&mut tx, &draft, id, &changed, now)
        .await
        .map_err(|error| ApiError::unique(error, "foreign_id", "has already been taken"))?;
    if sources_changed {
        parts::replace_sources(&mut tx, kind, id, &draft.sources, &state.encryptor, now).await?;
    }
    if rules_changed {
        parts::replace_rules(&mut tx, kind, id, &draft.rules, now).await?;
    }
    let affected = grantee_ids(&mut tx, kind.owner_column(), id).await?;
    bump_sync_config(&mut tx, &affected).await?;
    tx.commit().await?;

    if let Some(credential_id) = broker_credential_id {
        super::reconcile_wrapped_credential(state, credential_id).await;
    }
    Ok(Outcome::Saved(Saved { id, created: false }))
}

async fn lock_oid(
    conn: &mut PgConnection,
    kind: Kind,
    oid: &str,
) -> Result<store::ParentRow, ApiError> {
    let id = kind.decode_oid(oid).ok_or_else(ApiError::not_found)?;
    store::lock(conn, kind, id)
        .await?
        .ok_or_else(ApiError::not_found)
}

/// `request` with every top-level key it lacks taken from `base`.
fn merge_over(request: &mut Value, base: Value) {
    if let (Some(request), Value::Object(base)) = (request.as_object_mut(), base) {
        for (key, value) in base {
            request.entry(key).or_insert(value);
        }
    }
}

/// A single-source document that names an inline source but carries no
/// value keeps the stored one (a metadata edit does not re-enter secrets).
fn keep_inline_value(draft: &mut Draft, current: &[Source]) {
    if draft.kind.source_key().is_none() || draft.kind == Kind::SshKey {
        return;
    }
    let (Some(new), Some(old)) = (draft.sources.first_mut(), current.first()) else {
        return;
    };
    if new.inline() && old.inline() && parts::blank_str(new.secret.as_deref()) {
        new.secret = old.secret.clone();
    }
}

fn quoted(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn insert_row(
    conn: &mut PgConnection,
    draft: &Draft,
    created_by: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<i64, sqlx::Error> {
    let table = draft.kind.table();
    let columns = quoted(draft.kind.document_columns());
    sqlx::query_scalar(&format!(
        "INSERT INTO {table} ({columns}, created_by_id, created_at, updated_at) \
         SELECT {columns}, $2, $3, $3 FROM jsonb_populate_record(NULL::{table}, $1) RETURNING id"
    ))
    .bind(Value::Object(draft.columns.clone()))
    .bind(created_by)
    .bind(now.naive_utc())
    .fetch_one(conn)
    .await
}

/// Sets the `changed` columns (from the draft) and `updated_at`.
async fn update_row(
    conn: &mut PgConnection,
    draft: &Draft,
    id: i64,
    changed: &[&str],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let table = draft.kind.table();
    if changed.is_empty() {
        // Only sources or rules changed.
        sqlx::query(&format!("UPDATE {table} SET updated_at = $2 WHERE id = $1"))
            .bind(id)
            .bind(now.naive_utc())
            .execute(conn)
            .await?;
        return Ok(());
    }
    let columns = quoted(changed);
    sqlx::query(&format!(
        "UPDATE {table} SET ({columns}, updated_at) = \
         (SELECT {columns}, $3 FROM jsonb_populate_record(NULL::{table}, $2)) WHERE id = $1"
    ))
    .bind(id)
    .bind(Value::Object(draft.columns.clone()))
    .bind(now.naive_utc())
    .execute(conn)
    .await?;
    Ok(())
}

/// Deletes a secret with its grants, sources, and rules; the principals
/// that used it get a fresh config.
pub async fn destroy(state: &AppState, kind: Kind, oid: &str) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    let id = lock_oid(&mut tx, kind, oid).await?.id;
    // Resolved before the grants that name them are gone.
    let affected = grantee_ids(&mut tx, kind.owner_column(), id).await?;
    let owner = kind.owner_column();
    for table in ["grants", "secret_sources"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE {owner} = $1"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if kind.has_rules() {
        sqlx::query(&format!("DELETE FROM request_rules WHERE {owner} = $1"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(&format!("DELETE FROM {} WHERE id = $1", kind.table()))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    bump_sync_config(&mut tx, &affected).await?;
    tx.commit().await?;
    Ok(())
}
