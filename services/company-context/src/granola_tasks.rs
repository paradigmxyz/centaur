use std::collections::{BTreeMap, BTreeSet};

use absurd::{SpawnOptions, TaskContext};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};
use tracing::{error, info, warn};

use crate::{
    config::{
        GRANOLA_CREDENTIALS_RECONCILE_TASK, GRANOLA_NOTE_EMBED_TASK, GRANOLA_NOTES_FETCH_TASK,
        GRANOLA_SYNC_TASK,
    },
    credentials::GranolaCredential,
    errors::{is_rejected, rejected},
    extraction::{chunk_text, hex_sha256},
    granola::Meeting,
    tasks::{TaskState, bounded_error, run_task},
};

/// Granola's `get_meetings` tool accepts at most ten meeting IDs.
const MEETING_DETAILS_BATCH_SIZE: usize = 10;
const WATERMARK_OVERLAP_MINUTES: i64 = 5;

#[derive(Debug, Deserialize, Serialize)]
pub struct GranolaReconcileParams {
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GranolaSyncParams {
    pub credential_id: i64,
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NotesFetchParams {
    pub credential_id: i64,
    pub account_email: String,
    /// Listed meetings, used when Granola omits a meeting's details.
    pub meetings: Vec<Meeting>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NoteEmbedParams {
    pub note_id: String,
    pub revision: i64,
}

#[derive(Debug, Serialize)]
pub struct NoteSummary {
    status: &'static str,
    notes: usize,
}

impl NoteSummary {
    fn new(status: &'static str, notes: usize) -> Self {
        Self { status, notes }
    }
}

pub fn register(state: &TaskState) -> Result<()> {
    let reconcile_state = state.clone();
    state.absurd.register_task(
        GRANOLA_CREDENTIALS_RECONCILE_TASK,
        move |params: GranolaReconcileParams, ctx| {
            let state = reconcile_state.clone();
            async move { run_task(&ctx, reconcile_credentials(&state, params, &ctx)).await }
        },
    )?;

    let sync_state = state.clone();
    state
        .absurd
        .register_task(GRANOLA_SYNC_TASK, move |params: GranolaSyncParams, ctx| {
            let state = sync_state.clone();
            async move { run_task(&ctx, sync_account(&state, params, &ctx)).await }
        })?;

    let fetch_state = state.clone();
    state.absurd.register_task(
        GRANOLA_NOTES_FETCH_TASK,
        move |params: NotesFetchParams, ctx| {
            let state = fetch_state.clone();
            async move { run_task(&ctx, fetch_notes(&state, params, &ctx)).await }
        },
    )?;

    let embed_state = state.clone();
    state.absurd.register_task(
        GRANOLA_NOTE_EMBED_TASK,
        move |params: NoteEmbedParams, ctx| {
            let state = embed_state.clone();
            async move { run_task(&ctx, embed_note(&state, params, &ctx)).await }
        },
    )?;
    Ok(())
}

/// Deactivates observations from dead or deleted credentials and removes notes
/// that no live credential still observes.
async fn reconcile_credentials(
    state: &TaskState,
    params: GranolaReconcileParams,
    ctx: &TaskContext,
) -> Result<NoteSummary> {
    let retained_ids = state.credentials.retained_granola_credential_ids().await?;
    let (deactivated, notes_deleted) = remove_unobserved_notes(&state.pool, &retained_ids).await?;
    info!(
        event = "company_context_granola_credentials_reconciled",
        task_id = ctx.task_id(),
        bucket = params.bucket,
        observations_deactivated = deactivated,
        notes_deleted
    );
    Ok(NoteSummary::new("completed", notes_deleted))
}

/// Returns the number of observations deactivated and notes removed.
async fn remove_unobserved_notes(pool: &PgPool, retained_ids: &[i64]) -> Result<(u64, usize)> {
    let mut tx = pool.begin().await?;
    let deactivated = sqlx::query(
        r#"
        UPDATE company_context_data.granola_broker_observations
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
        SELECT notes.note_id
        FROM company_context_system.granola_notes notes
        WHERE notes.embedding_status <> 'deleted'
          AND NOT EXISTS (
              SELECT 1
              FROM company_context_data.granola_broker_observations observations
              WHERE observations.note_id = notes.note_id
                AND observations.active
          )
        FOR UPDATE OF notes
        "#,
    )
    .fetch_all(&mut *tx)
    .await?;
    // Recheck after locking: a fetch that committed meanwhile keeps its note,
    // and a fetch that commits later republishes the cleared note because its
    // content hash no longer matches.
    let unobserved: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT candidates.note_id
        FROM unnest($1::text[]) AS candidates(note_id)
        WHERE NOT EXISTS (
            SELECT 1
            FROM company_context_data.granola_broker_observations observations
            WHERE observations.note_id = candidates.note_id
              AND observations.active
        )
        "#,
    )
    .bind(&candidates)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM company_context_data.granola_documents WHERE note_id = ANY($1)")
        .bind(&unobserved)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.granola_notes
        SET title = '',
            owner = '{}'::jsonb,
            attendees = '[]'::jsonb,
            summary_markdown = '',
            content_text = '',
            content_hash = '',
            metadata = '{}'::jsonb,
            revision = revision + 1,
            embedding_status = 'deleted',
            last_error = '',
            updated_at = NOW()
        WHERE note_id = ANY($1)
        "#,
    )
    .bind(&unobserved)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((deactivated, unobserved.len()))
}

/// Lists the account's recent meetings and fetches their details in batches.
async fn sync_account(
    state: &TaskState,
    params: GranolaSyncParams,
    ctx: &TaskContext,
) -> Result<NoteSummary> {
    let credential = state
        .credentials
        .granola_credential(params.credential_id)
        .await?;
    let scope = format!("granola:broker:{}", credential.id);
    sqlx::query(
        r#"
        INSERT INTO company_context_system.granola_checkpoints (scope_id)
        VALUES ($1)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(&scope)
    .execute(&state.pool)
    .await?;
    let result = async {
        let watermark: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT watermark_time FROM company_context_system.granola_checkpoints WHERE scope_id = $1",
        )
        .bind(&scope)
        .fetch_one(&state.pool)
        .await?;
        let mut session = state.granola.session(&credential).await?;
        let account_email = match session.account_email().await? {
            email if !email.is_empty() => email,
            _ => credential.provider_email.trim().to_lowercase(),
        };
        if account_email.is_empty() {
            return Err(rejected("Granola account did not provide an email"));
        }
        let now = Utc::now();
        let start = match watermark {
            Some(watermark) => watermark - chrono::Duration::minutes(WATERMARK_OVERLAP_MINUTES),
            None => now - chrono::Duration::days(state.config.granola_initial_lookback_days as i64),
        };
        let meetings = session
            .list_meetings(start.date_naive(), now.date_naive())
            .await?;
        for batch in meetings.chunks(MEETING_DETAILS_BATCH_SIZE) {
            let ids = batch
                .iter()
                .map(|meeting| meeting.id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            state
                .absurd
                .spawn(
                    GRANOLA_NOTES_FETCH_TASK,
                    NotesFetchParams {
                        credential_id: credential.id,
                        account_email: account_email.clone(),
                        meetings: batch.to_vec(),
                    },
                    SpawnOptions {
                        idempotency_key: Some(format!(
                            "granola.notes.fetch:{}:{}:{}",
                            credential.id,
                            params.bucket,
                            hex_sha256(ids.as_bytes())
                        )),
                        ..SpawnOptions::default()
                    },
                )
                .await?;
        }
        let latest = meetings
            .iter()
            .filter_map(Meeting::occurred_at)
            .map(|date| date.with_timezone(&Utc))
            .max();
        sqlx::query(
            r#"
            UPDATE company_context_system.granola_checkpoints
            SET watermark_time = GREATEST(watermark_time, $2),
                last_success_at = NOW(),
                last_error = '',
                updated_at = NOW()
            WHERE scope_id = $1
            "#,
        )
        .bind(&scope)
        .bind(latest)
        .execute(&state.pool)
        .await?;
        Ok(meetings.len())
    }
    .await;

    match result {
        Ok(meetings) => {
            info!(
                event = "company_context_granola_sync_completed",
                task_id = ctx.task_id(),
                scope,
                meetings
            );
            Ok(NoteSummary::new("completed", meetings))
        }
        Err(error) => {
            if let Err(db_error) = sqlx::query(
                r#"
                UPDATE company_context_system.granola_checkpoints
                SET last_error = $2,
                    updated_at = NOW()
                WHERE scope_id = $1
                "#,
            )
            .bind(&scope)
            .bind(bounded_error(&error))
            .execute(&state.pool)
            .await
            {
                error!(event = "company_context_failure_record_failed", scope, error = %db_error);
            }
            if is_rejected(&error) {
                warn!(
                    event = "company_context_granola_sync_rejected",
                    task_id = ctx.task_id(),
                    scope,
                    error = %error
                );
                return Ok(NoteSummary::new("rejected", 0));
            }
            Err(error)
        }
    }
}

/// Fetches details for a batch of meetings and stages the notes whose content
/// changed. Transcripts are left to the Granola tool, which fetches them on demand.
async fn fetch_notes(
    state: &TaskState,
    params: NotesFetchParams,
    ctx: &TaskContext,
) -> Result<NoteSummary> {
    let credential = state
        .credentials
        .granola_credential(params.credential_id)
        .await?;
    let result = async {
        let mut session = state.granola.session(&credential).await?;
        let ids = params
            .meetings
            .iter()
            .map(|meeting| meeting.id.clone())
            .collect::<Vec<_>>();
        let details: BTreeMap<String, Meeting> = session
            .get_meetings(&ids)
            .await?
            .into_iter()
            .map(|meeting| (meeting.id.clone(), meeting))
            .collect();
        let mut staged = 0;
        for listed in &params.meetings {
            let meeting = details.get(&listed.id).unwrap_or(listed);
            let Some(revision) =
                stage_note(&state.pool, &credential, &params.account_email, meeting).await?
            else {
                continue;
            };
            state
                .absurd
                .spawn(
                    GRANOLA_NOTE_EMBED_TASK,
                    NoteEmbedParams {
                        note_id: meeting.id.clone(),
                        revision,
                    },
                    SpawnOptions {
                        idempotency_key: Some(format!(
                            "granola.note.embed:{}:{revision}:{}",
                            meeting.id,
                            state.embeddings.model()
                        )),
                        ..SpawnOptions::default()
                    },
                )
                .await?;
            staged += 1;
        }
        Ok(staged)
    }
    .await;

    match result {
        Ok(staged) => {
            info!(
                event = "company_context_granola_notes_fetched",
                task_id = ctx.task_id(),
                credential_id = credential.id,
                meetings = params.meetings.len(),
                notes_staged = staged
            );
            Ok(NoteSummary::new("completed", staged))
        }
        Err(error) if is_rejected(&error) => {
            warn!(
                event = "company_context_granola_notes_rejected",
                task_id = ctx.task_id(),
                credential_id = credential.id,
                error = %error
            );
            Ok(NoteSummary::new("rejected", 0))
        }
        Err(error) => Err(error),
    }
}

fn note_content(meeting: &Meeting) -> String {
    [meeting.title.trim(), meeting.summary_markdown.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Records the credential's observation and stages the note. Returns the
/// revision to embed when the note is awaiting publication.
async fn stage_note(
    pool: &PgPool,
    credential: &GranolaCredential,
    account_email: &str,
    meeting: &Meeting,
) -> Result<Option<i64>> {
    let owner = json!(meeting.owner.clone().unwrap_or_default());
    let attendees = json!(meeting.attendees);
    let content_text = note_content(meeting);
    let content_hash = hex_sha256(
        json!([meeting.date, owner, attendees, content_text])
            .to_string()
            .as_bytes(),
    );
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_data.granola_broker_observations (
            broker_credential_id, note_id, provider_email, provider_subject,
            active, last_seen_at, updated_at
        )
        VALUES ($1, $2, $3, $4, TRUE, NOW(), NOW())
        ON CONFLICT (broker_credential_id, note_id) DO UPDATE
        SET provider_email = EXCLUDED.provider_email,
            provider_subject = EXCLUDED.provider_subject,
            active = TRUE,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(credential.id)
    .bind(&meeting.id)
    .bind(account_email)
    .bind(&credential.provider_subject)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_system.granola_notes (
            note_id, title, owner, attendees, summary_markdown, content_text,
            content_hash, source_created_at, metadata
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (note_id) DO UPDATE
        SET title = EXCLUDED.title,
            owner = EXCLUDED.owner,
            attendees = EXCLUDED.attendees,
            summary_markdown = EXCLUDED.summary_markdown,
            content_text = EXCLUDED.content_text,
            content_hash = EXCLUDED.content_hash,
            source_created_at = EXCLUDED.source_created_at,
            metadata = EXCLUDED.metadata,
            revision = granola_notes.revision + 1,
            embedding_status = 'pending',
            last_error = '',
            updated_at = NOW()
        WHERE granola_notes.content_hash IS DISTINCT FROM EXCLUDED.content_hash
        "#,
    )
    .bind(&meeting.id)
    .bind(&meeting.title)
    .bind(&owner)
    .bind(&attendees)
    .bind(&meeting.summary_markdown)
    .bind(&content_text)
    .bind(&content_hash)
    .bind(meeting.occurred_at())
    .bind(json!({ "date": meeting.date, "source": "granola_mcp" }))
    .execute(&mut *tx)
    .await?;
    // Also returns unchanged notes that are still pending, so a lost embed
    // spawn is recovered by the next fetch.
    let row = sqlx::query(
        r#"
        UPDATE company_context_system.granola_notes
        SET last_seen_at = NOW()
        WHERE note_id = $1
        RETURNING revision, embedding_status
        "#,
    )
    .bind(&meeting.id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let status: String = row.try_get("embedding_status")?;
    Ok((status == "pending").then(|| row.get("revision")))
}

async fn embed_note(
    state: &TaskState,
    params: NoteEmbedParams,
    ctx: &TaskContext,
) -> Result<NoteSummary> {
    let Some(row) = sqlx::query(
        r#"
        SELECT title, owner, attendees, content_text, source_created_at
        FROM company_context_system.granola_notes
        WHERE note_id = $1
          AND revision = $2
          AND embedding_status = 'pending'
        "#,
    )
    .bind(&params.note_id)
    .bind(params.revision)
    .fetch_optional(&state.pool)
    .await?
    else {
        return Ok(NoteSummary::new("superseded", 0));
    };
    let title: String = row.try_get("title")?;
    let owner: serde_json::Value = row.try_get("owner")?;
    let attendees: serde_json::Value = row.try_get("attendees")?;
    let content_text: String = row.try_get("content_text")?;
    let occurred_at: Option<DateTime<Utc>> = row.try_get("source_created_at")?;
    let owner_text = |key| {
        owner
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    let chunks = chunk_text(&content_text, state.config.chunk_chars)
        .into_iter()
        .map(|chunk| {
            let document_id = format!("granola:{}:{}", params.note_id, chunk.chunk_id);
            let content_hash = hex_sha256(format!("{title}\n\n{}", chunk.body).as_bytes());
            (document_id, chunk.chunk_id, chunk.body, content_hash)
        })
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        let error = rejected("staged Granola note has no content");
        record_embedding_failure(&state.pool, &params, true, &error).await;
        return Ok(NoteSummary::new("rejected", 0));
    }
    // Reuse vectors for chunks whose text is unchanged.
    let reusable: BTreeSet<String> = sqlx::query_scalar(
        r#"
        SELECT embeddings.document_id
        FROM company_context_data.granola_document_embeddings embeddings
        JOIN unnest($1::text[], $2::text[]) AS chunks(document_id, content_hash)
          ON chunks.document_id = embeddings.document_id
         AND chunks.content_hash = embeddings.content_hash
        WHERE embeddings.model = $3
          AND embeddings.dimensions = $4
        "#,
    )
    .bind(
        chunks
            .iter()
            .map(|chunk| chunk.0.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(
        chunks
            .iter()
            .map(|chunk| chunk.3.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(state.embeddings.model())
    .bind(state.embeddings.dimensions() as i32)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .collect();
    let inputs = chunks
        .iter()
        .filter(|chunk| !reusable.contains(&chunk.0))
        .map(|(_, _, body, _)| format!("{title}\n\n{body}"))
        .collect::<Vec<_>>();
    let result = if inputs.is_empty() {
        Ok(Vec::new())
    } else {
        state.embeddings.embed(&inputs).await
    };
    let mut embeddings = match result {
        Ok(embeddings) => embeddings.into_iter(),
        Err(error) => {
            let rejected = is_rejected(&error);
            record_embedding_failure(&state.pool, &params, rejected, &error).await;
            if rejected {
                warn!(
                    event = "company_context_granola_embedding_rejected",
                    task_id = ctx.task_id(),
                    note_id = params.note_id,
                    error = %error
                );
                return Ok(NoteSummary::new("rejected", 0));
            }
            return Err(error);
        }
    };

    let mut tx = state.pool.begin().await?;
    let current = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT revision = $2 AND embedding_status = 'pending'
        FROM company_context_system.granola_notes
        WHERE note_id = $1
        FOR UPDATE
        "#,
    )
    .bind(&params.note_id)
    .bind(params.revision)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if !current {
        tx.rollback().await?;
        return Ok(NoteSummary::new("superseded", 0));
    }
    let mut document_ids = Vec::with_capacity(chunks.len());
    for (document_id, chunk_id, body, content_hash) in &chunks {
        document_ids.push(document_id.clone());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.granola_documents (
                document_id, note_id, chunk_id, title, body, owner_email,
                owner_name, attendees, occurred_at, content_hash, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
            ON CONFLICT (document_id) DO UPDATE
            SET title = EXCLUDED.title,
                body = EXCLUDED.body,
                owner_email = EXCLUDED.owner_email,
                owner_name = EXCLUDED.owner_name,
                attendees = EXCLUDED.attendees,
                occurred_at = EXCLUDED.occurred_at,
                content_hash = EXCLUDED.content_hash,
                updated_at = NOW()
            "#,
        )
        .bind(document_id)
        .bind(&params.note_id)
        .bind(chunk_id)
        .bind(&title)
        .bind(body)
        .bind(owner_text("email"))
        .bind(owner_text("name"))
        .bind(&attendees)
        .bind(occurred_at)
        .bind(content_hash)
        .execute(&mut *tx)
        .await?;
        if reusable.contains(document_id) {
            continue;
        }
        let embedding = embeddings
            .next()
            .context("embeddings response omitted a chunk")?;
        sqlx::query(
            r#"
            INSERT INTO company_context_data.granola_document_embeddings (
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
        .bind(state.embeddings.model())
        .bind(state.embeddings.dimensions() as i32)
        .bind(content_hash)
        .bind(serde_json::to_string(&embedding)?)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        r#"
        DELETE FROM company_context_data.granola_documents
        WHERE note_id = $1
          AND NOT (document_id = ANY($2::text[]))
        "#,
    )
    .bind(&params.note_id)
    .bind(&document_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.granola_notes
        SET embedding_status = 'completed',
            last_error = '',
            published_at = NOW(),
            updated_at = NOW()
        WHERE note_id = $1
        "#,
    )
    .bind(&params.note_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_granola_note_published",
        task_id = ctx.task_id(),
        note_id = params.note_id,
        chunks = document_ids.len()
    );
    Ok(NoteSummary::new("completed", 1))
}

async fn record_embedding_failure(
    pool: &PgPool,
    params: &NoteEmbedParams,
    rejected: bool,
    error: &anyhow::Error,
) {
    // Retryable failures stay pending so that the next attempt can publish.
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE company_context_system.granola_notes
        SET embedding_status = CASE WHEN $3 THEN 'rejected' ELSE embedding_status END,
            last_error = $4,
            updated_at = NOW()
        WHERE note_id = $1
          AND revision = $2
          AND embedding_status = 'pending'
        "#,
    )
    .bind(&params.note_id)
    .bind(params.revision)
    .bind(rejected)
    .bind(bounded_error(error))
    .execute(pool)
    .await
    {
        error!(
            event = "company_context_failure_record_failed",
            note_id = params.note_id,
            error = %db_error
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use sqlx::{Connection, Executor, PgConnection};

    use super::*;
    use crate::{database, granola::Participant};

    fn credential(id: i64) -> GranolaCredential {
        GranolaCredential {
            id,
            access_token: "token".to_owned(),
            provider_email: String::new(),
            provider_subject: format!("granola-user-{id}"),
        }
    }

    fn meeting(summary: &str) -> Meeting {
        Meeting {
            id: "meeting-1".to_owned(),
            title: "Planning".to_owned(),
            date: "Jul 8, 2026 5:30 PM GMT+2".to_owned(),
            owner: Some(Participant {
                name: "Ada".to_owned(),
                email: "ada@example.com".to_owned(),
            }),
            attendees: Vec::new(),
            summary_markdown: summary.to_owned(),
        }
    }

    async fn note(pool: &PgPool) -> (String, String, i64) {
        let row = sqlx::query(
            "SELECT content_text, embedding_status, revision FROM company_context_system.granola_notes WHERE note_id = 'meeting-1'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (row.get(0), row.get(1), row.get(2))
    }

    #[tokio::test]
    async fn notes_are_republished_on_change_and_removed_when_unobserved() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let mut admin = PgConnection::connect(&database_url).await.unwrap();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!("company_context_granola_{}_{nanos}", std::process::id());
        admin
            .execute(format!(r#"create database "{name}""#).as_str())
            .await
            .unwrap();
        let mut test_url = url::Url::parse(&database_url).unwrap();
        test_url.set_path(&name);
        let pool = database::connect_and_migrate(test_url.as_str())
            .await
            .unwrap();

        // Unchanged pending notes are returned again so a lost embed spawn recovers.
        let staged = stage_note(
            &pool,
            &credential(1),
            "ada@example.com",
            &meeting("Ship it."),
        )
        .await
        .unwrap();
        assert_eq!(staged, Some(1));
        assert_eq!(
            note(&pool).await,
            ("Planning\n\nShip it.".to_owned(), "pending".to_owned(), 1)
        );
        (&pool)
            .execute(
                r#"
            UPDATE company_context_system.granola_notes SET embedding_status = 'completed';
            INSERT INTO company_context_data.granola_documents
                (document_id, note_id, chunk_id, body, content_hash)
            VALUES ('granola:meeting-1:000000', 'meeting-1', '000000', 'Ship it.', 'hash');
            "#,
            )
            .await
            .unwrap();
        let unchanged = stage_note(
            &pool,
            &credential(2),
            "bob@example.com",
            &meeting("Ship it."),
        )
        .await
        .unwrap();
        assert_eq!(unchanged, None);
        let changed = stage_note(
            &pool,
            &credential(2),
            "bob@example.com",
            &meeting("Ship it today."),
        )
        .await
        .unwrap();
        assert_eq!(changed, Some(2));

        // A note stays while any retained credential observes it.
        assert_eq!(remove_unobserved_notes(&pool, &[2]).await.unwrap(), (1, 0));
        assert_eq!(note(&pool).await.1, "pending");

        assert_eq!(remove_unobserved_notes(&pool, &[]).await.unwrap(), (1, 1));
        assert_eq!(note(&pool).await, (String::new(), "deleted".to_owned(), 3));
        let documents: i64 =
            sqlx::query_scalar("SELECT count(*) FROM company_context_data.granola_documents")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(documents, 0);

        // Observing the note again republishes it.
        let restored = stage_note(
            &pool,
            &credential(1),
            "ada@example.com",
            &meeting("Ship it today."),
        )
        .await
        .unwrap();
        assert_eq!(restored, Some(4));

        pool.close().await;
        admin
            .execute(format!(r#"drop database if exists "{name}""#).as_str())
            .await
            .unwrap();
    }
}
