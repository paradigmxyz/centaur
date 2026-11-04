//! Indexes files attached to projected Slack messages. Projection
//! records which messages share each file; each file is then downloaded with
//! a credential that can read files in one of its conversations, its text is
//! extracted and chunked, and every chunk is published as its own document
//! with its own embedding. Downloaded bytes are discarded after extraction.

use std::collections::{BTreeMap, BTreeSet};

use absurd::{SpawnOptions, TaskContext};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction, types::Json};
use tracing::{error, info, warn};

use crate::{
    config::{
        Config, PDF_MIME_TYPE, SLACK_FILE_DOCUMENT_ID_PREFIX, SLACK_FILE_EMBED_TASK,
        SLACK_FILE_EXTRACT_TASK,
    },
    embeddings::EmbeddingsClient,
    errors::{is_denied, is_rejected, rejected},
    extraction::{
        chunk_text, extract_pandoc_text, extract_pdf_text, extract_plain_text, hex_sha256,
        pandoc_reader,
    },
    slack::{SlackClient, SlackFile},
    tasks::{TaskState, bounded_error, run_task},
};

/// Version of a file Slack listed without its details, which a later listing
/// with details replaces.
const INCOMPLETE_VERSION: &str = "incomplete";

#[derive(Debug, Deserialize, Serialize)]
pub struct FileExtractParams {
    pub file_id: String,
    pub source_version: String,
    pub credential_id: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FileEmbedParams {
    pub file_id: String,
    pub source_version: String,
    pub content_hash: String,
}

#[derive(Debug, Serialize)]
pub struct FileSummary {
    status: &'static str,
    chunks: usize,
}

impl FileSummary {
    fn new(status: &'static str, chunks: usize) -> Self {
        Self { status, chunks }
    }
}

/// How a file's text is extracted.
#[derive(Debug, PartialEq)]
enum Format {
    Pdf,
    Pandoc(&'static str),
    Text,
}

#[derive(Debug, PartialEq)]
enum Classification {
    Supported(Format),
    Unsupported(String),
    Deleted,
}

fn classify(file: &SlackFile) -> Classification {
    if file.mode == "tombstone" {
        return Classification::Deleted;
    }
    if file.mode.is_empty() {
        return Classification::Unsupported("Slack did not include the file's details".to_owned());
    }
    if !matches!(file.mode.as_str(), "hosted" | "snippet") {
        return Classification::Unsupported(format!("Slack {} files are not indexed", file.mode));
    }
    if file.url_private_download.is_empty() {
        return Classification::Unsupported(
            "Slack did not include the file's download URL".to_owned(),
        );
    }
    let format = if file.filetype == "pdf" || file.mimetype == PDF_MIME_TYPE {
        Format::Pdf
    } else if let Some(reader) = pandoc_reader(&file.filetype, &file.mimetype) {
        Format::Pandoc(reader)
    } else if file.mode == "snippet"
        || (file.mimetype.starts_with("text/") && file.mimetype != "text/html")
        || file.mimetype == "application/json"
    {
        Format::Text
    } else {
        return Classification::Unsupported(format!(
            "{} files are not indexed",
            [&file.filetype, &file.mimetype]
                .into_iter()
                .find(|kind| !kind.is_empty())
                .map_or("these", String::as_str)
        ));
    };
    Classification::Supported(format)
}

/// Identifies the content Slack reports for a file, so a change to it is
/// extracted again.
fn source_version(file: &SlackFile) -> String {
    if file.mode.is_empty() {
        return INCOMPLETE_VERSION.to_owned();
    }
    hex_sha256(
        json!([
            file.mode,
            file.name,
            file.title,
            file.mimetype,
            file.filetype,
            file.size,
            file.url_private_download
        ])
        .to_string()
        .as_bytes(),
    )
}

pub fn register(state: &TaskState) -> Result<()> {
    let extract_state = state.clone();
    state.absurd.register_task(
        SLACK_FILE_EXTRACT_TASK,
        move |params: FileExtractParams, ctx| {
            let state = extract_state.clone();
            async move { run_task(&ctx, extract_file(&state, params, &ctx)).await }
        },
    )?;

    let embed_state = state.clone();
    state.absurd.register_task(
        SLACK_FILE_EMBED_TASK,
        move |params: FileEmbedParams, ctx| {
            let state = embed_state.clone();
            async move {
                run_task(
                    &ctx,
                    embed_file(&state.pool, &state.embeddings, params, ctx.task_id()),
                )
                .await
            }
        },
    )?;
    Ok(())
}

/// Replaces the file shares of a channel day's messages with `shares`, the
/// `(message_ts, file)` pairs of its rendered messages, and records each
/// file. Files no longer supported lose their documents.
pub(crate) async fn record_shares(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: &str,
    day: NaiveDate,
    shares: &[(String, Value)],
) -> Result<()> {
    sqlx::query(
        r#"
        DELETE FROM company_context_data.slack_file_shares shares
        USING company_context_system.slack_messages messages
        WHERE shares.conversation_id = $1
          AND messages.conversation_id = shares.conversation_id
          AND messages.message_ts = shares.message_ts
          AND messages.projection_day = $2
        "#,
    )
    .bind(conversation_id)
    .bind(day)
    .execute(&mut **tx)
    .await?;

    // Keep the most complete listing of each file, in a consistent order to
    // avoid deadlocking with other projections.
    let mut files = BTreeMap::<&str, (SlackFile, &Value)>::new();
    let mut share_files = Vec::with_capacity(shares.len());
    let mut share_messages = Vec::with_capacity(shares.len());
    for (message_ts, value) in shares {
        let Some(file_id) = value["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Ok(file) = serde_json::from_value::<SlackFile>(value.clone()) else {
            continue;
        };
        share_files.push(file_id);
        share_messages.push(message_ts.as_str());
        let listed = files
            .entry(file_id)
            .or_insert_with(|| (file.clone(), value));
        if listed.0.mode.is_empty() && !file.mode.is_empty() {
            *listed = (file, value);
        }
    }
    for (file, metadata) in files.values() {
        let (status, last_error) = match classify(file) {
            Classification::Supported(_) => ("pending", String::new()),
            Classification::Unsupported(reason) => ("rejected", reason),
            Classification::Deleted => ("deleted", String::new()),
        };
        sqlx::query(
            r#"
            INSERT INTO company_context_system.slack_files (
                file_id, name, title, mimetype, filetype, mode, size, user_id,
                permalink, source_created_at, source_version, extraction_status,
                last_error, metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            ON CONFLICT (file_id) DO UPDATE
            SET name = EXCLUDED.name,
                title = EXCLUDED.title,
                mimetype = EXCLUDED.mimetype,
                filetype = EXCLUDED.filetype,
                mode = EXCLUDED.mode,
                size = EXCLUDED.size,
                user_id = EXCLUDED.user_id,
                permalink = EXCLUDED.permalink,
                source_created_at = EXCLUDED.source_created_at,
                metadata = EXCLUDED.metadata,
                source_version = EXCLUDED.source_version,
                extraction_status = CASE
                    WHEN slack_files.source_version = EXCLUDED.source_version
                        THEN slack_files.extraction_status
                    ELSE EXCLUDED.extraction_status
                END,
                embedding_status = CASE
                    WHEN slack_files.source_version = EXCLUDED.source_version
                        THEN slack_files.embedding_status
                    ELSE 'pending'
                END,
                last_error = CASE
                    WHEN slack_files.source_version = EXCLUDED.source_version
                        THEN slack_files.last_error
                    ELSE EXCLUDED.last_error
                END,
                task_requested_at = CASE
                    WHEN slack_files.source_version = EXCLUDED.source_version
                        THEN slack_files.task_requested_at
                END,
                denied_credential_ids = CASE
                    WHEN slack_files.source_version = EXCLUDED.source_version
                        THEN slack_files.denied_credential_ids
                    ELSE '{}'
                END,
                updated_at = NOW()
            -- A listing without details does not replace one with them.
            WHERE EXCLUDED.source_version <> $15
            "#,
        )
        .bind(&file.id)
        .bind(&file.name)
        .bind(&file.title)
        .bind(&file.mimetype)
        .bind(&file.filetype)
        .bind(&file.mode)
        .bind(file.size)
        .bind(&file.user)
        .bind(&file.permalink)
        .bind(
            file.created
                .and_then(|created| DateTime::<Utc>::from_timestamp(created, 0)),
        )
        .bind(source_version(file))
        .bind(status)
        .bind(&last_error)
        .bind(metadata)
        .bind(INCOMPLETE_VERSION)
        .execute(&mut **tx)
        .await?;
    }
    let file_ids: Vec<&str> = files.keys().copied().collect();
    sqlx::query(
        r#"
        DELETE FROM company_context_data.slack_file_documents documents
        USING company_context_system.slack_files files
        WHERE files.file_id = documents.file_id
          AND files.file_id = ANY($1::text[])
          AND files.extraction_status IN ('rejected', 'deleted')
        "#,
    )
    .bind(&file_ids)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM company_context_system.slack_file_chunks chunks
        USING company_context_system.slack_files files
        WHERE files.file_id = chunks.file_id
          AND files.file_id = ANY($1::text[])
          AND files.extraction_status IN ('rejected', 'deleted')
        "#,
    )
    .bind(&file_ids)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_data.slack_file_shares
            (file_id, conversation_id, message_ts)
        SELECT file_id, $3, message_ts
        FROM unnest($1::text[], $2::text[]) AS shared(file_id, message_ts)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(&share_files)
    .bind(&share_messages)
    .bind(conversation_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Unfinished work on a file is enqueued again once its last request is this
/// old, so a lost or exhausted task cannot leave the file unfinished.
const TASK_RETRY_INTERVAL: &str = "1 hour";

/// The next task a file with unfinished work needs.
#[derive(Debug, PartialEq)]
enum NextTask {
    Extract,
    /// Extraction staged text with this hash; it awaits publication.
    Embed(String),
}

/// A file shared in a conversation whose work is unfinished and not
/// requested within the retry interval.
#[derive(Debug, PartialEq)]
struct DueFile {
    file_id: String,
    source_version: String,
    next: NextTask,
    denied_credential_ids: Vec<i64>,
}

async fn due_files(pool: &PgPool, conversation_id: &str) -> Result<Vec<DueFile>> {
    let rows = sqlx::query(&format!(
        r#"
        SELECT files.file_id, files.source_version, files.extraction_status,
               files.content_hash, files.denied_credential_ids
        FROM company_context_system.slack_files files
        WHERE (
              files.extraction_status = 'pending'
              OR (files.extraction_status = 'completed' AND files.embedding_status = 'pending')
          )
          AND (
              files.task_requested_at IS NULL
              OR files.task_requested_at < NOW() - INTERVAL '{TASK_RETRY_INTERVAL}'
          )
          AND EXISTS (
              SELECT 1
              FROM company_context_data.slack_file_shares shares
              WHERE shares.file_id = files.file_id
                AND shares.conversation_id = $1
          )
        ORDER BY files.file_id
        "#
    ))
    .bind(conversation_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let extraction_status: String = row.try_get("extraction_status")?;
            Ok(DueFile {
                file_id: row.try_get("file_id")?,
                source_version: row.try_get("source_version")?,
                next: if extraction_status == "completed" {
                    NextTask::Embed(row.try_get("content_hash")?)
                } else {
                    NextTask::Extract
                },
                denied_credential_ids: row.try_get("denied_credential_ids")?,
            })
        })
        .collect()
}

/// Records that a due file's next task is being enqueued. Returns the time
/// of the request, which distinguishes the task from earlier ones, or `None`
/// if another projection requested it first.
async fn claim(pool: &PgPool, file: &DueFile) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar(&format!(
        r#"
        UPDATE company_context_system.slack_files
        SET task_requested_at = NOW()
        WHERE file_id = $1
          AND source_version = $2
          AND (
              task_requested_at IS NULL
              OR task_requested_at < NOW() - INTERVAL '{TASK_RETRY_INTERVAL}'
          )
        RETURNING task_requested_at
        "#
    ))
    .bind(&file.file_id)
    .bind(&file.source_version)
    .fetch_optional(pool)
    .await?)
}

/// Enqueues the next task of the conversation's files with unfinished work.
/// Each file is extracted with a live credential observing the conversation
/// that can read files and that Slack has not refused the file to; without
/// one, the file waits for a later projection.
pub(crate) async fn spawn_due(state: &TaskState, conversation_id: &str) -> Result<usize> {
    let due = due_files(&state.pool, conversation_id).await?;
    if due.is_empty() {
        return Ok(0);
    }
    let readers = if due.iter().any(|file| file.next == NextTask::Extract) {
        file_readers(state, conversation_id).await?
    } else {
        Vec::new()
    };
    let mut spawned = 0;
    let mut waiting = 0;
    for file in &due {
        let reader = readers
            .iter()
            .copied()
            .find(|id| !file.denied_credential_ids.contains(id));
        if file.next == NextTask::Extract && reader.is_none() {
            waiting += 1;
            continue;
        }
        let Some(requested_at) = claim(&state.pool, file).await? else {
            continue;
        };
        let attempt = requested_at.timestamp_micros().to_string();
        match (&file.next, reader) {
            (NextTask::Embed(content_hash), _) => {
                spawn_embed(
                    state,
                    FileEmbedParams {
                        file_id: file.file_id.clone(),
                        source_version: file.source_version.clone(),
                        content_hash: content_hash.clone(),
                    },
                    &attempt,
                )
                .await?
            }
            (NextTask::Extract, Some(credential_id)) => {
                state
                    .absurd
                    .spawn(
                        SLACK_FILE_EXTRACT_TASK,
                        FileExtractParams {
                            file_id: file.file_id.clone(),
                            source_version: file.source_version.clone(),
                            credential_id,
                        },
                        SpawnOptions {
                            idempotency_key: Some(format!(
                                "slack.file.extract:{}:{}:{attempt}",
                                file.file_id, file.source_version
                            )),
                            ..SpawnOptions::default()
                        },
                    )
                    .await?;
            }
            (NextTask::Extract, None) => unreachable!("files without a reader wait"),
        }
        spawned += 1;
    }
    if waiting > 0 {
        info!(
            event = "company_context_slack_files_awaiting_reader",
            conversation_id,
            files = waiting
        );
    }
    Ok(spawned)
}

/// Lists the live credentials observing the conversation that can read files.
async fn file_readers(state: &TaskState, conversation_id: &str) -> Result<Vec<i64>> {
    let observers: Vec<i64> = sqlx::query_scalar(
        r#"
        SELECT broker_credential_id
        FROM company_context_data.slack_broker_observations
        WHERE conversation_id = $1
          AND active
        ORDER BY last_seen_at DESC, broker_credential_id
        "#,
    )
    .bind(conversation_id)
    .fetch_all(&state.pool)
    .await?;
    let mut readers = Vec::new();
    for credential_id in observers {
        match state.credentials.slack_credential(credential_id).await {
            Ok(credential) if credential.can_read_files => readers.push(credential.id),
            Ok(_) => {}
            Err(error) => warn!(
                event = "company_context_slack_file_credential_unavailable",
                credential_id,
                error = %error
            ),
        }
    }
    Ok(readers)
}

async fn spawn_embed(state: &TaskState, params: FileEmbedParams, attempt: &str) -> Result<()> {
    let key = format!(
        "slack.file.embed:{}:{}:{}:{}:{attempt}",
        params.file_id,
        params.source_version,
        params.content_hash,
        state.embeddings.model()
    );
    state
        .absurd
        .spawn(
            SLACK_FILE_EMBED_TASK,
            params,
            SpawnOptions {
                idempotency_key: Some(key),
                ..SpawnOptions::default()
            },
        )
        .await?;
    Ok(())
}

async fn extract_file(
    state: &TaskState,
    params: FileExtractParams,
    ctx: &TaskContext,
) -> Result<FileSummary> {
    // A failed lookup is retried. A credential that can no longer read files
    // leaves the file for a later projection to pick another.
    let credential = state
        .credentials
        .slack_credential(params.credential_id)
        .await?;
    if !credential.can_read_files {
        warn!(
            event = "company_context_slack_file_credential_unavailable",
            task_id = ctx.task_id(),
            credential_id = params.credential_id,
            error = "credential cannot read files"
        );
        return Ok(FileSummary::new("skipped", 0));
    }
    let outcome = extract(
        &state.pool,
        &state.slack,
        &state.config,
        &params,
        &credential.access_token,
        ctx.task_id(),
    )
    .await?;
    let Extraction::Staged {
        content_hash,
        chunks,
    } = outcome
    else {
        return Ok(FileSummary::new(outcome.status(), 0));
    };
    spawn_embed(
        state,
        FileEmbedParams {
            file_id: params.file_id.clone(),
            source_version: params.source_version.clone(),
            content_hash,
        },
        ctx.task_id(),
    )
    .await?;
    info!(
        event = "company_context_slack_file_extracted",
        task_id = ctx.task_id(),
        file_id = params.file_id,
        chunks
    );
    Ok(FileSummary::new("completed", chunks))
}

#[derive(Debug, PartialEq)]
enum Extraction {
    Staged {
        content_hash: String,
        chunks: usize,
    },
    /// The file changed or was extracted meanwhile.
    Superseded,
    /// Slack refused the file to the credential; another may read it.
    Denied,
    Rejected,
}

impl Extraction {
    fn status(&self) -> &'static str {
        match self {
            Self::Staged { .. } => "completed",
            Self::Superseded => "superseded",
            Self::Denied => "denied",
            Self::Rejected => "rejected",
        }
    }
}

