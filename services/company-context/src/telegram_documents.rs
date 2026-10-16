//! Projects the Telegram group messages the Telegram ingress stores into
//! retrieval documents, and expires them. Each chat's UTC day is rendered as a
//! transcript split into chunks, like a Slack channel day, and every chunk is
//! a document with its own embedding. Telegram's Bot API offers no history,
//! edits are not delivered, and deletions are not reported, so the messages
//! are kept as they arrived until they age out of the retention window.
//! Expiry runs even while the Telegram indexer is disabled, which pauses only
//! projection.

use std::collections::{BTreeSet, HashMap};

use absurd::{SpawnOptions, TaskContext};
use anyhow::{Context, Result};
use chrono::{DateTime, Days, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row, types::Json};
use tracing::{error, info, warn};

use crate::{
    config::{
        TELEGRAM_CHAT_DAY_EMBED_TASK, TELEGRAM_CHAT_PROJECT_TASK, TELEGRAM_CHATS_PROJECT_TASK,
        TELEGRAM_DOCUMENT_ID_PREFIX,
    },
    embeddings::EmbeddingsClient,
    errors::is_rejected,
    extraction::hex_sha256,
    slack_documents::{Chunk, Message, render_day},
    tasks::{TaskState, bounded_error, run_task},
};

/// Bump to render every chat day again on the next projection. Chunks whose
/// text is unchanged keep their vectors.
const PROJECTION_VERSION: i32 = 1;
/// How much of a replied-to message is quoted before a reply.
const REPLY_PREVIEW_CHARS: usize = 80;

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatsProjectParams {
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatProjectParams {
    pub chat_id: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatDayEmbedParams {
    pub chat_id: i64,
    pub day: NaiveDate,
    pub revision: i64,
}

#[derive(Debug, Serialize)]
pub struct ProjectionSummary {
    status: &'static str,
    days: usize,
}

impl ProjectionSummary {
    fn new(status: &'static str, days: usize) -> Self {
        Self { status, days }
    }
}

pub fn register(state: &TaskState) -> Result<()> {
    let chats_state = state.clone();
    state.absurd.register_task(
        TELEGRAM_CHATS_PROJECT_TASK,
        move |params: ChatsProjectParams, ctx| {
            let state = chats_state.clone();
            async move { run_task(&ctx, project_chats(&state, params, &ctx)).await }
        },
    )?;

    let project_state = state.clone();
    state.absurd.register_task(
        TELEGRAM_CHAT_PROJECT_TASK,
        move |params: ChatProjectParams, ctx| {
            let state = project_state.clone();
            async move { run_task(&ctx, project_chat(&state, params, &ctx)).await }
        },
    )?;

    let embed_state = state.clone();
    state.absurd.register_task(
        TELEGRAM_CHAT_DAY_EMBED_TASK,
        move |params: ChatDayEmbedParams, ctx| {
            let state = embed_state.clone();
            async move {
                run_task(
                    &ctx,
                    embed_day(&state.pool, &state.embeddings, params, ctx.task_id()),
                )
                .await
            }
        },
    )?;
    Ok(())
}

/// The first UTC day still inside the retention window, which holds the last
/// `retention_days` UTC days, today included.
fn retention_cutoff(now: DateTime<Utc>, retention_days: usize) -> NaiveDate {
    now.date_naive()
        .checked_sub_days(Days::new(retention_days.saturating_sub(1) as u64))
        .unwrap_or(NaiveDate::MIN)
}

/// Expires days older than the retention window, then, while the Telegram
/// indexer is enabled, enqueues projection of every chat with a day to render.
async fn project_chats(
    state: &TaskState,
    params: ChatsProjectParams,
    ctx: &TaskContext,
) -> Result<ProjectionSummary> {
    let cutoff = retention_cutoff(Utc::now(), state.config.telegram_retention_days);
    let (messages_expired, days_expired) = expire(&state.pool, cutoff).await?;
    let chat_ids: BTreeSet<i64> = if state.config.telegram_enabled {
        stale_days(&state.pool, None, cutoff)
            .await?
            .into_iter()
            .map(|(chat_id, _)| chat_id)
            .collect()
    } else {
        BTreeSet::new()
    };
    for chat_id in &chat_ids {
        state
            .absurd
            .spawn(
                TELEGRAM_CHAT_PROJECT_TASK,
                ChatProjectParams { chat_id: *chat_id },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "telegram.chat.project:{chat_id}:{}",
                        params.bucket
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
    }
    info!(
        event = "company_context_telegram_chats_projected",
        task_id = ctx.task_id(),
        chats_pending = chat_ids.len(),
        messages_expired,
        days_expired,
        cutoff = %cutoff
    );
    Ok(ProjectionSummary::new("completed", chat_ids.len()))
}

/// Removes messages and chat days older than `cutoff`. Documents and
/// embeddings go with their chat day.
async fn expire(pool: &PgPool, cutoff: NaiveDate) -> Result<(u64, u64)> {
    let mut tx = pool.begin().await?;
    let messages = sqlx::query(
        "DELETE FROM company_context_system.telegram_messages WHERE projection_day < $1",
    )
    .bind(cutoff)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let days = sqlx::query("DELETE FROM company_context_system.telegram_chat_days WHERE day < $1")
        .bind(cutoff)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok((messages, days))
}

/// Renders a chat's stale days and enqueues publication of the ones whose
/// content changed.
async fn project_chat(
    state: &TaskState,
    params: ChatProjectParams,
    ctx: &TaskContext,
) -> Result<ProjectionSummary> {
    let chat_id = params.chat_id;
    let cutoff = retention_cutoff(Utc::now(), state.config.telegram_retention_days);
    let days = stale_days(&state.pool, Some(chat_id), cutoff).await?;
    let mut pending = 0;
    for (_, day) in &days {
        let Some(revision) =
            project_day(&state.pool, chat_id, *day, state.config.chunk_chars).await?
        else {
            continue;
        };
        state
            .absurd
            .spawn(
                TELEGRAM_CHAT_DAY_EMBED_TASK,
                ChatDayEmbedParams {
                    chat_id,
                    day: *day,
                    revision,
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "telegram.chat_day.embed:{chat_id}:{day}:{revision}:{}",
                        state.embeddings.model()
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        pending += 1;
    }
    info!(
        event = "company_context_telegram_chat_projected",
        task_id = ctx.task_id(),
        chat_id,
        days_rendered = days.len(),
        days_pending = pending
    );
    Ok(ProjectionSummary::new("completed", pending))
}

/// Returns the chat days on or after `cutoff` to render, for one chat or all
/// of them: those never rendered, rendered before a message arrived or the
/// chat was renamed, rendered by another projection version, or still
/// awaiting publication.
async fn stale_days(
    pool: &PgPool,
    chat_id: Option<i64>,
    cutoff: NaiveDate,
) -> Result<Vec<(i64, NaiveDate)>> {
    Ok(sqlx::query_as(
        r#"
        WITH titles AS (
            SELECT DISTINCT ON (chat_id) chat_id, chat_title
            FROM company_context_system.telegram_messages
            WHERE chat_title <> ''
              AND ($1::bigint IS NULL OR chat_id = $1)
            ORDER BY chat_id, sent_at DESC, message_id DESC
        )
        SELECT messages.chat_id, messages.projection_day
        FROM company_context_system.telegram_messages messages
        LEFT JOIN titles ON titles.chat_id = messages.chat_id
        LEFT JOIN company_context_system.telegram_chat_days days
          ON days.chat_id = messages.chat_id
         AND days.day = messages.projection_day
        WHERE ($1::bigint IS NULL OR messages.chat_id = $1)
          AND messages.projection_day >= $2
        GROUP BY messages.chat_id, messages.projection_day, titles.chat_title,
                 days.chat_id, days.day
        HAVING days.day IS NULL
            OR max(messages.received_at) > days.rendered_at
            OR days.projection_version <> $3
            OR days.chat_title <> COALESCE(titles.chat_title, '')
            OR days.embedding_status = 'pending'
        ORDER BY 1, 2
        "#,
    )
    .bind(chat_id)
    .bind(cutoff)
    .bind(PROJECTION_VERSION)
    .fetch_all(pool)
    .await?)
}

/// Renders one chat day. Returns the revision to publish when the day is
/// awaiting publication.
async fn project_day(
    pool: &PgPool,
    chat_id: i64,
    day: NaiveDate,
    max_chars: usize,
) -> Result<Option<i64>> {
    let row = sqlx::query(
        r#"
        SELECT NOW() AS rendered_at,
               COALESCE((
                   SELECT chat_title
                   FROM company_context_system.telegram_messages
                   WHERE chat_id = $1
                     AND chat_title <> ''
                   ORDER BY sent_at DESC, message_id DESC
                   LIMIT 1
               ), '') AS chat_title
        "#,
    )
    .bind(chat_id)
    .fetch_one(pool)
    .await?;
    let rendered_at: DateTime<Utc> = row.try_get("rendered_at")?;
    let chat_title: String = row.try_get("chat_title")?;
    let messages = load_day(pool, chat_id, day).await?;
    if messages.is_empty() {
        // Every message of the day was expired meanwhile.
        return Ok(None);
    }
    let label = if chat_title.is_empty() {
        format!("Telegram chat {chat_id}")
    } else {
        chat_title.clone()
    };
    let title = format!("{label} (Telegram) — {day}");
    let chunks = render_day(&format!("{label} (Telegram)"), day, &messages, max_chars);
    let content_hash = hex_sha256(
        json!([PROJECTION_VERSION, title, chunks])
            .to_string()
            .as_bytes(),
    );

    // A rendering older than the stored one is discarded, so a slow
    // projection cannot overwrite a newer one.
    let row = sqlx::query(
        r#"
        INSERT INTO company_context_system.telegram_chat_days (
            chat_id, day, projection_version, chat_title, message_count, title,
            chunks, content_hash, rendered_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (chat_id, day) DO UPDATE
        SET projection_version = EXCLUDED.projection_version,
            chat_title = EXCLUDED.chat_title,
            message_count = EXCLUDED.message_count,
            title = EXCLUDED.title,
            chunks = EXCLUDED.chunks,
            content_hash = EXCLUDED.content_hash,
            rendered_at = EXCLUDED.rendered_at,
            revision = CASE
                WHEN telegram_chat_days.content_hash = EXCLUDED.content_hash
                    THEN telegram_chat_days.revision
                ELSE telegram_chat_days.revision + 1
            END,
            embedding_status = CASE
                WHEN telegram_chat_days.content_hash = EXCLUDED.content_hash
                    THEN telegram_chat_days.embedding_status
                ELSE 'pending'
            END,
            last_error = CASE
                WHEN telegram_chat_days.content_hash = EXCLUDED.content_hash
                    THEN telegram_chat_days.last_error
                ELSE ''
            END,
            updated_at = NOW()
        WHERE telegram_chat_days.rendered_at <= EXCLUDED.rendered_at
        RETURNING revision, embedding_status
        "#,
    )
    .bind(chat_id)
    .bind(day)
    .bind(PROJECTION_VERSION)
    .bind(&chat_title)
    .bind(messages.len() as i32)
    .bind(&title)
    .bind(Json(&chunks))
    .bind(&content_hash)
    .bind(rendered_at)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let status: String = row.try_get("embedding_status")?;
    Ok((status == "pending").then(|| row.get("revision")))
}

/// Loads a chat day's messages with their authors and text rendered. Replies
/// quote the start of the message they answer when it is on the same day.
async fn load_day(pool: &PgPool, chat_id: i64, day: NaiveDate) -> Result<Vec<Message>> {
    let rows = sqlx::query(
        r#"
        SELECT message_id, sender_id, sender_name, sender_username,
               reply_to_message_id, text, media, sent_at
        FROM company_context_system.telegram_messages
        WHERE chat_id = $1
          AND projection_day = $2
        ORDER BY sent_at, message_id
        "#,
    )
    .bind(chat_id)
    .bind(day)
    .fetch_all(pool)
    .await?;

    let mut messages = Vec::with_capacity(rows.len());
    let mut by_id: HashMap<i64, (String, String)> = HashMap::new();
    for row in rows {
        let message_id: i64 = row.try_get("message_id")?;
        let sender_id: Option<i64> = row.try_get("sender_id")?;
        let sender_name: String = row.try_get("sender_name")?;
        let sender_username: String = row.try_get("sender_username")?;
        let author = author_name(&sender_name, &sender_username, sender_id);
        let Json(media): Json<Value> = row.try_get("media")?;
        let text = message_text(&row.try_get::<String, _>("text")?, &media);
        if text.is_empty() {
            continue;
        }
        by_id.insert(message_id, (author.clone(), text.clone()));
        let reply_to: Option<i64> = row.try_get("reply_to_message_id")?;
        let text = match reply_to.and_then(|id| by_id.get(&id)) {
            Some((replied_author, replied_text)) => {
                let preview: String = replied_text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(REPLY_PREVIEW_CHARS)
                    .collect();
                format!("(reply to {replied_author}: {preview}) {text}")
            }
            None => text,
        };
        let sent_at: DateTime<Utc> = row.try_get("sent_at")?;
        messages.push(Message {
            // Ordered keys, so that the renderer keeps arrival order.
            ts: format!(
                "{:020}:{:020}",
                sent_at.timestamp_micros().max(0),
                message_id.max(0)
            ),
            thread_ts: None,
            occurred_at: sent_at,
            author,
            text,
        });
    }
    Ok(messages)
}

fn author_name(name: &str, username: &str, sender_id: Option<i64>) -> String {
    if !name.trim().is_empty() {
        name.trim().to_owned()
    } else if !username.trim().is_empty() {
        format!("@{}", username.trim())
    } else if let Some(id) = sender_id {
        id.to_string()
    } else {
        "unknown".to_owned()
    }
}

/// A message's text, followed by a label for each attached medium.
fn message_text(text: &str, media: &Value) -> String {
    let mut text = text.trim().to_owned();
    for item in media.as_array().into_iter().flatten() {
        let kind = item["kind"].as_str().unwrap_or("file");
        let label = match item["name"].as_str().filter(|name| !name.is_empty()) {
            Some(name) => format!("[{kind}: {name}]"),
            None => format!("[{kind}]"),
        };
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&label);
    }
    text
}

/// Publishes a rendered chat day: one document and embedding per chunk.
/// Vectors are reused for chunks whose text is unchanged.
async fn embed_day(
    pool: &PgPool,
    embeddings: &EmbeddingsClient,
    params: ChatDayEmbedParams,
    task_id: &str,
) -> Result<ProjectionSummary> {
    let Some(row) = sqlx::query(
        r#"
        SELECT title, chunks, chat_title
        FROM company_context_system.telegram_chat_days
        WHERE chat_id = $1
          AND day = $2
          AND revision = $3
          AND embedding_status = 'pending'
        "#,
    )
    .bind(params.chat_id)
    .bind(params.day)
    .bind(params.revision)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(ProjectionSummary::new("superseded", 0));
    };
    let title: String = row.try_get("title")?;
    let chat_title: String = row.try_get("chat_title")?;
    let Json(chunks): Json<Vec<Chunk>> = row.try_get("chunks")?;
    let documents = chunks
        .iter()
        .enumerate()
        .map(|(ordinal, chunk)| {
            let chunk_id = format!("{ordinal:06}");
            let document_id = format!(
                "{TELEGRAM_DOCUMENT_ID_PREFIX}{}:{}:{chunk_id}",
                params.chat_id, params.day
            );
            (
                document_id,
                chunk_id,
                chunk,
                hex_sha256(chunk.body.as_bytes()),
            )
        })
        .collect::<Vec<_>>();

    let reusable: BTreeSet<String> = sqlx::query_scalar(
        r#"
        SELECT embeddings.document_id
        FROM company_context_data.telegram_document_embeddings embeddings
        JOIN unnest($1::text[], $2::text[]) AS chunks(document_id, content_hash)
          ON chunks.document_id = embeddings.document_id
         AND chunks.content_hash = embeddings.content_hash
        WHERE embeddings.model = $3
          AND embeddings.dimensions = $4
        "#,
    )
    .bind(
        documents
            .iter()
            .map(|doc| doc.0.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(
        documents
            .iter()
            .map(|doc| doc.3.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(embeddings.model())
    .bind(embeddings.dimensions() as i32)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let inputs = documents
        .iter()
        .filter(|doc| !reusable.contains(&doc.0))
        .map(|doc| doc.2.body.clone())
        .collect::<Vec<_>>();
    let result = if inputs.is_empty() {
        Ok(Vec::new())
    } else {
        embeddings.embed(&inputs).await
    };
    let mut vectors = match result {
        Ok(vectors) => vectors.into_iter(),
        Err(error) => {
            let rejected = is_rejected(&error);
            record_embedding_failure(pool, &params, rejected, &error).await;
            if rejected {
                warn!(
                    event = "company_context_telegram_embedding_rejected",
                    task_id,
                    chat_id = params.chat_id,
                    day = %params.day,
                    error = %error
                );
                return Ok(ProjectionSummary::new("rejected", 0));
            }
            return Err(error);
        }
    };

    let mut tx = pool.begin().await?;
    let current = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT revision = $3 AND embedding_status = 'pending'
        FROM company_context_system.telegram_chat_days
        WHERE chat_id = $1
          AND day = $2
        FOR UPDATE
        "#,
    )
    .bind(params.chat_id)
    .bind(params.day)
    .bind(params.revision)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if !current {
        tx.rollback().await?;
        return Ok(ProjectionSummary::new("superseded", 0));
    }
    let mut document_ids = Vec::with_capacity(documents.len());
    for (document_id, chunk_id, chunk, content_hash) in &documents {
        document_ids.push(document_id.clone());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.telegram_documents (
                document_id, chat_id, day, chunk_id, title, body, chat_title,
                first_message_at, last_message_at, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (document_id) DO UPDATE
            SET title = EXCLUDED.title,
                body = EXCLUDED.body,
                chat_title = EXCLUDED.chat_title,
                first_message_at = EXCLUDED.first_message_at,
                last_message_at = EXCLUDED.last_message_at,
                content_hash = EXCLUDED.content_hash,
                updated_at = NOW()
            "#,
        )
        .bind(document_id)
        .bind(params.chat_id)
        .bind(params.day)
        .bind(chunk_id)
        .bind(&title)
        .bind(&chunk.body)
        .bind(&chat_title)
        .bind(chunk.first_message_at)
        .bind(chunk.last_message_at)
        .bind(content_hash)
        .execute(&mut *tx)
        .await?;
        if reusable.contains(document_id) {
            continue;
        }
        let vector = vectors
            .next()
            .context("embeddings response omitted a chunk")?;
        sqlx::query(
            r#"
            INSERT INTO company_context_data.telegram_document_embeddings (
                document_id, model, dimensions, content_hash, embedding
            )
            VALUES ($1, $2, $3, $4, $5::vector)
            ON CONFLICT (document_id) DO UPDATE
            SET model = EXCLUDED.model,
                dimensions = EXCLUDED.dimensions,
                content_hash = EXCLUDED.content_hash,
                embedding = EXCLUDED.embedding,
                updated_at = NOW()
            "#,
        )
        .bind(document_id)
        .bind(embeddings.model())
        .bind(embeddings.dimensions() as i32)
        .bind(content_hash)
        .bind(serde_json::to_string(&vector)?)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        r#"
        DELETE FROM company_context_data.telegram_documents
        WHERE chat_id = $1
          AND day = $2
          AND NOT (document_id = ANY($3::text[]))
        "#,
    )
    .bind(params.chat_id)
    .bind(params.day)
    .bind(&document_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.telegram_chat_days
        SET embedding_status = 'completed',
            last_error = '',
            published_at = NOW(),
            updated_at = NOW()
        WHERE chat_id = $1
          AND day = $2
        "#,
    )
    .bind(params.chat_id)
    .bind(params.day)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_telegram_chat_day_published",
        task_id,
        chat_id = params.chat_id,
        day = %params.day,
        chunks = document_ids.len()
    );
    Ok(ProjectionSummary::new("completed", 1))
}

async fn record_embedding_failure(
    pool: &PgPool,
    params: &ChatDayEmbedParams,
    rejected: bool,
    error: &anyhow::Error,
) {
    // Retryable failures stay pending so that the next attempt can publish.
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE company_context_system.telegram_chat_days
        SET embedding_status = CASE WHEN $4 THEN 'rejected' ELSE embedding_status END,
            last_error = $5,
            updated_at = NOW()
        WHERE chat_id = $1
          AND day = $2
          AND revision = $3
          AND embedding_status = 'pending'
        "#,
    )
    .bind(params.chat_id)
    .bind(params.day)
    .bind(params.revision)
    .bind(rejected)
    .bind(bounded_error(error))
    .execute(pool)
    .await
    {
        error!(
            event = "company_context_failure_record_failed",
            chat_id = params.chat_id,
            error = %db_error
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use clap::Parser;
    use sqlx::Executor;

    use super::*;
    use crate::{config::Config, test_support::TestDatabase};

    #[test]
    fn retention_keeps_the_configured_number_of_days() {
        let now = DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            retention_cutoff(now, 90),
            NaiveDate::from_ymd_opt(2026, 7, 13).unwrap()
        );
        // One day of retention keeps only today.
        assert_eq!(
            retention_cutoff(now, 1),
            NaiveDate::from_ymd_opt(2026, 10, 10).unwrap()
        );
    }

    #[test]
    fn authors_and_media_are_readable() {
        assert_eq!(author_name(" Ada ", "ada", Some(1)), "Ada");
        assert_eq!(author_name("", "ada", Some(1)), "@ada");
        assert_eq!(author_name("", "", Some(1)), "1");
        assert_eq!(author_name("", "", None), "unknown");
        assert_eq!(
            message_text(
                " see this ",
                &json!([{"kind": "photo"}, {"kind": "document", "name": "plan.pdf"}])
            ),
            "see this [photo] [document: plan.pdf]"
        );
        assert_eq!(message_text("", &json!([])), "");
    }

    async fn fake_embeddings(
        axum::extract::State(inputs): axum::extract::State<Arc<AtomicUsize>>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        let count = body["input"].as_array().unwrap().len();
        inputs.fetch_add(count, Ordering::SeqCst);
        axum::Json(json!({
            "data": (0..count)
                .map(|index| json!({ "index": index, "embedding": vec![0.5; 1536] }))
                .collect::<Vec<_>>()
        }))
    }

    async fn bodies(pool: &PgPool) -> Vec<(String, String)> {
        sqlx::query_as(
            r#"
            SELECT documents.document_id, documents.body
            FROM company_context_data.telegram_documents documents
            JOIN company_context_data.telegram_document_embeddings embeddings
              USING (document_id)
            ORDER BY 1
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn chat_days_are_published_follow_new_messages_and_expire() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "telegram_documents").await;
        let pool = &database.pool;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let inputs = Arc::new(AtomicUsize::new(0));
        let router = axum::Router::new()
            .route("/embeddings", axum::routing::post(fake_embeddings))
            .with_state(inputs.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let config = Config::try_parse_from([
            "centaur-company-context",
            "--database-url",
            "postgresql://context",
            "--console-database-url",
            "postgresql://console",
            "--active-record-primary-key",
            "primary",
            "--active-record-key-derivation-salt",
            "salt",
            "--openai-api-key",
            "test-key",
            "--openai-base-url",
            &format!("http://{address}"),
            "--slack-bot-token",
            "xoxb-test",
            "--jwt-signing-secret",
            "secret",
        ])
        .unwrap();
        let embeddings = EmbeddingsClient::new(&config).unwrap();

        // 2023-11-14 22:13:20 UTC and the next day.
        pool.execute(
            r#"
            INSERT INTO company_context_system.telegram_messages
                (chat_id, message_id, chat_title, sender_id, sender_name, sender_username,
                 reply_to_message_id, text, media, sent_at)
            VALUES
                (-100, 1, 'Ops', 11, 'Ada', 'ada', NULL, 'ship it?', '[]', to_timestamp(1700000000)),
                (-100, 2, 'Ops', 12, '', 'bob', 1, 'shipped', '[{"kind": "photo"}]', to_timestamp(1700000060)),
                (-100, 3, 'Ops', 13, '', '', NULL, '', '[]', to_timestamp(1700000120)),
                (-100, 4, 'Ops', 11, 'Ada', 'ada', NULL, 'next day', '[]', to_timestamp(1700090000)),
                (-200, 1, 'Other', 21, 'Cy', '', NULL, 'elsewhere', '[]', to_timestamp(1700000000));
            "#,
        )
        .await
        .unwrap();
        let day1 = NaiveDate::from_ymd_opt(2023, 11, 14).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2023, 11, 15).unwrap();
        let cutoff = NaiveDate::from_ymd_opt(2023, 1, 1).unwrap();
        assert_eq!(
            stale_days(pool, None, cutoff).await.unwrap(),
            [(-200, day1), (-100, day1), (-100, day2)]
        );
        assert_eq!(
            stale_days(pool, Some(-100), cutoff).await.unwrap(),
            [(-100, day1), (-100, day2)]
        );

        let publish = |day, revision| {
            embed_day(
                pool,
                &embeddings,
                ChatDayEmbedParams {
                    chat_id: -100,
                    day,
                    revision,
                },
                "task",
            )
        };
        for day in [day1, day2] {
            assert_eq!(project_day(pool, -100, day, 6000).await.unwrap(), Some(1));
            assert_eq!(publish(day, 1).await.unwrap().status, "completed");
        }
        assert!(
            stale_days(pool, Some(-100), cutoff)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            bodies(pool).await,
            [
                (
                    "telegram:-100:2023-11-14:000000".to_owned(),
                    "Ops (Telegram) · 2023-11-14 · 22:13–22:14 UTC\n\n\
                     [22:13] Ada: ship it?\n\n\
                     [22:14] @bob: (reply to Ada: ship it?) shipped [photo]"
                        .to_owned()
                ),
                (
                    "telegram:-100:2023-11-15:000000".to_owned(),
                    "Ops (Telegram) · 2023-11-15 · 23:13–23:13 UTC\n\n[23:13] Ada: next day"
                        .to_owned()
                ),
            ]
        );
        assert_eq!(inputs.load(Ordering::SeqCst), 2);

        // A new message republishes only its day.
        pool.execute(
            r#"
            INSERT INTO company_context_system.telegram_messages
                (chat_id, message_id, chat_title, sender_id, sender_name, text, sent_at)
            VALUES (-100, 5, 'Ops', 12, 'Bob', 'late reply', to_timestamp(1700000300))
            "#,
        )
        .await
        .unwrap();
        assert_eq!(
            stale_days(pool, Some(-100), cutoff).await.unwrap(),
            [(-100, day1)]
        );
        assert_eq!(project_day(pool, -100, day1, 6000).await.unwrap(), Some(2));
        assert_eq!(publish(day1, 1).await.unwrap().status, "superseded");
        assert_eq!(publish(day1, 2).await.unwrap().status, "completed");
        assert!(bodies(pool).await[0].1.ends_with("[22:18] Bob: late reply"));
        assert_eq!(inputs.load(Ordering::SeqCst), 3);

        // Renaming the chat renders its days again.
        pool.execute(
            r#"
            INSERT INTO company_context_system.telegram_messages
                (chat_id, message_id, chat_title, sender_id, sender_name, text, sent_at)
            VALUES (-100, 6, 'Ops Team', 11, 'Ada', 'renamed', to_timestamp(1700090100))
            "#,
        )
        .await
        .unwrap();
        assert_eq!(
            stale_days(pool, Some(-100), cutoff).await.unwrap(),
            [(-100, day1), (-100, day2)]
        );

        // Expiry removes old days with their documents and embeddings.
        assert_eq!(expire(pool, day2).await.unwrap(), (5, 1));
        assert!(
            bodies(pool)
                .await
                .iter()
                .all(|(id, _)| id.starts_with("telegram:-100:2023-11-15:"))
        );
        let vectors: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM company_context_data.telegram_document_embeddings",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(vectors, 1);
        assert_eq!(stale_days(pool, None, day2).await.unwrap(), [(-100, day2)]);

        server.abort();
        database.drop().await;
    }
}
