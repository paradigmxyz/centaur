//! Reads and response payloads for the principals area.

use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use time::PrimitiveDateTime;

use super::{
    PERMISSION_COLUMNS, PrincipalRow, ROLE_COLUMNS, Result, RoleRow, like_pattern,
    merge_permissions, role_summary,
};
use crate::model::{self, ConsoleModel};

/// A principal with the permissions and roles every payload needs.
#[derive(Clone, Debug)]
pub struct PrincipalView {
    pub row: PrincipalRow,
    pub direct_permissions: Vec<super::PermissionRow>,
    /// Role permissions of every assigned role, ordered by permission id.
    pub role_permissions: Vec<super::PermissionRow>,
    pub roles: Vec<RoleRow>,
}

/// A role with its direct Slack channel permissions.
#[derive(Clone, Debug)]
pub struct RoleView {
    pub row: RoleRow,
    pub permissions: Vec<super::PermissionRow>,
}

/// One row in the console's Slack channel picker.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct SlackChannelOption {
    pub channel_id: String,
    pub name: String,
    pub private: bool,
}

pub async fn find_principal_by_oid(pool: &PgPool, oid: &str) -> Result<PrincipalView> {
    let row = model::find_by_oid::<model::Principal, PrincipalRow, _>(pool, oid)
        .await?
        .ok_or_else(model::Principal::not_found)?;
    principal_view_for(pool, row).await
}

pub async fn find_principal_by_foreign_id(
    pool: &PgPool,
    foreign_id: &str,
) -> Result<PrincipalView> {
    let row = model::find_by_foreign_id::<model::Principal, PrincipalRow, _>(pool, foreign_id)
        .await?
        .ok_or_else(model::Principal::not_found)?;
    principal_view_for(pool, row).await
}

pub async fn find_role_by_oid(pool: &PgPool, oid: &str) -> Result<RoleView> {
    let row = model::find_by_oid::<model::Role, RoleRow, _>(pool, oid)
        .await?
        .ok_or_else(model::Role::not_found)?;
    role_view_for(pool, row).await
}

pub async fn find_role_by_foreign_id(pool: &PgPool, foreign_id: &str) -> Result<RoleView> {
    let row = model::find_by_foreign_id::<model::Role, RoleRow, _>(pool, foreign_id)
        .await?
        .ok_or_else(model::Role::not_found)?;
    role_view_for(pool, row).await
}

pub async fn load_principal_view(pool: &PgPool, id: i64) -> Result<Option<PrincipalView>> {
    let Some(row) = model::find_by_id::<model::Principal, PrincipalRow, _>(pool, id).await? else {
        return Ok(None);
    };
    principal_view_for(pool, row).await.map(Some)
}

pub async fn load_role_view(pool: &PgPool, id: i64) -> Result<Option<RoleView>> {
    let Some(row) = model::find_by_id::<model::Role, RoleRow, _>(pool, id).await? else {
        return Ok(None);
    };
    role_view_for(pool, row).await.map(Some)
}

async fn principal_view_for(pool: &PgPool, row: PrincipalRow) -> Result<PrincipalView> {
    let direct_permissions = direct_permissions(pool, row.id).await?;
    let roles = principal_roles(pool, row.id).await?;
    let role_permissions = if roles.is_empty() {
        Vec::new()
    } else {
        let role_ids: Vec<i64> = roles.iter().map(|role| role.id).collect();
        sqlx::query_as(&format!(
            "SELECT {PERMISSION_COLUMNS} FROM slack_channel_permissions \
             WHERE role_id = ANY($1) ORDER BY id"
        ))
        .bind(&role_ids)
        .fetch_all(pool)
        .await?
    };
    Ok(PrincipalView {
        row,
        direct_permissions,
        role_permissions,
        roles,
    })
}

async fn role_view_for(pool: &PgPool, row: RoleRow) -> Result<RoleView> {
    let permissions = sqlx::query_as(&format!(
        "SELECT {PERMISSION_COLUMNS} FROM slack_channel_permissions \
         WHERE role_id = $1 ORDER BY channel_id, id"
    ))
    .bind(row.id)
    .fetch_all(pool)
    .await?;
    Ok(RoleView { row, permissions })
}

pub(crate) async fn direct_permissions(
    pool: &PgPool,
    principal_id: i64,
) -> Result<Vec<super::PermissionRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {PERMISSION_COLUMNS} FROM slack_channel_permissions \
         WHERE principal_id = $1 ORDER BY channel_id, id"
    ))
    .bind(principal_id)
    .fetch_all(pool)
    .await?)
}

