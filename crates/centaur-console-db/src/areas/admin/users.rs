//! Console's Users page: every console user, and the approve/disable/promote/
//! demote actions. Disabling also revokes the user's MCP OAuth refresh tokens.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::PgPool;
use time::PrimitiveDateTime;

use crate::Result;
use crate::model::{ConsoleModel, User as UserModel};
use crate::write;

const USER_COLUMNS: &str = "\
    u.id, u.email, u.name, u.status, u.admin, u.approved_at, u.created_at, u.updated_at, \
    a.id AS approved_by_id, a.email AS approved_by_email";

/// A Console user as the Users page renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRow {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
    pub admin: bool,
    pub approved_at: Option<PrimitiveDateTime>,
    pub approved_by: Option<ApproverRow>,
    /// Identity providers, de-duplicated and sorted.
    pub identity_providers: Vec<String>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApproverRow {
    pub id: i64,
    pub email: String,
}

/// The joined projection the Users page reads: the user's own columns plus the
/// approver's id and email, which become [`ApproverRow`].
#[derive(sqlx::FromRow)]
struct UserRecord {
    id: i64,
    email: String,
    name: Option<String>,
    status: String,
    admin: bool,
    approved_at: Option<PrimitiveDateTime>,
    approved_by_id: Option<i64>,
    approved_by_email: Option<String>,
    created_at: PrimitiveDateTime,
    updated_at: PrimitiveDateTime,
}

impl UserRecord {
    fn into_row(self) -> UserRow {
        UserRow {
            id: self.id,
            email: self.email,
            name: self.name,
            status: self.status,
            admin: self.admin,
            approved_at: self.approved_at,
            approved_by: match (self.approved_by_id, self.approved_by_email) {
                (Some(id), Some(email)) => Some(ApproverRow { id, email }),
                _ => None,
            },
            identity_providers: Vec::new(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Every console user, optionally filtered by `status`, in Console's order:
/// admins first, then email, then id. The list is never paginated. The system
/// user api-rs writes as is not an operator and is never listed.
pub async fn admin_users(pool: &PgPool, status: Option<&str>) -> Result<Vec<UserRow>> {
    let records: Vec<UserRecord> = sqlx::query_as(&format!(
        "SELECT {USER_COLUMNS} FROM {} u LEFT JOIN {} a ON a.id = u.approved_by_id \
         WHERE ($1::text IS NULL OR u.status = $1) AND u.email <> $2 \
         ORDER BY u.admin DESC, u.email ASC, u.id ASC",
        UserModel::TABLE,
        UserModel::TABLE,
    ))
    .bind(status)
    .bind(crate::SYSTEM_USER_EMAIL)
    .fetch_all(pool)
    .await?;
    let mut users = records
        .into_iter()
        .map(UserRecord::into_row)
        .collect::<Vec<_>>();
    let ids = users.iter().map(|user| user.id).collect::<Vec<_>>();
    let mut providers = identity_providers(pool, &ids).await?;
    for user in &mut users {
        if let Some(providers) = providers.remove(&user.id) {
            user.identity_providers = providers;
        }
    }
    Ok(users)
}

/// The console user whose id is `id`, or `None` when it does not exist.
///
/// The lookup keeps its own `LEFT JOIN` (the payload renders the approver) and
/// uses the model descriptor only for the table name.
pub async fn admin_user(pool: &PgPool, id: i64) -> Result<Option<UserRow>> {
    let record: Option<UserRecord> = sqlx::query_as(&format!(
        "SELECT {USER_COLUMNS} FROM {} u LEFT JOIN {} a ON a.id = u.approved_by_id \
         WHERE u.id = $1 AND u.email <> $2",
        UserModel::TABLE,
        UserModel::TABLE,
    ))
    .bind(id)
    .bind(crate::SYSTEM_USER_EMAIL)
    .fetch_optional(pool)
    .await?;
    let Some(record) = record else {
        return Ok(None);
    };
    let mut user = record.into_row();
    user.identity_providers = identity_providers(pool, &[id])
        .await?
        .remove(&id)
        .unwrap_or_default();
    Ok(Some(user))
}

/// Activate the account and record the approver.
///
/// `approved_at` is the database clock, so this stays a plain statement; the
/// shared update helpers only bind values the caller already has.
pub async fn approve_user(pool: &PgPool, id: i64, approver_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE users SET status = 'active', approved_at = now(), approved_by_id = $2, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(approver_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Disable the account and revoke its usable MCP OAuth refresh tokens, in the
/// same transaction.
pub async fn disable_user(pool: &PgPool, id: i64) -> Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query(&write::update_sql(UserModel::TABLE, &["status"]))
        .bind("disabled")
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    // Revocation stamps both time columns, so it is not an ordinary timestamped
    // attribute update.
    sqlx::query(
        "UPDATE mcp_oauth_refresh_tokens SET revoked_at = now(), updated_at = now() \
         WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Promoting also activates: an admin must be able to use the console.
pub async fn promote_user(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query(&write::update_sql(UserModel::TABLE, &["admin", "status"]))
        .bind(true)
        .bind("active")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn demote_user(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query(&write::update_sql(UserModel::TABLE, &["admin"]))
        .bind(false)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ProviderRecord {
    user_id: i64,
    provider: Option<String>,
}

/// The `user_identities.provider` values per user, sorted and de-duplicated.
async fn identity_providers(pool: &PgPool, user_ids: &[i64]) -> Result<BTreeMap<i64, Vec<String>>> {
    if user_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let records: Vec<ProviderRecord> = sqlx::query_as(
        "SELECT user_id, provider FROM user_identities WHERE user_id = ANY($1) ORDER BY user_id, id",
    )
    .bind(user_ids)
    .fetch_all(pool)
    .await?;
    let mut providers: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for record in records {
        if let Some(provider) = record.provider {
            providers
                .entry(record.user_id)
                .or_default()
                .insert(provider);
        }
    }
    Ok(providers
        .into_iter()
        .map(|(user_id, providers)| (user_id, providers.into_iter().collect()))
        .collect())
}
