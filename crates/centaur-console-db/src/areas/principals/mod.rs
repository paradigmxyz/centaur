//! Console's principals area: principals, roles, grants, role assignments,
//! and Slack channel permissions.
//!
//! Side-effect writes (default roles, sync-config cache bumps, credential
//! reconciliation) happen here inside the same transaction as the change that
//! causes them.
//!
//! Rows derive [`sqlx::FromRow`] and are read with `query_as`; lookups go
//! through [`crate::model`] descriptors, timestamps render through
//! [`crate::timestamp`], and validation collects into [`crate::FieldErrors`].

mod grant;
mod read;
mod request;
mod rules;
mod write;

pub use crate::areas::secrets::Grantee;
pub use grant::{
    EffectiveGrantPayload, GrantPayload, GrantableSummary, GrantableSummaryPayload,
    console_grant_payload, console_show_grant, create_grant, destroy_grant, effective_grants,
    grant_payload, list_grants, show_grant,
};
pub use read::{
    ConsolePrincipalPayload, PrincipalPayload, PrincipalView, RolePayload, RoleView,
    SlackChannelOption, SlackChannelOptionPayload, console_principal_payload,
    find_principal_by_foreign_id, find_principal_by_oid, find_role_by_foreign_id, find_role_by_oid,
    load_principal_view, load_role_view, permission_channel_ids, principal_payload,
    principal_roles, role_payload, slack_catalog_configured, slack_catalog_error,
    slack_channel_option, slack_channels,
};
pub use request::{
    AssignRole, GrantWrite, PermissionSetWrite, PermissionWrite, PrincipalWrite, RoleWrite,
};
pub use write::{
    assign_role, create_principal, create_role, delete_principal, delete_role,
    destroy_slack_channel_permission, principal_roles_payload, remove_role,
    replace_slack_channel_permissions, upsert_principal, upsert_role,
    upsert_slack_channel_permission,
};

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use time::PrimitiveDateTime;

use crate::FieldErrors;
use crate::areas::secrets::SecretKind;
use crate::model::ConsoleModel;

/// A failure in the principals area, carrying the HTTP meaning a route renders.
#[derive(Debug)]
pub enum PrincipalError {
    NotFound(String),
    BadRequest(String),
    /// Validation messages rendered under `details`.
    Validation(FieldErrors),
    Internal(String),
    Database(sqlx::Error),
    Encryption(active_record_encryption::Error),
}

impl From<sqlx::Error> for PrincipalError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<active_record_encryption::Error> for PrincipalError {
    fn from(error: active_record_encryption::Error) -> Self {
        Self::Encryption(error)
    }
}

impl From<FieldErrors> for PrincipalError {
    fn from(errors: FieldErrors) -> Self {
        Self::Validation(errors)
    }
}

impl From<crate::Error> for PrincipalError {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Database(error) => Self::Database(error),
            crate::Error::Encryption(error) => Self::Encryption(error),
            crate::Error::InvalidData(message) => Self::Internal(message),
            crate::Error::NotFound(message) => Self::NotFound(message),
        }
    }
}

pub type Result<T> = std::result::Result<T, PrincipalError>;

pub const PRINCIPAL_COLUMNS: &str = "id, foreign_id, name, kind, labels, slack_user_id, \
     slack_channel_id, slack_team_id, slack_email, console_user_id, sandbox_repo_cache, \
     sandbox_observability_enabled, sandbox_sessions_read_enabled, \
     sandbox_workflows_read_enabled, sandbox_workflows_write_enabled, sync_config_cache_version, \
     created_by_id, created_at, updated_at";

pub const ROLE_COLUMNS: &str = "id, foreign_id, name, labels, assign_by_default, \
     created_by_id, created_at, updated_at";

pub(crate) const PERMISSION_COLUMNS: &str = "id, principal_id, role_id, channel_id, \
     upload_enabled, download_enabled, history_enabled, created_at, updated_at";

pub const GRANT_COLUMNS: &str = "id, principal_id, role_id, static_secret_id, \
     gcp_auth_secret_id, gcp_id_token_secret_id, aws_auth_secret_id, oauth_token_secret_id, \
     pg_dsn_secret_id, hmac_secret_id, ssh_key_secret_id, priority, created_by_id, created_at, \
     updated_at";