/// Extracts a pending file with a credential's token and records the
/// outcome. Retryable failures are returned for the task to retry.
async fn extract(
    pool: &PgPool,
    slack: &SlackClient,
    config: &Config,
    params: &FileExtractParams,
    access_token: &str,
    task_id: &str,
) -> Result<Extraction> {
    let error = match stage_file(pool, slack, config, params, access_token).await {
        Ok(Some((content_hash, chunks))) => {
            return Ok(Extraction::Staged {
                content_hash,
                chunks,
            });
        }
        Ok(None) => return Ok(Extraction::Superseded),
        Err(error) => error,
    };
    if is_denied(&error) {
        record_denied(pool, params, &error).await;
        warn!(
            event = "company_context_slack_file_denied",
            task_id,
            file_id = params.file_id,
            credential_id = params.credential_id,
            error = %error
        );
        return Ok(Extraction::Denied);
    }
    let rejected = is_rejected(&error);
    record_failure(
        pool,
        &params.file_id,
        &params.source_version,
        false,
        rejected,
        &error,
    )
    .await;
    if !rejected {
        return Err(error);
    }
    warn!(
        event = "company_context_slack_file_rejected",
        task_id,
        file_id = params.file_id,
        error = %error
    );
    Ok(Extraction::Rejected)
}

