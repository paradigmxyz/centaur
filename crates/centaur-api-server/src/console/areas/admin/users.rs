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

use super::common::{self, json_ok};
use crate::{
    AppState,
    console::guard::GuardedRoutes,
    console::{
        http::{ConsoleError, json_data},
        request::ConsoleQuery,
    },
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
    /// A blank filter is no filter, but the stored value is matched exactly (no
    /// trimming). The gate already rejected an unknown value, so anything else
    /// here is blank.
    fn filter(&self) -> Option<&str> {
        self.status
            .as_deref()
            .filter(|status| STATUSES.contains(status))
    }
}

/// The status inclusion rule. An unknown filter answers a plain 400 rather than
/// the 422 field envelope.
fn status_filter(value: &Option<String>, _: &()) -> garde::Result {
    match value.as_deref() {
        // A blank filter is no filter at all.
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

pub(super) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .console_admin("/users", get(index))
        .console_admin("/users/{id}/approve", post(approve))
        .console_admin("/users/{id}/disable", post(disable))
        .console_admin("/users/{id}/promote", post(promote))
        .console_admin("/users/{id}/demote", post(demote))
}

async fn index(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleQuery(query): ConsoleQuery<UsersQuery>,
) -> Result<Response, ConsoleError> {
    if let Err(report) = query.validate() {
        let message = plain_message(&report).expect("a failed status gate carries a message");
        return Err(ConsoleError::bad_request(message));
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
) -> Result<Response, ConsoleError> {
    act(&state, &id, user, UserOp::Approve).await
}

async fn disable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    act(&state, &id, user, UserOp::Disable).await
}

async fn promote(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    act(&state, &id, user, UserOp::Promote).await
}

async fn demote(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    act(&state, &id, user, UserOp::Demote).await
}

/// The four Users-page actions; they differ only in their write and whether
/// they refuse the caller's own account.
#[derive(Clone, Copy)]
enum UserOp {
    Approve,
    Disable,
    Promote,
    Demote,
}

impl UserOp {
    /// The refusal Console renders for the caller's own account.
    fn self_refusal(self) -> Option<&'static str> {
        match self {
            Self::Disable => Some("You can't disable your own account."),
            Self::Demote => Some("You can't demote your own account."),
            Self::Approve | Self::Promote => None,
        }
    }
}

async fn act(
    state: &AppState,
    oid: &str,
    user: ConsoleUser,
    operation: UserOp,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(state)?;
    let target = load(&pool, oid).await?;
    if let Some(message) = operation.self_refusal()
        && target.id == user.id
    {
        return Err(self_error(message));
    }
    match operation {
        UserOp::Approve => approve_user(&pool, target.id, user.id).await?,
        UserOp::Disable => disable_user(&pool, target.id).await?,
        UserOp::Promote => promote_user(&pool, target.id).await?,
        UserOp::Demote => demote_user(&pool, target.id).await?,
    }
    render_user(&pool, target.id, user.id).await
}

async fn load(pool: &sqlx::PgPool, oid: &str) -> Result<UserRow, ConsoleError> {
    Ok(admin_user(pool, common::oid::<UserModel>(oid)?)
        .await?
        .ok_or_else(UserModel::not_found)?)
}

/// The reloaded row after a write.
async fn render_user(pool: &sqlx::PgPool, id: i64, current: i64) -> Result<Response, ConsoleError> {
    let row = admin_user(pool, id)
        .await?
        .ok_or_else(|| ConsoleError::internal(format!("user {id} vanished after its update")))?;
    Ok(json_data(payload(&row, current)))
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
        // The filter value itself is not trimmed.
        assert_eq!(filter(r#"{"status": " active "}"#), Err(()));
    }
}