/// The principal's roles in assignment order (`order(:id)`).
pub async fn principal_roles(pool: &PgPool, principal_id: i64) -> Result<Vec<RoleRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {ROLE_COLUMNS} FROM roles \
         WHERE id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1) \
         ORDER BY id"
    ))
    .bind(principal_id)
    .fetch_all(pool)
    .await?)
}

/// `principal.labels_with_sandbox_capabilities`.
pub fn labels_with_cache(row: &PrincipalRow) -> Value {
    let mut labels = row.labels.as_object().cloned().unwrap_or_default();
    labels.insert(
        super::SANDBOX_REPO_CACHE_LABEL.to_owned(),
        Value::String(row.sandbox_repo_cache.clone()),
    );
    Value::Object(labels)
}

fn permissions_payload(
    rows: &[super::PermissionRow],
    merge: bool,
) -> Vec<super::PermissionPayload> {
    if merge {
        merge_permissions(rows)
    } else {
        rows.iter().map(super::PermissionPayload::from).collect()
    }
}

/// The bearer payload rendered for a principal.
#[derive(Clone, Debug, Serialize)]
pub struct PrincipalPayload {
    pub id: String,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub labels: Value,
    pub slack_channel_permissions: Vec<super::PermissionPayload>,
    pub effective_slack_channel_permissions: Vec<super::PermissionPayload>,
    pub sandbox_repo_cache: String,
    pub sandbox_observability_enabled: bool,
    pub sandbox_sessions_read_enabled: bool,
    pub sandbox_workflows_read_enabled: bool,
    pub sandbox_workflows_write_enabled: bool,
    #[serde(with = "crate::timestamp::primitive")]
    pub created_at: PrimitiveDateTime,
    #[serde(with = "crate::timestamp::primitive")]
    pub updated_at: PrimitiveDateTime,
}

impl From<&PrincipalView> for PrincipalPayload {
    fn from(view: &PrincipalView) -> Self {
        let mut effective = view.direct_permissions.clone();
        effective.extend(view.role_permissions.iter().cloned());
        Self {
            id: model::Principal::oid(view.row.id),
            foreign_id: view.row.foreign_id.clone(),
            name: view.row.name.clone(),
            labels: labels_with_cache(&view.row),
            slack_channel_permissions: permissions_payload(&view.direct_permissions, false),
            effective_slack_channel_permissions: permissions_payload(&effective, true),
            sandbox_repo_cache: view.row.sandbox_repo_cache.clone(),
            sandbox_observability_enabled: view.row.sandbox_observability_enabled,
            sandbox_sessions_read_enabled: view.row.sandbox_sessions_read_enabled,
            sandbox_workflows_read_enabled: view.row.sandbox_workflows_read_enabled,
            sandbox_workflows_write_enabled: view.row.sandbox_workflows_write_enabled,
            created_at: view.row.created_at,
            updated_at: view.row.updated_at,
        }
    }
}

pub fn principal_payload(view: &PrincipalView) -> PrincipalPayload {
    PrincipalPayload::from(view)
}

/// The console payload adds the identity fields and inherited permissions.
#[derive(Clone, Debug, Serialize)]
pub struct ConsolePrincipalPayload {
    #[serde(flatten)]
    pub principal: PrincipalPayload,
    pub kind: String,
    pub slack_user_id: Option<String>,
    pub roles: Vec<super::RoleSummaryPayload>,
    pub inherited_slack_channel_permissions: Vec<super::PermissionPayload>,
}

pub fn console_principal_payload(view: &PrincipalView) -> ConsolePrincipalPayload {
    ConsolePrincipalPayload {
        principal: PrincipalPayload::from(view),
        kind: view.row.kind.clone(),
        slack_user_id: view.row.slack_user_id.clone(),
        roles: view.roles.iter().map(role_summary).collect(),
        inherited_slack_channel_permissions: permissions_payload(&view.role_permissions, true),
    }
}

/// The payload rendered for a role.
#[derive(Clone, Debug, Serialize)]
pub struct RolePayload {
    pub id: String,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub labels: Value,
    pub slack_channel_permissions: Vec<super::PermissionPayload>,
    #[serde(with = "crate::timestamp::primitive")]
    pub created_at: PrimitiveDateTime,
    #[serde(with = "crate::timestamp::primitive")]
    pub updated_at: PrimitiveDateTime,
}