pub(crate) const PRINCIPAL_KINDS: &[&str] = &[
    "unknown",
    "user",
    "console_user",
    "workflow",
    "slack_channel",
    "slack_dm",
    "discord_channel",
    "linear_issue",
    "github_user",
    "teams_user",
    "teams_conversation",
];

/// The model's opaque-id prefixes, which a `foreign_id` must not shadow.
pub(crate) const PRINCIPAL_RESERVED_PREFIX: &str = "prn_";
pub(crate) const ROLE_RESERVED_PREFIX: &str = "role_";

pub(crate) const SANDBOX_REPO_CACHE_VALUES: &[&str] = &["none", "public", "all"];

pub(crate) const SANDBOX_REPO_CACHE_LABEL: &str = "centaur.sandbox_repo_cache";

pub(crate) const PERMISSION_ATTRIBUTES: [&str; 3] =
    ["upload_enabled", "download_enabled", "history_enabled"];

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PrincipalRow {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub kind: String,
    pub labels: Value,
    pub slack_user_id: Option<String>,
    pub slack_channel_id: Option<String>,
    pub slack_team_id: Option<String>,
    pub slack_email: Option<String>,
    pub console_user_id: Option<i64>,
    pub sandbox_repo_cache: String,
    pub sandbox_observability_enabled: bool,
    pub sandbox_sessions_read_enabled: bool,
    pub sandbox_workflows_read_enabled: bool,
    pub sandbox_workflows_write_enabled: bool,
    pub sync_config_cache_version: i64,
    pub created_by_id: i64,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct RoleRow {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub labels: Value,
    pub assign_by_default: bool,
    pub created_by_id: i64,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PermissionRow {
    pub id: i64,
    pub principal_id: Option<i64>,
    pub role_id: Option<i64>,
    pub channel_id: String,
    pub upload_enabled: bool,
    pub download_enabled: bool,
    pub history_enabled: bool,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct GrantRow {
    pub id: i64,
    pub principal_id: Option<i64>,
    pub role_id: Option<i64>,
    pub static_secret_id: Option<i64>,
    pub gcp_auth_secret_id: Option<i64>,
    pub gcp_id_token_secret_id: Option<i64>,
    pub aws_auth_secret_id: Option<i64>,
    pub oauth_token_secret_id: Option<i64>,
    pub pg_dsn_secret_id: Option<i64>,
    pub hmac_secret_id: Option<i64>,
    pub ssh_key_secret_id: Option<i64>,
    pub priority: i32,
    pub created_by_id: i64,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

impl GrantRow {
    /// The first non-null grantable column, in the model's association order.
    pub fn grantable(&self) -> Option<(SecretKind, i64)> {
        SecretKind::ALL
            .into_iter()
            .find_map(|kind| kind.id_in(self).map(|id| (kind, id)))
    }

    pub fn grantee(&self) -> Option<Grantee> {
        if let Some(id) = self.principal_id {
            return Some(Grantee::Principal(id));
        }
        self.role_id.map(Grantee::Role)
    }
}

/// One Slack channel permission as the API renders it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PermissionPayload {
    pub channel_id: String,
    pub upload_enabled: bool,
    pub download_enabled: bool,
    pub history_enabled: bool,
}

impl From<&PermissionRow> for PermissionPayload {
    fn from(row: &PermissionRow) -> Self {
        Self {
            channel_id: row.channel_id.clone(),
            upload_enabled: row.upload_enabled,
            download_enabled: row.download_enabled,
            history_enabled: row.history_enabled,
        }
    }
}

/// A role as the list, roles endpoint, and grant payloads render it.
#[derive(Clone, Debug, Serialize)]
pub struct RoleSummaryPayload {
    pub id: String,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
}

/// Merge permission rows by channel, direct permissions first, OR-ing flags.
pub(crate) fn merge_permissions(rows: &[PermissionRow]) -> Vec<PermissionPayload> {
    let mut ordered: Vec<&PermissionRow> = rows.iter().collect();
    ordered.sort_by_key(|row| {
        (
            if row.principal_id.is_some() { 0 } else { 1 },
            row.role_id.unwrap_or(0),
            row.id,
        )
    });
    let mut by_channel: BTreeMap<String, [bool; 3]> = BTreeMap::new();
    for row in ordered {
        let channel = row.channel_id.trim().to_uppercase();
        let entry = by_channel.entry(channel).or_insert([false; 3]);
        entry[0] |= row.upload_enabled;
        entry[1] |= row.download_enabled;
        entry[2] |= row.history_enabled;
    }
    by_channel
        .into_iter()
        .map(|(channel, flags)| PermissionPayload {
            channel_id: channel,
            upload_enabled: flags[0],
            download_enabled: flags[1],
            history_enabled: flags[2],
        })
        .collect()
}

pub(crate) fn role_summary(row: &RoleRow) -> RoleSummaryPayload {
    RoleSummaryPayload {
        id: crate::model::Role::oid(row.id),
        foreign_id: row.foreign_id.clone(),
        name: row.name.clone(),
    }
}

/// The distinct principal ids reachable through a grantee's grants.
pub(crate) async fn grantee_principals(
    conn: &mut sqlx::PgConnection,
    principal_id: Option<i64>,
    role_id: Option<i64>,
) -> sqlx::Result<Vec<i64>> {
    if let Some(id) = principal_id {
        return Ok(vec![id]);
    }
    let Some(role_id) = role_id else {
        return Ok(Vec::new());
    };
    sqlx::query_scalar("SELECT principal_id FROM principal_roles WHERE role_id = $1")
        .bind(role_id)
        .fetch_all(conn)
        .await
}

/// The singleton row a principal write needs, without creating it.
pub(crate) async fn principal_defaults(
    conn: &mut sqlx::PgConnection,
) -> Result<PrincipalRowDefaults> {
    let row: Option<PrincipalDefaultsRow> = sqlx::query_as(
        "SELECT default_sandbox_repo_cache, default_sandbox_observability_enabled, \
                default_sandbox_sessions_read_enabled, default_sandbox_workflows_read_enabled, \
                default_sandbox_workflows_write_enabled \
         FROM system_settings ORDER BY id LIMIT 1",
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(PrincipalRowDefaults::from).unwrap_or_default())
}

#[derive(sqlx::FromRow)]
struct PrincipalDefaultsRow {
    default_sandbox_repo_cache: String,
    default_sandbox_observability_enabled: bool,
    default_sandbox_sessions_read_enabled: bool,
    default_sandbox_workflows_read_enabled: bool,
    default_sandbox_workflows_write_enabled: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PrincipalRowDefaults {
    pub sandbox_repo_cache: String,
    pub sandbox_observability_enabled: bool,
    pub sandbox_sessions_read_enabled: bool,
    pub sandbox_workflows_read_enabled: bool,
    pub sandbox_workflows_write_enabled: bool,
}

impl From<PrincipalDefaultsRow> for PrincipalRowDefaults {
    fn from(row: PrincipalDefaultsRow) -> Self {
        Self {
            sandbox_repo_cache: row.default_sandbox_repo_cache,
            sandbox_observability_enabled: row.default_sandbox_observability_enabled,
            sandbox_sessions_read_enabled: row.default_sandbox_sessions_read_enabled,
            sandbox_workflows_read_enabled: row.default_sandbox_workflows_read_enabled,
            sandbox_workflows_write_enabled: row.default_sandbox_workflows_write_enabled,
        }
    }
}

impl Default for PrincipalRowDefaults {
    fn default() -> Self {
        Self {
            sandbox_repo_cache: "all".to_owned(),
            sandbox_observability_enabled: true,
            sandbox_sessions_read_enabled: false,
            sandbox_workflows_read_enabled: false,
            sandbox_workflows_write_enabled: false,
        }
    }
}

/// An ILIKE pattern that escapes `\`, `%`, and `_` in `query`.
pub fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for character in query.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_pattern_escapes_every_ilike_metacharacter() {
        assert_eq!(like_pattern("100%_done\\"), "%100\\%\\_done\\\\%");
        assert_eq!(like_pattern("diff-channel"), "%diff-channel%");
    }
}
