//! JSON for principals, roles, and grants. Lists are rendered with one
//! query per association for the whole page (never per row).

use std::collections::{BTreeMap, HashMap};

use centaur_console_db::oid;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use super::{Ref, SECRET_KINDS, SecretKind};

/// The label that mirrors `sandbox_repo_cache` in `labels`.
pub const REPO_CACHE_LABEL: &str = "centaur.sandbox_repo_cache";

pub const PRINCIPAL_COLUMNS: &str = "id, foreign_id, name, kind, labels, slack_user_id, \
    sandbox_repo_cache, sandbox_observability_enabled, sandbox_sessions_read_enabled, \
    sandbox_workflows_read_enabled, sandbox_workflows_write_enabled, \
    created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at";

pub const ROLE_COLUMNS: &str = "id, foreign_id, name, labels, \
    created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at";

#[derive(sqlx::FromRow)]
pub struct PrincipalRow {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub kind: String,
    pub labels: Value,
    pub slack_user_id: Option<String>,
    pub sandbox_repo_cache: String,
    pub sandbox_observability_enabled: bool,
    pub sandbox_sessions_read_enabled: bool,
    pub sandbox_workflows_read_enabled: bool,
    pub sandbox_workflows_write_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
pub struct RoleRow {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub labels: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A Slack channel permission as rendered (and as stored).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct PermissionRow {
    pub channel_id: String,
    pub upload_enabled: bool,
    pub download_enabled: bool,
    pub history_enabled: bool,
}

/// One row per channel (upper-cased), flags OR-ed, sorted by channel id.
pub fn merge_permissions<'a>(
    rows: impl IntoIterator<Item = &'a PermissionRow>,
) -> Vec<PermissionRow> {
    let mut merged: BTreeMap<String, PermissionRow> = BTreeMap::new();
    for row in rows {
        let channel_id = row.channel_id.trim().to_uppercase();
        let entry = merged
            .entry(channel_id.clone())
            .or_insert_with(|| PermissionRow {
                channel_id,
                upload_enabled: false,
                download_enabled: false,
                history_enabled: false,
            });
        entry.upload_enabled |= row.upload_enabled;
        entry.download_enabled |= row.download_enabled;
        entry.history_enabled |= row.history_enabled;
    }
    merged.into_values().collect()
}

pub async fn owner_permissions(
    db: &PgPool,
    owner_column: &'static str,
    ids: &[i64],
) -> Result<HashMap<i64, Vec<PermissionRow>>, sqlx::Error> {
    let pairs: Vec<OwnedPermission> = sqlx::query_as(&format!(
        "SELECT {owner_column} AS owner_id, channel_id, upload_enabled, download_enabled, \
         history_enabled FROM slack_channel_permissions WHERE {owner_column} = ANY($1) \
         ORDER BY channel_id COLLATE \"C\", id"
    ))
    .bind(ids)
    .fetch_all(db)
    .await?;
    let mut by_owner: HashMap<i64, Vec<PermissionRow>> = HashMap::new();
    for pair in pairs {
        by_owner.entry(pair.owner_id).or_default().push(pair.row);
    }
    Ok(by_owner)
}

#[derive(sqlx::FromRow)]
struct OwnedPermission {
    owner_id: i64,
    #[sqlx(flatten)]
    row: PermissionRow,
}

pub async fn principals_json(
    db: &PgPool,
    rows: Vec<PrincipalRow>,
) -> Result<Vec<Value>, sqlx::Error> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let mut direct = owner_permissions(db, "principal_id", &ids).await?;
    // Roles, once for the page, then the permissions of those roles.
    #[derive(sqlx::FromRow)]
    struct Assignment {
        principal_id: i64,
        #[sqlx(flatten)]
        role: RoleRef,
    }
    #[derive(sqlx::FromRow)]
    struct RoleRef {
        id: i64,
        foreign_id: Option<String>,
        name: Option<String>,
    }
    let assignments: Vec<Assignment> = sqlx::query_as(
        "SELECT pr.principal_id, r.id, r.foreign_id, r.name FROM principal_roles pr \
         JOIN roles r ON r.id = pr.role_id WHERE pr.principal_id = ANY($1) ORDER BY r.id",
    )
    .bind(&ids)
    .fetch_all(db)
    .await?;
    let mut role_ids: Vec<i64> = assignments.iter().map(|a| a.role.id).collect();
    role_ids.sort_unstable();
    role_ids.dedup();
    let role_permissions = owner_permissions(db, "role_id", &role_ids).await?;
    let mut roles_of: HashMap<i64, Vec<&RoleRef>> = HashMap::new();
    for assignment in &assignments {
        roles_of
            .entry(assignment.principal_id)
            .or_default()
            .push(&assignment.role);
    }

