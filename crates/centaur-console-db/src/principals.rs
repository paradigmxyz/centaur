use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::{
    Result,
    config::{SecretValues, Subject, build_config},
    models::Config,
};

const PRINCIPAL_COLUMNS: &str = "id, foreign_id, name, kind, labels, slack_user_id, \
     slack_channel_id, slack_team_id, slack_email, console_user_id, sandbox_repo_cache, \
     sandbox_observability_enabled";

/// A principal row, with the attributes callers compare and report.
#[derive(Clone, Debug, PartialEq)]
pub struct Principal {
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
}

/// A Slack channel permission granted directly to a principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlackChannelPermission {
    pub upload_enabled: bool,
    pub download_enabled: bool,
    pub history_enabled: bool,
}

pub async fn find_principal(pool: &PgPool, id: i64) -> Result<Option<Principal>> {
    let row = sqlx::query(&format!(
        "SELECT {PRINCIPAL_COLUMNS} FROM principals WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(principal).transpose()
}

pub async fn find_principal_by_foreign_id(
    pool: &PgPool,
    foreign_id: &str,
) -> Result<Option<Principal>> {
    let row = sqlx::query(&format!(
        "SELECT {PRINCIPAL_COLUMNS} FROM principals WHERE foreign_id = $1"
    ))
    .bind(foreign_id)
    .fetch_optional(pool)
    .await?;
    row.as_ref().map(principal).transpose()
}

/// The Console user whose email is exactly `email`.
pub async fn find_user_id_by_email(pool: &PgPool, email: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await?)
}

pub async fn find_direct_slack_channel_permission(
    pool: &PgPool,
    principal_id: i64,
    channel_id: &str,
) -> Result<Option<SlackChannelPermission>> {
    let row = sqlx::query(
        "SELECT upload_enabled, download_enabled, history_enabled \
         FROM slack_channel_permissions WHERE principal_id = $1 AND channel_id = $2",
    )
    .bind(principal_id)
    .bind(channel_id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(SlackChannelPermission {
            upload_enabled: row.try_get("upload_enabled")?,
            download_enabled: row.try_get("download_enabled")?,
            history_enabled: row.try_get("history_enabled")?,
        })
    })
    .transpose()
}

/// The principal's own configuration with every secret value redacted: what
/// its proxy is served, before requester credentials and minted tokens.
/// `None` when the principal does not exist.
pub async fn effective_config(pool: &PgPool, principal_id: i64) -> Result<Option<Config>> {
    let Some(row) = sqlx::query(
        "SELECT to_jsonb(pr) AS principal, u.id AS console_user_id, u.email AS console_user_email, \
                COALESCE(( \
                    SELECT jsonb_agg(effective.channel_id ORDER BY effective.channel_id) \
                    FROM ( \
                        SELECT permissions.channel_id \
                        FROM slack_channel_permissions permissions \
                        WHERE permissions.principal_id = pr.id \
                           OR permissions.role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = pr.id) \
                        GROUP BY permissions.channel_id \
                        HAVING bool_or(permissions.history_enabled) \
                    ) effective \
                ), '[]'::jsonb) AS slack_history_channel_ids \
         FROM principals pr \
         LEFT JOIN users u ON u.id = pr.console_user_id \
         WHERE pr.id = $1",
    )
    .bind(principal_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let principal: Value = row.try_get("principal")?;
    let console_user_email: Option<String> = row.try_get("console_user_email")?;
    let slack_history_channel_ids: Value = row.try_get("slack_history_channel_ids")?;
    let subject = Subject {
        principal_id,
        principal: Some(&principal),
        console_user_id: row.try_get("console_user_id")?,
        console_user_email: console_user_email.as_deref(),
        slack_history_channel_ids: &slack_history_channel_ids,
        proxy_labels: &Value::Object(Default::default()),
    };
    build_config(pool, SecretValues::Redacted, &subject, None)
        .await
        .map(Some)
}

fn principal(row: &PgRow) -> Result<Principal> {
    Ok(Principal {
        id: row.try_get("id")?,
        foreign_id: row.try_get("foreign_id")?,
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        labels: row.try_get("labels")?,
        slack_user_id: row.try_get("slack_user_id")?,
        slack_channel_id: row.try_get("slack_channel_id")?,
        slack_team_id: row.try_get("slack_team_id")?,
        slack_email: row.try_get("slack_email")?,
        console_user_id: row.try_get("console_user_id")?,
        sandbox_repo_cache: row.try_get("sandbox_repo_cache")?,
        sandbox_observability_enabled: row.try_get("sandbox_observability_enabled")?,
    })
}
