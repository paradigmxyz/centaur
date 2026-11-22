//! Console's Users page: list every console user, and approve, disable, promote,
//! or demote one. Admin-only, and never paginated.

use axum::{
    extract::{Path, State},
    response::Response,
    routing::{get, post},
};
use centaur_console_db::ConsoleUser;
use centaur_console_db::areas::admin::{
    ApproverRow, UserRow, admin_user, admin_users, approve_user, demote_user, disable_user,
    promote_user,
};
use centaur_console_db::model::{ConsoleModel, User as UserModel};
use centaur_console_db::validation::plain_message;
use garde::Validate;
use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

use super::common::{self, AdminError, json_ok};
use crate::{
    AppState,
    console::{http::ConsoleError, request::ConsoleQuery},
    console_session::ConsoleRoutes,
};

/// `User.statuses.keys`, in the enum's declaration order.
const STATUSES: [&str; 3] = ["pending", "active", "disabled"];

/// The Users page filters by `status`; the list is never paginated.
#[derive(Deserialize, garde::Validate)]
struct UsersQuery {
    #[serde(default)]
    #[garde(custom(status_filter))]
    status: Option<String>,
}

impl UsersQuery {
    /// Rails' `params[:status].present?`: a blank filter is no filter, but the
    /// stored value is matched exactly (no trimming).
    fn filter(&self) -> Option<&str> {
        self.status
            .as_deref()
            .filter(|status| !status.trim().is_empty())
    }
}

/// `User.statuses` inclusion. Rails raises `ActionController::BadRequest` for an
/// unknown filter, so this gate answers a plain 400 rather than the 422 field
/// envelope.
fn status_filter(value: &Option<String>, _: &()) -> garde::Result {
    match value.as_deref() {
        // A blank filter is no filter at all, as `params[:status].present?` is.
        Some(status) if !status.trim().is_empty() && !STATUSES.contains(&status) => Err(
            garde::Error::new(format!("status must be one of {}", STATUSES.join(", "))),
        ),
        _ => Ok(()),
    }
}

/// A Console user as the Users page renders it.
#[derive(Serialize)]
struct UserPayload {
    id: String,
    email: String,
    name: Option<String>,
    status: String,
    admin: bool,
    current: bool,
    identity_providers: Vec<String>,
    #[serde(with = "centaur_console_db::timestamp::primitive_option")]
    approved_at: Option<PrimitiveDateTime>,
    approved_by: Option<ApproverPayload>,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
}

#[derive(Serialize)]
struct ApproverPayload {
    id: String,
    email: String,
}

impl From<&ApproverRow> for ApproverPayload {
    fn from(approver: &ApproverRow) -> Self {
        Self {
            id: UserModel::oid(approver.id),
            email: approver.email.clone(),
        }
    }
}

/// The unpaginated list envelope: `{data, meta: {total}}`.
#[derive(Serialize)]
struct UsersPayload {
    data: Vec<UserPayload>,
    meta: UsersMeta,
}

#[derive(Serialize)]
struct UsersMeta {
    total: usize,
}

#[derive(Serialize)]
struct UserPayloadEnvelope {
    data: UserPayload,
}

pub(super) fn console(routes: ConsoleRoutes) -> ConsoleRoutes {
    routes
        .admin("/users", get(index))
        .admin("/users/{id}/approve", post(approve))
        .admin("/users/{id}/disable", post(disable))
        .admin("/users/{id}/promote", post(promote))
        .admin("/users/{id}/demote", post(demote))
}

async fn index(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleQuery(query): ConsoleQuery<UsersQuery>,
) -> Result<Response, AdminError> {
    if let Err(report) = query.validate() {
        let message = plain_message(&report).expect("a failed status gate carries a message");
        return Err(ConsoleError::bad_request(message).into());
    }
    let status = query.filter();
    let pool = common::pool(&state)?;
    let users = admin_users(&pool, status).await?;
    Ok(json_ok(UsersPayload {
        meta: UsersMeta { total: users.len() },
        data: users
            .iter()
            .map(|row| payload(row, user.id))
            .collect::<Vec<_>>(),
    }))
}

async fn approve(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    let target = load(&pool, &id).await?;
    approve_user(&pool, target.id, user.id).await?;
    render_user(&pool, target.id, user.id).await
}

async fn disable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    let target = load(&pool, &id).await?;
    if target.id == user.id {
        return Err(self_error("You can't disable your own account.").into());
    }
    disable_user(&pool, target.id).await?;
    render_user(&pool, target.id, user.id).await
}

async fn promote(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    let target = load(&pool, &id).await?;
    promote_user(&pool, target.id).await?;
    render_user(&pool, target.id, user.id).await
}

async fn demote(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    let target = load(&pool, &id).await?;
    if target.id == user.id {
        return Err(self_error("You can't demote your own account.").into());
    }
    demote_user(&pool, target.id).await?;
    render_user(&pool, target.id, user.id).await
}

async fn load(pool: &sqlx::PgPool, oid: &str) -> Result<UserRow, AdminError> {
    let id = UserModel::decode_oid(oid).ok_or_else(UserModel::not_found)?;
    Ok(admin_user(pool, id)
        .await?
        .ok_or_else(UserModel::not_found)?)
}

/// The reloaded row after a write, as Rails' `render_user` does.
async fn render_user(pool: &sqlx::PgPool, id: i64, current: i64) -> Result<Response, AdminError> {
    let row = admin_user(pool, id)
        .await?
        .ok_or_else(|| ConsoleError::internal(format!("user {id} vanished after its update")))?;
    Ok(json_ok(UserPayloadEnvelope {
        data: payload(&row, current),
    }))
}

fn self_error(message: &str) -> ConsoleError {
    ConsoleError::unprocessable(message).with_reason("self")
}

fn payload(row: &UserRow, current_user_id: i64) -> UserPayload {
    UserPayload {
        id: UserModel::oid(row.id),
        email: row.email.clone(),
        name: row.name.clone(),
        status: row.status.clone(),
        admin: row.admin,
        current: row.id == current_user_id,
        identity_providers: row.identity_providers.clone(),
        approved_at: row.approved_at,
        approved_by: row.approved_by.as_ref().map(ApproverPayload::from),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(body: &str) -> Result<Option<String>, ()> {
        let query: UsersQuery = serde_json::from_str(body).expect("a typed query");
        query.validate().map_err(|_| ())?;
        Ok(query.filter().map(str::to_owned))
    }

    #[test]
    fn the_status_filter_is_a_list_membership_gate() {
        assert_eq!(filter(r#"{}"#), Ok(None));
        assert_eq!(filter(r#"{"status": ""}"#), Ok(None));
        assert_eq!(filter(r#"{"status": "  "}"#), Ok(None));
        assert_eq!(
            filter(r#"{"status": "active"}"#),
            Ok(Some("active".to_owned()))
        );
        assert_eq!(filter(r#"{"status": "bogus"}"#), Err(()));
        // The filter value itself is not trimmed, as Rails' `where` is not.
        assert_eq!(filter(r#"{"status": " active "}"#), Err(()));
    }
}