/// Excludes the credential from extracting this version of the file and
/// lets the next projection pick another.
async fn record_denied(pool: &PgPool, params: &FileExtractParams, error: &anyhow::Error) {
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE company_context_system.slack_files
        SET denied_credential_ids = array_append(denied_credential_ids, $3),
            task_requested_at = NULL,
            last_error = $4,
            updated_at = NOW()
        WHERE file_id = $1
          AND source_version = $2
          AND extraction_status = 'pending'
          AND NOT ($3 = ANY(denied_credential_ids))
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.source_version)
    .bind(params.credential_id)
    .bind(bounded_error(error))
    .execute(pool)
    .await
    {
        error!(
            event = "company_context_failure_record_failed",
            file_id = params.file_id,
            error = %db_error
        );
    }
}

/// Downloads and extracts a pending file and stages its chunks. Returns the
/// extracted text's hash and chunk count, or `None` if the file changed or
/// was extracted meanwhile.
async fn stage_file(
    pool: &PgPool,
    slack: &SlackClient,
    config: &Config,
    params: &FileExtractParams,
    access_token: &str,
) -> Result<Option<(String, usize)>> {
    let metadata: Option<Json<Value>> = sqlx::query_scalar(
        r#"
        SELECT metadata
        FROM company_context_system.slack_files
        WHERE file_id = $1
          AND source_version = $2
          AND extraction_status = 'pending'
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.source_version)
    .fetch_optional(pool)
    .await?;
    let Some(Json(metadata)) = metadata else {
        return Ok(None);
    };
    let file: SlackFile =
        serde_json::from_value(metadata).context("decode staged Slack file metadata")?;
    let Classification::Supported(format) = classify(&file) else {
        return Err(rejected("Slack file is not supported"));
    };
    if file
        .size
        .is_some_and(|size| size > config.max_pdf_bytes as i64)
    {
        return Err(rejected("Slack file exceeds the configured byte limit"));
    }
    let bytes = slack
        .download_file(
            &file.url_private_download,
            access_token,
            config.max_pdf_bytes,
        )
        .await?;
    let text = match format {
        Format::Pdf => {
            if !bytes.starts_with(b"%PDF-") {
                return Err(rejected("Slack file is not a PDF"));
            }
            extract_pdf_text(bytes, config.extraction_timeout, config.max_extracted_bytes).await?
        }
        Format::Pandoc(reader) => {
            extract_pandoc_text(
                bytes,
                reader,
                config.extraction_timeout,
                config.max_extracted_bytes,
            )
            .await?
        }
        Format::Text => extract_plain_text(bytes, config.max_extracted_bytes, "Slack text file")?,
    };
    let chunks = chunk_text(&text, config.chunk_chars);
    if chunks.is_empty() {
        return Err(rejected("Slack file produced no non-empty chunks"));
    }
    let content_hash = hex_sha256(text.as_bytes());

    let mut tx = pool.begin().await?;
    let current: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM company_context_system.slack_files
        WHERE file_id = $1
          AND source_version = $2
          AND extraction_status = 'pending'
        FOR UPDATE
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.source_version)
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_none() {
        tx.rollback().await?;
        return Ok(None);
    }
    sqlx::query(
        r#"
        UPDATE company_context_system.slack_files
        SET content_hash = $2,
            extraction_status = 'completed',
            embedding_status = 'pending',
            last_error = '',
            -- The embed task enqueued next gets a full retry interval.
            task_requested_at = NOW(),
            updated_at = NOW()
        WHERE file_id = $1
        "#,
    )
    .bind(&params.file_id)
    .bind(&content_hash)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM company_context_system.slack_file_chunks WHERE file_id = $1")
        .bind(&params.file_id)
        .execute(&mut *tx)
        .await?;
    for chunk in &chunks {
        sqlx::query(
            r#"
            INSERT INTO company_context_system.slack_file_chunks
                (file_id, chunk_id, ordinal, body, content_hash)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(&params.file_id)
        .bind(&chunk.chunk_id)
        .bind(chunk.ordinal as i32)
        .bind(&chunk.body)
        .bind(&chunk.content_hash)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some((content_hash, chunks.len())))
}

