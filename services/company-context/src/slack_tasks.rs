use std::{sync::Arc, time::Duration};

use absurd::{Client as AbsurdClient, TaskContext};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::PgPool;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    config::{SLACK_CREDENTIALS_RECONCILE_TASK, SLACK_USER_DISCOVER_TASK},
    credentials::ConsoleCredentials,
    errors::{is_rejected, rejected},
    slack::{AuthTest, Conversation, ConversationsPage, SlackClient, SlackMethod, SlackReply},
    slack_rate_limit::RateLimiter,
    tasks::run_task,
};

/// Waits up to this long for a rate-limit slot without releasing the worker.
const INLINE_WAIT: Duration = Duration::from_secs(5);
/// Rate-limited attempts at one request before the task fails and retries.
const RATE_LIMITED_ATTEMPTS: u32 = 5;
/// Slack requires `users.conversations` page sizes below 1000.
const CONVERSATIONS_PAGE_SIZE: &str = "999";

#[derive(Clone)]
pub struct SlackTaskState {
    pub pool: PgPool,
    pub credentials: Arc<ConsoleCredentials>,
    pub slack: SlackClient,
    pub limiter: RateLimiter,
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

    absurd.register_task(
        SLACK_USER_DISCOVER_TASK,
        move |params: SlackDiscoverParams, ctx| {
            let state = state.clone();
            async move { run_task(&ctx, discover(&state, params, &ctx)).await }
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
        // auth.test has its own generous limit, so it is not paced, but its
        // result is checkpointed so a resumed run does not call it again.
        let identity: AuthTest = match ctx.begin_step("slack.auth.test").await? {
            handle if handle.done => handle.state.context("auth.test checkpoint is empty")?,
            handle => {
                let SlackReply::Ok(body) = state
                    .slack
                    .call("auth.test", &credential.access_token, &[])
                    .await?
                else {
                    bail!("Slack rate limited auth.test");
                };
                let identity: AuthTest =
                    serde_json::from_value(body).context("decode Slack auth.test response")?;
                if identity.team_id.is_empty() || identity.user_id.is_empty() {
                    return Err(rejected("Slack auth.test did not identify a user"));
                }
                ctx.complete_step(handle, identity).await?
            }
        };

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
            conversations.extend(page.channels);
            cursor = page.response_metadata.next_cursor;
            if cursor.is_empty() {
                break;
            }
        }
        record_discovery(&state.pool, credential.id, &identity, &conversations).await?;
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

/// Calls a paced Slack method as a durable step. The response is checkpointed
/// so a resumed run does not repeat the request, and each reserved slot is
/// checkpointed so a run resumed after waiting for it does not reserve another.
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
    for attempt in 1..=RATE_LIMITED_ATTEMPTS {
        wait_for_slot(
            limiter,
            ctx,
            &format!("{step}.slot.{attempt}"),
            team_id,
            method,
        )
        .await?;
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

/// Waits for a reserved send slot. Short waits sleep in place; longer ones
/// suspend the task so the worker can run other tasks meanwhile.
async fn wait_for_slot(
    limiter: &RateLimiter,
    ctx: &TaskContext,
    step: &str,
    team_id: &str,
    method: SlackMethod,
) -> Result<()> {
    let handle = ctx.begin_step::<DateTime<Utc>>(step).await?;
    let send_at = match handle.state {
        Some(send_at) if handle.done => send_at,
        _ => {
            let wait = limiter.reserve(team_id, method).await?;
            let send_at = Utc::now() + chrono::Duration::from_std(wait)?;
            ctx.complete_step(handle, send_at).await?
        }
    };
    let wait = (send_at - Utc::now()).to_std().unwrap_or_default();
    if wait <= INLINE_WAIT {
        sleep(wait).await;
    } else {
        ctx.sleep_until(&format!("{step}.wait"), send_at).await?;
    }
    Ok(())
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
}
