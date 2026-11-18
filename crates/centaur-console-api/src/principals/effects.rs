//! What follows a committed principal write: credential reconciliation, the
//! owner's personal SSH key, and the Slack channel catalog. They run after
//! the transaction committed, so a failure is logged and never fails the
//! request that already succeeded.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::{principal::Saved, reconcile};
use crate::AppState;

/// What a committed principal write tells the follow-ups.
pub struct Committed {
    pub principal_id: i64,
    pub created: bool,
    /// `kind`, `console_user_id`, or the Slack identity changed (or the
    /// principal is new).
    pub identity_changed: bool,
    /// Set for a newly created Slack channel principal.
    pub slack_channel_id: Option<String>,
}

impl From<&Saved> for Committed {
    fn from(saved: &Saved) -> Self {
        Self {
            principal_id: saved.id,
            created: saved.created,
            identity_changed: saved.identity_changed,
            slack_channel_id: saved.slack_channel_id.clone(),
        }
    }
}

pub async fn principal_committed(state: &AppState, committed: Committed) {
    after_write(
        &state.db,
        state.now(),
        committed.principal_id,
        committed.identity_changed,
    )
    .await;
    if committed.created
        && let Some(channel_id) = committed.slack_channel_id
    {
        let catalog = crate::misc::slack::Catalog::from_state(state);
        if catalog.configured() {
            catalog.spawn_channel_refresh(channel_id);
        }
    }
}

/// Credential reconciliation, then the personal key when the principal's
/// identity changed. For callers that hold a pool rather than the app state
/// (the scheduled-task claim provisions inside its own transaction).
pub async fn after_write(
    db: &PgPool,
    now: DateTime<Utc>,
    principal_id: i64,
    identity_changed: bool,
) {
    let mut identity_changed = identity_changed;
    match reconcile::apply_for_principal(db, now, principal_id).await {
        Ok(changed) => identity_changed |= changed,
        Err(error) => tracing::warn!(%error, "credential reconciliation for a principal failed"),
    }
    if identity_changed {
        grant_personal_key(db, now, principal_id).await;
    }
}

/// Grants the owner's personal SSH key to a principal that verifiably is
/// that owner.
pub async fn grant_personal_key(db: &PgPool, now: DateTime<Utc>, principal_id: i64) {
    if let Err(error) =
        centaur_console_db::personal_ssh_keys::apply_for_principal(db, principal_id, now).await
    {
        tracing::warn!(%error, "personal SSH key grant failed");
    }
}

/// A credential or wrapper secret appeared: grant it to the principals it
/// belongs to.
pub async fn credential_changed(state: &AppState, credential_id: i64) {
    match reconcile::apply_for_credential(&state.db, state.now(), credential_id).await {
        Ok(outcome) => {
            for id in outcome.identity_changed {
                grant_personal_key(&state.db, state.now(), id).await;
            }
        }
        Err(error) => tracing::warn!(%error, "credential reconciliation failed"),
    }
}

/// A user's Slack identity was linked: reconcile the credentials they own.
pub async fn slack_identity_linked(state: &AppState, user_id: i64) {
    match reconcile::reconcile_owner_credentials(&state.db, state.now(), user_id).await {
        Ok(outcome) => {
            for id in outcome.identity_changed {
                grant_personal_key(&state.db, state.now(), id).await;
            }
        }
        Err(error) => tracing::warn!(%error, "user identity credential reconciliation failed"),
    }
}
