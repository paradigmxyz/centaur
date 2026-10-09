use std::{collections::HashMap, sync::Arc, time::Duration};

use absurd::{Client as AbsurdClient, SpawnOptions, TaskContext};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sqlx::{PgPool, types::Json};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    config::{
        QUEUE_NAME, SLACK_CONVERSATION_PROJECT_TASK, SLACK_CONVERSATION_SYNC_TASK,
        SLACK_CREDENTIALS_RECONCILE_TASK, SLACK_THREAD_SYNC_TASK, SLACK_USER_DISCOVER_TASK,
        SLACK_USERS_SYNC_TASK,
    },
    credentials::ConsoleCredentials,
    errors::{is_rejected, rejected},
    slack::{
        AuthTest, Conversation, ConversationsPage, MessagesPage, SlackClient, SlackMethod,
        SlackReply, User, UsersPage,
    },
    slack_documents::ConversationProjectParams,
    slack_files,
    slack_rate_limit::RateLimiter,
    tasks::{bounded_error, run_task},
};

/// A wait for a rate-limit slot longer than this extends the task's lease
/// first, so the claim does not expire while the worker sleeps.
const LEASE_EXTENSION_WAIT: Duration = Duration::from_secs(30);
/// Rate-limited attempts at one request before the task fails and retries.
const RATE_LIMITED_ATTEMPTS: u32 = 5;
/// Slack requires `users.conversations` page sizes below 1000.
const CONVERSATIONS_PAGE_SIZE: &str = "999";
/// Slack recommends at most 200 messages per history or replies page.
const MESSAGES_PAGE_SIZE: &str = "200";
/// Slack recommends at most 200 users per `users.list` page.
const USERS_PAGE_SIZE: &str = "200";
/// Each history sync rereads at least this much recent history, so edits and
/// new replies to threads started within it are picked up. Replies to older
/// threads are not.
const THREAD_REFRESH_WINDOW: chrono::Duration = chrono::Duration::hours(72);
/// A history sync that has not stored a page for this long no longer holds
/// its conversation.
const HISTORY_SYNC_LEASE: &str = "1 hour";

#[derive(Clone)]
pub struct SlackTaskState {
    pub pool: PgPool,
    pub absurd: AbsurdClient,
    /// Spawns thread syncs onto their own queue.
    pub threads: AbsurdClient,
    pub credentials: Arc<ConsoleCredentials>,
    pub slack: SlackClient,
    pub limiter: RateLimiter,
    /// Conversations to synchronize; empty synchronizes every conversation.
    pub channel_ids: Vec<String>,
    /// How far back message history is synchronized.
    pub history: chrono::Duration,
    /// Per-conversation overrides of `history`.
    pub channel_history: HashMap<String, chrono::Duration>,
    /// The app's bot token, which lists workspace users.
    pub bot_token: String,
}

impl SlackTaskState {
    fn history(&self, conversation_id: &str) -> chrono::Duration {
        self.channel_history
            .get(conversation_id)
            .copied()
            .unwrap_or(self.history)
    }
}

