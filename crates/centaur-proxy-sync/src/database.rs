use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::{ApiError, models::ProxyRecord};

pub(crate) async fn load_proxy(
    pool: &PgPool,
    token: &str,
) -> Result<Option<ProxyRecord>, ApiError> {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let row = sqlx::query(
        "SELECT p.id, p.name, p.labels, p.principal_id, p.requester_principal_id, \
                p.principal_assigned_at AT TIME ZONE 'UTC' AS principal_assigned_at, \
                p.requester_principal_assigned_at AT TIME ZONE 'UTC' AS requester_principal_assigned_at, \
                pr.sync_config_cache_version AS principal_cache_version, \
                to_jsonb(pr) AS principal, u.email AS console_user_email, u.id AS console_user_id, \
                COALESCE(( \
                    SELECT jsonb_agg(effective.channel_id ORDER BY effective.channel_id) \
                    FROM ( \
                        SELECT permissions.channel_id \
                        FROM slack_channel_permissions permissions \
                        WHERE permissions.principal_id = p.principal_id \
                           OR permissions.role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = p.principal_id) \
                        GROUP BY permissions.channel_id \
                        HAVING bool_or(permissions.history_enabled) \
                    ) effective \
                ), '[]'::jsonb) AS slack_history_channel_ids \
         FROM proxies p \
         LEFT JOIN principals pr ON pr.id = p.principal_id \
         LEFT JOIN users u ON u.id = pr.console_user_id \
         WHERE p.bearer_token_hash = $1",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
    .map_err(centaur_console_db::Error::Database)?;
    Ok(row.map(|row| ProxyRecord {
        id: row.get("id"),
        name: row.get("name"),
        labels: row.get("labels"),
        principal_id: row.get("principal_id"),
        requester_principal_id: row.get("requester_principal_id"),
        principal_assigned_at: row.get("principal_assigned_at"),
        requester_principal_assigned_at: row.get("requester_principal_assigned_at"),
        principal_cache_version: row.get("principal_cache_version"),
        principal: row.get("principal"),
        console_user_email: row.get("console_user_email"),
        console_user_id: row.get("console_user_id"),
        slack_history_channel_ids: row.get("slack_history_channel_ids"),
    }))
}
