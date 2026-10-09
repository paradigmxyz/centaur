//! Projects staged Slack messages into retrieval documents. Each channel day
//! is rendered as a transcript with thread replies under their parent, split
//! into chunks that together cover the whole day. Every chunk is a document
//! with its own embedding, so no part of a busy day is lost to truncation.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use absurd::{SpawnOptions, TaskContext};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row, types::Json};
use tracing::{error, info, warn};

use crate::{
    config::{
        SLACK_CHANNEL_DAY_EMBED_TASK, SLACK_CONVERSATION_PROJECT_TASK, SLACK_DOCUMENT_ID_PREFIX,
    },
    embeddings::EmbeddingsClient,
    errors::is_rejected,
    extraction::{hex_sha256, split_long_text},
    slack_files,
    tasks::{TaskState, bounded_error, run_task},
};

/// Bump to render every channel day again on its conversation's next sync.
/// Chunks whose text is unchanged keep their vectors. Version 2 records the
/// days' file shares.
const PROJECTION_VERSION: i32 = 2;
/// Message subtypes that carry conversation content. Others, such as joins
/// and topic changes, are not rendered.
const RENDERED_SUBTYPES: [&str; 4] = [
    "bot_message",
    "file_share",
    "me_message",
    "thread_broadcast",
];
/// How much of a thread's parent message is repeated where the thread
/// continues in another chunk.
const CONTINUED_PREVIEW_CHARS: usize = 200;

