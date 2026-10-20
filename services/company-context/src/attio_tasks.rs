use std::collections::BTreeSet;

use absurd::{SpawnOptions, TaskContext};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};
use tracing::{error, info, warn};

use crate::{
    attio::{CallRecording, Meeting},
    config::{
        ATTIO_CREDENTIALS_RECONCILE_TASK, ATTIO_DOCUMENT_ID_PREFIX, ATTIO_MEETING_EMBED_TASK,
        ATTIO_MEETINGS_FETCH_TASK, ATTIO_SYNC_TASK,
    },
    credentials::AttioCredential,
    errors::{is_rejected, rejected},
    extraction::{chunk_text, hex_sha256},
    tasks::{TaskState, bounded_error, run_task},
};

const MEETINGS_FETCH_BATCH_SIZE: usize = 10;
/// Each sync rereads meetings that ended this long before the previous sync,
/// so transcripts that finish processing after a meeting are picked up.
const REREAD_HOURS: i64 = 72;

#[derive(Debug, Deserialize, Serialize)]
pub struct AttioReconcileParams {
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AttioSyncParams {
    pub credential_id: i64,
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MeetingsFetchParams {
    pub credential_id: i64,
    pub meetings: Vec<Meeting>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MeetingEmbedParams {
    pub credential_id: i64,
    pub meeting_id: String,
    pub revision: i64,
}

#[derive(Debug, Serialize)]
pub struct MeetingSummary {
    status: &'static str,
    meetings: usize,
}

impl MeetingSummary {
    fn new(status: &'static str, meetings: usize) -> Self {
        Self { status, meetings }
    }
}

pub fn register(state: &TaskState) -> Result<()> {
    let reconcile_state = state.clone();
    state.absurd.register_task(
        ATTIO_CREDENTIALS_RECONCILE_TASK,
        move |params: AttioReconcileParams, ctx| {
            let state = reconcile_state.clone();
            async move { run_task(&ctx, reconcile_credentials(&state, params, &ctx)).await }
        },
    )?;

    let sync_state = state.clone();
    state
        .absurd
        .register_task(ATTIO_SYNC_TASK, move |params: AttioSyncParams, ctx| {
            let state = sync_state.clone();
            async move { run_task(&ctx, sync_workspace(&state, params, &ctx)).await }
        })?;

    let fetch_state = state.clone();
    state.absurd.register_task(
        ATTIO_MEETINGS_FETCH_TASK,
        move |params: MeetingsFetchParams, ctx| {
            let state = fetch_state.clone();
            async move { run_task(&ctx, fetch_meetings(&state, params, &ctx)).await }
        },
    )?;

    let embed_state = state.clone();
    state.absurd.register_task(
        ATTIO_MEETING_EMBED_TASK,
        move |params: MeetingEmbedParams, ctx| {
            let state = embed_state.clone();
            async move { run_task(&ctx, embed_meeting(&state, params, &ctx)).await }
        },
    )?;
    Ok(())
}

/// Removes the meetings and checkpoints of dead or deleted credentials.
async fn reconcile_credentials(
    state: &TaskState,
    params: AttioReconcileParams,
    ctx: &TaskContext,
) -> Result<MeetingSummary> {
    let retained_ids = state.credentials.retained_attio_credential_ids().await?;
    let removed = remove_unretained(&state.pool, &retained_ids).await?;
    info!(
        event = "company_context_attio_credentials_reconciled",
        task_id = ctx.task_id(),
        bucket = params.bucket,
        meetings_removed = removed
    );
    Ok(MeetingSummary::new("completed", removed as usize))
}

async fn remove_unretained(pool: &PgPool, retained_ids: &[i64]) -> Result<u64> {
    let mut tx = pool.begin().await?;
    // Documents and embeddings cascade. Dropping the checkpoint makes a
    // revived credential backfill its meetings again.
    let removed = sqlx::query(
        r#"
        DELETE FROM company_context_system.attio_meetings
        WHERE NOT (broker_credential_id = ANY($1::bigint[]))
        "#,
    )
    .bind(retained_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(
        r#"
        DELETE FROM company_context_system.attio_checkpoints
        WHERE NOT (split_part(scope_id, ':broker:', 2) = ANY($1::text[]))
        "#,
    )
    .bind(retained_ids.iter().map(i64::to_string).collect::<Vec<_>>())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(removed)
}

/// Lists the workspace's meetings since the checkpoint and fetches their
/// transcripts in batches.
async fn sync_workspace(
    state: &TaskState,
    params: AttioSyncParams,
    ctx: &TaskContext,
) -> Result<MeetingSummary> {
    let credential = state
        .credentials
        .attio_credential(params.credential_id)
        .await?;
    let scope = format!("attio:broker:{}", credential.id);
    sqlx::query(
        r#"
        INSERT INTO company_context_system.attio_checkpoints (scope_id)
        VALUES ($1)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(&scope)
    .execute(&state.pool)
    .await?;
    let result = async {
        let watermark: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT watermark_time FROM company_context_system.attio_checkpoints WHERE scope_id = $1",
        )
        .bind(&scope)
        .fetch_one(&state.pool)
        .await?;
        let now = Utc::now();
        let ends_from = match watermark {
            Some(watermark) => watermark - chrono::Duration::hours(REREAD_HOURS),
            None => now - chrono::Duration::days(state.config.attio_initial_lookback_days as i64),
        };
        // Upcoming meetings are left to Calendar; each is listed once it starts.
        let meetings = state
            .attio
            .list_meetings(&credential.access_token, ends_from, now)
            .await?
            .into_iter()
            .filter(|meeting| !meeting.id.meeting_id.is_empty())
            .collect::<Vec<_>>();
        for batch in meetings.chunks(MEETINGS_FETCH_BATCH_SIZE) {
            let ids = batch
                .iter()
                .map(|meeting| meeting.id.meeting_id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            state
                .absurd
                .spawn(
                    ATTIO_MEETINGS_FETCH_TASK,
                    MeetingsFetchParams {
                        credential_id: credential.id,
                        meetings: batch.to_vec(),
                    },
                    SpawnOptions {
                        idempotency_key: Some(format!(
                            "attio.meetings.fetch:{}:{}:{}",
                            credential.id,
                            params.bucket,
                            hex_sha256(ids.as_bytes())
                        )),
                        ..SpawnOptions::default()
                    },
                )
                .await?;
        }
        sqlx::query(
            r#"
            UPDATE company_context_system.attio_checkpoints
            SET watermark_time = GREATEST(watermark_time, $2),
                last_success_at = NOW(),
                last_error = '',
                updated_at = NOW()
            WHERE scope_id = $1
            "#,
        )
        .bind(&scope)
        .bind(now)
        .execute(&state.pool)
        .await?;
        Ok(meetings.len())
    }
    .await;

    match result {
        Ok(meetings) => {
            info!(
                event = "company_context_attio_sync_completed",
                task_id = ctx.task_id(),
                scope,
                meetings
            );
            Ok(MeetingSummary::new("completed", meetings))
        }
        Err(error) => {
            if let Err(db_error) = sqlx::query(
                r#"
                UPDATE company_context_system.attio_checkpoints
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
                    event = "company_context_attio_sync_rejected",
                    task_id = ctx.task_id(),
                    scope,
                    error = %error
                );
                return Ok(MeetingSummary::new("rejected", 0));
            }
            Err(error)
        }
    }
}

/// Fetches the transcripts of a batch of meetings and stages the meetings
/// whose content changed. A meeting Attio refuses is skipped until a later
/// sync lists it again.
async fn fetch_meetings(
    state: &TaskState,
    params: MeetingsFetchParams,
    ctx: &TaskContext,
) -> Result<MeetingSummary> {
    let credential = state
        .credentials
        .attio_credential(params.credential_id)
        .await?;
    let mut staged = 0;
    for meeting in &params.meetings {
        let meeting_id = &meeting.id.meeting_id;
        let recordings = match completed_recordings(state, &credential, meeting_id).await {
            Ok(recordings) => recordings,
            Err(error) if is_rejected(&error) => {
                warn!(
                    event = "company_context_attio_meeting_rejected",
                    task_id = ctx.task_id(),
                    credential_id = credential.id,
                    meeting_id,
                    error = %error
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some(revision) = stage_meeting(&state.pool, &credential, meeting, &recordings).await?
        else {
            continue;
        };
        state
            .absurd
            .spawn(
                ATTIO_MEETING_EMBED_TASK,
                MeetingEmbedParams {
                    credential_id: credential.id,
                    meeting_id: meeting_id.clone(),
                    revision,
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "attio.meeting.embed:{}:{meeting_id}:{revision}:{}",
                        credential.id,
                        state.embeddings.model()
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        staged += 1;
    }
    info!(
        event = "company_context_attio_meetings_fetched",
        task_id = ctx.task_id(),
        credential_id = credential.id,
        meetings = params.meetings.len(),
        meetings_staged = staged
    );
    Ok(MeetingSummary::new("completed", staged))
}

/// The meeting's completed call recordings, with their transcripts.
async fn completed_recordings(
    state: &TaskState,
    credential: &AttioCredential,
    meeting_id: &str,
) -> Result<Vec<CallRecording>> {
    let mut recordings = Vec::new();
    for listed in state
        .attio
        .list_call_recordings(&credential.access_token, meeting_id)
        .await?
    {
        if listed.status != "completed" || listed.id.call_recording_id.is_empty() {
            continue;
        }
        recordings.push(
            state
                .attio
                .call_recording(
                    &credential.access_token,
                    meeting_id,
                    &listed.id.call_recording_id,
                )
                .await?,
        );
    }
    Ok(recordings)
}

/// Renders the meeting as searchable text: its title, participants,
/// description, and transcripts, one paragraph per speaker turn.
fn meeting_content(meeting: &Meeting, recordings: &[CallRecording]) -> String {
    let mut parts = vec![meeting.title.trim().to_owned()];
    let participants = meeting
        .participants
        .iter()
        .filter_map(|participant| {
            let name = participant.name.as_deref().unwrap_or_default().trim();
            let email = participant.email_address.as_deref().unwrap_or_default();
            let label = match (name.is_empty(), email.is_empty()) {
                (true, true) => return None,
                (true, false) => email.to_owned(),
                (false, true) => name.to_owned(),
                (false, false) => format!("{name} <{email}>"),
            };
            Some(if participant.is_organizer {
                format!("{label} (organizer)")
            } else {
                label
            })
        })
        .collect::<Vec<_>>();
    if !participants.is_empty() {
        parts.push(format!("Participants: {}", participants.join(", ")));
    }
    parts.push(meeting.description.trim().to_owned());
    for recording in recordings {
        let mut turns: Vec<(String, String)> = Vec::new();
        for segment in recording
            .transcript
            .iter()
            .flat_map(|transcript| &transcript.segments)
        {
            let speech = segment.speech.trim();
            if speech.is_empty() {
                continue;
            }
            let speaker = segment.speaker.name.trim();
            match turns.last_mut() {
                Some((last, text)) if last == speaker => {
                    text.push(' ');
                    text.push_str(speech);
                }
                _ => turns.push((speaker.to_owned(), speech.to_owned())),
            }
        }
        if turns.is_empty() {
            continue;
        }
        parts.push("Transcript:".to_owned());
        parts.extend(turns.into_iter().map(|(speaker, text)| {
            if speaker.is_empty() {
                text
            } else {
                format!("{speaker}: {text}")
            }
        }));
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Stages the meeting. Returns the revision to embed when the meeting is
/// awaiting publication.
async fn stage_meeting(
    pool: &PgPool,
    credential: &AttioCredential,
    meeting: &Meeting,
    recordings: &[CallRecording],
) -> Result<Option<i64>> {
    let participants = json!(meeting.participants);
    let linked_records = json!(meeting.linked_records);
    let url = recordings
        .iter()
        .map(|recording| recording.web_url.as_str())
        .find(|url| !url.is_empty())
        .unwrap_or_default();
    let starts_at = meeting.start.instant();
    let ends_at = meeting.end.instant();
    let content_text = meeting_content(meeting, recordings);
    let content_hash = hex_sha256(
        json!([
            meeting.title,
            url,
            starts_at,
            ends_at,
            participants,
            linked_records,
            content_text
        ])
        .to_string()
        .as_bytes(),
    );
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_system.attio_meetings (
            broker_credential_id, meeting_id, provider_subject, title, url,
            starts_at, ends_at, participants, linked_records, content_text,
            content_hash
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ON CONFLICT (broker_credential_id, meeting_id) DO UPDATE
        SET provider_subject = EXCLUDED.provider_subject,
            title = EXCLUDED.title,
            url = EXCLUDED.url,
            starts_at = EXCLUDED.starts_at,
            ends_at = EXCLUDED.ends_at,
            participants = EXCLUDED.participants,
            linked_records = EXCLUDED.linked_records,
            content_text = EXCLUDED.content_text,
            content_hash = EXCLUDED.content_hash,
            revision = attio_meetings.revision + 1,
            embedding_status = 'pending',
            last_error = '',
            updated_at = NOW()
        WHERE attio_meetings.content_hash IS DISTINCT FROM EXCLUDED.content_hash
           OR attio_meetings.provider_subject IS DISTINCT FROM EXCLUDED.provider_subject
        "#,
    )
    .bind(credential.id)
    .bind(&meeting.id.meeting_id)
    .bind(&credential.workspace_id)
    .bind(&meeting.title)
    .bind(url)
    .bind(starts_at)
    .bind(ends_at)
    .bind(&participants)
    .bind(&linked_records)
    .bind(&content_text)
    .bind(&content_hash)
    .execute(&mut *tx)
    .await?;
    // Also returns unchanged meetings that are still pending, so a lost embed
    // spawn is recovered by the next fetch.
    let row = sqlx::query(
        r#"
        UPDATE company_context_system.attio_meetings
        SET last_seen_at = NOW()
        WHERE broker_credential_id = $1
          AND meeting_id = $2
        RETURNING revision, embedding_status
        "#,
    )
    .bind(credential.id)
    .bind(&meeting.id.meeting_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let status: String = row.try_get("embedding_status")?;
    Ok((status == "pending").then(|| row.get("revision")))
}

async fn embed_meeting(
    state: &TaskState,
    params: MeetingEmbedParams,
    ctx: &TaskContext,
) -> Result<MeetingSummary> {
    let Some(row) = sqlx::query(
        r#"
        SELECT provider_subject, title, url, starts_at, ends_at, participants,
               linked_records, content_text
        FROM company_context_system.attio_meetings
        WHERE broker_credential_id = $1
          AND meeting_id = $2
          AND revision = $3
          AND embedding_status = 'pending'
        "#,
    )
    .bind(params.credential_id)
    .bind(&params.meeting_id)
    .bind(params.revision)
    .fetch_optional(&state.pool)
    .await?
    else {
        return Ok(MeetingSummary::new("superseded", 0));
    };
    let provider_subject: String = row.try_get("provider_subject")?;
    let title: String = row.try_get("title")?;
    let url: String = row.try_get("url")?;
    let starts_at: Option<DateTime<Utc>> = row.try_get("starts_at")?;
    let ends_at: Option<DateTime<Utc>> = row.try_get("ends_at")?;
    let participants: serde_json::Value = row.try_get("participants")?;
    let linked_records: serde_json::Value = row.try_get("linked_records")?;
    let content_text: String = row.try_get("content_text")?;

    let chunks = chunk_text(&content_text, state.config.chunk_chars)
        .into_iter()
        .map(|chunk| {
            let document_id = format!(
                "{ATTIO_DOCUMENT_ID_PREFIX}{}:{}:{}",
                params.credential_id, params.meeting_id, chunk.chunk_id
            );
            let content_hash = hex_sha256(format!("{title}\n\n{}", chunk.body).as_bytes());
            (document_id, chunk.chunk_id, chunk.body, content_hash)
        })
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        let error = rejected("staged Attio meeting has no content");
        record_embedding_failure(&state.pool, &params, true, &error).await;
        return Ok(MeetingSummary::new("rejected", 0));
    }
    // Reuse vectors for chunks whose text is unchanged.
    let reusable: BTreeSet<String> = sqlx::query_scalar(
        r#"
        SELECT embeddings.document_id
        FROM company_context_data.attio_document_embeddings embeddings
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
                    event = "company_context_attio_embedding_rejected",
                    task_id = ctx.task_id(),
                    meeting_id = params.meeting_id,
                    error = %error
                );
                return Ok(MeetingSummary::new("rejected", 0));
            }
            return Err(error);
        }
    };

    let mut tx = state.pool.begin().await?;
    let current = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT revision = $3 AND embedding_status = 'pending'
        FROM company_context_system.attio_meetings
        WHERE broker_credential_id = $1
          AND meeting_id = $2
        FOR UPDATE
        "#,
    )
    .bind(params.credential_id)
    .bind(&params.meeting_id)
    .bind(params.revision)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if !current {
        tx.rollback().await?;
        return Ok(MeetingSummary::new("superseded", 0));
    }
    let mut document_ids = Vec::with_capacity(chunks.len());
    for (document_id, chunk_id, body, content_hash) in &chunks {
        document_ids.push(document_id.clone());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.attio_documents (
                document_id, broker_credential_id, meeting_id, provider_subject,
                chunk_id, title, body, url, starts_at, ends_at, participants,
                linked_records, content_hash, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW())
            ON CONFLICT (document_id) DO UPDATE
            SET provider_subject = EXCLUDED.provider_subject,
                title = EXCLUDED.title,
                body = EXCLUDED.body,
                url = EXCLUDED.url,
                starts_at = EXCLUDED.starts_at,
                ends_at = EXCLUDED.ends_at,
                participants = EXCLUDED.participants,
                linked_records = EXCLUDED.linked_records,
                content_hash = EXCLUDED.content_hash,
                updated_at = NOW()
            "#,
        )
        .bind(document_id)
        .bind(params.credential_id)
        .bind(&params.meeting_id)
        .bind(&provider_subject)
        .bind(chunk_id)
        .bind(&title)
        .bind(body)
        .bind(&url)
        .bind(starts_at)
        .bind(ends_at)
        .bind(&participants)
        .bind(&linked_records)
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
            INSERT INTO company_context_data.attio_document_embeddings (
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
        DELETE FROM company_context_data.attio_documents
        WHERE broker_credential_id = $1
          AND meeting_id = $2
          AND NOT (document_id = ANY($3::text[]))
        "#,
    )
    .bind(params.credential_id)
    .bind(&params.meeting_id)
    .bind(&document_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.attio_meetings
        SET embedding_status = 'completed',
            last_error = '',
            published_at = NOW(),
            updated_at = NOW()
        WHERE broker_credential_id = $1
          AND meeting_id = $2
        "#,
    )
    .bind(params.credential_id)
    .bind(&params.meeting_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_attio_meeting_published",
        task_id = ctx.task_id(),
        credential_id = params.credential_id,
        meeting_id = params.meeting_id,
        chunks = document_ids.len()
    );
    Ok(MeetingSummary::new("completed", 1))
}

async fn record_embedding_failure(
    pool: &PgPool,
    params: &MeetingEmbedParams,
    rejected: bool,
    error: &anyhow::Error,
) {
    // Retryable failures stay pending so that the next attempt can publish.
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE company_context_system.attio_meetings
        SET embedding_status = CASE WHEN $4 THEN 'rejected' ELSE embedding_status END,
            last_error = $5,
            updated_at = NOW()
        WHERE broker_credential_id = $1
          AND meeting_id = $2
          AND revision = $3
          AND embedding_status = 'pending'
        "#,
    )
    .bind(params.credential_id)
    .bind(&params.meeting_id)
    .bind(params.revision)
    .bind(rejected)
    .bind(bounded_error(error))
    .execute(pool)
    .await
    {
        error!(
            event = "company_context_failure_record_failed",
            meeting_id = params.meeting_id,
            error = %db_error
        );
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use sqlx::Executor;

    use super::*;
    use crate::{
        attio::{MeetingId, MeetingTime, Participant, Segment, Speaker, Transcript},
        test_support::TestDatabase,
    };

    fn credential(id: i64) -> AttioCredential {
        AttioCredential {
            id,
            access_token: "token".to_owned(),
            workspace_id: format!("ws-{id}"),
        }
    }

    fn meeting(description: &str) -> Meeting {
        Meeting {
            id: MeetingId {
                meeting_id: "m-1".to_owned(),
            },
            title: "Acme renewal".to_owned(),
            description: description.to_owned(),
            start: MeetingTime {
                datetime: Some("2026-07-08T15:30:00Z".to_owned()),
                date: None,
            },
            end: MeetingTime {
                datetime: Some("2026-07-08T16:00:00Z".to_owned()),
                date: None,
            },
            participants: vec![
                Participant {
                    email_address: Some("ada@example.com".to_owned()),
                    name: None,
                    is_organizer: true,
                },
                Participant {
                    email_address: Some("bob@acme.test".to_owned()),
                    name: Some("Bob".to_owned()),
                    is_organizer: false,
                },
            ],
            linked_records: Vec::new(),
        }
    }

    fn recording(segments: &[(&str, &str)]) -> CallRecording {
        CallRecording {
            web_url: "https://app.attio.com/recording".to_owned(),
            status: "completed".to_owned(),
            transcript: Some(Transcript {
                segments: segments
                    .iter()
                    .map(|(speaker, speech)| Segment {
                        speech: (*speech).to_owned(),
                        speaker: Speaker {
                            name: (*speaker).to_owned(),
                        },
                    })
                    .collect(),
            }),
            ..CallRecording::default()
        }
    }

    #[test]
    fn content_merges_consecutive_speech_by_one_speaker() {
        let recordings = [recording(&[
            ("Ada", "Hi Bob."),
            ("Ada", "Ready to renew?"),
            ("Bob", "Yes."),
            ("", "  "),
        ])];
        assert_eq!(
            meeting_content(&meeting("Q3 renewal."), &recordings),
            "Acme renewal\n\n\
             Participants: ada@example.com (organizer), Bob <bob@acme.test>\n\n\
             Q3 renewal.\n\n\
             Transcript:\n\n\
             Ada: Hi Bob. Ready to renew?\n\n\
             Bob: Yes."
        );
    }

    async fn status(pool: &PgPool) -> (String, i64) {
        let row = sqlx::query(
            "SELECT embedding_status, revision FROM company_context_system.attio_meetings WHERE meeting_id = 'm-1'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (row.get(0), row.get(1))
    }

    #[tokio::test]
    async fn meetings_are_republished_on_change_and_removed_with_their_credential() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "attio").await;
        let pool = database.pool.clone();
        let credential = credential(1);

        // Unchanged pending meetings are returned again so a lost embed spawn recovers.
        assert_eq!(
            stage_meeting(&pool, &credential, &meeting("Q3"), &[])
                .await
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            stage_meeting(&pool, &credential, &meeting("Q3"), &[])
                .await
                .unwrap(),
            Some(1)
        );
        (&pool)
            .execute(
                r#"
            UPDATE company_context_system.attio_meetings SET embedding_status = 'completed';
            INSERT INTO company_context_data.attio_documents
                (document_id, broker_credential_id, meeting_id, provider_subject, chunk_id, body, content_hash)
            VALUES ('attio:1:m-1:000000', 1, 'm-1', 'ws-1', '000000', 'Q3', 'hash');
            INSERT INTO company_context_system.attio_checkpoints (scope_id, watermark_time)
            VALUES ('attio:broker:1', now()), ('attio:broker:2', now());
            "#,
            )
            .await
            .unwrap();
        assert_eq!(
            stage_meeting(&pool, &credential, &meeting("Q3"), &[])
                .await
                .unwrap(),
            None
        );
        // A transcript that finished processing republishes the meeting.
        let transcript = [recording(&[("Ada", "Renewing.")])];
        assert_eq!(
            stage_meeting(&pool, &credential, &meeting("Q3"), &transcript)
                .await
                .unwrap(),
            Some(2)
        );
        assert_eq!(status(&pool).await, ("pending".to_owned(), 2));

        // Removing the credential removes its meetings, documents, and
        // checkpoint, so a revived credential backfills again.
        assert_eq!(remove_unretained(&pool, &[1]).await.unwrap(), 0);
        assert_eq!(remove_unretained(&pool, &[]).await.unwrap(), 1);
        let remaining: (i64, i64) = sqlx::query_as(
            r#"
            SELECT (SELECT count(*) FROM company_context_data.attio_documents),
                   (SELECT count(*) FROM company_context_system.attio_checkpoints)
            "#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, (0, 0));

        database.drop().await;
    }
}
