//! Keeps the Console's Slack channel catalog current through the Slack client
//! the proxy routes use.
//!
//! Every [`PASS_INTERVAL`] a pass refreshes the channel list once it is an
//! hour old, then the members of up to [`MEMBERSHIP_BATCH_SIZE`] channels whose
//! membership is a day old. Advisory locks keep each refresh to one api-rs
//! replica at a time, and staleness is read from the catalog itself, so
//! replicas do not multiply the Slack calls.

use std::time::Duration;

use centaur_console_db::slack_bot_channels::{
    self, CatalogIdentity, RemoteChannel, StaleMembership,
};
use serde_json::Value;
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use super::{SlackFileProxyConfig, http_client, slack_api_call, slack_proxy_config};
use crate::ApiError;

/// Slack's own "try again later" error codes.
const TRANSIENT_SLACK_ERRORS: [&str; 2] = ["fatal_error", "internal_error"];

/// Why a Slack call failed, as the refresh decides what to do about it.
#[derive(Debug)]
enum SlackFailure {
    /// Slack or the network is having trouble (transport failure, a non-JSON
    /// or 5xx response, exhausted rate-limit retries, or Slack's transient
    /// codes): stop the pass without blaming the channel.
    Transient(String),
    /// Slack answered and refused, or answered without the expected data.
    Failed(String),
}

impl std::fmt::Display for SlackFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

/// One Web API call through the proxy's client, classified.
async fn call(
    config: &SlackFileProxyConfig,
    method: &str,
    form: &[(&str, String)],
) -> Result<Value, SlackFailure> {
    match slack_api_call(http_client(), config, method, form).await {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(code)) if TRANSIENT_SLACK_ERRORS.contains(&code.as_str()) => Err(
            SlackFailure::Transient(format!("Slack {method} failed: {code}")),
        ),
        Ok(Err(code)) => Err(SlackFailure::Failed(format!(
            "Slack {method} failed: {code}"
        ))),
        Err(error) => Err(SlackFailure::Transient(error.to_string())),
    }
}

const PASS_INTERVAL: Duration = Duration::from_secs(10 * 60);
const CATALOG_MAX_AGE_SECS: i64 = 60 * 60;
const MEMBERSHIP_TTL_SECS: i64 = 24 * 60 * 60;
const MEMBERSHIP_RETRY_SECS: i64 = 60 * 60;
const MEMBERSHIP_BATCH_SIZE: i64 = 100;
const PAGE_LIMIT: &str = "200";

const CATALOG_LOCK: i64 = 0x736c_6163_6b63_6174; // "slackcat"
const MEMBERSHIP_LOCK: i64 = 0x736c_6163_6b6d_656d; // "slackmem"

/// Refresh the catalog until the process exits, when `SLACK_BOT_TOKEN` is
/// set. `console_db` must allow writes.
pub fn spawn_slack_channel_catalog_refresh(console_db: PgPool) {
    let Ok(config) = slack_proxy_config() else {
        info!("SLACK_BOT_TOKEN is not configured; the Slack channel catalog will not refresh");
        return;
    };
    tokio::spawn(async move {
        let mut ticker = interval(PASS_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            refresh(&console_db, config).await;
        }
    });
    info!(
        interval_secs = PASS_INTERVAL.as_secs(),
        "started Slack channel catalog refresh"
    );
}

async fn refresh(pool: &PgPool, config: &SlackFileProxyConfig) {
    let catalog = with_lock(pool, CATALOG_LOCK, async {
        if slack_bot_channels::catalog_is_stale(pool, CATALOG_MAX_AGE_SECS).await? {
            let count = sync_channels(pool, config).await?;
            info!(channels = count, "refreshed the Slack channel catalog");
        }
        Ok(())
    });
    if let Err(error) = catalog.await {
        warn!(%error, "Slack channel catalog refresh failed");
    }
    if let Err(error) = with_lock(pool, MEMBERSHIP_LOCK, sync_memberships(pool, config)).await {
        warn!(%error, "Slack channel membership refresh failed");
    }
}

