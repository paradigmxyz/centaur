//! The Slack channel catalog: the channels the bot can see and who is in
//! them, kept in `slack_bot_channels` by two jobs the `console worker` runs
//! periodically (discovery hourly, membership every ten minutes) and the
//! request path starts on demand.
//!
//! Each job runs under a Postgres advisory lock so one replica works it at
//! a time. A retryable Slack failure (rate limit, transient error) is
//! retried in place after Slack's delay, up to three executions.

use std::{future::Future, time::Duration};

use centaur_console_db::Clock;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{AppState, env::Env};

pub const DEFAULT_API_URL: &str = "https://slack.com/api";
const CHANNEL_TYPES: &str = "public_channel,private_channel";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RATE_LIMIT_WAIT: f64 = 300.0;
const TRANSIENT_RETRY_AFTER: f64 = 30.0;
const MAX_EXECUTIONS: u32 = 3;
const MEMBERSHIP_BATCH: i64 = 100;

pub const REFRESH_LOOP: &str = "slack_channel_catalog_refresh";
pub const MEMBERSHIP_LOOP: &str = "slack_channel_catalog_membership";
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const MEMBERSHIP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// The bot token, from `CENTAUR_CONSOLE_SLACK_BOT_TOKEN` or `SLACK_BOT_TOKEN`.
fn token(env: &Env) -> Option<String> {
    env.var_present("CENTAUR_CONSOLE_SLACK_BOT_TOKEN")
        .or_else(|| env.var_present("SLACK_BOT_TOKEN"))
        .map(str::to_owned)
}

/// Whether a bot token is configured.
pub fn configured(env: &Env) -> bool {
    token(env).is_some()
}

/// Why a catalog operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// Slack answered with an error. `retry_after` is set for a rate limit
    /// or transient failure worth retrying.
    #[error("{message}")]
    Slack {
        message: String,
        code: Option<String>,
        retry_after: Option<f64>,
    },
    #[error("Slack request failed: {0}")]
    Http(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Slack channel {0} is not in the catalog")]
    ChannelMissing(String),
}

impl SyncError {
    fn api(message: String) -> Self {
        Self::Slack {
            message,
            code: None,
            retry_after: None,
        }
    }