    Ok(rows
        .into_iter()
        .map(|row| {
            let own = direct.remove(&row.id).unwrap_or_default();
            let roles = roles_of.remove(&row.id).unwrap_or_default();
            let inherited = merge_permissions(
                roles
                    .iter()
                    .filter_map(|role| role_permissions.get(&role.id))
                    .flatten(),
            );
            let effective = merge_permissions(own.iter().chain(&inherited));
            let mut labels = row.labels.as_object().cloned().unwrap_or_default();
            labels.insert(
                REPO_CACHE_LABEL.into(),
                row.sandbox_repo_cache.clone().into(),
            );
            json!({
                "id": oid("prn", row.id),
                "foreign_id": row.foreign_id,
                "name": row.name,
                "labels": labels,
                "kind": row.kind,
                "slack_user_id": row.slack_user_id,
                "slack_channel_permissions": own,
                "effective_slack_channel_permissions": effective,
                "inherited_slack_channel_permissions": inherited,
                "roles": roles.iter().map(|role| Ref {
                    id: oid("role", role.id),
                    foreign_id: role.foreign_id.clone(),
                    name: role.name.clone(),
                }).collect::<Vec<_>>(),
                "sandbox_repo_cache": row.sandbox_repo_cache,
                "sandbox_observability_enabled": row.sandbox_observability_enabled,
                "sandbox_sessions_read_enabled": row.sandbox_sessions_read_enabled,
                "sandbox_workflows_read_enabled": row.sandbox_workflows_read_enabled,
                "sandbox_workflows_write_enabled": row.sandbox_workflows_write_enabled,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
            })
        })
        .collect())
}

pub async fn roles_json(db: &PgPool, rows: Vec<RoleRow>) -> Result<Vec<Value>, sqlx::Error> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let mut permissions = owner_permissions(db, "role_id", &ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "id": oid("role", row.id),
                "foreign_id": row.foreign_id,
                "name": row.name,
                "labels": row.labels,
                "slack_channel_permissions": permissions.remove(&row.id).unwrap_or_default(),
                "created_at": row.created_at,
                "updated_at": row.updated_at,
            })
        })
        .collect())
}

