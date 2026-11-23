//! Keeps the Console's Slack channel catalog current (port of Rails'
//! `SlackChannelCatalogRefreshJob` and `SlackChannelCatalogMembershipRefreshJob`).
//!
//! A background loop refreshes the channel list once it is an hour old, and the
//! members of up to [`MEMBERSHIP_BATCH_SIZE`] channels whose membership is a day
//! old, on every pass. Passes take Postgres advisory locks, so with several
//! api-rs replicas each refresh runs on one of them; because staleness is read
//! from the catalog itself, replicas do not multiply the Slack calls.

use std::time::Duration;

use centaur_console_db::slack_bot_channels::{
    self, CatalogIdentity, RemoteChannel, StaleMembership,
};
use reqwest::StatusCode;
use serde_json::Value;
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval, sleep};
use tracing::{info, warn};

use crate::routes::non_empty_env;

const DEFAULT_SLACK_API_URL: &str = "https://slack.com/api";
const CHANNEL_TYPES: &str = "public_channel,private_channel";
const PAGE_LIMIT: &str = "200";

/// How often a pass runs (the membership job's schedule).
const PASS_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Refresh the channel list once it is this old (the catalog job's schedule).
const CATALOG_MAX_AGE_SECS: i64 = 60 * 60;
const MEMBERSHIP_TTL_SECS: i64 = 24 * 60 * 60;
const MEMBERSHIP_RETRY_SECS: i64 = 60 * 60;
const MEMBERSHIP_BATCH_SIZE: i64 = 100;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Attempts per Slack call when Slack asks to retry later.
const MAX_ATTEMPTS: u32 = 3;
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(5 * 60);
const TRANSIENT_RETRY_AFTER: Duration = Duration::from_secs(30);
const TRANSIENT_ERRORS: [&str; 2] = ["fatal_error", "internal_error"];

/// Advisory locks that keep each refresh to one process at a time.
const CATALOG_LOCK: i64 = 0x736c_6163_6b63_6174; // "slackcat"
const MEMBERSHIP_LOCK: i64 = 0x736c_6163_6b6d_656d; // "slackmem"

/// Refresh the catalog until the process exits, when a Slack bot token is
/// configured. `console_db` must allow writes.
pub fn spawn_slack_channel_catalog_refresh(console_db: PgPool) {
    let Some(client) = SlackClient::from_env() else {
        info!("Slack bot token is not configured; the Slack channel catalog will not refresh");
        return;
    };
    tokio::spawn(async move {
        let mut ticker = interval(PASS_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            refresh(&console_db, &client).await;
        }
    });
    info!(
        interval_secs = PASS_INTERVAL.as_secs(),
        "started Slack channel catalog refresh"
    );
}

/// One pass: the channel list when stale, then a batch of memberships.
async fn refresh(pool: &PgPool, client: &SlackClient) {
    let catalog = with_lock(pool, CATALOG_LOCK, async {
        if slack_bot_channels::catalog_is_stale(pool, CATALOG_MAX_AGE_SECS)
            .await
            .map_err(|error| error.to_string())?
        {
            let count = sync_channels(pool, client).await?;
            info!(channels = count, "refreshed the Slack channel catalog");
        }
        Ok(())
    })
    .await;
    if let Err(error) = catalog {
        warn!(%error, "Slack channel catalog refresh failed");
    }
    let memberships = with_lock(pool, MEMBERSHIP_LOCK, sync_memberships(pool, client)).await;
    if let Err(error) = memberships {
        warn!(%error, "Slack channel membership refresh failed");
    }
}

/// Run `work` while holding session advisory lock `key`; skip it when another
/// process holds the lock.
async fn with_lock(
    pool: &PgPool,
    key: i64,
    work: impl Future<Output = Result<(), String>>,
) -> Result<(), String> {
    let mut conn = pool.acquire().await.map_err(|error| error.to_string())?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await
        .map_err(|error| error.to_string())?;
    if !locked {
        return Ok(());
    }
    let result = work.await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut *conn)
        .await;
    result
}

/// `SlackChannelCatalogSync#sync_channels`: replace the catalog with every
/// channel the bot can see. Returns the channel count.
async fn sync_channels(pool: &PgPool, client: &SlackClient) -> Result<usize, String> {
    let identity = client.identity().await.map_err(|error| error.to_string())?;
    let channels = client.channels().await.map_err(|error| error.to_string())?;
    slack_bot_channels::replace_catalog(pool, &identity, &channels)
        .await
        .map_err(|error| error.to_string())?;
    Ok(channels.len())
}