#[derive(Debug, Deserialize, Serialize)]
pub struct ConversationProjectParams {
    pub conversation_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChannelDayEmbedParams {
    pub conversation_id: String,
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
    let project_state = state.clone();
    state.absurd.register_task(
        SLACK_CONVERSATION_PROJECT_TASK,
        move |params: ConversationProjectParams, ctx| {
            let state = project_state.clone();
            async move { run_task(&ctx, project_conversation(&state, params, &ctx)).await }
        },
    )?;

    let embed_state = state.clone();
    state.absurd.register_task(
        SLACK_CHANNEL_DAY_EMBED_TASK,
        move |params: ChannelDayEmbedParams, ctx| {
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

/// Renders a conversation's stale days and enqueues publication of the ones
/// whose content changed.
async fn project_conversation(
    state: &TaskState,
    params: ConversationProjectParams,
    ctx: &TaskContext,
) -> Result<ProjectionSummary> {
    let conversation_id = params.conversation_id.as_str();
    let days = stale_days(&state.pool, conversation_id).await?;
    let mut pending = 0;
    for day in &days {
        let Some(revision) =
            project_day(&state.pool, conversation_id, *day, state.config.chunk_chars).await?
        else {
            continue;
        };
        state
            .absurd
            .spawn(
                SLACK_CHANNEL_DAY_EMBED_TASK,
                ChannelDayEmbedParams {
                    conversation_id: conversation_id.to_owned(),
                    day: *day,
                    revision,
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "slack.channel_day.embed:{conversation_id}:{day}:{revision}:{}",
                        state.embeddings.model()
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        pending += 1;
    }
    let files = slack_files::spawn_due(state, conversation_id).await?;
    info!(
        event = "company_context_slack_conversation_projected",
        task_id = ctx.task_id(),
        conversation_id,
        days_rendered = days.len(),
        days_pending = pending,
        files_pending = files
    );
    Ok(ProjectionSummary::new("completed", pending))
}

/// Returns the channel days to render: those never rendered, rendered before
/// a message, the channel name, or a rendered user name changed, rendered by
/// another projection version, or still awaiting publication.
async fn stale_days(pool: &PgPool, conversation_id: &str) -> Result<Vec<NaiveDate>> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT messages.projection_day
        FROM company_context_system.slack_messages messages
        JOIN company_context_system.slack_conversations conversations
          ON conversations.conversation_id = messages.conversation_id
        LEFT JOIN company_context_system.slack_channel_days days
          ON days.conversation_id = messages.conversation_id
         AND days.day = messages.projection_day
        WHERE messages.conversation_id = $1
        GROUP BY messages.projection_day, conversations.conversation_id,
                 days.conversation_id, days.day
        HAVING days.day IS NULL
            OR max(messages.updated_at) > days.rendered_at
            OR days.projection_version <> $2
            OR days.channel_name <> conversations.name
            OR days.embedding_status = 'pending'
            OR EXISTS (
                SELECT 1
                FROM company_context_system.slack_users users
                WHERE users.user_id = ANY(days.user_ids)
                  AND users.updated_at > days.rendered_at
            )
        ORDER BY 1
        "#,
    )
    .bind(conversation_id)
    .bind(PROJECTION_VERSION)
    .fetch_all(pool)
    .await?)
}

/// Renders one channel day. Returns the revision to publish when the day is
/// awaiting publication.
async fn project_day(
    pool: &PgPool,
    conversation_id: &str,
    day: NaiveDate,
    max_chars: usize,
) -> Result<Option<i64>> {
    let Some(conversation) = sqlx::query(
        r#"
        SELECT name, NOW() AS rendered_at
        FROM company_context_system.slack_conversations
        WHERE conversation_id = $1
        "#,
    )
    .bind(conversation_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let channel_name: String = conversation.try_get("name")?;
    let rendered_at: DateTime<Utc> = conversation.try_get("rendered_at")?;
    let (messages, user_ids, shares) = load_day(pool, conversation_id, day).await?;
    let label = if channel_name.is_empty() {
        conversation_id
    } else {
        &channel_name
    };
    let title = format!("#{label} — {day}");
    let chunks = render_day(label, day, &messages, max_chars);
    let content_hash = hex_sha256(
        json!([PROJECTION_VERSION, title, chunks])
            .to_string()
            .as_bytes(),
    );

    let mut tx = pool.begin().await?;
    // Keep reconciliation from removing the conversation meanwhile.
    let observed: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM company_context_system.slack_conversations
        WHERE conversation_id = $1
        FOR KEY SHARE
        "#,
    )
    .bind(conversation_id)
    .fetch_optional(&mut *tx)
    .await?;
    if observed.is_none() {
        return Ok(None);
    }
    // A rendering older than the stored one is discarded, so a slow
    // projection cannot overwrite a newer one.
    let row = sqlx::query(
        r#"
        INSERT INTO company_context_system.slack_channel_days (
            conversation_id, day, projection_version, channel_name, user_ids,
            message_count, title, chunks, content_hash, rendered_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT (conversation_id, day) DO UPDATE
        SET projection_version = EXCLUDED.projection_version,
            channel_name = EXCLUDED.channel_name,
            user_ids = EXCLUDED.user_ids,
            message_count = EXCLUDED.message_count,
            title = EXCLUDED.title,
            chunks = EXCLUDED.chunks,
            content_hash = EXCLUDED.content_hash,
            rendered_at = EXCLUDED.rendered_at,
            revision = CASE
                WHEN slack_channel_days.content_hash = EXCLUDED.content_hash
                    THEN slack_channel_days.revision
                ELSE slack_channel_days.revision + 1
            END,
            embedding_status = CASE
                WHEN slack_channel_days.content_hash = EXCLUDED.content_hash
                    THEN slack_channel_days.embedding_status
                ELSE 'pending'
            END,
            last_error = CASE
                WHEN slack_channel_days.content_hash = EXCLUDED.content_hash
                    THEN slack_channel_days.last_error
                ELSE ''
            END,
            updated_at = NOW()
        WHERE slack_channel_days.rendered_at <= EXCLUDED.rendered_at
        RETURNING revision, embedding_status
        "#,
    )
    .bind(conversation_id)
    .bind(day)
    .bind(PROJECTION_VERSION)
    .bind(&channel_name)
    .bind(&user_ids)
    .bind(messages.len() as i32)
    .bind(&title)
    .bind(Json(&chunks))
    .bind(&content_hash)
    .bind(rendered_at)
    .fetch_optional(&mut *tx)
    .await?;
    if row.is_some() {
        slack_files::record_shares(&mut tx, conversation_id, day, &shares).await?;
    }
    tx.commit().await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let status: String = row.try_get("embedding_status")?;
    Ok((status == "pending").then(|| row.get("revision")))
}

/// A staged message with its author and text rendered.
#[derive(Clone, Debug)]
struct Message {
    ts: String,
    thread_ts: Option<String>,
    occurred_at: DateTime<Utc>,
    author: String,
    text: String,
}

/// A file shared by a message: its timestamp and the file as Slack listed it.
type FileShare = (String, Value);

/// Loads a channel day's rendered messages, the users they name, and the
/// files they share.
async fn load_day(
    pool: &PgPool,
    conversation_id: &str,
    day: NaiveDate,
) -> Result<(Vec<Message>, Vec<String>, Vec<FileShare>)> {
    let rows = sqlx::query(
        r#"
        SELECT message_ts, thread_ts, user_id, bot_id, text, occurred_at,
               COALESCE(raw_payload->>'username', raw_payload->'bot_profile'->>'name', '')
                   AS bot_name,
               COALESCE(raw_payload->'files', '[]'::jsonb) AS files
        FROM company_context_system.slack_messages
        WHERE conversation_id = $1
          AND projection_day = $2
          AND (subtype IS NULL OR subtype = ANY($3::text[]))
        ORDER BY occurred_at, message_ts
        "#,
    )
    .bind(conversation_id)
    .bind(day)
    .bind(RENDERED_SUBTYPES)
    .fetch_all(pool)
    .await?;

    let mut user_ids = BTreeSet::new();
    let mut channel_ids = BTreeSet::new();
    for row in &rows {
        let user_id: String = row.try_get("user_id")?;
        if !user_id.is_empty() {
            user_ids.insert(user_id);
        }
        let text: String = row.try_get("text")?;
        scan_tags(&text, |tag| {
            let target = tag.split_once('|').map_or(tag, |(target, _)| target);
            if let Some(id) = target.strip_prefix('@') {
                user_ids.insert(id.to_owned());
            } else if let Some(id) = target.strip_prefix('#') {
                channel_ids.insert(id.to_owned());
            }
            String::new()
        });
    }
    let user_ids: Vec<String> = user_ids.into_iter().collect();
    let channel_ids: Vec<String> = channel_ids.into_iter().collect();
    let users: HashMap<String, String> = sqlx::query_as(
        r#"
        SELECT user_id,
               COALESCE(NULLIF(real_name, ''), NULLIF(display_name, ''), NULLIF(name, ''), user_id)
        FROM company_context_system.slack_users
        WHERE user_id = ANY($1::text[])
        "#,
    )
    .bind(&user_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let channels: HashMap<String, String> = sqlx::query_as(
        r#"
        SELECT conversation_id, name
        FROM company_context_system.slack_conversations
        WHERE conversation_id = ANY($1::text[])
          AND name <> ''
        "#,
    )
    .bind(&channel_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    let mut messages = Vec::with_capacity(rows.len());
    let mut shares = Vec::new();
    for row in rows {
        let ts: String = row.try_get("message_ts")?;
        let user_id: String = row.try_get("user_id")?;
        let bot_id: String = row.try_get("bot_id")?;
        let bot_name: String = row.try_get("bot_name")?;
        let author = [
            users.get(&user_id).map(String::as_str).unwrap_or_default(),
            &user_id,
            &bot_name,
            &bot_id,
        ]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_owned();
        let Json(files): Json<Value> = row.try_get("files")?;
        let mut text = format_text(&row.try_get::<String, _>("text")?, &users, &channels)
            .trim()
            .to_owned();
        for file in files.as_array().into_iter().flatten() {
            shares.push((ts.clone(), file.clone()));
            let name = ["title", "name"]
                .into_iter()
                .filter_map(|key| file[key].as_str())
                .find(|name| !name.is_empty());
            if let Some(name) = name {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&format!("[file: {name}]"));
            }
        }
        if text.is_empty() {
            continue;
        }
        messages.push(Message {
            ts,
            thread_ts: row.try_get("thread_ts")?,
            occurred_at: row.try_get("occurred_at")?,
            author,
            text,
        });
    }
    Ok((messages, user_ids, shares))
}

/// Replaces each `<...>` tag in Slack message text with `render(tag)`.
fn scan_tags(text: &str, mut render: impl FnMut(&str) -> String) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(length) = rest[start..].find('>') else {
            break;
        };
        output.push_str(&rest[..start]);
        output.push_str(&render(&rest[start + 1..start + length]));
        rest = &rest[start + length + 1..];
    }
    output.push_str(rest);
    output
}

/// Renders Slack mrkdwn mentions, links, and escapes as readable text.
fn format_text(
    text: &str,
    users: &HashMap<String, String>,
    channels: &HashMap<String, String>,
) -> String {
    scan_tags(text, |tag| {
        let (target, label) = match tag.split_once('|') {
            Some((target, label)) => (target, Some(label).filter(|label| !label.is_empty())),
            None => (tag, None),
        };
        if let Some(id) = target.strip_prefix('@') {
            let name = users.get(id).map(String::as_str).or(label).unwrap_or(id);
            format!("@{name}")
        } else if let Some(id) = target.strip_prefix('#') {
            let name = label.or(channels.get(id).map(String::as_str)).unwrap_or(id);
            format!("#{name}")
        } else if let Some(command) = target.strip_prefix('!') {
            label.map_or_else(|| format!("@{command}"), str::to_owned)
        } else {
            match label {
                Some(label) if label != target => format!("{label} ({target})"),
                _ => target.to_owned(),
            }
        }
    })
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&amp;", "&")
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Chunk {
    body: String,
    first_message_at: DateTime<Utc>,
    last_message_at: DateTime<Utc>,
}

/// Rendered text spanning messages from `first` to `last`.
#[derive(Debug)]
struct Piece {
    text: String,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn time_label(day: NaiveDate, at: DateTime<Utc>) -> String {
    if at.date_naive() == day {
        at.format("%H:%M").to_string()
    } else {
        at.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// Renders a channel day into chunks of at most `max_chars` characters that
/// together contain every message. Messages stay whole unless one alone
/// exceeds a chunk; a thread split across chunks repeats its parent's start.
fn render_day(channel: &str, day: NaiveDate, messages: &[Message], max_chars: usize) -> Vec<Chunk> {
    let header = |first: &str, last: &str| format!("#{channel} · {day} · {first}–{last} UTC");
    let widest_time = "0000-00-00 00:00";
    let budget = max_chars
        .saturating_sub(char_count(&header(widest_time, widest_time)) + 2)
        .max(1);

    // Group replies under the thread they belong to, in thread order.
    let mut threads: BTreeMap<&str, (Option<&Message>, Vec<&Message>)> = BTreeMap::new();
    for message in messages {
        let root = message.thread_ts.as_deref().unwrap_or(&message.ts);
        let thread = threads.entry(root).or_default();
        if message.ts == root {
            thread.0 = Some(message);
        } else {
            thread.1.push(message);
        }
    }

    let entry = |message: &Message, reply: bool| Piece {
        text: format!(
            "{}[{}] {}: {}",
            if reply { "  ↳ " } else { "" },
            time_label(day, message.occurred_at),
            message.author,
            message.text
        ),
        first: message.occurred_at,
        last: message.occurred_at,
    };
    let mut pieces = Vec::new();
    for (parent, replies) in threads.values() {
        let continued = match parent {
            Some(parent) => {
                let preview: String = parent
                    .text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(CONTINUED_PREVIEW_CHARS)
                    .collect();
                format!("(thread continued) {}: {preview}", parent.author)
            }
            None => "(thread continued)".to_owned(),
        };
        let entries = parent
            .map(|parent| entry(parent, false))
            .into_iter()
            .chain(replies.iter().map(|reply| entry(reply, true)));
        let mut current: Option<Piece> = None;
        for (index, entry) in entries.enumerate() {
            if let Some(piece) = &mut current {
                if char_count(&piece.text) + 1 + char_count(&entry.text) <= budget {
                    piece.text.push('\n');
                    piece.text.push_str(&entry.text);
                    piece.last = entry.last;
                    continue;
                }
                pieces.extend(current.take());
            }
            let text = if index == 0 {
                entry.text.clone()
            } else {
                format!("{continued}\n{}", entry.text)
            };
            if char_count(&text) <= budget {
                current = Some(Piece { text, ..entry });
            } else {
                let mut parts = Vec::new();
                split_long_text(&entry.text, budget, &mut parts);
                pieces.extend(parts.into_iter().map(|text| Piece {
                    text,
                    first: entry.first,
                    last: entry.last,
                }));
            }
        }
        pieces.extend(current);
    }

    let mut chunks = Vec::new();
    let mut current: Option<Piece> = None;
    for piece in pieces {
        if let Some(chunk) = &mut current {
            if char_count(&chunk.text) + 2 + char_count(&piece.text) <= budget {
                chunk.text.push_str("\n\n");
                chunk.text.push_str(&piece.text);
                chunk.first = chunk.first.min(piece.first);
                chunk.last = chunk.last.max(piece.last);
                continue;
            }
            chunks.extend(current.take());
        }
        current = Some(piece);
    }
    chunks.extend(current);
    chunks
        .into_iter()
        .map(|chunk| Chunk {
            body: format!(
                "{}\n\n{}",
                header(&time_label(day, chunk.first), &time_label(day, chunk.last)),
                chunk.text
            ),
            first_message_at: chunk.first,
            last_message_at: chunk.last,
        })
        .collect()
}

/// Publishes a rendered channel day: one document and embedding per chunk.
/// Vectors are reused for chunks whose text is unchanged.
async fn embed_day(
    pool: &PgPool,
    embeddings: &EmbeddingsClient,
    params: ChannelDayEmbedParams,
    task_id: &str,
) -> Result<ProjectionSummary> {
    let Some(row) = sqlx::query(
        r#"
        SELECT days.title, days.chunks, days.channel_name, conversations.kind
        FROM company_context_system.slack_channel_days days
        JOIN company_context_system.slack_conversations conversations
          ON conversations.conversation_id = days.conversation_id
        WHERE days.conversation_id = $1
          AND days.day = $2
          AND days.revision = $3
          AND days.embedding_status = 'pending'
        "#,
    )
    .bind(&params.conversation_id)
    .bind(params.day)
    .bind(params.revision)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(ProjectionSummary::new("superseded", 0));
    };
    let title: String = row.try_get("title")?;
    let channel_name: String = row.try_get("channel_name")?;
    let kind: String = row.try_get("kind")?;
    let Json(chunks): Json<Vec<Chunk>> = row.try_get("chunks")?;
    let documents = chunks
        .iter()
        .enumerate()
        .map(|(ordinal, chunk)| {
            let chunk_id = format!("{ordinal:06}");
            let document_id = format!(
                "{SLACK_DOCUMENT_ID_PREFIX}{}:{}:{chunk_id}",
                params.conversation_id, params.day
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
        FROM company_context_data.slack_document_embeddings embeddings
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
                    event = "company_context_slack_embedding_rejected",
                    task_id,
                    conversation_id = params.conversation_id,
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
        FROM company_context_system.slack_channel_days
        WHERE conversation_id = $1
          AND day = $2
        FOR UPDATE
        "#,
    )
    .bind(&params.conversation_id)
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
            INSERT INTO company_context_data.slack_documents (
                document_id, conversation_id, day, chunk_id, title, body,
                channel_name, conversation_kind, first_message_at,
                last_message_at, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (document_id) DO UPDATE
            SET title = EXCLUDED.title,
                body = EXCLUDED.body,
                channel_name = EXCLUDED.channel_name,
                conversation_kind = EXCLUDED.conversation_kind,
                first_message_at = EXCLUDED.first_message_at,
                last_message_at = EXCLUDED.last_message_at,
                content_hash = EXCLUDED.content_hash,
                updated_at = NOW()
            "#,
        )
        .bind(document_id)
        .bind(&params.conversation_id)
        .bind(params.day)
        .bind(chunk_id)
        .bind(&title)
        .bind(&chunk.body)
        .bind(&channel_name)
        .bind(&kind)
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
            INSERT INTO company_context_data.slack_document_embeddings (
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
        DELETE FROM company_context_data.slack_documents
        WHERE conversation_id = $1
          AND day = $2
          AND NOT (document_id = ANY($3::text[]))
        "#,
    )
    .bind(&params.conversation_id)
    .bind(params.day)
    .bind(&document_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.slack_channel_days
        SET embedding_status = 'completed',
            last_error = '',
            published_at = NOW(),
            updated_at = NOW()
        WHERE conversation_id = $1
          AND day = $2
        "#,
    )
    .bind(&params.conversation_id)
    .bind(params.day)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_slack_channel_day_published",
        task_id,
        conversation_id = params.conversation_id,
        day = %params.day,
        chunks = document_ids.len()
    );
    Ok(ProjectionSummary::new("completed", 1))
}

async fn record_embedding_failure(
    pool: &PgPool,
    params: &ChannelDayEmbedParams,
    rejected: bool,
    error: &anyhow::Error,
) {
    // Retryable failures stay pending so that the next attempt can publish.
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE company_context_system.slack_channel_days
        SET embedding_status = CASE WHEN $4 THEN 'rejected' ELSE embedding_status END,
            last_error = $5,
            updated_at = NOW()
        WHERE conversation_id = $1
          AND day = $2
          AND revision = $3
          AND embedding_status = 'pending'
        "#,
    )
    .bind(&params.conversation_id)
    .bind(params.day)
    .bind(params.revision)
    .bind(rejected)
    .bind(bounded_error(error))
    .execute(pool)
    .await
    {
        error!(
            event = "company_context_failure_record_failed",
            conversation_id = params.conversation_id,
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

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn message(ts: i64, thread_ts: Option<i64>, author: &str, text: &str) -> Message {
        Message {
            ts: format!("{ts}.000000"),
            thread_ts: thread_ts.map(|ts| format!("{ts}.000000")),
            occurred_at: at(ts),
            author: author.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn chunks_cover_the_whole_day_within_the_limit() {
        // 2023-11-14 22:13:20 UTC.
        let start = 1_700_000_000;
        let day = at(start).date_naive();
        let mut messages = vec![message(start, None, "Ada", "Kickoff for the launch thread")];
        for reply in 1..=40 {
            messages.push(message(
                start + reply * 180,
                Some(start),
                "Bob",
                &format!("reply {reply} with some detail\nacross two lines"),
            ));
        }
        for top in 1..=30 {
            messages.push(message(
                start + 3_000 + top,
                None,
                "Cy",
                &format!("standalone message {top}"),
            ));
        }
        let oversized = "x".repeat(900);
        messages.push(message(start + 4_000, None, "Dee", &oversized));
        messages.sort_by(|left, right| left.ts.cmp(&right.ts));

        let max_chars = 500;
        let chunks = render_day("general", day, &messages, max_chars);
        assert!(chunks.len() > 3);
        for chunk in &chunks {
            assert!(char_count(&chunk.body) <= max_chars, "{}", chunk.body);
            assert!(chunk.body.starts_with("#general · 2023-11-14 · "));
            assert!(chunk.first_message_at <= chunk.last_message_at);
        }
        let all = chunks
            .iter()
            .map(|chunk| chunk.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Every message that fits in a chunk appears whole in exactly one.
        for message in messages.iter().filter(|message| message.text != oversized) {
            let line = format!("] {}: {}\n", message.author, message.text);
            assert_eq!(
                chunks
                    .iter()
                    .filter(|chunk| format!("{}\n", chunk.body).contains(&line))
                    .count(),
                1,
                "{line}"
            );
        }
        // Replies are rendered under their parent, from the next day too.
        assert!(all.contains("  ↳ [2023-11-15 00:13] Bob: reply 40"));
        // A thread continued in another chunk repeats its parent's start.
        assert!(all.contains("(thread continued) Ada: Kickoff for the launch thread\n  ↳ "));
        // An oversized message is split but kept whole across chunks.
        assert_eq!(all.matches('x').count(), oversized.len());
    }

    #[test]
    fn mentions_links_and_escapes_are_readable() {
        let users = HashMap::from([("U1".to_owned(), "Ada Lovelace".to_owned())]);
        let channels = HashMap::from([("C1".to_owned(), "general".to_owned())]);
        assert_eq!(
            format_text(
                "<@U1> and <@U2|bob> in <#C1> and <#C2|ops>: see <https://example.com|the doc> or <https://example.com> <!here> &lt;b&gt; &amp;",
                &users,
                &channels,
            ),
            "@Ada Lovelace and @bob in #general and #ops: see the doc (https://example.com) or https://example.com @here <b> &"
        );
        assert_eq!(
            format_text("unclosed <tag", &users, &channels),
            "unclosed <tag"
        );
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
            FROM company_context_data.slack_documents documents
            JOIN company_context_data.slack_document_embeddings embeddings
              USING (document_id)
            ORDER BY 1
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn channel_days_are_published_and_follow_their_messages_and_names() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "slack_documents").await;
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
            "jwt-secret",
        ])
        .unwrap();
        let embeddings = EmbeddingsClient::new(&config).unwrap();

        // 2023-11-14 22:13:20 UTC; the reply lands on the next UTC day.
        pool.execute(
            r#"
            INSERT INTO company_context_system.slack_conversations (conversation_id, team_id, kind, name)
            VALUES ('C1', 'T1', 'public_channel', 'general');
            INSERT INTO company_context_system.slack_messages
                (conversation_id, message_ts, thread_ts, user_id, subtype, text, occurred_at, raw_payload)
            VALUES
                ('C1', '1700000000.000000', '1700000000.000000', 'U1', NULL, 'ship it <@U2>?', to_timestamp(1700000000), '{}'),
                ('C1', '1700007200.000000', '1700000000.000000', 'U2', NULL, 'shipped', to_timestamp(1700007200), '{}'),
                ('C1', '1700000100.000000', NULL, 'U3', 'channel_join', 'joined', to_timestamp(1700000100), '{}'),
                ('C1', '1700090000.000000', NULL, 'U1', NULL, 'next day', to_timestamp(1700090000), '{}');
            "#,
        )
        .await
        .unwrap();
        let day1 = NaiveDate::from_ymd_opt(2023, 11, 14).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2023, 11, 15).unwrap();
        assert_eq!(stale_days(pool, "C1").await.unwrap(), [day1, day2]);

        let publish = |day, revision| {
            embed_day(
                pool,
                &embeddings,
                ChannelDayEmbedParams {
                    conversation_id: "C1".to_owned(),
                    day,
                    revision,
                },
                "task",
            )
        };
        for day in [day1, day2] {
            assert_eq!(project_day(pool, "C1", day, 6000).await.unwrap(), Some(1));
            assert_eq!(publish(day, 1).await.unwrap().status, "completed");
        }
        assert!(stale_days(pool, "C1").await.unwrap().is_empty());
        assert_eq!(
            bodies(pool).await,
            [
                (
                    "slack:C1:2023-11-14:000000".to_owned(),
                    "#general · 2023-11-14 · 22:13–2023-11-15 00:13 UTC\n\n\
                     [22:13] U1: ship it @U2?\n  ↳ [2023-11-15 00:13] U2: shipped"
                        .to_owned()
                ),
                (
                    "slack:C1:2023-11-15:000000".to_owned(),
                    "#general · 2023-11-15 · 23:13–23:13 UTC\n\n[23:13] U1: next day".to_owned()
                ),
            ]
        );
        assert_eq!(inputs.load(Ordering::SeqCst), 2);

        // Rereading unchanged messages renders the day again without
        // republishing it.
        pool.execute("UPDATE company_context_system.slack_messages SET updated_at = NOW()")
            .await
            .unwrap();
        assert_eq!(stale_days(pool, "C1").await.unwrap(), [day1, day2]);
        assert_eq!(project_day(pool, "C1", day1, 6000).await.unwrap(), None);
        assert_eq!(project_day(pool, "C1", day2, 6000).await.unwrap(), None);

        // Learning a rendered user's name republishes only the days naming them.
        pool.execute(
            r#"
            INSERT INTO company_context_system.slack_users (user_id, team_id, name, real_name)
            VALUES ('U2', 'T1', 'bob', 'Bob Builder')
            "#,
        )
        .await
        .unwrap();
        assert_eq!(stale_days(pool, "C1").await.unwrap(), [day1]);
        assert_eq!(project_day(pool, "C1", day1, 6000).await.unwrap(), Some(2));
        assert_eq!(publish(day1, 1).await.unwrap().status, "superseded");
        assert_eq!(publish(day1, 2).await.unwrap().status, "completed");
        assert!(bodies(pool).await[0].1.ends_with(
            "[22:13] U1: ship it @Bob Builder?\n  ↳ [2023-11-15 00:13] Bob Builder: shipped"
        ));
        assert_eq!(inputs.load(Ordering::SeqCst), 3);

        // Documents and embeddings go with their conversation.
        pool.execute(
            "DELETE FROM company_context_system.slack_conversations WHERE conversation_id = 'C1'",
        )
        .await
        .unwrap();
        assert!(bodies(pool).await.is_empty());
        let vectors: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM company_context_data.slack_document_embeddings",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(vectors, 0);

        server.abort();
        database.drop().await;
    }
}
