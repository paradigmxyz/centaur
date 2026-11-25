//! The Slack channel catalog: the channels the Slack bot can see, and each
//! active channel's members. api-rs refreshes it from Slack; the Console's
//! channel pickers and access checks read it.

use sqlx::{PgPool, Row};

use crate::Result;

/// The bot identity a catalog refresh records on every channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogIdentity {
    pub team_id: String,
    pub bot_user_id: String,
}

/// A channel as Slack lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteChannel {
    pub id: String,
    pub name: String,
    pub private: bool,
    pub archived: bool,
}

/// A channel whose membership is due for a refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleMembership {
    pub id: i64,
    pub channel_id: String,
}

/// Replace the catalog with `channels`, all listed for `identity`: every
/// existing row is deactivated, then each listed channel is upserted and
/// reactivated unless archived. Membership columns are left alone.
pub async fn replace_catalog(
    pool: &PgPool,
    identity: &CatalogIdentity,
    channels: &[RemoteChannel],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "UPDATE slack_bot_channels SET active = false, updated_at = (now() AT TIME ZONE 'UTC')",
    )
    .execute(&mut *tx)
    .await?;
    for channel in channels {
        sqlx::query(
            "INSERT INTO slack_bot_channels \
               (team_id, channel_id, bot_user_id, name, private, archived, active, last_seen_at, \
                created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NOT $6, (now() AT TIME ZONE 'UTC'), \
                     (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC')) \
             ON CONFLICT (team_id, channel_id) DO UPDATE SET \
               bot_user_id = EXCLUDED.bot_user_id, name = EXCLUDED.name, \
               private = EXCLUDED.private, archived = EXCLUDED.archived, \
               active = EXCLUDED.active, last_seen_at = EXCLUDED.last_seen_at, \
               updated_at = EXCLUDED.updated_at",
        )
        .bind(&identity.team_id)
        .bind(&channel.id)
        .bind(&identity.bot_user_id)
        .bind(&channel.name)
        .bind(channel.private)
        .bind(channel.archived)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Whether the catalog has not been refreshed within `max_age_secs`, or never.
pub async fn catalog_is_stale(pool: &PgPool, max_age_secs: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(max(last_seen_at) < (now() AT TIME ZONE 'UTC') - make_interval(secs => $1), true) \
         FROM slack_bot_channels",
    )
    .bind(max_age_secs as f64)
    .fetch_one(pool)
    .await?)
}

/// Active channels whose members were last refreshed more than
/// `refreshed_before_secs` ago (or never), skipping those attempted within
/// `attempted_before_secs`; never-refreshed first, at most `limit`.
pub async fn stale_memberships(
    pool: &PgPool,
    refreshed_before_secs: i64,
    attempted_before_secs: i64,
    limit: i64,
) -> Result<Vec<StaleMembership>> {
    let rows = sqlx::query(
        "SELECT id, channel_id FROM slack_bot_channels \
         WHERE active \
           AND (membership_refreshed_at IS NULL \
                OR membership_refreshed_at < (now() AT TIME ZONE 'UTC') - make_interval(secs => $1)) \
           AND (membership_last_attempted_at IS NULL \
                OR membership_last_attempted_at < (now() AT TIME ZONE 'UTC') - make_interval(secs => $2)) \
         ORDER BY membership_refreshed_at ASC NULLS FIRST, id \
         LIMIT $3",
    )
    .bind(refreshed_before_secs as f64)
    .bind(attempted_before_secs as f64)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(StaleMembership {
                id: row.try_get("id")?,
                channel_id: row.try_get("channel_id")?,
            })
        })
        .collect()
}

/// Record that a membership refresh of channel `id` started.
pub async fn start_membership_attempt(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE slack_bot_channels SET membership_last_attempted_at = (now() AT TIME ZONE 'UTC'), \
           membership_error = NULL, updated_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Store channel `id`'s members (sorted, unique) as freshly refreshed.
pub async fn record_members(pool: &PgPool, id: i64, member_user_ids: &[String]) -> Result<()> {
    sqlx::query(
        "UPDATE slack_bot_channels SET member_user_ids = $2, \
           membership_refreshed_at = (now() AT TIME ZONE 'UTC'), membership_error = NULL, \
           updated_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
    )
    .bind(id)
    .bind(member_user_ids)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a failed membership refresh; the channel waits out the retry
/// interval before the next attempt.
pub async fn record_membership_error(pool: &PgPool, id: i64, message: &str) -> Result<()> {
    sqlx::query(
        "UPDATE slack_bot_channels SET membership_last_attempted_at = (now() AT TIME ZONE 'UTC'), \
           membership_error = $2, updated_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
    )
    .bind(id)
    .bind(message)
    .execute(pool)
    .await?;
    Ok(())
}

/// Forget an attempt Slack asked to back off from, so the next pass retries
/// channel `id`.
pub async fn forget_membership_attempt(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE slack_bot_channels SET membership_last_attempted_at = NULL, \
           updated_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