/// The span a history sync reads: from `oldest` to the present, which it
/// records as synchronized up to `until`.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct HistoryWindow {
    oldest: DateTime<Utc>,
    until: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SlackReconcileParams {
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SlackDiscoverParams {
    pub credential_id: i64,
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConversationSyncParams {
    pub credential_id: i64,
    pub team_id: String,
    pub conversation_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ThreadSyncParams {
    pub credential_id: i64,
    pub team_id: String,
    pub conversation_id: String,
    pub thread_ts: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsersSyncParams {
    pub bucket: u64,
}

#[derive(Debug, Serialize)]
pub struct MessagesSummary {
    status: &'static str,
    messages: usize,
}

impl MessagesSummary {
    fn new(status: &'static str, messages: usize) -> Self {
        Self { status, messages }
    }
}

#[derive(Debug, Serialize)]
pub struct DiscoverySummary {
    status: &'static str,
    conversations: usize,
}

pub fn register(absurd: &AbsurdClient, state: SlackTaskState) -> Result<()> {
    let reconcile_state = state.clone();
    absurd.register_task(
        SLACK_CREDENTIALS_RECONCILE_TASK,
        move |params: SlackReconcileParams, ctx| {
            let state = reconcile_state.clone();
            async move { run_task(&ctx, reconcile_credentials(&state, params, &ctx)).await }
        },
    )?;

    let discover_state = state.clone();
    absurd.register_task(
        SLACK_USER_DISCOVER_TASK,
        move |params: SlackDiscoverParams, ctx| {
            let state = discover_state.clone();
            async move { run_task(&ctx, discover(&state, params, &ctx)).await }
        },
    )?;

    let conversation_state = state.clone();
    absurd.register_task(
        SLACK_CONVERSATION_SYNC_TASK,
        move |params: ConversationSyncParams, ctx| {
            let state = conversation_state.clone();
            async move { run_task(&ctx, sync_conversation(&state, params, &ctx)).await }
        },
    )?;

    // Thread syncs queued on the Slack queue by earlier versions still run
    // there; new ones are spawned onto the thread queue.
    for client in [absurd, &state.threads] {
        let thread_state = state.clone();
        client.register_task(
            SLACK_THREAD_SYNC_TASK,
            move |params: ThreadSyncParams, ctx| {
                let state = thread_state.clone();
                async move { run_task(&ctx, sync_thread(&state, params, &ctx)).await }
            },
        )?;
    }

    absurd.register_task(
        SLACK_USERS_SYNC_TASK,
        move |params: UsersSyncParams, ctx| {
            let state = state.clone();
            async move { run_task(&ctx, sync_users(&state, params, &ctx)).await }
        },
    )?;
    Ok(())
}

/// Deactivates identities and observations of dead or deleted credentials and
/// removes conversations no live credential still observes.
async fn reconcile_credentials(
    state: &SlackTaskState,
    params: SlackReconcileParams,
    ctx: &TaskContext,
) -> Result<DiscoverySummary> {
    let retained_ids = state.credentials.retained_slack_credential_ids().await?;
    let (deactivated, removed) = remove_unobserved(&state.pool, &retained_ids).await?;
    info!(
        event = "company_context_slack_credentials_reconciled",
        task_id = ctx.task_id(),
        bucket = params.bucket,
        observations_deactivated = deactivated,
        conversations_removed = removed
    );
    Ok(DiscoverySummary {
        status: "completed",
        conversations: removed,
    })
}

/// Records the conversations a credential's Slack user belongs to.
async fn discover(
    state: &SlackTaskState,
    params: SlackDiscoverParams,
    ctx: &TaskContext,
) -> Result<DiscoverySummary> {
    let credential = state
        .credentials
        .slack_credential(params.credential_id)
        .await?;
    let result = async {
        if credential.conversation_types.is_empty() {
            return Err(rejected(
                "Slack credential cannot read any ingested conversation type",
            ));
        }
        let identity = auth_test(&state.slack, ctx, &credential.access_token).await?;

        let types = credential.conversation_types.join(",");
        let mut conversations = Vec::new();
        let mut cursor = String::new();
        for page_number in 0.. {
            let page: ConversationsPage = paced_call(
                &state.slack,
                &state.limiter,
                ctx,
                &format!("slack.users.conversations.{page_number}"),
                &identity.team_id,
                SlackMethod::UsersConversations,
                &credential.access_token,
                &[
                    ("types", types.clone()),
                    ("limit", CONVERSATIONS_PAGE_SIZE.to_owned()),
                    ("cursor", cursor.clone()),
                ],
            )
            .await?;
            conversations.extend(page.channels.into_iter().filter(|conversation| {
                state.channel_ids.is_empty() || state.channel_ids.contains(&conversation.id)
            }));
            cursor = page.response_metadata.next_cursor;
            if cursor.is_empty() {
                break;
            }
        }
        record_discovery(&state.pool, credential.id, &identity, &conversations).await?;
        // The first discovery in a cycle to list a conversation syncs its history.
        for conversation in &conversations {
            state
                .absurd
                .spawn(
                    SLACK_CONVERSATION_SYNC_TASK,
                    ConversationSyncParams {
                        credential_id: credential.id,
                        team_id: identity.team_id.clone(),
                        conversation_id: conversation.id.clone(),
                    },
                    SpawnOptions {
                        idempotency_key: Some(format!(
                            "slack.conversation.sync:{}:{}",
                            conversation.id, params.bucket
                        )),
                        ..SpawnOptions::default()
                    },
                )
                .await?;
        }
        Ok(conversations.len())
    }
    .await;

    match result {
        Ok(conversations) => {
            info!(
                event = "company_context_slack_discovery_completed",
                task_id = ctx.task_id(),
                credential_id = credential.id,
                conversations
            );
            Ok(DiscoverySummary {
                status: "completed",
                conversations,
            })
        }
        Err(error) if is_rejected(&error) => {
            warn!(
                event = "company_context_slack_discovery_rejected",
                task_id = ctx.task_id(),
                credential_id = credential.id,
                error = %error
            );
            Ok(DiscoverySummary {
                status: "rejected",
                conversations: 0,
            })
        }
        Err(error) => Err(error),
    }
}

/// Stores a conversation's history since its last sync, rereading at least
/// the thread refresh window, and syncs each thread whose replies changed.
async fn sync_conversation(
    state: &SlackTaskState,
    params: ConversationSyncParams,
    ctx: &TaskContext,
) -> Result<MessagesSummary> {
    let conversation_id = params.conversation_id.as_str();
    let task_id = ctx.task_id();
    // Checkpoint the claim and window so a resumed run pages the same range.
    let window: Option<HistoryWindow> = match ctx.begin_step("slack.history.window").await? {
        handle if handle.done => handle.state.context("history window checkpoint is empty")?,
        handle => {
            let window = claim_history(&state.pool, conversation_id, task_id)
                .await?
                .map(|(synced_from, synced_until)| {
                    history_window(
                        synced_from,
                        synced_until,
                        Utc::now(),
                        state.history(conversation_id),
                    )
                });
            ctx.complete_step(handle, window).await?
        }
    };
    let Some(window) = window else {
        info!(
            event = "company_context_slack_history_skipped",
            task_id, conversation_id
        );
        return Ok(MessagesSummary::new("skipped", 0));
    };

    let result = async {
        let credential = state
            .credentials
            .slack_credential(params.credential_id)
            .await?;
        let oldest = format!(
            "{}.{:06}",
            window.oldest.timestamp(),
            window.oldest.timestamp_subsec_micros()
        );
        let mut stored = 0;
        let mut cursor = String::new();
        for page_number in 0.. {
            let page: MessagesPage = paced_call(
                &state.slack,
                &state.limiter,
                ctx,
                &format!("slack.conversations.history.{page_number}"),
                &params.team_id,
                SlackMethod::ConversationsHistory,
                &credential.access_token,
                &[
                    ("channel", conversation_id.to_owned()),
                    ("oldest", oldest.clone()),
                    ("limit", MESSAGES_PAGE_SIZE.to_owned()),
                    ("cursor", cursor.clone()),
                ],
            )
            .await?;
            stored +=
                store_messages(&state.pool, conversation_id, Some(task_id), &page.messages).await?;
            for message in &page.messages {
                let Some(ts) = message["ts"].as_str() else {
                    continue;
                };
                // Only parents carry latest_reply, so a thread is resynced
                // only when it has a reply it did not have before.
                if let Some(latest_reply) = message["latest_reply"].as_str() {
                    state
                        .threads
                        .spawn(
                            SLACK_THREAD_SYNC_TASK,
                            ThreadSyncParams {
                                credential_id: credential.id,
                                team_id: params.team_id.clone(),
                                conversation_id: conversation_id.to_owned(),
                                thread_ts: ts.to_owned(),
                            },
                            SpawnOptions {
                                idempotency_key: Some(format!(
                                    "slack.thread.sync:{conversation_id}:{ts}:{latest_reply}"
                                )),
                                ..SpawnOptions::default()
                            },
                        )
                        .await?;
                }
            }
            cursor = page.response_metadata.next_cursor;
            if cursor.is_empty() {
                break;
            }
        }
        finish_history(&state.pool, conversation_id, task_id, &window).await?;
        spawn_projection(&state.absurd, conversation_id, task_id).await?;
        Ok(stored)
    }
    .await;

    match result {
        Ok(messages) => {
            info!(
                event = "company_context_slack_history_synced",
                task_id, conversation_id, messages
            );
            Ok(MessagesSummary::new("completed", messages))
        }
        Err(error) => {
            let rejected = is_rejected(&error);
            // A rejected sync releases the conversation so the next cycle can
            // try another credential; a failed one keeps it for its retry.
            if let Err(db_error) =
                record_history_error(&state.pool, conversation_id, task_id, &error, rejected).await
            {
                warn!(event = "company_context_failure_record_failed", conversation_id, error = %db_error);
            }
            if rejected {
                warn!(
                    event = "company_context_slack_history_rejected",
                    task_id,
                    conversation_id,
                    error = %error
                );
                return Ok(MessagesSummary::new("rejected", 0));
            }
            Err(error)
        }
    }
}

/// Stores every message of a thread, including its parent.
async fn sync_thread(
    state: &SlackTaskState,
    params: ThreadSyncParams,
    ctx: &TaskContext,
) -> Result<MessagesSummary> {
    let result = async {
        let credential = state
            .credentials
            .slack_credential(params.credential_id)
            .await?;
        let mut stored = 0;
        let mut cursor = String::new();
        for page_number in 0.. {
            let page: MessagesPage = paced_call(
                &state.slack,
                &state.limiter,
                ctx,
                &format!("slack.conversations.replies.{page_number}"),
                &params.team_id,
                SlackMethod::ConversationsReplies,
                &credential.access_token,
                &[
                    ("channel", params.conversation_id.clone()),
                    ("ts", params.thread_ts.clone()),
                    ("limit", MESSAGES_PAGE_SIZE.to_owned()),
                    ("cursor", cursor.clone()),
                ],
            )
            .await?;
            stored +=
                store_messages(&state.pool, &params.conversation_id, None, &page.messages).await?;
            cursor = page.response_metadata.next_cursor;
            if cursor.is_empty() {
                break;
            }
        }
        spawn_projection(&state.absurd, &params.conversation_id, ctx.task_id()).await?;
        Ok(stored)
    }
    .await;

    match result {
        Ok(messages) => Ok(MessagesSummary::new("completed", messages)),
        Err(error) if is_rejected(&error) => {
            warn!(
                event = "company_context_slack_thread_rejected",
                task_id = ctx.task_id(),
                conversation_id = params.conversation_id,
                thread_ts = params.thread_ts,
                error = %error
            );
            Ok(MessagesSummary::new("rejected", 0))
        }
        Err(error) => Err(error),
    }
}

/// Stores every user of the bot's workspace, so projections can render names.
async fn sync_users(
    state: &SlackTaskState,
    params: UsersSyncParams,
    ctx: &TaskContext,
) -> Result<MessagesSummary> {
    let mut team_id = String::new();
    let result = async {
        let bot_token = state.bot_token.as_str();
        team_id = auth_test(&state.slack, ctx, bot_token).await?.team_id;
        let mut stored = 0;
        let mut cursor = String::new();
        for page_number in 0.. {
            let page: UsersPage = paced_call(
                &state.slack,
                &state.limiter,
                ctx,
                &format!("slack.users.list.{page_number}"),
                &team_id,
                SlackMethod::UsersList,
                bot_token,
                &[
                    ("limit", USERS_PAGE_SIZE.to_owned()),
                    ("cursor", cursor.clone()),
                ],
            )
            .await?;
            stored += store_users(&state.pool, &team_id, &page.members).await?;
            cursor = page.response_metadata.next_cursor;
            if cursor.is_empty() {
                break;
            }
        }
        Ok(stored)
    }
    .await;

    match result {
        Ok(users) => {
            info!(
                event = "company_context_slack_users_synced",
                task_id = ctx.task_id(),
                bucket = params.bucket,
                team_id,
                users_changed = users
            );
            Ok(MessagesSummary::new("completed", users))
        }
        Err(error) if is_rejected(&error) => {
            warn!(
                event = "company_context_slack_users_rejected",
                task_id = ctx.task_id(),
                bucket = params.bucket,
                error = %error
            );
            Ok(MessagesSummary::new("rejected", 0))
        }
        Err(error) => Err(error),
    }
}

/// Identifies a token's workspace and user. auth.test has its own generous
/// limit, so it is not paced, but its result is checkpointed so a resumed run
/// does not call it again.
async fn auth_test(slack: &SlackClient, ctx: &TaskContext, access_token: &str) -> Result<AuthTest> {
    let handle = ctx.begin_step::<AuthTest>("slack.auth.test").await?;
    if handle.done {
        return handle.state.context("auth.test checkpoint is empty");
    }
    let SlackReply::Ok(body) = slack.call("auth.test", access_token, &[]).await? else {
        bail!("Slack rate limited auth.test");
    };
    let identity: AuthTest =
        serde_json::from_value(body).context("decode Slack auth.test response")?;
    if identity.team_id.is_empty() || identity.user_id.is_empty() {
        return Err(rejected("Slack auth.test did not identify a user"));
    }
    Ok(ctx.complete_step(handle, identity).await?)
}

/// Projects a conversation's changed days after its messages were stored.
async fn spawn_projection(
    absurd: &AbsurdClient,
    conversation_id: &str,
    sync_task_id: &str,
) -> Result<()> {
    absurd
        .spawn(
            SLACK_CONVERSATION_PROJECT_TASK,
            ConversationProjectParams {
                conversation_id: conversation_id.to_owned(),
            },
            SpawnOptions {
                queue: Some(QUEUE_NAME.to_owned()),
                idempotency_key: Some(format!(
                    "slack.conversation.project:{conversation_id}:{sync_task_id}"
                )),
                ..SpawnOptions::default()
            },
        )
        .await?;
    Ok(())
}

/// Returns the span to read: the whole configured history if part of it has
/// not been synchronized yet, and otherwise everything since the last sync or
/// within the thread refresh window, whichever starts earlier.
fn history_window(
    synced_from: Option<DateTime<Utc>>,
    synced_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    history: chrono::Duration,
) -> HistoryWindow {
    let start = (now - history).max(DateTime::UNIX_EPOCH);
    let oldest = match (synced_from, synced_until) {
        (Some(from), Some(until)) if from <= start => until.min(now - THREAD_REFRESH_WINDOW),
        _ => start,
    };
    HistoryWindow { oldest, until: now }
}

/// Calls a paced Slack method as a durable step. The response is checkpointed
/// so a resumed run does not repeat the request. Each attempt waits for its
/// slot in place, holding the worker, so only running calls hold slots and a
/// rate-limited call retries before tasks queued behind it.
#[allow(clippy::too_many_arguments)]
async fn paced_call<T>(
    slack: &SlackClient,
    limiter: &RateLimiter,
    ctx: &TaskContext,
    step: &str,
    team_id: &str,
    method: SlackMethod,
    access_token: &str,
    params: &[(&str, String)],
) -> Result<T>
where
    T: Serialize + DeserializeOwned + Send + 'static,
{
    let handle = ctx.begin_step::<T>(step).await?;
    if handle.done {
        return handle
            .state
            .with_context(|| format!("{step} checkpoint is empty"));
    }
    for _ in 0..RATE_LIMITED_ATTEMPTS {
        let wait = limiter.reserve(team_id, method).await?;
        if wait > LEASE_EXTENSION_WAIT {
            ctx.heartbeat(Some(wait + LEASE_EXTENSION_WAIT)).await?;
        }
        sleep(wait).await;
        match slack.call(method.name(), access_token, params).await? {
            SlackReply::Ok(body) => {
                let value = serde_json::from_value(body)
                    .with_context(|| format!("decode Slack {} response", method.name()))?;
                return Ok(ctx.complete_step(handle, value).await?);
            }
            SlackReply::RateLimited(retry_after) => {
                warn!(
                    event = "company_context_slack_rate_limited",
                    task_id = ctx.task_id(),
                    method = method.name(),
                    retry_after_seconds = retry_after.as_secs()
                );
                limiter.rate_limited(team_id, method, retry_after).await?;
            }
        }
    }
    bail!(
        "Slack rate limited {} {RATE_LIMITED_ATTEMPTS} times",
        method.name()
    )
}

/// Replaces a credential's observations with the conversations it listed.
async fn record_discovery(
    pool: &PgPool,
    credential_id: i64,
    identity: &AuthTest,
    conversations: &[Conversation],
) -> Result<()> {
    // Lock conversations in a consistent order to avoid deadlocking with
    // reconciliation and other discoveries.
    let mut conversations = conversations.to_vec();
    conversations.sort_by(|left, right| left.id.cmp(&right.id));
    conversations.dedup_by(|left, right| left.id == right.id);
    let ids: Vec<&str> = conversations.iter().map(|item| item.id.as_str()).collect();
    let kinds: Vec<&str> = conversations.iter().map(Conversation::kind).collect();
    let names: Vec<&str> = conversations
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    let archived: Vec<bool> = conversations.iter().map(|item| item.is_archived).collect();
    let topics: Vec<&str> = conversations
        .iter()
        .map(|item| item.topic.value.as_str())
        .collect();
    let purposes: Vec<&str> = conversations
        .iter()
        .map(|item| item.purpose.value.as_str())
        .collect();

    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_data.slack_broker_identities
            (broker_credential_id, team_id, provider_subject)
        VALUES ($1, $2, $3)
        ON CONFLICT (broker_credential_id) DO UPDATE
        SET team_id = EXCLUDED.team_id,
            provider_subject = EXCLUDED.provider_subject,
            active = TRUE,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(credential_id)
    .bind(&identity.team_id)
    .bind(&identity.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_system.slack_conversations
            (conversation_id, team_id, kind, name, is_archived, topic, purpose)
        SELECT conversation_id, $2, kind, name, is_archived, topic, purpose
        FROM unnest($1::text[], $3::text[], $4::text[], $5::boolean[], $6::text[], $7::text[])
            AS listed(conversation_id, kind, name, is_archived, topic, purpose)
        ORDER BY conversation_id
        ON CONFLICT (conversation_id) DO UPDATE
        SET team_id = EXCLUDED.team_id,
            kind = EXCLUDED.kind,
            name = EXCLUDED.name,
            is_archived = EXCLUDED.is_archived,
            topic = EXCLUDED.topic,
            purpose = EXCLUDED.purpose,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(&ids)
    .bind(&identity.team_id)
    .bind(&kinds)
    .bind(&names)
    .bind(&archived)
    .bind(&topics)
    .bind(&purposes)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_data.slack_broker_observations
            (broker_credential_id, conversation_id, provider_subject)
        SELECT $1, conversation_id, $3
        FROM unnest($2::text[]) AS listed(conversation_id)
        ON CONFLICT (broker_credential_id, conversation_id) DO UPDATE
        SET provider_subject = EXCLUDED.provider_subject,
            active = TRUE,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(credential_id)
    .bind(&ids)
    .bind(&identity.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_data.slack_broker_observations
        SET active = FALSE,
            updated_at = NOW()
        WHERE broker_credential_id = $1
          AND active
          AND NOT (conversation_id = ANY($2::text[]))
        "#,
    )
    .bind(credential_id)
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Claims a conversation's history for a sync task. Returns the span already
/// synchronized, or `None` if the conversation is gone or another live sync
/// holds it.
#[allow(clippy::type_complexity)]
async fn claim_history(
    pool: &PgPool,
    conversation_id: &str,
    task_id: &str,
) -> Result<Option<(Option<DateTime<Utc>>, Option<DateTime<Utc>>)>> {
    Ok(sqlx::query_as(&format!(
        r#"
        UPDATE company_context_system.slack_conversations
        SET history_sync_task_id = $2,
            history_sync_heartbeat_at = NOW()
        WHERE conversation_id = $1
          AND (
              history_sync_task_id IS NULL
              OR history_sync_task_id = $2
              OR history_sync_heartbeat_at < NOW() - INTERVAL '{HISTORY_SYNC_LEASE}'
          )
        RETURNING history_synced_from, history_synced_until
        "#
    ))
    .bind(conversation_id)
    .bind(task_id)
    .fetch_optional(pool)
    .await?)
}

/// Extends the synchronized span by the window read and releases the
/// conversation.
async fn finish_history(
    pool: &PgPool,
    conversation_id: &str,
    task_id: &str,
    window: &HistoryWindow,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE company_context_system.slack_conversations
        SET history_synced_from = LEAST(history_synced_from, $3),
            history_synced_until = GREATEST(history_synced_until, $4),
            history_last_error = '',
            history_sync_task_id = NULL,
            history_sync_heartbeat_at = NULL,
            updated_at = NOW()
        WHERE conversation_id = $1
          AND history_sync_task_id = $2
        "#,
    )
    .bind(conversation_id)
    .bind(task_id)
    .bind(window.oldest)
    .bind(window.until)
    .execute(pool)
    .await?;
    Ok(())
}

async fn record_history_error(
    pool: &PgPool,
    conversation_id: &str,
    task_id: &str,
    error: &anyhow::Error,
    release: bool,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE company_context_system.slack_conversations
        SET history_last_error = $3,
            history_sync_task_id = CASE WHEN $4 THEN NULL ELSE history_sync_task_id END,
            history_sync_heartbeat_at = CASE WHEN $4 THEN NULL ELSE history_sync_heartbeat_at END,
            updated_at = NOW()
        WHERE conversation_id = $1
          AND history_sync_task_id = $2
        "#,
    )
    .bind(conversation_id)
    .bind(task_id)
    .bind(bounded_error(error))
    .bind(release)
    .execute(pool)
    .await?;
    Ok(())
}

/// Upserts messages as Slack sent them and returns how many were stored. A
/// history sync passes its task ID to extend its hold on the conversation.
async fn store_messages(
    pool: &PgPool,
    conversation_id: &str,
    sync_task_id: Option<&str>,
    messages: &[Value],
) -> Result<usize> {
    let mut tx = pool.begin().await?;
    // Lock the conversation so reconciliation cannot remove it, and its
    // messages, while this page is stored.
    let observed: Option<bool> = sqlx::query_scalar(
        r#"
        UPDATE company_context_system.slack_conversations
        SET history_sync_heartbeat_at = CASE
                WHEN history_sync_task_id = $2 THEN NOW()
                ELSE history_sync_heartbeat_at
            END
        WHERE conversation_id = $1
        RETURNING TRUE
        "#,
    )
    .bind(conversation_id)
    .bind(sync_task_id)
    .fetch_optional(&mut *tx)
    .await?;
    if observed.is_none() {
        return Err(rejected(format!(
            "Slack conversation {conversation_id} is no longer observed"
        )));
    }
    let stored = sqlx::query(
        r#"
        INSERT INTO company_context_system.slack_messages
            (conversation_id, message_ts, thread_ts, user_id, bot_id, subtype, text,
             reply_count, latest_reply_ts, edited_ts, occurred_at, raw_payload)
        SELECT DISTINCT ON (message->>'ts')
            $1,
            message->>'ts',
            message->>'thread_ts',
            COALESCE(message->>'user', ''),
            COALESCE(message->>'bot_id', ''),
            message->>'subtype',
            COALESCE(message->>'text', ''),
            COALESCE((message->>'reply_count')::integer, 0),
            message->>'latest_reply',
            message->'edited'->>'ts',
            to_timestamp((message->>'ts')::double precision),
            message
        FROM jsonb_array_elements($2::jsonb) AS listed(message)
        WHERE COALESCE(message->>'ts', '') <> ''
        ORDER BY message->>'ts'
        ON CONFLICT (conversation_id, message_ts) DO UPDATE
        SET thread_ts = EXCLUDED.thread_ts,
            user_id = EXCLUDED.user_id,
            bot_id = EXCLUDED.bot_id,
            subtype = EXCLUDED.subtype,
            text = EXCLUDED.text,
            reply_count = EXCLUDED.reply_count,
            latest_reply_ts = EXCLUDED.latest_reply_ts,
            edited_ts = EXCLUDED.edited_ts,
            raw_payload = EXCLUDED.raw_payload,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(conversation_id)
    .bind(Json(messages))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(stored as usize)
}

/// Upserts a page of users and returns how many changed.
async fn store_users(pool: &PgPool, team_id: &str, users: &[User]) -> Result<usize> {
    let mut users = users.to_vec();
    users.sort_by(|left, right| left.id.cmp(&right.id));
    users.dedup_by(|left, right| left.id == right.id);
    users.retain(|user| !user.id.is_empty());
    let ids: Vec<&str> = users.iter().map(|user| user.id.as_str()).collect();
    let names: Vec<&str> = users.iter().map(|user| user.name.as_str()).collect();
    let real_names: Vec<&str> = users
        .iter()
        .map(|user| match user.profile.real_name.as_str() {
            "" => user.real_name.as_str(),
            real_name => real_name,
        })
        .collect();
    let display_names: Vec<&str> = users
        .iter()
        .map(|user| user.profile.display_name.as_str())
        .collect();
    let bots: Vec<bool> = users.iter().map(|user| user.is_bot).collect();
    let deleted: Vec<bool> = users.iter().map(|user| user.deleted).collect();
    let changed = sqlx::query(
        r#"
        INSERT INTO company_context_system.slack_users
            (user_id, team_id, name, real_name, display_name, is_bot, deleted)
        SELECT user_id, $2, name, real_name, display_name, is_bot, deleted
        FROM unnest($1::text[], $3::text[], $4::text[], $5::text[], $6::boolean[], $7::boolean[])
            AS listed(user_id, name, real_name, display_name, is_bot, deleted)
        ON CONFLICT (user_id) DO UPDATE
        SET team_id = EXCLUDED.team_id,
            name = EXCLUDED.name,
            real_name = EXCLUDED.real_name,
            display_name = EXCLUDED.display_name,
            is_bot = EXCLUDED.is_bot,
            deleted = EXCLUDED.deleted,
            updated_at = NOW()
        WHERE (slack_users.team_id, slack_users.name, slack_users.real_name,
               slack_users.display_name, slack_users.is_bot, slack_users.deleted)
            IS DISTINCT FROM
              (EXCLUDED.team_id, EXCLUDED.name, EXCLUDED.real_name,
               EXCLUDED.display_name, EXCLUDED.is_bot, EXCLUDED.deleted)
        "#,
    )
    .bind(&ids)
    .bind(team_id)
    .bind(&names)
    .bind(&real_names)
    .bind(&display_names)
    .bind(&bots)
    .bind(&deleted)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(changed as usize)
}

/// Returns the number of observations deactivated and conversations removed.
async fn remove_unobserved(pool: &PgPool, retained_ids: &[i64]) -> Result<(u64, usize)> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        UPDATE company_context_data.slack_broker_identities
        SET active = FALSE,
            updated_at = NOW()
        WHERE active
          AND NOT (broker_credential_id = ANY($1::bigint[]))
        "#,
    )
    .bind(retained_ids)
    .execute(&mut *tx)
    .await?;
    let deactivated = sqlx::query(
        r#"
        UPDATE company_context_data.slack_broker_observations
        SET active = FALSE,
            updated_at = NOW()
        WHERE active
          AND NOT (broker_credential_id = ANY($1::bigint[]))
        "#,
    )
    .bind(retained_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let candidates: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT conversations.conversation_id
        FROM company_context_system.slack_conversations conversations
        WHERE NOT EXISTS (
            SELECT 1
            FROM company_context_data.slack_broker_observations observations
            WHERE observations.conversation_id = conversations.conversation_id
              AND observations.active
        )
        ORDER BY conversations.conversation_id
        FOR UPDATE OF conversations
        "#,
    )
    .fetch_all(&mut *tx)
    .await?;
    // Recheck after locking: a discovery that committed meanwhile keeps its
    // conversation, and one that commits later recreates it.
    let removed = sqlx::query(
        r#"
        DELETE FROM company_context_system.slack_conversations conversations
        WHERE conversations.conversation_id = ANY($1::text[])
          AND NOT EXISTS (
              SELECT 1
              FROM company_context_data.slack_broker_observations observations
              WHERE observations.conversation_id = conversations.conversation_id
                AND observations.active
          )
        "#,
    )
    .bind(&candidates)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Files go with the last message sharing them.
    slack_files::remove_unshared(&mut tx).await?;
    // Users go with the last live credential in their workspace.
    sqlx::query(
        r#"
        DELETE FROM company_context_system.slack_users users
        WHERE NOT EXISTS (
            SELECT 1
            FROM company_context_data.slack_broker_identities identities
            WHERE identities.team_id = users.team_id
              AND identities.active
        )
        "#,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((deactivated, removed as usize))
}

#[cfg(test)]
mod tests {
    use std::env;

    use serde_json::json;
    use sqlx::Row;

    use super::*;
    use crate::test_support::TestDatabase;

    fn identity(user_id: &str) -> AuthTest {
        AuthTest {
            team_id: "T1".to_owned(),
            user_id: user_id.to_owned(),
        }
    }

    fn conversation(id: &str, name: &str, is_private: bool) -> Conversation {
        serde_json::from_value(json!({
            "id": id,
            "name": name,
            "is_private": is_private,
            "topic": { "value": format!("{name} topic") },
        }))
        .unwrap()
    }

    async fn conversations(pool: &PgPool) -> Vec<(String, String, String)> {
        sqlx::query(
            "SELECT conversation_id, kind, name FROM company_context_system.slack_conversations ORDER BY 1",
        )
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
    }

    async fn active_observations(pool: &PgPool) -> Vec<(i64, String, String)> {
        sqlx::query(
            r#"
            SELECT broker_credential_id, conversation_id, provider_subject
            FROM company_context_data.slack_broker_observations
            WHERE active
            ORDER BY 1, 2
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
    }

    #[tokio::test]
    async fn conversations_follow_live_observations() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "slack_discovery").await;
        let pool = &database.pool;
        let general = conversation("C1", "general", false);
        let secret = conversation("G1", "secret", true);

        record_discovery(pool, 1, &identity("U1"), &[general.clone(), secret.clone()])
            .await
            .unwrap();
        record_discovery(pool, 2, &identity("U2"), std::slice::from_ref(&general))
            .await
            .unwrap();
        assert_eq!(
            conversations(pool).await,
            [
                (
                    "C1".to_owned(),
                    "public_channel".to_owned(),
                    "general".to_owned()
                ),
                (
                    "G1".to_owned(),
                    "private_channel".to_owned(),
                    "secret".to_owned()
                ),
            ]
        );
        assert_eq!(
            active_observations(pool).await,
            [
                (1, "C1".to_owned(), "U1".to_owned()),
                (1, "G1".to_owned(), "U1".to_owned()),
                (2, "C1".to_owned(), "U2".to_owned()),
            ]
        );

        // Leaving a channel ends that user's observation, and the channel goes
        // once no live credential observes it.
        let renamed = conversation("C1", "general-renamed", false);
        record_discovery(pool, 1, &identity("U1"), &[renamed])
            .await
            .unwrap();
        assert_eq!(remove_unobserved(pool, &[1, 2]).await.unwrap(), (0, 1));
        assert_eq!(
            conversations(pool).await,
            [(
                "C1".to_owned(),
                "public_channel".to_owned(),
                "general-renamed".to_owned()
            )]
        );

        // A dead credential's observations and identity are deactivated.
        assert_eq!(remove_unobserved(pool, &[2]).await.unwrap(), (1, 0));
        assert_eq!(
            active_observations(pool).await,
            [(2, "C1".to_owned(), "U2".to_owned())]
        );
        let active_identities: Vec<i64> = sqlx::query_scalar(
            "SELECT broker_credential_id FROM company_context_data.slack_broker_identities WHERE active",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(active_identities, [2]);

        // Observing a removed conversation again restores it.
        assert_eq!(remove_unobserved(pool, &[]).await.unwrap(), (1, 1));
        record_discovery(pool, 1, &identity("U1"), &[secret])
            .await
            .unwrap();
        assert_eq!(
            active_observations(pool).await,
            [(1, "G1".to_owned(), "U1".to_owned())]
        );

        database.drop().await;
    }

    #[test]
    fn history_window_backfills_unsynced_history_then_rereads_recent_history() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let days = chrono::Duration::days;
        let window = |from, until, history| history_window(from, until, now, history).oldest;

        // A conversation never synced reads its whole history.
        assert_eq!(window(None, None, days(90)), now - days(90));
        // A recent sync still rereads the thread refresh window.
        let recent = Some(now - chrono::Duration::hours(1));
        assert_eq!(
            window(Some(now - days(90)), recent, days(90)),
            now - THREAD_REFRESH_WINDOW
        );
        // An older sync is read from so no messages are skipped.
        let stale = Some(now - days(10));
        assert_eq!(
            window(Some(now - days(90)), stale, days(90)),
            now - days(10)
        );
        // Raising a conversation's history backfills what was not synced.
        assert_eq!(
            window(Some(now - days(90)), recent, days(3650)),
            now - days(3650)
        );
        // History never starts before the Unix epoch.
        assert_eq!(window(None, None, days(36_500)), DateTime::UNIX_EPOCH);
        assert_eq!(history_window(None, None, now, days(90)).until, now);
    }

    fn user(id: &str, real_name: &str) -> User {
        serde_json::from_value(json!({
            "id": id,
            "name": id.to_lowercase(),
            "profile": { "real_name": real_name, "display_name": "" },
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn users_change_only_when_rendered_fields_change_and_go_with_their_workspace() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "slack_users").await;
        let pool = &database.pool;
        record_discovery(pool, 1, &identity("U1"), &[])
            .await
            .unwrap();

        let ada = user("U1", "Ada");
        assert_eq!(
            store_users(pool, "T1", &[ada.clone(), user("U2", "Bob")])
                .await
                .unwrap(),
            2
        );
        assert_eq!(store_users(pool, "T1", &[ada]).await.unwrap(), 0);
        assert_eq!(
            store_users(pool, "T1", &[user("U1", "Ada Lovelace")])
                .await
                .unwrap(),
            1
        );
        let users = || async {
            sqlx::query_scalar::<_, String>(
                "SELECT real_name FROM company_context_system.slack_users ORDER BY user_id",
            )
            .fetch_all(pool)
            .await
            .unwrap()
        };
        assert_eq!(users().await, ["Ada Lovelace", "Bob"]);

        remove_unobserved(pool, &[1]).await.unwrap();
        assert_eq!(users().await.len(), 2);
        remove_unobserved(pool, &[]).await.unwrap();
        assert!(users().await.is_empty());

        database.drop().await;
    }

    async fn message_texts(pool: &PgPool) -> Vec<(String, String, Option<String>)> {
        sqlx::query(
            "SELECT message_ts, text, thread_ts FROM company_context_system.slack_messages ORDER BY 1",
        )
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
    }

    #[tokio::test]
    async fn history_is_stored_once_per_conversation_and_removed_with_it() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "slack_messages").await;
        let pool = &database.pool;
        record_discovery(
            pool,
            1,
            &identity("U1"),
            &[conversation("C1", "general", false)],
        )
        .await
        .unwrap();

        // Only one sync holds a conversation at a time.
        assert_eq!(
            claim_history(pool, "C1", "task-a").await.unwrap(),
            Some((None, None))
        );
        assert_eq!(claim_history(pool, "C1", "task-b").await.unwrap(), None);
        assert_eq!(claim_history(pool, "C9", "task-b").await.unwrap(), None);

        let page = [
            json!({ "ts": "1700000009.900000", "user": "U1", "text": "hello", "reply_count": 1, "latest_reply": "1700000010.000001" }),
            json!({ "ts": "1700000010.000001", "user": "U2", "text": "reply", "thread_ts": "1700000009.900000" }),
            json!({ "type": "message", "text": "no timestamp" }),
        ];
        assert_eq!(
            store_messages(pool, "C1", Some("task-a"), &page)
                .await
                .unwrap(),
            2
        );
        let edited = [
            json!({ "ts": "1700000009.900000", "user": "U1", "text": "hello, edited", "edited": { "ts": "1700000011.000000" } }),
        ];
        assert_eq!(store_messages(pool, "C1", None, &edited).await.unwrap(), 1);
        assert_eq!(
            message_texts(pool).await,
            [
                (
                    "1700000009.900000".to_owned(),
                    "hello, edited".to_owned(),
                    None
                ),
                (
                    "1700000010.000001".to_owned(),
                    "reply".to_owned(),
                    Some("1700000009.900000".to_owned())
                ),
            ]
        );

        // The synchronized span only grows.
        let at = |seconds| DateTime::from_timestamp(seconds, 0).unwrap();
        let first = HistoryWindow {
            oldest: at(1_000),
            until: at(5_000),
        };
        finish_history(pool, "C1", "task-a", &first).await.unwrap();
        assert_eq!(
            claim_history(pool, "C1", "task-b").await.unwrap(),
            Some((Some(at(1_000)), Some(at(5_000))))
        );
        let recent = HistoryWindow {
            oldest: at(4_000),
            until: at(6_000),
        };
        finish_history(pool, "C1", "task-b", &recent).await.unwrap();
        assert_eq!(
            claim_history(pool, "C1", "task-c").await.unwrap(),
            Some((Some(at(1_000)), Some(at(6_000))))
        );
        // Only the sync holding the conversation records its span.
        let stalled = HistoryWindow {
            oldest: at(0),
            until: at(9_000),
        };
        finish_history(pool, "C1", "task-a", &stalled)
            .await
            .unwrap();
        assert_eq!(
            claim_history(pool, "C1", "task-c").await.unwrap(),
            Some((Some(at(1_000)), Some(at(6_000))))
        );

        // Messages go with their conversation, and are not stored again.
        remove_unobserved(pool, &[]).await.unwrap();
        assert!(message_texts(pool).await.is_empty());
        let error = store_messages(pool, "C1", Some("task-c"), &page)
            .await
            .unwrap_err();
        assert!(is_rejected(&error));

        database.drop().await;
    }
}
