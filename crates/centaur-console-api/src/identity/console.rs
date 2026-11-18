//! The console API's identity endpoints:
//!
//! - `GET /me` (pending users allowed): who the caller is, the CSRF token
//!   the SPA sends back as `X-CSRF-Token`, features, nav, and the one-shot
//!   flash;
//! - `DELETE /session` (pending users allowed): sign out;
//! - `/users` (admins): list, approve, disable, promote, demote.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::{decode_oid, oid};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::settings;
use crate::{
    ApiError, ApiResult, AppState,
    auth::{ConsoleCaller, Session},
    extract::{Path, Query, data},
};

#[derive(Serialize)]
struct Nav {
    control: bool,
    principals: bool,
    roles: bool,
    secrets: bool,
    credentials: bool,
    oauth_apps: bool,
    users: bool,
    api_clients: bool,
    settings: bool,
    workflows: bool,
    integrations: bool,
    skills: bool,
    scheduled_tasks: bool,
}

/// `GET /me`. Never cached: it carries the CSRF token.
pub async fn me(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    session: Session,
) -> ApiResult {
    let (csrf_token, flash) = {
        let mut session = session.lock();
        let token = session.csrf_token().to_owned();
        (token, session.take_flash())
    };
    let admin = user.admin;
    let api_clients = settings::api_clients_enabled(&state.config.env);
    let active = user.is_active();
    let mut flash_json = serde_json::Map::new();
    for (key, value) in [("notice", flash.notice), ("alert", flash.alert)] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            flash_json.insert(key.into(), value.into());
        }
    }
    let body = data(serde_json::json!({
        "user": {
            "id": oid("usr", user.id),
            "email": user.email,
            "name": user.name,
            "status": user.status,
            "admin": user.admin,
        },
        "auth_method": "session",
        "acting_admin": admin,
        "csrf_token": csrf_token,
        "features": {
            "api_clients": api_clients,
            "password_login": settings::password_login_enabled(&state.config.env),
        },
        "nav": Nav {
            control: admin,
            principals: admin,
            roles: admin,
            secrets: admin,
            credentials: admin,
            oauth_apps: admin,
            users: admin,
            api_clients: admin && api_clients,
            settings: admin,
            workflows: admin,
            integrations: active,
            skills: active,
            scheduled_tasks: active,
        },
        "flash": flash_json,
    }));
    Ok(([(http::header::CACHE_CONTROL, "no-store")], body).into_response())
}

/// `DELETE /session`: signs out, leaving a notice for the login page.
pub async fn sign_out(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    session: Session,
) -> StatusCode {
    let mut session = session.lock();
    session.sign_out(state.now());
    session.set_notice("Signed out.");
    StatusCode::NO_CONTENT
}

// --- users ------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct UserRow {
    id: i64,
    email: String,
    name: Option<String>,
    status: String,
    admin: bool,
    identity_providers: Vec<String>,
    approved_at: Option<DateTime<Utc>>,
    approved_by_id: Option<i64>,
    approved_by_email: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const SELECT_USERS: &str = "SELECT u.id, u.email, u.name, u.status, u.admin, \
    ARRAY(SELECT DISTINCT provider FROM user_identities WHERE user_id = u.id ORDER BY provider) \
        AS identity_providers, \
    u.approved_at AT TIME ZONE 'UTC' AS approved_at, u.approved_by_id, \
    ab.email AS approved_by_email, \
    u.created_at AT TIME ZONE 'UTC' AS created_at, u.updated_at AT TIME ZONE 'UTC' AS updated_at \
    FROM users u LEFT JOIN users ab ON ab.id = u.approved_by_id";

