use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::{ApiError, models::ProxyRecord, tokens::CODEMODE_SANDBOX_ID};

/// The principal attributes a built configuration depends on.
///
/// `{principal}` is the SQL expression for the principal id, so the same
/// definition serves a proxy (`p.principal_id`) and a principal without one
/// (`pr.id`).
fn principal_fields(principal: &str) -> String {
    format!(
        "pr.sync_config_cache_version AS principal_cache_version, \
         to_jsonb(pr) AS principal, u.email AS console_user_email, u.id AS console_user_id, \
         COALESCE(u.admin, false) AS console_user_admin, u.status AS console_user_status, \
         EXISTS ( \
             SELECT 1 FROM slack_bot_channels sb \
             WHERE sb.team_id = pr.slack_team_id \
         ) AS slack_bot_home_team, \
         EXISTS ( \
             SELECT 1 FROM grants rg JOIN static_secrets rs ON rs.id = rg.static_secret_id \
             JOIN broker_credentials rb ON rb.id = rs.broker_credential_id \
             JOIN oauth_apps ra ON ra.id = rb.oauth_app_id \
             WHERE rg.principal_id = {principal} AND ra.owner_only \
         ) AS has_owner_only_grant, \
         COALESCE(( \
             SELECT jsonb_agg(effective.channel_id ORDER BY effective.channel_id) \
             FROM ( \
                 SELECT permissions.channel_id \
                 FROM slack_channel_permissions permissions \
                 WHERE permissions.principal_id = {principal} \
                    OR permissions.role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = {principal}) \
                 GROUP BY permissions.channel_id \
                 HAVING bool_or(permissions.history_enabled) \
             ) effective \
         ), '[]'::jsonb) AS slack_history_channel_ids"
    )
}

pub(crate) async fn load_proxy(
    pool: &PgPool,
    token: &str,
) -> Result<Option<ProxyRecord>, ApiError> {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let sql = format!(
        "SELECT p.id, p.name, p.labels, p.principal_id, p.requester_principal_id, \
                p.principal_assigned_at AT TIME ZONE 'UTC' AS principal_assigned_at, \
                p.requester_principal_assigned_at AT TIME ZONE 'UTC' AS requester_principal_assigned_at, \
                {fields} \
         FROM proxies p \
         LEFT JOIN principals pr ON pr.id = p.principal_id \
         LEFT JOIN users u ON u.id = pr.console_user_id \
         WHERE p.bearer_token_hash = $1",
        fields = principal_fields("p.principal_id"),
    );
    let row = sqlx::query(&sql)
        .bind(token_hash)
        .fetch_optional(pool)
        .await
        .map_err(centaur_console_db::Error::Database)?;
    Ok(row.map(|row| {
        let mut record = ProxyRecord {
            id: row.get("id"),
            name: row.get("name"),
            labels: row.get("labels"),
            principal_id: row.get("principal_id"),
            requester_principal_id: row.get("requester_principal_id"),
            principal_assigned_at: row.get("principal_assigned_at"),
            requester_principal_assigned_at: row.get("requester_principal_assigned_at"),
            principal_cache_version: None,
            principal: None,
            console_user_email: None,
            console_user_id: None,
            console_user_admin: false,
            console_user_status: None,
            slack_bot_home_team: false,
            slack_history_channel_ids: json!([]),
            has_owner_only_grant: false,
        };
        apply_principal_fields(&row, &mut record);
        record
    }))
}

/// The configuration inputs for a principal with no proxy row, as the Code
/// Mode tenant route needs them. The synthetic record keeps the sandbox name
/// and an empty label set; the principal itself is re-read live.
pub(crate) async fn load_principal_record(
    pool: &PgPool,
    principal_id: i64,
    requester_principal_id: Option<i64>,
) -> Result<Option<ProxyRecord>, ApiError> {
    let sql = format!(
        "SELECT {fields} \
         FROM principals pr \
         LEFT JOIN users u ON u.id = pr.console_user_id \
         WHERE pr.id = $1",
        fields = principal_fields("pr.id"),
    );
    let row = sqlx::query(&sql)
        .bind(principal_id)
        .fetch_optional(pool)
        .await
        .map_err(centaur_console_db::Error::Database)?;
    Ok(row.map(|row| {
        let mut record = ProxyRecord {
            id: 0,
            name: CODEMODE_SANDBOX_ID.to_owned(),
            labels: json!({}),
            principal_id: Some(principal_id),
            requester_principal_id,
            principal_assigned_at: None,
            requester_principal_assigned_at: None,
            principal_cache_version: None,
            principal: None,
            console_user_email: None,
            console_user_id: None,
            console_user_admin: false,
            console_user_status: None,
            slack_bot_home_team: false,
            slack_history_channel_ids: json!([]),
            has_owner_only_grant: false,
        };
        apply_principal_fields(&row, &mut record);
        record
    }))
}

fn apply_principal_fields(row: &PgRow, record: &mut ProxyRecord) {
    record.principal_cache_version = row.get("principal_cache_version");
    record.principal = row.get("principal");
    record.console_user_email = row.get("console_user_email");
    record.console_user_id = row.get("console_user_id");
    record.console_user_admin = row.get("console_user_admin");
    record.console_user_status = row.get("console_user_status");
    record.slack_bot_home_team = row.get("slack_bot_home_team");
    record.slack_history_channel_ids = row.get("slack_history_channel_ids");
    record.has_owner_only_grant = row.get("has_owner_only_grant");
}
