//! Where a console user's scheduled-task output may go (`SlackDeliveryPolicy`):
//! their own Slack DM, public channels, and private channels both they and
//! the bot are in.

use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

/// A user's resolved Slack identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlackIdentity {
    pub user_id: String,
    pub team_id: String,
}

fn upper_alnum(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn slack_user_id(value: &str) -> bool {
    value == "USLACK"
        || (value.len() >= 9 && value.starts_with(['U', 'W']) && upper_alnum(&value[1..]))
}

fn slack_team_id(value: &str) -> bool {
    value.len() >= 9 && value.starts_with(['T', 'E']) && upper_alnum(&value[1..])
}

/// A Slack channel or user id a task may deliver to.
pub fn valid_destination(value: &str) -> bool {
    value.len() >= 9 && value.starts_with(['C', 'D', 'G', 'U', 'W']) && upper_alnum(&value[1..])
}

/// The one well-formed identity among the user's Slack sign-ins, their
/// console-user principals, and their Slack broker credentials; `None` when
/// there is none or several.
pub async fn slack_identity(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<SlackIdentity>, sqlx::Error> {
    let candidates: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT subject, team FROM ( \
           SELECT subject, team_id AS team FROM user_identities \
             WHERE user_id = $1 AND provider = 'slack' \
           UNION ALL \
           SELECT slack_user_id, slack_team_id FROM principals \
             WHERE kind = 'console_user' AND console_user_id = $1 \
           UNION ALL \
           SELECT bc.provider_subject, \
                  COALESCE(NULLIF(bc.labels ->> 'slack_team_id', ''), \
                           NULLIF(oa.labels ->> 'slack_team_id', '')) \
             FROM broker_credentials bc JOIN oauth_apps oa ON oa.id = bc.oauth_app_id \
             WHERE bc.created_by_id = $1 AND oa.provider = 'slack') identities",
    )
    .bind(user_id)
    .fetch_all(conn)
    .await?;
    let mut valid = candidates.into_iter().filter_map(|(user, team)| {
        let (user_id, team_id) = (user?, team?);
        (slack_user_id(&user_id) && slack_team_id(&team_id))
            .then_some(SlackIdentity { user_id, team_id })
    });
    Ok(match (valid.next(), valid.next()) {
        (Some(identity), None) => Some(identity),
        _ => None,
    })
}

/// Channels the bot is in that `identity` may deliver to: active channels
/// of the identity's workspace, private ones only when the user and the bot
/// are both members.
const ALLOWED: &str = "active AND NOT archived AND ($1::text IS NULL OR team_id = $1) \
     AND (NOT private OR ($2::text IS NOT NULL AND member_user_ids @> ARRAY[bot_user_id, $2]::text[]))";

fn bind_identity(identity: Option<&SlackIdentity>) -> (Option<&str>, Option<&str>) {
    (
        identity.map(|identity| identity.team_id.as_str()),
        identity.map(|identity| identity.user_id.as_str()),
    )
}

/// Whether `destination` is the user's DM or an allowed channel.
pub async fn allowed(
    conn: &mut PgConnection,
    identity: Option<&SlackIdentity>,
    destination: &str,
) -> Result<bool, sqlx::Error> {
    let destination = destination.trim().to_uppercase();
    if identity.is_some_and(|identity| identity.user_id == destination) {
        return Ok(true);
    }
    let (team, user) = bind_identity(identity);
    sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM slack_bot_channels WHERE {ALLOWED} AND channel_id = $3)"
    ))
    .bind(team)
    .bind(user)
    .bind(destination)
    .fetch_one(conn)
    .await
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ChannelOption {
    pub channel_id: String,
    pub name: String,
    pub private: bool,
}

/// Allowed channels whose name or id contains `query`, by name.
pub async fn allowed_channels(
    conn: &mut PgConnection,
    identity: Option<&SlackIdentity>,
    query: &str,
    limit: i64,
) -> Result<Vec<ChannelOption>, sqlx::Error> {
    let (team, user) = bind_identity(identity);
    let needle = query.trim();
    let pattern = (!needle.is_empty()).then(|| {
        let escaped = needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    });
    sqlx::query_as(&format!(
        "SELECT channel_id, name, private FROM slack_bot_channels WHERE {ALLOWED} \
         AND ($3::text IS NULL OR name ILIKE $3 OR channel_id ILIKE $3) \
         ORDER BY lower(name), channel_id LIMIT $4"
    ))
    .bind(team)
    .bind(user)
    .bind(pattern)
    .bind(limit)
    .fetch_all(conn)
    .await
}

/// Whether the bot is in `channel_id` (of `team_id`, when known): the
/// destination check of a Slack channel principal's task.
pub async fn active_channel_exists<'e>(
    db: impl PgExecutor<'e>,
    team_id: Option<&str>,
    channel_id: &str,
) -> Result<bool, sqlx::Error> {
    let team = team_id.filter(|team| !team.trim().is_empty());
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM slack_bot_channels WHERE active AND NOT archived \
         AND ($1::text IS NULL OR team_id = $1) AND channel_id = $2)",
    )
    .bind(team)
    .bind(channel_id)
    .fetch_one(db)
    .await
}

/// Why the channel picker may be empty: the catalog is loading.
pub async fn catalog_empty<'e>(db: impl PgExecutor<'e>) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM slack_bot_channels)")
        .fetch_one(db)
        .await
}

/// Private channels of `team_id` whose member list was never fetched.
pub async fn memberships_loading<'e>(
    db: impl PgExecutor<'e>,
    team_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM slack_bot_channels WHERE active AND NOT archived \
         AND team_id = $1 AND private AND membership_refreshed_at IS NULL)",
    )
    .bind(team_id)
    .fetch_one(db)
    .await
}

/// `(channel_id, name)` of every known channel in `ids`.
pub async fn channel_names<'e>(
    db: impl PgExecutor<'e>,
    ids: &[String],
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT channel_id, name FROM slack_bot_channels WHERE channel_id = ANY($1)")
        .bind(ids)
        .fetch_all(db)
        .await
}