fn payload(row: UserRow, current_id: i64) -> serde_json::Value {
    serde_json::json!({
        "id": oid("usr", row.id),
        "email": row.email,
        "name": row.name,
        "status": row.status,
        "admin": row.admin,
        "current": row.id == current_id,
        "identity_providers": row.identity_providers,
        "approved_at": row.approved_at,
        "approved_by": row.approved_by_id.zip(row.approved_by_email).map(
            |(id, email)| serde_json::json!({"id": oid("usr", id), "email": email})
        ),
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

pub const STATUSES: [&str; 3] = ["pending", "active", "disabled"];

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
}

/// `GET /users[?status=]`: every user, admins first, then by email.
pub async fn list_users(
    State(state): State<AppState>,
    ConsoleCaller(caller): ConsoleCaller,
    Query(query): Query<ListQuery>,
) -> ApiResult {
    let status = query.status.filter(|status| !status.is_empty());
    if let Some(status) = &status
        && !STATUSES.contains(&status.as_str())
    {
        return Err(ApiError::BadRequest(format!(
            "status must be one of {}",
            STATUSES.join(", ")
        )));
    }
    let rows: Vec<UserRow> = sqlx::query_as(&format!(
        "{SELECT_USERS} WHERE ($1::text IS NULL OR u.status = $1) \
         ORDER BY u.admin DESC, u.email ASC, u.id ASC"
    ))
    .bind(status)
    .fetch_all(&state.db)
    .await?;
    let data: Vec<_> = rows
        .into_iter()
        .map(|row| payload(row, caller.id))
        .collect();
    let total = data.len();
    Ok(axum::Json(serde_json::json!({"data": data, "meta": {"total": total}})).into_response())
}

fn user_id(oid: &str) -> Result<i64, ApiError> {
    decode_oid("usr", oid).ok_or_else(ApiError::not_found)
}

async fn render_user(state: &AppState, id: i64, caller_id: i64) -> ApiResult {
    let row: UserRow = sqlx::query_as(&format!("{SELECT_USERS} WHERE u.id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(data(payload(row, caller_id)).into_response())
}

fn not_yourself(what: &str) -> ApiError {
    ApiError::Unprocessable {
        message: format!("You can't {what} your own account."),
        reason: Some("self"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Approve,
    Disable,
    Promote,
    Demote,
}

/// Applies `action` to the user; 404 when there is no such user.
async fn transition(state: &AppState, caller_id: i64, oid: &str, action: Action) -> ApiResult {
    let id = user_id(oid)?;
    if id == caller_id && matches!(action, Action::Disable | Action::Demote) {
        return Err(not_yourself(if action == Action::Disable {
            "disable"
        } else {
            "demote"
        }));
    }
    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    let update = match action {
        Action::Approve => sqlx::query(
            "UPDATE users SET status = 'active', approved_at = $2, approved_by_id = $3, \
             updated_at = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(now)
        .bind(caller_id),
        Action::Disable => {
            sqlx::query("UPDATE users SET status = 'disabled', updated_at = $2 WHERE id = $1")
                .bind(id)
                .bind(now)
        }
        Action::Promote => sqlx::query(
            "UPDATE users SET admin = true, status = 'active', updated_at = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(now),
        Action::Demote => {
            sqlx::query("UPDATE users SET admin = false, updated_at = $2 WHERE id = $1")
                .bind(id)
                .bind(now)
        }
    };
    if update.execute(&mut *tx).await?.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    if action == Action::Disable {
        // A disabled user's MCP OAuth refresh tokens stop working.
        sqlx::query(
            "UPDATE mcp_oauth_refresh_tokens SET revoked_at = $2, updated_at = $2 \
             WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > $2",
        )
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    render_user(state, id, caller_id).await
}

macro_rules! user_action {
    ($(#[$doc:meta])* $name:ident, $action:expr) => {
        $(#[$doc])*
        pub async fn $name(
            State(state): State<AppState>,
            ConsoleCaller(caller): ConsoleCaller,
            Path(id): Path<String>,
        ) -> ApiResult {
            transition(&state, caller.id, &id, $action).await
        }
    };
}

user_action!(
    /// `POST /users/{id}/approve`: active, with the approver and time.
    approve,
    Action::Approve
);
user_action!(
    /// `POST /users/{id}/disable` (not your own account).
    disable,
    Action::Disable
);
user_action!(
    /// `POST /users/{id}/promote`: admin, and active (an admin must be able
    /// to use the console).
    promote,
    Action::Promote
);
user_action!(
    /// `POST /users/{id}/demote` (not your own account).
    demote,
    Action::Demote
);