/// `SlackChannelCatalogSync#sync_memberships`: refresh a batch of stale
/// memberships. A channel Slack refuses keeps its error and waits out the
/// retry interval; when Slack keeps asking to back off, the pass stops and the
/// channel is retried next pass.
async fn sync_memberships(pool: &PgPool, client: &SlackClient) -> Result<(), String> {
    let channels = slack_bot_channels::stale_memberships(
        pool,
        MEMBERSHIP_TTL_SECS,
        MEMBERSHIP_RETRY_SECS,
        MEMBERSHIP_BATCH_SIZE,
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut refreshed = 0;
    for StaleMembership { id, channel_id } in &channels {
        slack_bot_channels::start_membership_attempt(pool, *id)
            .await
            .map_err(|error| error.to_string())?;
        match client.members(channel_id).await {
            Ok(members) => {
                slack_bot_channels::record_members(pool, *id, &members)
                    .await
                    .map_err(|error| error.to_string())?;
                refreshed += 1;
            }
            Err(error @ SlackError::Retryable { .. }) => {
                slack_bot_channels::forget_membership_attempt(pool, *id)
                    .await
                    .map_err(|error| error.to_string())?;
                return Err(error.to_string());
            }
            Err(error) => {
                let message = error.to_string();
                warn!(channel_id, error = %message, "could not refresh Slack channel membership");
                slack_bot_channels::record_membership_error(pool, *id, &message)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    if !channels.is_empty() {
        info!(
            channels = channels.len(),
            refreshed, "refreshed Slack channel memberships"
        );
    }
    Ok(())
}

/// A Slack Web API failure. Messages name the method, never the token.
#[derive(Debug, PartialEq)]
enum SlackError {
    /// Slack asked to retry after a delay (rate limit or transient error).
    Retryable {
        message: String,
        retry_after: Duration,
    },
    Failed(String),
}

impl std::fmt::Display for SlackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable { message, .. } | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

// No Debug: the token must never reach a log line.
struct SlackClient {
    http: reqwest::Client,
    api_url: String,
    token: String,
}

impl SlackClient {
    /// `SlackChannelCatalogSync.token`: the Console's bot token, or the shared one.
    fn from_env() -> Option<Self> {
        let token = non_empty_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN")
            .or_else(|| non_empty_env("SLACK_BOT_TOKEN"))?;
        let api_url =
            non_empty_env("SLACK_API_URL").unwrap_or_else(|| DEFAULT_SLACK_API_URL.to_owned());
        Some(Self::new(api_url, token))
    }

    fn new(api_url: String, token: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .expect("reqwest client configuration is valid");
        Self {
            http,
            api_url: api_url.trim_end_matches('/').to_owned(),
            token,
        }
    }

    async fn identity(&self) -> Result<CatalogIdentity, SlackError> {
        let body = self.call("auth.test", &[]).await?;
        let field = |name: &str| {
            body.get(name)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Ok(CatalogIdentity {
            team_id: field("team_id").ok_or_else(|| {
                SlackError::Failed("Slack auth.test did not return a team ID.".to_owned())
            })?,
            bot_user_id: field("user_id").ok_or_else(|| {
                SlackError::Failed("Slack auth.test did not return a bot user ID.".to_owned())
            })?,
        })
    }

    /// Every channel the bot can see, by lowercase name then ID.
    async fn channels(&self) -> Result<Vec<RemoteChannel>, SlackError> {
        let rows = self
            .pages(
                "conversations.list",
                "channels",
                &[
                    ("types", CHANNEL_TYPES),
                    ("exclude_archived", "false"),
                    ("limit", PAGE_LIMIT),
                ],
            )
            .await?;
        let mut channels: Vec<RemoteChannel> = rows.iter().filter_map(parse_channel).collect();
        channels
            .sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
        Ok(channels)
    }

    /// A channel's member user IDs, sorted and unique.
    async fn members(&self, channel_id: &str) -> Result<Vec<String>, SlackError> {
        let rows = self
            .pages(
                "conversations.members",
                "members",
                &[("channel", channel_id), ("limit", PAGE_LIMIT)],
            )
            .await?;
        let mut members: Vec<String> = rows
            .iter()
            .filter_map(|member| match member {
                Value::String(id) => Some(id.clone()),
                Value::Null => None,
                other => Some(other.to_string()),
            })
            .filter(|id| !id.trim().is_empty())
            .collect();
        members.sort();
        members.dedup();
        Ok(members)
    }

    /// Every row of a cursor-paginated method's `key` array.
    async fn pages(
        &self,
        method: &str,
        key: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<Value>, SlackError> {
        let mut rows = Vec::new();
        let mut cursor = String::new();
        loop {
            let mut query = params.to_vec();
            if !cursor.is_empty() {
                query.push(("cursor", &cursor));
            }
            let body = self.call(method, &query).await?;
            if let Some(page) = body.get(key).and_then(Value::as_array) {
                rows.extend(page.iter().cloned());
            }
            let next = body
                .pointer("/response_metadata/next_cursor")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if next.is_empty() {
                return Ok(rows);
            }
            cursor = next;
        }
    }

    /// One Web API call, retried while Slack asks to retry later.
    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, SlackError> {
        let mut attempt = 1;
        loop {
            match self.call_once(method, params).await {
                Err(SlackError::Retryable { retry_after, .. }) if attempt < MAX_ATTEMPTS => {
                    sleep(retry_after).await;
                    attempt += 1;
                }
                result => return result,
            }
        }
    }

    async fn call_once(&self, method: &str, params: &[(&str, &str)]) -> Result<Value, SlackError> {
        let url = url::Url::parse_with_params(&format!("{}/{method}", self.api_url), params)
            .map_err(|_| SlackError::Failed(format!("Slack API URL for {method} is invalid.")))?;
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| {
                SlackError::Failed(format!(
                    "Slack API request for {method} failed: {}",
                    error.without_url()
                ))
            })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await.map_err(|error| {
            SlackError::Failed(format!(
                "Slack API response for {method} failed: {}",
                error.without_url()
            ))
        })?;
        parse_response(method, status, retry_after.as_deref(), &body)
    }
}

/// `SlackApi.parse_response!`.
fn parse_response(
    method: &str,
    status: StatusCode,
    retry_after: Option<&str>,
    body: &[u8],
) -> Result<Value, SlackError> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let seconds = retry_after
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .unwrap_or(1.0);
        return Err(SlackError::Retryable {
            message: format!("Slack API rate limited for {method}."),
            retry_after: Duration::from_secs_f64(seconds).min(MAX_RATE_LIMIT_WAIT),
        });
    }
    let body: Value = serde_json::from_slice(body).map_err(|_| {
        SlackError::Failed(format!("Slack API response for {method} was not JSON."))
    })?;
    if !status.is_success() {
        return Err(SlackError::Failed(format!(
            "Slack API returned HTTP {} for {method}.",
            status.as_u16()
        )));
    }
    let error = body.get("error").and_then(Value::as_str);
    if let Some(code) = error.filter(|code| TRANSIENT_ERRORS.contains(code)) {
        return Err(SlackError::Retryable {
            message: format!("Slack API returned {code} for {method}."),
            retry_after: TRANSIENT_RETRY_AFTER,
        });
    }
    if body.get("ok") != Some(&Value::Bool(true)) {
        return Err(SlackError::Failed(format!(
            "Slack API returned {} for {method}.",
            error.unwrap_or("an unknown error")
        )));
    }
    Ok(body)
}

/// `SlackChannelCatalogSync#parse_channel`: a channel with an ID and name, and
/// a `C`/`G` ID (not a DM).
fn parse_channel(channel: &Value) -> Option<RemoteChannel> {
    let text = |key: &str| match channel.get(key) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let id = text("id");
    let name = Some(text("name_normalized"))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| text("name"));
    if id.trim().is_empty() || name.trim().is_empty() {
        return None;
    }
    if !(id.starts_with('C') || id.starts_with('G')) {
        return None;
    }
    Some(RemoteChannel {
        id,
        name,
        private: channel.get("is_private") == Some(&Value::Bool(true)),
        archived: channel.get("is_archived") == Some(&Value::Bool(true)),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use axum::{
        Json, Router,
        extract::{Path, Query},
        http::{HeaderMap, StatusCode as AxumStatus, header},
        response::{IntoResponse, Response},
        routing::get,
    };
    use serde_json::json;
    use sqlx::{Connection, Executor, PgConnection, postgres::PgPoolOptions};

    use super::*;

    #[test]
    fn parses_slack_responses_like_rails() {
        let ok = parse_response("auth.test", StatusCode::OK, None, br#"{"ok":true,"x":1}"#);
        assert_eq!(ok.unwrap()["x"], 1);
        assert_eq!(
            parse_response("m", StatusCode::TOO_MANY_REQUESTS, Some("7"), b""),
            Err(SlackError::Retryable {
                message: "Slack API rate limited for m.".to_owned(),
                retry_after: Duration::from_secs(7),
            })
        );
        for header in [None, Some("0"), Some("soon")] {
            assert!(matches!(
                parse_response("m", StatusCode::TOO_MANY_REQUESTS, header, b""),
                Err(SlackError::Retryable { retry_after, .. }) if retry_after == Duration::from_secs(1)
            ));
        }
        assert!(matches!(
            parse_response("m", StatusCode::TOO_MANY_REQUESTS, Some("9999"), b""),
            Err(SlackError::Retryable { retry_after, .. }) if retry_after == MAX_RATE_LIMIT_WAIT
        ));
        assert!(matches!(
            parse_response("m", StatusCode::OK, None, br#"{"ok":false,"error":"internal_error"}"#),
            Err(SlackError::Retryable { retry_after, .. }) if retry_after == TRANSIENT_RETRY_AFTER
        ));
        assert_eq!(
            parse_response(
                "m",
                StatusCode::OK,
                None,
                br#"{"ok":false,"error":"not_in_channel"}"#
            ),
            Err(SlackError::Failed(
                "Slack API returned not_in_channel for m.".to_owned()
            ))
        );
        assert_eq!(
            parse_response("m", StatusCode::OK, None, br#"{"ok":false}"#),
            Err(SlackError::Failed(
                "Slack API returned an unknown error for m.".to_owned()
            ))
        );
        assert_eq!(
            parse_response("m", StatusCode::BAD_GATEWAY, None, br#"{}"#),
            Err(SlackError::Failed(
                "Slack API returned HTTP 502 for m.".to_owned()
            ))
        );
        assert_eq!(
            parse_response("m", StatusCode::OK, None, b"<html>"),
            Err(SlackError::Failed(
                "Slack API response for m was not JSON.".to_owned()
            ))
        );
    }

    #[test]
    fn parses_only_named_channels() {
        let channel = parse_channel(&json!({
            "id": "C1", "name": "general", "name_normalized": "General",
            "is_private": true, "is_archived": false
        }))
        .unwrap();
        assert_eq!(
            channel,
            RemoteChannel {
                id: "C1".to_owned(),
                name: "General".to_owned(),
                private: true,
                archived: false,
            }
        );
        assert_eq!(
            parse_channel(&json!({"id": "G2", "name": "legacy", "name_normalized": ""}))
                .unwrap()
                .name,
            "legacy"
        );
        assert!(parse_channel(&json!({"id": "D3", "name": "dm"})).is_none());
        assert!(parse_channel(&json!({"id": "C4"})).is_none());
        assert!(parse_channel(&json!("C5")).is_none());
    }

    /// A Slack double: two pages of channels, and per-channel member replies.
    #[derive(Clone, Default)]
    struct FakeSlack {
        rate_limited_once: Arc<Mutex<bool>>,
        authorized: Arc<Mutex<Vec<String>>>,
    }

    async fn fake_method(
        axum::extract::State(slack): axum::extract::State<FakeSlack>,
        Path(method): Path<String>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        if let Some(value) = headers.get(header::AUTHORIZATION) {
            slack
                .authorized
                .lock()
                .unwrap()
                .push(value.to_str().unwrap().to_owned());
        }
        let body = match method.as_str() {
            "auth.test" => json!({"ok": true, "team_id": "T1", "user_id": "UBOT"}),
            "conversations.list" if !query.contains_key("cursor") => json!({
                "ok": true,
                "channels": [
                    {"id": "C2", "name": "zeta", "is_private": false},
                    {"id": "D9", "name": "direct"}
                ],
                "response_metadata": {"next_cursor": "page2"}
            }),
            "conversations.list" => json!({
                "ok": true,
                "channels": [
                    {"id": "C1", "name": "Alpha", "is_private": true},
                    {"id": "C3", "name": "old", "is_archived": true}
                ],
                "response_metadata": {"next_cursor": ""}
            }),
            "conversations.members" => match query.get("channel").map(String::as_str) {
                Some("C1") => {
                    let mut limited = slack.rate_limited_once.lock().unwrap();
                    if !*limited {
                        *limited = true;
                        return (
                            AxumStatus::TOO_MANY_REQUESTS,
                            [(header::RETRY_AFTER, "1")],
                            "",
                        )
                            .into_response();
                    }
                    json!({"ok": true, "members": ["U2", "UBOT", "U2", ""]})
                }
                _ => json!({"ok": false, "error": "not_in_channel"}),
            },
            _ => json!({"ok": false, "error": "unknown_method"}),
        };
        Json(body).into_response()
    }

    /// channel_id, name, private, archived, active, member_user_ids, membership_error.
    type CatalogRow = (
        String,
        String,
        bool,
        bool,
        bool,
        Vec<String>,
        Option<String>,
    );

    async fn scratch_console_db() -> Option<(PgPool, PgConnection, String)> {
        let Ok(url) = std::env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "CONSOLE_DB_TEST_DATABASE_URL must be set in CI"
            );
            return None;
        };
        let mut admin = PgConnection::connect(&url).await.unwrap();
        let mut suffix = [0_u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let name = format!("slack_catalog_{}", hex::encode(suffix));
        admin
            .execute(format!("CREATE DATABASE {name} TEMPLATE template0").as_str())
            .await
            .unwrap();
        let options = url
            .parse::<sqlx::postgres::PgConnectOptions>()
            .unwrap()
            .database(&name);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        centaur_console_db::migrate(&pool).await.unwrap();
        Some((pool, admin, name))
    }

    #[tokio::test]
    async fn refreshes_the_catalog_and_memberships_from_slack() {
        let Some((pool, mut admin, name)) = scratch_console_db().await else {
            return;
        };
        // A channel the bot no longer sees is deactivated by the refresh.
        sqlx::query(
            "INSERT INTO slack_bot_channels (team_id, channel_id, bot_user_id, name, active, \
               created_at, updated_at) VALUES ('T1', 'CGONE', 'UBOT', 'gone', true, now(), now())",
        )
        .execute(&pool)
        .await
        .unwrap();

        let slack = FakeSlack::default();
        let app = Router::new()
            .route("/api/{method}", get(fake_method))
            .with_state(slack.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = SlackClient::new(format!("http://{address}/api/"), "xoxb-test".to_owned());

        refresh(&pool, &client).await;

        let rows: Vec<CatalogRow> = sqlx::query_as(
            "SELECT channel_id, name, private, archived, active, member_user_ids, \
                   membership_error FROM slack_bot_channels ORDER BY channel_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let strings = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect();
        assert_eq!(
            rows,
            vec![
                (
                    "C1".to_owned(),
                    "Alpha".to_owned(),
                    true,
                    false,
                    true,
                    strings(&["U2", "UBOT"]),
                    None
                ),
                (
                    "C2".to_owned(),
                    "zeta".to_owned(),
                    false,
                    false,
                    true,
                    Vec::new(),
                    Some("Slack API returned not_in_channel for conversations.members.".to_owned())
                ),
                (
                    "C3".to_owned(),
                    "old".to_owned(),
                    false,
                    true,
                    false,
                    Vec::new(),
                    None
                ),
                (
                    "CGONE".to_owned(),
                    "gone".to_owned(),
                    false,
                    false,
                    false,
                    Vec::new(),
                    None
                ),
            ]
        );
        assert!(*slack.rate_limited_once.lock().unwrap());
        assert!(
            slack
                .authorized
                .lock()
                .unwrap()
                .iter()
                .all(|value| value == "Bearer xoxb-test")
        );

        // The catalog is fresh and every membership was just attempted, so a
        // second pass calls Slack for nothing.
        let calls = slack.authorized.lock().unwrap().len();
        refresh(&pool, &client).await;
        assert_eq!(slack.authorized.lock().unwrap().len(), calls);

        pool.close().await;
        admin
            .execute(format!("DROP DATABASE {name} WITH (FORCE)").as_str())
            .await
            .unwrap();
    }
}