pub async fn principal_json(db: &PgPool, id: i64) -> Result<Value, sqlx::Error> {
    let row: PrincipalRow = sqlx::query_as(&format!(
        "SELECT {PRINCIPAL_COLUMNS} FROM principals WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(db)
    .await?;
    Ok(principals_json(db, vec![row]).await?.remove(0))
}

pub async fn role_json(db: &PgPool, id: i64) -> Result<Value, sqlx::Error> {
    let row: RoleRow = sqlx::query_as(&format!("SELECT {ROLE_COLUMNS} FROM roles WHERE id = $1"))
        .bind(id)
        .fetch_one(db)
        .await?;
    Ok(roles_json(db, vec![row]).await?.remove(0))
}

// --- grants -----------------------------------------------------------------

pub const GRANT_COLUMNS: &str = "id, principal_id, role_id, static_secret_id, gcp_auth_secret_id, \
    gcp_id_token_secret_id, aws_auth_secret_id, oauth_token_secret_id, pg_dsn_secret_id, \
    hmac_secret_id, ssh_key_secret_id, \
    created_at AT TIME ZONE 'UTC' AS created_at, updated_at AT TIME ZONE 'UTC' AS updated_at";

#[derive(sqlx::FromRow)]
pub struct GrantRow {
    pub id: i64,
    pub principal_id: Option<i64>,
    pub role_id: Option<i64>,
    static_secret_id: Option<i64>,
    gcp_auth_secret_id: Option<i64>,
    gcp_id_token_secret_id: Option<i64>,
    aws_auth_secret_id: Option<i64>,
    oauth_token_secret_id: Option<i64>,
    pg_dsn_secret_id: Option<i64>,
    hmac_secret_id: Option<i64>,
    ssh_key_secret_id: Option<i64>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl GrantRow {
    /// The grantable this grant names, in kind order.
    pub fn grantable(&self) -> Option<(&'static SecretKind, i64)> {
        let ids = [
            self.static_secret_id,
            self.gcp_auth_secret_id,
            self.gcp_id_token_secret_id,
            self.aws_auth_secret_id,
            self.oauth_token_secret_id,
            self.pg_dsn_secret_id,
            self.hmac_secret_id,
            self.ssh_key_secret_id,
        ];
        SECRET_KINDS
            .iter()
            .zip(ids)
            .find_map(|(kind, id)| id.map(|id| (kind, id)))
    }
}

/// `(foreign_id, name)` of a grantable.
type Summary = (Option<String>, Option<String>);

/// Grants with the name and foreign id of what they grant (`grantable`).
pub async fn grants_json(db: &PgPool, rows: Vec<GrantRow>) -> Result<Vec<Value>, sqlx::Error> {
    let mut summaries: HashMap<(&'static str, i64), Summary> = HashMap::new();
    for kind in &SECRET_KINDS {
        let ids: Vec<i64> = rows
            .iter()
            .filter_map(GrantRow::grantable)
            .filter(|(k, _)| k.column == kind.column)
            .map(|(_, id)| id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        let found: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(&format!(
            "SELECT id, foreign_id, name FROM {} WHERE id = ANY($1)",
            kind.table
        ))
        .bind(&ids)
        .fetch_all(db)
        .await?;
        for (id, foreign_id, name) in found {
            summaries.insert((kind.column, id), (foreign_id, name));
        }
    }
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut out = serde_json::Map::new();
            out.insert("id".into(), oid("grant", row.id).into());
            if let Some(id) = row.principal_id {
                out.insert("principal_id".into(), oid("prn", id).into());
            } else if let Some(id) = row.role_id {
                out.insert("role_id".into(), oid("role", id).into());
            }
            if let Some((kind, id)) = row.grantable() {
                out.insert(kind.column.into(), oid(kind.prefix, id).into());
                let (foreign_id, name) = summaries
                    .get(&(kind.column, id))
                    .cloned()
                    .unwrap_or_default();
                out.insert(
                    "grantable".into(),
                    json!({
                        "kind": kind.kind,
                        "kind_label": kind.label,
                        "id": oid(kind.prefix, id),
                        "foreign_id": foreign_id,
                        "name": name,
                    }),
                );
            }
            out.insert("created_at".into(), json!(row.created_at));
            out.insert("updated_at".into(), json!(row.updated_at));
            Value::Object(out)
        })
        .collect())
}

pub async fn grant_json(db: &PgPool, id: i64) -> Result<Value, sqlx::Error> {
    let row: GrantRow =
        sqlx::query_as(&format!("SELECT {GRANT_COLUMNS} FROM grants WHERE id = $1"))
            .bind(id)
            .fetch_one(db)
            .await?;
    Ok(grants_json(db, vec![row]).await?.remove(0))
}