/// Publishes a file's staged chunks: one document and embedding per chunk.
/// Vectors are reused for chunks whose text is unchanged.
async fn embed_file(
    pool: &PgPool,
    embeddings: &EmbeddingsClient,
    params: FileEmbedParams,
    task_id: &str,
) -> Result<FileSummary> {
    let Some(row) = sqlx::query(
        r#"
        SELECT title, name, mimetype, filetype, permalink, user_id, source_created_at
        FROM company_context_system.slack_files
        WHERE file_id = $1
          AND source_version = $2
          AND content_hash = $3
          AND extraction_status = 'completed'
          AND embedding_status = 'pending'
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.source_version)
    .bind(&params.content_hash)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(FileSummary::new("superseded", 0));
    };
    let title: String = row.try_get("title")?;
    let title = if title.is_empty() {
        row.try_get("name")?
    } else {
        title
    };
    let mimetype: String = row.try_get("mimetype")?;
    let filetype: String = row.try_get("filetype")?;
    let permalink: String = row.try_get("permalink")?;
    let author_id: String = row.try_get("user_id")?;
    let source_created_at: Option<DateTime<Utc>> = row.try_get("source_created_at")?;
    let chunks: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT chunk_id, body
        FROM company_context_system.slack_file_chunks
        WHERE file_id = $1
        ORDER BY ordinal
        "#,
    )
    .bind(&params.file_id)
    .fetch_all(pool)
    .await?;
    let documents = chunks
        .into_iter()
        .map(|(chunk_id, body)| {
            let input = if title.is_empty() {
                body.clone()
            } else {
                format!("{title}\n\n{body}")
            };
            (
                format!(
                    "{SLACK_FILE_DOCUMENT_ID_PREFIX}{}:{chunk_id}",
                    params.file_id
                ),
                chunk_id,
                body,
                hex_sha256(input.as_bytes()),
                input,
            )
        })
        .collect::<Vec<_>>();

    let reusable: BTreeSet<String> = sqlx::query_scalar(
        r#"
        SELECT embeddings.document_id
        FROM company_context_data.slack_file_document_embeddings embeddings
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
        .map(|doc| doc.4.clone())
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
            record_failure(
                pool,
                &params.file_id,
                &params.source_version,
                true,
                rejected,
                &error,
            )
            .await;
            if rejected {
                warn!(
                    event = "company_context_slack_file_embedding_rejected",
                    task_id,
                    file_id = params.file_id,
                    error = %error
                );
                return Ok(FileSummary::new("rejected", 0));
            }
            return Err(error);
        }
    };

    let mut tx = pool.begin().await?;
    let current: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT TRUE
        FROM company_context_system.slack_files
        WHERE file_id = $1
          AND source_version = $2
          AND content_hash = $3
          AND extraction_status = 'completed'
          AND embedding_status = 'pending'
        FOR UPDATE
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.source_version)
    .bind(&params.content_hash)
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_none() {
        tx.rollback().await?;
        return Ok(FileSummary::new("superseded", 0));
    }
    let mut document_ids = Vec::with_capacity(documents.len());
    for (document_id, chunk_id, body, content_hash, _) in &documents {
        document_ids.push(document_id.clone());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.slack_file_documents (
                document_id, file_id, chunk_id, title, body, mimetype, filetype,
                url, author_id, source_created_at, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (document_id) DO UPDATE
            SET title = EXCLUDED.title,
                body = EXCLUDED.body,
                mimetype = EXCLUDED.mimetype,
                filetype = EXCLUDED.filetype,
                url = EXCLUDED.url,
                author_id = EXCLUDED.author_id,
                source_created_at = EXCLUDED.source_created_at,
                content_hash = EXCLUDED.content_hash,
                updated_at = NOW()
            "#,
        )
        .bind(document_id)
        .bind(&params.file_id)
        .bind(chunk_id)
        .bind(&title)
        .bind(body)
        .bind(&mimetype)
        .bind(&filetype)
        .bind(&permalink)
        .bind(&author_id)
        .bind(source_created_at)
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
            INSERT INTO company_context_data.slack_file_document_embeddings (
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
        DELETE FROM company_context_data.slack_file_documents
        WHERE file_id = $1
          AND NOT (document_id = ANY($2::text[]))
        "#,
    )
    .bind(&params.file_id)
    .bind(&document_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.slack_files
        SET embedding_status = 'completed',
            last_error = '',
            published_at = NOW(),
            updated_at = NOW()
        WHERE file_id = $1
        "#,
    )
    .bind(&params.file_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_slack_file_published",
        task_id,
        file_id = params.file_id,
        chunks = document_ids.len()
    );
    Ok(FileSummary::new("completed", document_ids.len()))
}