/// Run `work` holding session advisory lock `key`; skip it when another
/// process holds the lock.
async fn with_lock(
    pool: &PgPool,
    key: i64,
    work: impl Future<Output = Result<(), ApiError>>,
) -> Result<(), ApiError> {
    let mut conn = pool.acquire().await.map_err(internal)?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(key)
        .fetch_one(&mut *conn)
        .await
        .map_err(internal)?;
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

/// Replace the catalog with every
/// channel the bot can see.
async fn sync_channels(pool: &PgPool, config: &SlackFileProxyConfig) -> Result<usize, ApiError> {
    let identity = call(config, "auth.test", &[])
        .await
        .map_err(|failure| ApiError::Internal(failure.to_string()))?;
    let field = |name: &str| {
        identity
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Internal(format!("Slack auth.test returned no {name}")))
    };
    let identity = CatalogIdentity {
        team_id: field("team_id")?,
        bot_user_id: field("user_id")?,
    };
    let form = [
        ("types", "public_channel,private_channel"),
        ("exclude_archived", "false"),
    ];
    let mut channels: Vec<RemoteChannel> = pages(config, "conversations.list", "channels", &form)
        .await
        .map_err(|failure| ApiError::Internal(failure.to_string()))?
        .iter()
        .filter_map(parse_channel)
        .collect();
    channels.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
    slack_bot_channels::replace_catalog(pool, &identity, &channels).await?;
    Ok(channels.len())
}

/// Refresh a batch of stale
/// memberships. A channel Slack refuses keeps the error and waits out the
/// retry interval; when Slack or the network is in trouble, the pass stops
/// and the channel is retried next pass.
async fn sync_memberships(pool: &PgPool, config: &SlackFileProxyConfig) -> Result<(), ApiError> {
    let channels = slack_bot_channels::stale_memberships(
        pool,
        MEMBERSHIP_TTL_SECS,
        MEMBERSHIP_RETRY_SECS,
        MEMBERSHIP_BATCH_SIZE,
    )
    .await?;
    for StaleMembership { id, channel_id } in &channels {
        slack_bot_channels::start_membership_attempt(pool, *id).await?;
        let form = [("channel", channel_id.as_str())];
        match pages(config, "conversations.members", "members", &form).await {
            Ok(members) => {
                let mut members: Vec<String> = members
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
                    .collect();
                members.sort();
                members.dedup();
                slack_bot_channels::record_members(pool, *id, &members).await?;
            }
            Err(SlackFailure::Transient(message)) => {
                slack_bot_channels::forget_membership_attempt(pool, *id).await?;
                return Err(ApiError::ServiceUnavailable(message));
            }
            Err(SlackFailure::Failed(message)) => {
                warn!(channel_id, error = %message, "could not refresh Slack channel membership");
                slack_bot_channels::record_membership_error(pool, *id, &message).await?;
            }
        }
    }
    Ok(())
}

/// Every row of a cursor-paginated method's `key` array.
async fn pages(
    config: &SlackFileProxyConfig,
    method: &str,
    key: &str,
    form: &[(&str, &str)],
) -> Result<Vec<Value>, SlackFailure> {
    let mut rows = Vec::new();
    let mut cursor = String::new();
    loop {
        let mut page_form: Vec<(&str, String)> = form
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect();
        page_form.push(("limit", PAGE_LIMIT.to_owned()));
        if !cursor.is_empty() {
            page_form.push(("cursor", cursor.clone()));
        }
        let body = call(config, method, &page_form).await?;
        // A page without its array is not an empty page: replacing the catalog
        // or a membership with it would drop what Console already knows.
        let page = body.get(key).and_then(Value::as_array).ok_or_else(|| {
            SlackFailure::Failed(format!("Slack {method} returned no {key} array"))
        })?;
        rows.extend(page.iter().cloned());
        cursor = body
            .pointer("/response_metadata/next_cursor")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if cursor.is_empty() {
            return Ok(rows);
        }
    }
}

