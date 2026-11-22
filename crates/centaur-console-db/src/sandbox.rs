use sqlx::PgPool;

use crate::{Result, decode_oid};

/// The proxy fields sandbox entitlement JWTs are checked against.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ConsoleProxy {
    pub id: i64,
    pub name: String,
    pub principal_id: Option<i64>,
}

impl ConsoleProxy {
    /// Console only treats a proxy as authoritative while it carries a
    /// principal; an unassigned proxy can boot and wait for assignment.
    pub fn assigned(&self) -> bool {
        self.principal_id.is_some()
    }
}

/// The proxy named by `oid`, when it exists.
pub async fn find_console_proxy(pool: &PgPool, oid: &str) -> Result<Option<ConsoleProxy>> {
    let Some(id) = decode_oid("prx", oid) else {
        return Ok(None);
    };
    Ok(
        sqlx::query_as("SELECT id, name, principal_id FROM proxies WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// Whether `team_id` belongs to a workspace the Slack bot itself is in. Used
/// to keep the console API's sandbox-delegated authority inside the bot's home
/// workspace.
pub async fn slack_bot_home_team(pool: &PgPool, team_id: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM slack_bot_channels WHERE team_id = $1)")
            .bind(team_id)
            .fetch_one(pool)
            .await?,
    )
}