/// Records a failed extraction or embedding of the file's current version.
/// Retryable failures stay pending so that the next attempt can finish. A
/// rejected version also loses whatever an earlier version published.
async fn record_failure(
    pool: &PgPool,
    file_id: &str,
    source_version: &str,
    embedding: bool,
    rejected: bool,
    error: &anyhow::Error,
) {
    let result = async {
        let mut tx = pool.begin().await?;
        let recorded = sqlx::query(
            r#"
            UPDATE company_context_system.slack_files
            SET extraction_status = CASE
                    WHEN $4 AND NOT $3 THEN 'rejected'
                    ELSE extraction_status
                END,
                embedding_status = CASE
                    WHEN $4 AND $3 THEN 'rejected'
                    ELSE embedding_status
                END,
                last_error = $5,
                updated_at = NOW()
            WHERE file_id = $1
              AND source_version = $2
              AND (CASE WHEN $3 THEN embedding_status ELSE extraction_status END) = 'pending'
            "#,
        )
        .bind(file_id)
        .bind(source_version)
        .bind(embedding)
        .bind(rejected)
        .bind(bounded_error(error))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if rejected && recorded > 0 {
            sqlx::query("DELETE FROM company_context_data.slack_file_documents WHERE file_id = $1")
                .bind(file_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM company_context_system.slack_file_chunks WHERE file_id = $1")
                .bind(file_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Result::<()>::Ok(())
    }
    .await;
    if let Err(db_error) = result {
        error!(
            event = "company_context_failure_record_failed",
            file_id,
            error = %db_error
        );
    }
}

/// Removes files that no message shares anymore, with their documents and
/// embeddings. Returns how many were removed.
pub(crate) async fn remove_unshared(tx: &mut Transaction<'_, Postgres>) -> Result<u64> {
    let candidates: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT files.file_id
        FROM company_context_system.slack_files files
        WHERE NOT EXISTS (
            SELECT 1
            FROM company_context_data.slack_file_shares shares
            WHERE shares.file_id = files.file_id
        )
        ORDER BY files.file_id
        FOR UPDATE OF files
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;
    // Recheck after locking: a projection that committed a share meanwhile
    // keeps its file.
    Ok(sqlx::query(
        r#"
        DELETE FROM company_context_system.slack_files files
        WHERE files.file_id = ANY($1::text[])
          AND NOT EXISTS (
              SELECT 1
              FROM company_context_data.slack_file_shares shares
              WHERE shares.file_id = files.file_id
          )
        "#,
    )
    .bind(&candidates)
    .execute(&mut **tx)
    .await?
    .rows_affected())
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

    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use clap::Parser;
    use sqlx::Executor;

    use super::*;
    use crate::test_support::TestDatabase;

    fn file(value: Value) -> SlackFile {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn files_are_classified_by_mode_and_type() {
        let hosted = |filetype: &str, mimetype: &str| {
            classify(&file(json!({
                "id": "F1",
                "mode": "hosted",
                "filetype": filetype,
                "mimetype": mimetype,
                "url_private_download": "https://files.slack.com/files-pri/T1-F1/download/f",
            })))
        };
        assert_eq!(
            hosted("pdf", "application/pdf"),
            Classification::Supported(Format::Pdf)
        );
        assert_eq!(
            hosted(
                "docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            ),
            Classification::Supported(Format::Pandoc("docx"))
        );
        assert_eq!(
            hosted("xlsx", ""),
            Classification::Supported(Format::Pandoc("xlsx"))
        );
        assert_eq!(
            hosted("rtf", "text/rtf"),
            Classification::Supported(Format::Pandoc("rtf"))
        );
        assert_eq!(
            hosted("csv", "text/csv"),
            Classification::Supported(Format::Text)
        );
        assert!(matches!(
            hosted("html", "text/html"),
            Classification::Unsupported(_)
        ));
        assert!(matches!(
            hosted("doc", "application/msword"),
            Classification::Unsupported(_)
        ));
        assert!(matches!(
            hosted("png", "image/png"),
            Classification::Unsupported(_)
        ));
        assert_eq!(
            classify(&file(json!({
                "id": "F1",
                "mode": "snippet",
                "filetype": "python",
                "mimetype": "text/x-python",
                "url_private_download": "https://files.slack.com/files-pri/T1-F1/download/f.py",
            }))),
            Classification::Supported(Format::Text)
        );
        for listed in [
            json!({ "id": "F1", "mode": "external", "filetype": "gdoc" }),
            json!({ "id": "F1", "mode": "hosted", "filetype": "pdf" }),
            json!({ "id": "F1", "file_access": "check_file_info" }),
        ] {
            assert!(matches!(
                classify(&file(listed)),
                Classification::Unsupported(_)
            ));
        }
        assert_eq!(
            classify(&file(json!({ "id": "F1", "mode": "tombstone" }))),
            Classification::Deleted
        );
    }

    async fn fake_files(
        axum::extract::Path((_, name)): axum::extract::Path<(String, String)>,
        headers: HeaderMap,
    ) -> Response {
        if headers.get("authorization").map(|value| value.as_bytes()) != Some(b"Bearer token-1") {
            return StatusCode::FORBIDDEN.into_response();
        }
        let body = if name == "blank.txt" {
            " \n"
        } else {
            "launch checklist\n\nship it"
        };
        ([("content-type", "text/plain")], body).into_response()
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

    async fn statuses(pool: &PgPool) -> Vec<(String, String, String)> {
        sqlx::query_as(
            r#"
            SELECT file_id, extraction_status, embedding_status
            FROM company_context_system.slack_files
            ORDER BY file_id
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn documents(pool: &PgPool) -> Vec<(String, String, String)> {
        sqlx::query_as(
            r#"
            SELECT documents.document_id, documents.title, documents.body
            FROM company_context_data.slack_file_documents documents
            JOIN company_context_data.slack_file_document_embeddings embeddings
              USING (document_id)
            ORDER BY 1
            "#,
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn project(pool: &PgPool, conversation_id: &str, day: NaiveDate) {
        let shares: Vec<(String, Value)> = sqlx::query_as(
            r#"
            SELECT message_ts, file
            FROM company_context_system.slack_messages,
                 jsonb_array_elements(COALESCE(raw_payload->'files', '[]'::jsonb)) AS files(file)
            WHERE conversation_id = $1
              AND projection_day = $2
            "#,
        )
        .bind(conversation_id)
        .bind(day)
        .fetch_all(pool)
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        record_shares(&mut tx, conversation_id, day, &shares)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn shared_files_are_published_once_and_follow_their_messages() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "slack_files").await;
        let pool = &database.pool;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let inputs = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route("/files-pri/{team_file}/download/{name}", get(fake_files))
            .route("/embeddings", post(fake_embeddings))
            .with_state(inputs.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let base_url = format!("http://{address}");
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
            &base_url,
            "--slack-bot-token",
            "xoxb-test",
            "--jwt-signing-secret",
            "jwt-secret",
            "--slack-files-base-url",
            &base_url,
        ])
        .unwrap();
        let embeddings = EmbeddingsClient::new(&config).unwrap();
        let slack = SlackClient::new(&config).unwrap();

        // The same snippet is shared in two channels; a Google Doc link is
        // listed but not indexed.
        let snippet = json!({
            "id": "F1",
            "name": "checklist.txt",
            "title": "Launch checklist",
            "mode": "snippet",
            "filetype": "text",
            "mimetype": "text/plain",
            "size": 25,
            "user": "U1",
            "created": 1700000000,
            "url_private_download": format!("{base_url}/files-pri/T1-F1/download/checklist.txt"),
        });
        let external =
            json!({ "id": "F2", "name": "Plan", "mode": "external", "filetype": "gdoc" });
        sqlx::query(
            r#"
            INSERT INTO company_context_system.slack_conversations (conversation_id, team_id, kind, name)
            VALUES ('C1', 'T1', 'public_channel', 'general'), ('C2', 'T1', 'public_channel', 'launch');
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        for (conversation_id, files) in [
            ("C1", json!([snippet, external])),
            (
                "C2",
                json!([{ "id": "F1", "file_access": "check_file_info" }]),
            ),
        ] {
            sqlx::query(
                r#"
                INSERT INTO company_context_system.slack_messages
                    (conversation_id, message_ts, user_id, subtype, text, occurred_at, raw_payload)
                VALUES ($1, '1700000000.000000', 'U1', 'file_share', '', to_timestamp(1700000000),
                        jsonb_build_object('files', $2::jsonb))
                "#,
            )
            .bind(conversation_id)
            .bind(&files)
            .execute(pool)
            .await
            .unwrap();
        }
        let day = NaiveDate::from_ymd_opt(2023, 11, 14).unwrap();
        project(pool, "C1", day).await;
        project(pool, "C2", day).await;
        assert_eq!(
            statuses(pool).await,
            [
                ("F1".to_owned(), "pending".to_owned(), "pending".to_owned()),
                ("F2".to_owned(), "rejected".to_owned(), "pending".to_owned()),
            ]
        );

        let version = || async {
            sqlx::query_scalar::<_, String>(
                "SELECT source_version FROM company_context_system.slack_files WHERE file_id = 'F1'",
            )
            .fetch_one(pool)
            .await
            .unwrap()
        };
        let params = |source_version: &str, credential_id| FileExtractParams {
            file_id: "F1".to_owned(),
            source_version: source_version.to_owned(),
            credential_id,
        };
        let due = |denied: &[i64], next| {
            vec![DueFile {
                file_id: "F1".to_owned(),
                source_version: String::new(),
                next,
                denied_credential_ids: denied.to_vec(),
            }]
        };
        let due_now = || async {
            due_files(pool, "C1")
                .await
                .unwrap()
                .into_iter()
                .map(|file| DueFile {
                    source_version: String::new(),
                    ..file
                })
                .collect::<Vec<_>>()
        };
        let overdue = || {
            pool.execute(
                "UPDATE company_context_system.slack_files SET task_requested_at = NOW() - INTERVAL '2 hours'",
            )
        };
        let v1 = version().await;
        assert_eq!(due_now().await, due(&[], NextTask::Extract));

        // Requesting the extraction holds the file until the request is
        // overdue, so projections do not enqueue it twice.
        let [file] = due_files(pool, "C1").await.unwrap().try_into().unwrap();
        assert!(claim(pool, &file).await.unwrap().is_some());
        assert!(claim(pool, &file).await.unwrap().is_none());
        assert!(due_now().await.is_empty());

        // A credential Slack refuses the file to is excluded, and the file is
        // due again for another credential.
        assert_eq!(
            extract(pool, &slack, &config, &params(&v1, 2), "token-2", "task")
                .await
                .unwrap(),
            Extraction::Denied
        );
        assert_eq!(due_now().await, due(&[2], NextTask::Extract));

        let Extraction::Staged {
            content_hash,
            chunks: 1,
        } = extract(pool, &slack, &config, &params(&v1, 1), "token-1", "task")
            .await
            .unwrap()
        else {
            panic!("the snippet is staged");
        };
        assert_eq!(
            extract(pool, &slack, &config, &params(&v1, 1), "token-1", "task")
                .await
                .unwrap(),
            Extraction::Superseded,
            "an extracted version is not extracted again"
        );
        // A lost embed task is enqueued again once overdue.
        assert!(due_now().await.is_empty());
        overdue().await.unwrap();
        assert_eq!(
            due_now().await,
            due(&[2], NextTask::Embed(content_hash.clone()))
        );
        let publish = || {
            embed_file(
                pool,
                &embeddings,
                FileEmbedParams {
                    file_id: "F1".to_owned(),
                    source_version: v1.clone(),
                    content_hash: content_hash.clone(),
                },
                "task",
            )
        };
        assert_eq!(publish().await.unwrap().status, "completed");
        assert_eq!(publish().await.unwrap().status, "superseded");
        assert_eq!(
            documents(pool).await,
            [(
                "slack-file:F1:000000".to_owned(),
                "Launch checklist".to_owned(),
                "launch checklist\n\nship it".to_owned()
            )]
        );
        assert_eq!(inputs.load(Ordering::SeqCst), 1);
        overdue().await.unwrap();
        assert!(due_now().await.is_empty());

        // Projecting the days again keeps the published file.
        project(pool, "C1", day).await;
        project(pool, "C2", day).await;
        assert_eq!(statuses(pool).await[0].1, "completed");
        assert_eq!(documents(pool).await.len(), 1);

        // A file still shared elsewhere stays when one conversation goes.
        pool.execute(
            "DELETE FROM company_context_system.slack_conversations WHERE conversation_id = 'C2'",
        )
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(remove_unshared(&mut tx).await.unwrap(), 0);
        tx.commit().await.unwrap();
        assert_eq!(documents(pool).await.len(), 1);

        // A new version starts over with every credential, and keeps the
        // published text until it is replaced or rejected.
        let mut edited = snippet.clone();
        edited["size"] = json!(2);
        edited["url_private_download"] =
            json!(format!("{base_url}/files-pri/T1-F1/download/blank.txt"));
        sqlx::query(
            r#"
            UPDATE company_context_system.slack_messages
            SET raw_payload = jsonb_build_object('files', jsonb_build_array($1::jsonb))
            WHERE conversation_id = 'C1'
            "#,
        )
        .bind(&edited)
        .execute(pool)
        .await
        .unwrap();
        project(pool, "C1", day).await;
        let v2 = version().await;
        assert_ne!(v1, v2);
        assert_eq!(due_now().await, due(&[], NextTask::Extract));
        assert_eq!(documents(pool).await.len(), 1);
        assert_eq!(
            extract(pool, &slack, &config, &params(&v2, 1), "token-1", "task")
                .await
                .unwrap(),
            Extraction::Rejected
        );
        assert_eq!(statuses(pool).await[0].1, "rejected");
        assert!(documents(pool).await.is_empty());

        // Deleting the file in Slack marks it deleted.
        pool.execute(
            r#"
            UPDATE company_context_system.slack_messages
            SET raw_payload = '{"files": [{"id": "F1", "mode": "tombstone"}]}'
            WHERE conversation_id = 'C1'
            "#,
        )
        .await
        .unwrap();
        project(pool, "C1", day).await;
        assert_eq!(
            statuses(pool).await,
            [
                ("F1".to_owned(), "deleted".to_owned(), "pending".to_owned()),
                ("F2".to_owned(), "rejected".to_owned(), "pending".to_owned()),
            ]
        );

        // Files go once no message shares them.
        pool.execute(
            "DELETE FROM company_context_system.slack_conversations WHERE conversation_id = 'C1'",
        )
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(remove_unshared(&mut tx).await.unwrap(), 2);
        tx.commit().await.unwrap();
        assert!(statuses(pool).await.is_empty());

        server.abort();
        database.drop().await;
    }
}