pub fn role_payload(view: &RoleView) -> RolePayload {
    RolePayload {
        id: model::Role::oid(view.row.id),
        foreign_id: view.row.foreign_id.clone(),
        name: view.row.name.clone(),
        labels: view.row.labels.clone(),
        slack_channel_permissions: permissions_payload(&view.permissions, false),
        created_at: view.row.created_at,
        updated_at: view.row.updated_at,
    }
}

/// One Slack channel option, as the console's pickers render it.
#[derive(Clone, Debug, Serialize)]
pub struct SlackChannelOptionPayload {
    pub channel_id: String,
    pub name: String,
    pub private: bool,
    pub label: String,
    pub description: String,
}

pub fn slack_channel_option(row: &SlackChannelOption) -> SlackChannelOptionPayload {
    SlackChannelOptionPayload {
        channel_id: row.channel_id.clone(),
        name: row.name.clone(),
        private: row.private,
        label: format!("#{}", row.name),
        description: format!(
            "{} · {}",
            row.channel_id,
            if row.private { "Private" } else { "Public" }
        ),
    }
}

/// Whether a Slack bot token is configured.
pub fn slack_catalog_configured() -> bool {
    ["CENTAUR_CONSOLE_SLACK_BOT_TOKEN", "SLACK_BOT_TOKEN"]
        .into_iter()
        .any(|name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        })
}

/// The catalog error message, evaluated after the catalog was consulted.
pub async fn slack_catalog_error(pool: &PgPool) -> crate::Result<Option<&'static str>> {
    if !slack_catalog_configured() {
        return Ok(Some("SLACK_BOT_TOKEN is not configured."));
    }
    let any: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM slack_bot_channels)")
        .fetch_one(pool)
        .await?;
    Ok((!any).then_some("Slack channel catalog is loading. Enter a channel ID or reload shortly."))
}

/// The channel options of a Slack channel lookup.
pub async fn slack_channels(
    pool: &PgPool,
    ids: &[String],
    q: Option<&str>,
    exclude_channel_ids: &[String],
) -> Result<Vec<SlackChannelOption>> {
    let ids: Vec<String> = ids
        .iter()
        .map(|id| id.trim().to_uppercase())
        .filter(|id| !id.is_empty())
        .take(200)
        .collect();
    let rows: Vec<SlackChannelOption> = if !ids.is_empty() {
        sqlx::query_as(
            "SELECT channel_id, name, private FROM slack_bot_channels \
             WHERE channel_id = ANY($1) ORDER BY lower(name), channel_id",
        )
        .bind(&ids)
        .fetch_all(pool)
        .await?
    } else {
        // Each placeholder is the position of the value bound below, so the
        // conditions and the binds stay in one order.
        let excluded = !exclude_channel_ids.is_empty();
        let search = q.map(str::trim).filter(|search| !search.is_empty());
        let mut conditions = vec!["active = true".to_owned(), "archived = false".to_owned()];
        if excluded {
            conditions.push("channel_id <> ALL($1::text[])".to_owned());
        }
        if search.is_some() {
            let position = 1 + usize::from(excluded);
            conditions.push(format!(
                "(name ILIKE ${position} OR channel_id ILIKE ${position})"
            ));
        }
        let query_sql = format!(
            "SELECT channel_id, name, private FROM slack_bot_channels WHERE {} \
             ORDER BY lower(name), channel_id LIMIT 20",
            conditions.join(" AND ")
        );
        let mut query = sqlx::query_as(&query_sql);
        if excluded {
            query = query.bind(exclude_channel_ids.to_vec());
        }
        if let Some(search) = search {
            query = query.bind(like_pattern(search));
        }
        query.fetch_all(pool).await?
    };
    Ok(rows)
}

/// The channel ids a principal's or role's own permission rows cover.
pub async fn permission_channel_ids(
    pool: &PgPool,
    principal_id: Option<i64>,
    role_id: Option<i64>,
) -> Result<Vec<String>> {
    let rows = match (principal_id, role_id) {
        (Some(principal_id), _) => {
            sqlx::query_scalar(
                "SELECT channel_id FROM slack_channel_permissions \
             WHERE principal_id = $1 ORDER BY id",
            )
            .bind(principal_id)
            .fetch_all(pool)
            .await?
        }
        (_, Some(role_id)) => {
            sqlx::query_scalar(
                "SELECT channel_id FROM slack_channel_permissions \
             WHERE role_id = $1 ORDER BY id",
            )
            .bind(role_id)
            .fetch_all(pool)
            .await?
        }
        _ => Vec::new(),
    };
    Ok(rows)
}