/// A named channel with a `C`/`G` ID.
fn parse_channel(channel: &Value) -> Option<RemoteChannel> {
    let text = |key: &str| {
        channel
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    };
    let id = text("id")?;
    let name = text("name_normalized").or_else(|| text("name"))?;
    if !(id.starts_with('C') || id.starts_with('G')) {
        return None;
    }
    Some(RemoteChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        private: channel.get("is_private") == Some(&Value::Bool(true)),
        archived: channel.get("is_archived") == Some(&Value::Bool(true)),
    })
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use axum::{
        Form, Json, Router,
        extract::{Path, State},
        response::{IntoResponse, Response},
        routing::post,
    };
    use serde_json::json;
    use sqlx::{Connection, Executor, PgConnection, postgres::PgPoolOptions};

    use super::*;

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
        let legacy = json!({"id": "G2", "name": "legacy", "name_normalized": ""});
        assert_eq!(parse_channel(&legacy).unwrap().name, "legacy");
        assert!(parse_channel(&json!({"id": "D3", "name": "dm"})).is_none());
        assert!(parse_channel(&json!({"id": "C4"})).is_none());
    }

    /// The calls the fake answered, and whether Slack is having an outage.
    type FakeSlackState = (Arc<Mutex<usize>>, Arc<Mutex<bool>>);

    /// A Slack double: two pages of channels, members for C1, an error for
    /// every other channel, and a count of the calls it answered. While
    /// `outage` is set, `conversations.members` answers Slack's transient
    /// `internal_error`.
    async fn fake_slack(
        State((calls, outage)): State<FakeSlackState>,
        Path(method): Path<String>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Response {
        *calls.lock().unwrap() += 1;
        if method == "conversations.members" && *outage.lock().unwrap() {
            return Json(json!({"ok": false, "error": "internal_error"})).into_response();
        }
        let body = match method.as_str() {
            "auth.test" => json!({"ok": true, "team_id": "T1", "user_id": "UBOT"}),
            "conversations.list" if !form.contains_key("cursor") => json!({
                "ok": true,
                "channels": [
                    {"id": "C2", "name": "zeta"},
                    {"id": "C4", "name": "malformed"},
                    {"id": "D9", "name": "direct"}
                ],
                "response_metadata": {"next_cursor": "page2"}
            }),
            "conversations.list" => json!({
                "ok": true,
                "channels": [
                    {"id": "C1", "name": "Alpha", "is_private": true},
                    {"id": "C3", "name": "old", "is_archived": true}
                ]
            }),
            "conversations.members" if form.get("channel").is_some_and(|c| c == "C1") => {
                json!({"ok": true, "members": ["U2", "UBOT", "U2", ""]})
            }
            // A success without the members array must not read as no members.
            "conversations.members" if form.get("channel").is_some_and(|c| c == "C4") => {
                json!({"ok": true})
            }
            _ => json!({"ok": false, "error": "not_in_channel"}),
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

    #[tokio::test]
    async fn refreshes_the_catalog_and_memberships_from_slack() {
        let Ok(url) = std::env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "CONSOLE_DB_TEST_DATABASE_URL must be set in CI"
            );
            return;
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
        let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
        centaur_console_db::migrate(&pool).await.unwrap();
        // A channel the bot no longer sees is deactivated by the refresh.
        sqlx::query(
            "INSERT INTO slack_bot_channels (team_id, channel_id, bot_user_id, name, active, \
               created_at, updated_at) VALUES ('T1', 'CGONE', 'UBOT', 'gone', true, now(), now())",
        )
        .execute(&pool)
        .await
        .unwrap();

        let calls = Arc::new(Mutex::new(0));
        let outage = Arc::new(Mutex::new(false));
        let app = Router::new()
            .route("/api/{method}", post(fake_slack))
            .with_state((calls.clone(), outage.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = SlackFileProxyConfig {
            api_url: format!("http://{address}/api"),
            bot_token: "xoxb-test".to_owned(),
            max_upload_bytes: 1,
        };

        refresh(&pool, &config).await;

        let rows: Vec<CatalogRow> = sqlx::query_as(
            "SELECT channel_id, name, private, archived, active, member_user_ids, \
               membership_error FROM slack_bot_channels ORDER BY channel_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let row =
            |id: &str, name: &str, flags: [bool; 3], members: &[&str], error: Option<&str>| {
                (
                    id.to_owned(),
                    name.to_owned(),
                    flags[0],
                    flags[1],
                    flags[2],
                    members.iter().map(|member| (*member).to_owned()).collect(),
                    error.map(str::to_owned),
                )
            };
        let refused = "Slack conversations.members failed: not_in_channel";
        assert_eq!(
            rows,
            vec![
                row("C1", "Alpha", [true, false, true], &["U2", "UBOT"], None),
                row("C2", "zeta", [false, false, true], &[], Some(refused)),
                row("C3", "old", [false, true, false], &[], None),
                row(
                    "C4",
                    "malformed",
                    [false, false, true],
                    &[],
                    Some("Slack conversations.members returned no members array")
                ),
                row("CGONE", "gone", [false, false, false], &[], None),
            ]
        );

        // The catalog is fresh and every membership was just attempted, so a
        // second pass calls Slack for nothing.
        let answered = *calls.lock().unwrap();
        refresh(&pool, &config).await;
        assert_eq!(*calls.lock().unwrap(), answered);

        // During a Slack outage a stale channel keeps its members and gets no
        // error; its attempt is forgotten so the next pass retries it.
        *outage.lock().unwrap() = true;
        sqlx::query(
            "UPDATE slack_bot_channels SET membership_refreshed_at = NULL, \
               membership_last_attempted_at = NULL WHERE channel_id = 'C1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        refresh(&pool, &config).await;
        let after: (Vec<String>, Option<String>, bool) = sqlx::query_as(
            "SELECT member_user_ids, membership_error, membership_last_attempted_at IS NULL \
             FROM slack_bot_channels WHERE channel_id = 'C1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            after,
            (vec!["U2".to_owned(), "UBOT".to_owned()], None, true)
        );

        pool.close().await;
        admin
            .execute(format!("DROP DATABASE {name} WITH (FORCE)").as_str())
            .await
            .unwrap();
    }
}