    fn retry_after(&self) -> Option<f64> {
        match self {
            Self::Slack { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    fn code(&self) -> Option<&str> {
        match self {
            Self::Slack { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

/// A channel as `conversations.list` / `conversations.info` describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteChannel {
    id: String,
    name: String,
    private: bool,
    archived: bool,
}

/// Channels (`C…`) and private groups (`G…`); not DMs.
fn parse_channel(channel: &Value) -> Option<RemoteChannel> {
    let id = channel.get("id")?.as_str()?.trim();
    let name = channel
        .get("name_normalized")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .or_else(|| channel.get("name").and_then(Value::as_str))?
        .trim();
    if id.is_empty() || name.is_empty() || !(id.starts_with('C') || id.starts_with('G')) {
        return None;
    }
    let flag = |key: &str| channel.get(key).and_then(Value::as_bool).unwrap_or(false);
    Some(RemoteChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        private: flag("is_private"),
        archived: flag("is_archived"),
    })
}

/// The Slack Web API calls and catalog writes. Cheap to clone.
#[derive(Clone)]
pub struct Catalog {
    db: PgPool,
    clock: Clock,
    http: reqwest::Client,
    token: Option<String>,
    api_url: String,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("api_url", &self.api_url)
            .finish_non_exhaustive()
    }
}

impl Catalog {
    pub fn new(db: PgPool, clock: Clock, http: reqwest::Client, env: &Env) -> Self {
        Self {
            db,
            clock,
            http,
            token: token(env),
            api_url: env
                .var_present("SLACK_API_URL")
                .unwrap_or(DEFAULT_API_URL)
                .trim_end_matches('/')
                .to_owned(),
        }
    }

    pub fn from_state(state: &AppState) -> Self {
        Self::new(
            state.db.clone(),
            state.clock.clone(),
            state.http.clone(),
            &state.config.env,
        )
    }

    /// For callers outside the v2 state (builds its own HTTP client).
    pub fn standalone(db: PgPool, clock: Clock, env: &Env) -> Option<Self> {
        match crate::http::client(env) {
            Ok(http) => Some(Self::new(db, clock, http, env)),
            Err(error) => {
                tracing::warn!(%error, "Slack catalog needs an HTTP client");
                None
            }
        }
    }

    pub fn configured(&self) -> bool {
        self.token.is_some()
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, SyncError> {
        let token = self
            .token
            .as_deref()
            .ok_or_else(|| SyncError::api("SLACK_BOT_TOKEN is not configured.".into()))?;
        let mut url = url::Url::parse(&format!("{}/{method}", self.api_url))
            .map_err(|_| SyncError::api("SLACK_API_URL is not a valid URL.".into()))?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().filter(|(_, value)| !value.trim().is_empty()));
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| SyncError::Http(e.without_url().to_string()))?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v > 0.0 && v.is_finite())
            .unwrap_or(1.0)
            .min(MAX_RATE_LIMIT_WAIT);
        let body = response
            .bytes()
            .await
            .map_err(|e| SyncError::Http(e.without_url().to_string()))?;
        parse_response(status.as_u16(), retry_after, &body, method)
    }

    /// Every item under `key` across the pages of a cursor-paginated call.
    async fn pages(
        &self,
        method: &str,
        key: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<Value>, SyncError> {
        let mut items = Vec::new();
        let mut cursor = String::new();
        loop {
            let mut page = params.to_vec();
            page.push(("cursor", &cursor));
            let body = self.call(method, &page).await?;
            if let Some(Value::Array(rows)) = body.get(key) {
                items.extend(rows.iter().cloned());
            }
            cursor = body
                .pointer("/response_metadata/next_cursor")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            if cursor.is_empty() {
                return Ok(items);
            }
        }
    }

    /// `(team id, bot user id)` of the token.
    async fn identity(&self) -> Result<(String, String), SyncError> {
        let body = self.call("auth.test", &[]).await?;
        let field = |key: &str, what: &str| {
            body.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| SyncError::api(format!("Slack auth.test did not return a {what}.")))
        };
        Ok((
            field("team_id", "team ID")?,
            field("user_id", "bot user ID")?,
        ))
    }

    async fn catalog_identity(&self) -> Result<(String, String), SyncError> {
        let known: Option<(String, String)> =
            sqlx::query_as("SELECT team_id, bot_user_id FROM slack_bot_channels LIMIT 1")
                .fetch_optional(&self.db)
                .await?;
        match known {
            Some(identity) => Ok(identity),
            None => self.identity().await,
        }
    }

    /// Replaces the catalog with Slack's channel list (channels Slack no
    /// longer lists become inactive); the number of channels.
    pub async fn sync_channels(&self) -> Result<usize, SyncError> {
        let (team_id, bot_user_id) = self.identity().await?;
        let rows = self
            .pages(
                "conversations.list",
                "channels",
                &[
                    ("types", CHANNEL_TYPES),
                    ("exclude_archived", "false"),
                    ("limit", "200"),
                ],
            )
            .await?;
        let channels: Vec<RemoteChannel> = rows.iter().filter_map(parse_channel).collect();
        let now = self.now().naive_utc();
        let mut tx = self.db.begin().await?;
        sqlx::query("UPDATE slack_bot_channels SET active = FALSE, updated_at = $1")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        for channel in &channels {
            upsert(&mut tx, &team_id, &bot_user_id, channel, now).await?;
        }
        tx.commit().await?;
        Ok(channels.len())
    }

    /// Refreshes one channel from `conversations.info`; a channel the bot
    /// cannot see is deactivated (`false`).
    async fn import_channel(&self, channel_id: &str) -> Result<bool, SyncError> {
        let (team_id, bot_user_id) = self.catalog_identity().await?;
        let now = self.now().naive_utc();
        let info = self
            .call("conversations.info", &[("channel", channel_id)])
            .await;
        match info {
            Ok(body) => {
                let channel = body.get("channel").and_then(parse_channel).ok_or_else(|| {
                    SyncError::api(format!(
                        "Slack conversations.info did not return channel {channel_id}."
                    ))
                })?;
                let mut conn = self.db.acquire().await?;
                upsert(&mut conn, &team_id, &bot_user_id, &channel, now).await?;
                Ok(true)
            }
            Err(error) if error.code() == Some("channel_not_found") => {
                sqlx::query(
                    "UPDATE slack_bot_channels SET active = FALSE, updated_at = $3 \
                     WHERE team_id = $1 AND channel_id = $2",
                )
                .bind(&team_id)
                .bind(channel_id)
                .bind(now)
                .execute(&self.db)
                .await?;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Stores the complete member list of one channel.
    async fn channel_members(&self, channel_id: &str) -> Result<(), SyncError> {
        let attempted = sqlx::query(
            "UPDATE slack_bot_channels SET membership_last_attempted_at = $2, \
             membership_error = NULL, updated_at = $2 WHERE channel_id = $1",
        )
        .bind(channel_id)
        .bind(self.now().naive_utc())
        .execute(&self.db)
        .await?;
        if attempted.rows_affected() == 0 {
            return Err(SyncError::ChannelMissing(channel_id.to_owned()));
        }
        let members = self
            .pages(
                "conversations.members",
                "members",
                &[("channel", channel_id), ("limit", "200")],
            )
            .await;
        let now = self.now().naive_utc();
        match members {
            Ok(rows) => {
                let mut ids: Vec<String> = rows
                    .iter()
                    .filter_map(|row| row.as_str().map(|id| id.trim().to_owned()))
                    .filter(|id| !id.is_empty())
                    .collect();
                ids.sort();
                ids.dedup();
                sqlx::query(
                    "UPDATE slack_bot_channels SET member_user_ids = $2, \
                     membership_refreshed_at = $3, membership_error = NULL, updated_at = $3 \
                     WHERE channel_id = $1",
                )
                .bind(channel_id)
                .bind(&ids)
                .bind(now)
                .execute(&self.db)
                .await?;
                Ok(())
            }
            Err(error) if error.retry_after().is_some() => {
                // Not attempted yet, as far as the next pass is concerned.
                sqlx::query(
                    "UPDATE slack_bot_channels SET membership_last_attempted_at = NULL, \
                     updated_at = $2 WHERE channel_id = $1",
                )
                .bind(channel_id)
                .bind(now)
                .execute(&self.db)
                .await?;
                Err(error)
            }
            Err(error) => {
                sqlx::query(
                    "UPDATE slack_bot_channels SET membership_error = $2, updated_at = $3 \
                     WHERE channel_id = $1",
                )
                .bind(channel_id)
                .bind(error.to_string())
                .bind(now)
                .execute(&self.db)
                .await?;
                Err(error)
            }
        }
    }

    /// Refreshes one batch of stale memberships (active channels not
    /// refreshed for a day nor attempted within the hour, oldest first). A
    /// retryable failure stops the batch; others are logged.
    async fn sync_memberships(&self) -> Result<(), SyncError> {
        let now = self.now();
        let stale: Vec<String> = sqlx::query_scalar(
            "SELECT channel_id FROM slack_bot_channels WHERE active AND NOT archived \
             AND (membership_refreshed_at IS NULL OR membership_refreshed_at < $1) \
             AND (membership_last_attempted_at IS NULL OR membership_last_attempted_at < $2) \
             ORDER BY membership_refreshed_at ASC NULLS FIRST, id ASC LIMIT $3",
        )
        .bind((now - chrono::Duration::hours(24)).naive_utc())
        .bind((now - chrono::Duration::hours(1)).naive_utc())
        .bind(MEMBERSHIP_BATCH)
        .fetch_all(&self.db)
        .await?;
        for channel_id in stale {
            match self.channel_members(&channel_id).await {
                Ok(()) => {}
                Err(error) if error.retry_after().is_some() => return Err(error),
                Err(error) => {
                    tracing::warn!(%channel_id, %error, "could not sync Slack membership");
                }
            }
        }
        Ok(())
    }

    /// `SlackChannelCatalogRefreshJob`: call under the refresh lock.
    pub async fn refresh(&self) -> Result<(), SyncError> {
        if !self.configured() {
            return Ok(());
        }
        with_retries("Slack channel catalog discovery", || async {
            self.sync_channels().await.map(|_| ())
        })
        .await?;
        Ok(())
    }

    /// The membership job: one channel (its row and members), or with `None`
    /// the periodic batch. Call under the membership lock.
    pub async fn refresh_membership(&self, channel_id: Option<&str>) -> Result<(), SyncError> {
        if !self.configured() {
            return Ok(());
        }
        with_retries("Slack channel membership refresh", || async {
            match channel_id {
                Some(id) => {
                    if self.import_channel(id).await? {
                        self.channel_members(id).await?;
                    }
                    Ok(())
                }
                None => self.sync_memberships().await,
            }
        })
        .await?;
        Ok(())
    }

    /// Starts one catalog discovery in the background when Slack is
    /// configured and the catalog is empty, unless one is already running.
    pub async fn enqueue_if_empty(&self) {
        if !self.configured() {
            return;
        }
        match sqlx::query_scalar::<_, bool>("SELECT NOT EXISTS (SELECT 1 FROM slack_bot_channels)")
            .fetch_one(&self.db)
            .await
        {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(%error, "could not check the Slack channel catalog");
                return;
            }
        }
        let catalog = self.clone();
        tokio::spawn(async move {
            let outcome = with_lock(&catalog.db, REFRESH_LOOP, false, || async {
                if let Err(error) = catalog.refresh().await {
                    tracing::warn!(%error, "Slack channel catalog sync failed");
                }
            })
            .await;
            if let Err(error) = outcome {
                tracing::warn!(%error, "could not start the Slack channel catalog sync");
            }
        });
    }

    /// Refreshes one channel's row and members in the background (best
    /// effort: the periodic pass covers a failure).
    pub fn spawn_channel_refresh(&self, channel_id: String) {
        let catalog = self.clone();
        tokio::spawn(async move {
            let outcome = with_lock(&catalog.db, MEMBERSHIP_LOOP, true, || async {
                if let Err(error) = catalog.refresh_membership(Some(&channel_id)).await {
                    tracing::warn!(%channel_id, %error, "Slack channel membership refresh failed");
                }
            })
            .await;
            if let Err(error) = outcome {
                tracing::warn!(%error, "Slack channel membership refresh could not run");
            }
        });
    }
}

async fn upsert(
    conn: &mut sqlx::PgConnection,
    team_id: &str,
    bot_user_id: &str,
    channel: &RemoteChannel,
    now: chrono::NaiveDateTime,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO slack_bot_channels (team_id, channel_id, bot_user_id, name, private, archived, \
                                         active, last_seen_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, NOT $6, $7, $7, $7) \
         ON CONFLICT (team_id, channel_id) DO UPDATE SET bot_user_id = EXCLUDED.bot_user_id, \
            name = EXCLUDED.name, private = EXCLUDED.private, archived = EXCLUDED.archived, \
            active = EXCLUDED.active, last_seen_at = EXCLUDED.last_seen_at, updated_at = EXCLUDED.updated_at",
    )
    .bind(team_id)
    .bind(&channel.id)
    .bind(bot_user_id)
    .bind(&channel.name)
    .bind(channel.private)
    .bind(channel.archived)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// The JSON of a successful Slack call, or why it failed.
fn parse_response(
    status: u16,
    retry_after: f64,
    body: &[u8],
    method: &str,
) -> Result<Value, SyncError> {
    if status == 429 {
        return Err(SyncError::Slack {
            message: format!("Slack API rate limited for {method}."),
            code: Some("ratelimited".into()),
            retry_after: Some(retry_after),
        });
    }
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(SyncError::api(format!(
            "Slack API response for {method} was too large."
        )));
    }
    let body: Value = serde_json::from_slice(body)
        .map_err(|_| SyncError::api(format!("Slack API response for {method} was not JSON.")))?;
    if !(200..300).contains(&status) {
        return Err(SyncError::api(format!(
            "Slack API returned HTTP {status} for {method}."
        )));
    }
    if body.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(body);
    }
    let code = body.get("error").and_then(Value::as_str).map(str::to_owned);
    let transient = matches!(code.as_deref(), Some("fatal_error" | "internal_error"));
    Err(SyncError::Slack {
        message: format!(
            "Slack API returned {} for {method}.",
            code.as_deref().unwrap_or("an unknown error")
        ),
        retry_after: transient.then_some(TRANSIENT_RETRY_AFTER),
        code,
    })
}

/// Runs a job body, retrying retryable failures after Slack's delay until
/// the third execution, after which the job is dropped with a warning.
async fn with_retries<F, Fut>(job: &str, mut body: F) -> Result<(), SyncError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), SyncError>>,
{
    let mut executions = 1;
    loop {
        match body().await {
            Ok(()) => return Ok(()),
            Err(error) if error.retry_after().is_some() => {
                if executions >= MAX_EXECUTIONS {
                    tracing::warn!("{job} dropped after repeated retryable Slack failures");
                    return Ok(());
                }
                let wait = error.retry_after().unwrap_or(1.0).max(0.0);
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                executions += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// The advisory lock key of a worker loop: FNV-1a of
/// `centaur-console-worker:<name>`, folded into the positive `i64` range.
pub fn lock_key(name: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("centaur-console-worker:{name}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (hash & 0x7fff_ffff_ffff_ffff) as i64
}

/// Runs `job` holding the advisory lock `name`. With `wait`, polls until
/// the lock is free; without, returns `Ok(None)` when someone else has it.
///
/// The lock is transaction-scoped (`pg_try_advisory_xact_lock`) and the
/// transaction is held for the pass, so it ends with the pass however the
/// pass ends: commit, error, panic, or a dropped future (shutdown
/// `abort_all`). A session-scoped lock would need an explicit unlock, and a
/// pass that unwound would return its connection to the pool still holding
/// it, blocking every replica until the process restarted.
pub async fn with_lock<F, Fut, T>(
    pool: &PgPool,
    name: &str,
    wait: bool,
    job: F,
) -> Result<Option<T>, sqlx::Error>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let key = lock_key(name);
    let transaction = loop {
        let mut transaction = pool.begin().await?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(key)
            .fetch_one(&mut *transaction)
            .await?;
        if locked {
            break transaction;
        }
        // Ends the transaction (and so releases nothing: the lock was not
        // taken) and returns the connection to the pool.
        drop(transaction);
        if !wait {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let output = job().await;
    // Committing releases the lock; so does dropping the transaction if the
    // pass never gets here.
    transaction.commit().await?;
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rate_limits_carry_a_bounded_delay() {
        let error = parse_response(429, 300.0, b"", "conversations.members").unwrap_err();
        assert_eq!(error.retry_after(), Some(300.0));
        assert!(error.to_string().contains("conversations.members"));
    }

    #[test]
    fn transient_errors_retry_and_others_do_not() {
        let transient =
            parse_response(200, 1.0, br#"{"ok":false,"error":"internal_error"}"#, "x").unwrap_err();
        assert_eq!(transient.retry_after(), Some(TRANSIENT_RETRY_AFTER));
        let denied =
            parse_response(200, 1.0, br#"{"ok":false,"error":"missing_scope"}"#, "x").unwrap_err();
        assert_eq!(denied.retry_after(), None);
        assert_eq!(denied.code(), Some("missing_scope"));
        assert!(parse_response(500, 1.0, b"{}", "x").is_err());
        assert!(parse_response(200, 1.0, b"<html>", "x").is_err());
        assert!(parse_response(200, 1.0, br#"{"ok":true}"#, "x").is_ok());
    }

    #[test]
    fn channels_are_public_or_private_not_dms() {
        let parse = |value: Value| parse_channel(&value);
        assert_eq!(
            parse(json!({"id": "C1", "name": "general"})),
            Some(RemoteChannel {
                id: "C1".into(),
                name: "general".into(),
                private: false,
                archived: false
            })
        );
        assert_eq!(
            parse(json!({"id": "D1", "user": "U1", "is_im": true})),
            None
        );
        assert_eq!(parse(json!({"id": "C1", "name": ""})), None);
        let group = parse(
            json!({"id": "G1", "name": "x", "name_normalized": "y", "is_private": true, "is_archived": true}),
        )
        .unwrap();
        assert_eq!(
            (group.name.as_str(), group.private, group.archived),
            ("y", true, true)
        );
    }

    #[test]
    fn lock_keys_are_stable_and_distinct() {
        assert_eq!(lock_key("catalog"), lock_key("catalog"));
        assert_ne!(lock_key(REFRESH_LOOP), lock_key(MEMBERSHIP_LOOP));
        assert!(lock_key(REFRESH_LOOP) >= 0);
    }
}
