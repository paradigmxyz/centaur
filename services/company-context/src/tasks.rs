use std::{collections::BTreeSet, sync::Arc};

use absurd::{Client as AbsurdClient, Error as AbsurdError, SpawnOptions, TaskContext};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tracing::{error, info, warn};

use crate::{
    config::{
        Config, DOCUMENT_DELETE_TASK, DOCUMENT_EMBED_TASK, DRIVE_CREDENTIALS_RECONCILE_TASK,
        DRIVE_SCAN_TASK, PDF_EXTRACT_TASK,
    },
    credentials::GoogleCredential,
    drive::{DriveClient, DriveFile, Permission},
    embeddings::EmbeddingsClient,
    errors::{is_rejected, rejected},
    extraction::{chunk_text, extract_pdf_text, hex_sha256},
};

#[derive(Clone)]
pub struct TaskState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub absurd: AbsurdClient,
    pub drive: DriveClient,
    pub embeddings: EmbeddingsClient,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReconcileCredentialsParams {
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ScanParams {
    pub credential_id: i64,
    pub requested_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExtractParams {
    pub credential_id: i64,
    pub credential_revision: String,
    pub file: DriveFile,
    pub observation_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EmbedParams {
    pub credential_id: i64,
    pub credential_revision: String,
    pub file_id: String,
    pub content_hash: String,
    pub observation_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeleteParams {
    pub file_id: String,
    pub observation_key: String,
}

#[derive(Debug, Serialize)]
pub struct TaskSummary {
    status: &'static str,
    files: usize,
}

pub fn register(state: TaskState) -> Result<()> {
    let reconcile_state = state.clone();
    state.absurd.register_task(
        DRIVE_CREDENTIALS_RECONCILE_TASK,
        move |params: ReconcileCredentialsParams, ctx| {
            let state = reconcile_state.clone();
            async move { task_result(reconcile_credentials(&state, params, &ctx).await) }
        },
    )?;

    let scan_state = state.clone();
    state
        .absurd
        .register_task(DRIVE_SCAN_TASK, move |params: ScanParams, ctx| {
            let state = scan_state.clone();
            async move { task_result(scan_drive(&state, params, &ctx).await) }
        })?;

    let extract_state = state.clone();
    state
        .absurd
        .register_task(PDF_EXTRACT_TASK, move |params: ExtractParams, ctx| {
            let state = extract_state.clone();
            async move { task_result(extract_pdf(&state, params, &ctx).await) }
        })?;

    let embed_state = state.clone();
    state
        .absurd
        .register_task(DOCUMENT_EMBED_TASK, move |params: EmbedParams, ctx| {
            let state = embed_state.clone();
            async move { task_result(embed_document(&state, params, &ctx).await) }
        })?;

    let delete_client = state.absurd.clone();
    let delete_state = state;
    delete_client.register_task(DOCUMENT_DELETE_TASK, move |params: DeleteParams, ctx| {
        let state = delete_state.clone();
        async move { task_result(delete_document(&state, params, &ctx).await) }
    })?;
    Ok(())
}

async fn reconcile_credentials(
    state: &TaskState,
    params: ReconcileCredentialsParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let retained_ids = state
        .drive
        .credentials()
        .retained_google_credential_ids()
        .await?;
    let mut tx = state.pool.begin().await?;
    let stale_observations = sqlx::query(
        r#"
        UPDATE company_context_system.google_drive_broker_observations
        SET active = FALSE,
            updated_at = NOW()
        WHERE active
          AND NOT (broker_credential_id = ANY($1::bigint[]))
        RETURNING broker_credential_id, file_id
        "#,
    )
    .bind(&retained_ids)
    .fetch_all(&mut *tx)
    .await?;
    for observation in &stale_observations {
        let credential_id: i64 = observation.try_get("broker_credential_id")?;
        let file_id: String = observation.try_get("file_id")?;
        sqlx::query(
            r#"
            DELETE FROM company_context_data.google_drive_document_access
            WHERE file_id = $1
              AND permission_id = $2
            "#,
        )
        .bind(file_id)
        .bind(format!("broker:{credential_id}"))
        .execute(&mut *tx)
        .await?;
    }

    let candidate_rows = sqlx::query(
        r#"
        SELECT files.file_id
        FROM company_context_system.google_drive_files files
        WHERE NOT EXISTS (
            SELECT 1
            FROM company_context_system.google_drive_broker_observations observations
            WHERE observations.file_id = files.file_id
              AND observations.active
        )
          AND (
              files.extraction_status <> 'deleted'
              OR EXISTS (
                  SELECT 1
                  FROM company_context_data.google_drive_documents documents
                  WHERE documents.file_id = files.file_id
              )
          )
        "#,
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut deletions = Vec::new();
    let mut seen = BTreeSet::new();
    for row in candidate_rows {
        let file_id: String = row.try_get("file_id")?;
        if !seen.insert(file_id.clone()) {
            continue;
        }
        sqlx::query(
            r#"
            SELECT file_id
            FROM company_context_system.google_drive_files
            WHERE file_id = $1
            FOR UPDATE
            "#,
        )
        .bind(&file_id)
        .fetch_one(&mut *tx)
        .await?;
        let visible = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM company_context_system.google_drive_broker_observations
                WHERE file_id = $1
                  AND active
            )
            "#,
        )
        .bind(&file_id)
        .fetch_one(&mut *tx)
        .await?;
        if visible {
            continue;
        }
        let observation_key = format!("reconcile:{}:{file_id}", params.bucket);
        sqlx::query(
            r#"
            UPDATE company_context_system.google_drive_files
            SET source_version = $2,
                observation_key = $2,
                extraction_status = 'deleted',
                embedding_status = 'deleted',
                last_error = '',
                updated_at = NOW()
            WHERE file_id = $1
            "#,
        )
        .bind(&file_id)
        .bind(&observation_key)
        .execute(&mut *tx)
        .await?;
        deletions.push((file_id, observation_key));
    }
    tx.commit().await?;

    for (file_id, observation_key) in &deletions {
        state
            .absurd
            .spawn(
                DOCUMENT_DELETE_TASK,
                DeleteParams {
                    file_id: file_id.clone(),
                    observation_key: observation_key.clone(),
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "drive.document.delete:reconcile:{observation_key}"
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
    }
    info!(
        event = "company_context_credentials_reconciled",
        task_id = ctx.task_id(),
        observations_deactivated = stale_observations.len(),
        files_enqueued = deletions.len()
    );
    Ok(TaskSummary {
        status: "completed",
        files: deletions.len(),
    })
}

async fn scan_drive(
    state: &TaskState,
    params: ScanParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let credential = state
        .drive
        .credentials()
        .google_credential(params.credential_id)
        .await?;
    let scope = format!("user:broker:{}", credential.id);
    ensure_checkpoint(&state.pool, &scope).await?;
    let mut total = 0;
    for _ in 0..state.config.max_scan_pages {
        let checkpoint = load_checkpoint(&state.pool, &scope).await?;
        if !checkpoint.initial_scan_completed {
            let start_token = if checkpoint.initial_start_page_token.is_empty() {
                let token = state.drive.start_page_token(credential.id).await?;
                sqlx::query(
                    r#"
                    UPDATE company_context_system.google_drive_checkpoints
                    SET initial_start_page_token = $2,
                        updated_at = NOW()
                    WHERE scope_id = $1
                    "#,
                )
                .bind(&scope)
                .bind(&token)
                .execute(&state.pool)
                .await?;
                token
            } else {
                checkpoint.initial_start_page_token.clone()
            };
            let page = state
                .drive
                .list_user_pdfs(
                    credential.id,
                    state.config.scan_page_size,
                    nonempty(&checkpoint.initial_page_token),
                )
                .await?;
            total += enqueue_files(state, &credential, page.files).await?;
            if let Some(next_page_token) = page.next_page_token {
                sqlx::query(
                    r#"
                    UPDATE company_context_system.google_drive_checkpoints
                    SET initial_page_token = $2,
                        last_error = '',
                        updated_at = NOW()
                    WHERE scope_id = $1
                    "#,
                )
                .bind(&scope)
                .bind(next_page_token)
                .execute(&state.pool)
                .await?;
                continue;
            }
            sqlx::query(
                r#"
                UPDATE company_context_system.google_drive_checkpoints
                SET initial_scan_completed = TRUE,
                    initial_page_token = '',
                    changes_page_token = $2,
                    last_success_at = NOW(),
                    last_error = '',
                    updated_at = NOW()
                WHERE scope_id = $1
                "#,
            )
            .bind(&scope)
            .bind(start_token)
            .execute(&state.pool)
            .await?;
            continue;
        }

        if checkpoint.changes_page_token.is_empty() {
            return Err(anyhow!(
                "completed Drive checkpoint has no changes page token"
            ));
        }
        let page = state
            .drive
            .list_user_changes(
                credential.id,
                state.config.scan_page_size,
                &checkpoint.changes_page_token,
            )
            .await?;
        for change in page.changes {
            match change.file {
                Some(file) if !file.drive_id.is_empty() => {
                    // Shared Drives will use independent drive-scoped tasks and checkpoints.
                }
                Some(file) if !change.removed && file.is_active_user_pdf() => {
                    total += enqueue_file(state, &credential, file).await? as usize;
                }
                _ => {
                    enqueue_delete(
                        state,
                        &credential,
                        change.file_id,
                        &checkpoint.changes_page_token,
                    )
                    .await?;
                }
            }
        }
        if let Some(next_page_token) = page.next_page_token {
            sqlx::query(
                r#"
                UPDATE company_context_system.google_drive_checkpoints
                SET changes_page_token = $2,
                    last_error = '',
                    updated_at = NOW()
                WHERE scope_id = $1
                "#,
            )
            .bind(&scope)
            .bind(next_page_token)
            .execute(&state.pool)
            .await?;
            continue;
        }
        let new_token = page
            .new_start_page_token
            .context("Drive change page omitted both nextPageToken and newStartPageToken")?;
        sqlx::query(
            r#"
            UPDATE company_context_system.google_drive_checkpoints
            SET changes_page_token = $2,
                last_success_at = NOW(),
                last_error = '',
                updated_at = NOW()
            WHERE scope_id = $1
            "#,
        )
        .bind(&scope)
        .bind(new_token)
        .execute(&state.pool)
        .await?;
        break;
    }
    info!(
        event = "company_context_drive_user_scan_completed",
        task_id = ctx.task_id(),
        files_enqueued = total
    );
    Ok(TaskSummary {
        status: "completed",
        files: total,
    })
}

async fn enqueue_files(
    state: &TaskState,
    credential: &GoogleCredential,
    files: Vec<DriveFile>,
) -> Result<usize> {
    let mut count = 0;
    for file in files.into_iter().filter(DriveFile::is_active_user_pdf) {
        count += enqueue_file(state, credential, file).await? as usize;
    }
    Ok(count)
}

async fn enqueue_file(
    state: &TaskState,
    credential: &GoogleCredential,
    file: DriveFile,
) -> Result<bool> {
    let source_version = file.source_version();
    let observation_key = format!("file:{}:{source_version}", file.id);
    let needs_processing = observe_file(&state.pool, credential, &file, &observation_key).await?;
    if !needs_processing {
        return Ok(false);
    }
    let result = state
        .absurd
        .spawn(
            PDF_EXTRACT_TASK,
            ExtractParams {
                credential_id: credential.id,
                credential_revision: credential.revision.clone(),
                file: file.clone(),
                observation_key: observation_key.clone(),
            },
            SpawnOptions {
                idempotency_key: Some(format!(
                    "drive.pdf.extract:{}:{}:{source_version}:{}",
                    credential.id, file.id, credential.revision
                )),
                ..SpawnOptions::default()
            },
        )
        .await?;
    Ok(result.created)
}

async fn enqueue_delete(
    state: &TaskState,
    credential: &GoogleCredential,
    file_id: String,
    change_key: &str,
) -> Result<()> {
    if file_id.is_empty() {
        return Ok(());
    }
    let observation_key = format!("delete:{}:{file_id}:{change_key}", credential.id);
    if !observe_delete(&state.pool, credential.id, &file_id, &observation_key).await? {
        return Ok(());
    }
    state
        .absurd
        .spawn(
            DOCUMENT_DELETE_TASK,
            DeleteParams {
                file_id: file_id.clone(),
                observation_key: observation_key.clone(),
            },
            SpawnOptions {
                idempotency_key: Some(format!(
                    "drive.document.delete:{}:{file_id}:{change_key}",
                    credential.id
                )),
                ..SpawnOptions::default()
            },
        )
        .await?;
    Ok(())
}

async fn observe_file(
    pool: &PgPool,
    credential: &GoogleCredential,
    file: &DriveFile,
    observation_key: &str,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_system.google_drive_broker_observations (
            broker_credential_id, file_id, provider_email, provider_subject,
            observation_key, active, last_seen_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, TRUE, NOW(), NOW())
        ON CONFLICT (broker_credential_id, file_id) DO UPDATE
        SET provider_email = EXCLUDED.provider_email,
            provider_subject = EXCLUDED.provider_subject,
            observation_key = EXCLUDED.observation_key,
            active = TRUE,
            last_seen_at = NOW(),
            updated_at = NOW()
        "#,
    )
    .bind(credential.id)
    .bind(&file.id)
    .bind(&credential.provider_email)
    .bind(&credential.provider_subject)
    .bind(observation_key)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_data.google_drive_document_access (
            file_id, permission_id, permission_type, role, email_address,
            source_version, updated_at
        )
        VALUES ($1, $2, 'broker_user', 'reader', $3, $4, NOW())
        ON CONFLICT (file_id, permission_id) DO UPDATE
        SET email_address = EXCLUDED.email_address,
            source_version = EXCLUDED.source_version,
            updated_at = NOW()
        "#,
    )
    .bind(&file.id)
    .bind(format!("broker:{}", credential.id))
    .bind(&credential.provider_email)
    .bind(file.source_version())
    .execute(&mut *tx)
    .await?;
    let updated = sqlx::query(
        r#"
        INSERT INTO company_context_system.google_drive_files (
            file_id, name, mime_type, drive_id, web_view_link, source_version,
            observation_key, source_created_at, source_modified_at,
            extraction_status, embedding_status, last_error, metadata,
            last_seen_at, updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9,
            'pending', 'pending', '', $10, NOW(), NOW()
        )
        ON CONFLICT (file_id) DO UPDATE
        SET name = EXCLUDED.name,
            mime_type = EXCLUDED.mime_type,
            drive_id = EXCLUDED.drive_id,
            web_view_link = EXCLUDED.web_view_link,
            source_version = EXCLUDED.source_version,
            observation_key = EXCLUDED.observation_key,
            source_created_at = EXCLUDED.source_created_at,
            source_modified_at = EXCLUDED.source_modified_at,
            extraction_status = 'pending',
            embedding_status = 'pending',
            last_error = '',
            metadata = EXCLUDED.metadata,
            last_seen_at = NOW(),
            updated_at = NOW()
        WHERE google_drive_files.observation_key IS DISTINCT FROM EXCLUDED.observation_key
          AND (
              google_drive_files.source_modified_at IS NULL
              OR EXCLUDED.source_modified_at IS NULL
              OR EXCLUDED.source_modified_at >= google_drive_files.source_modified_at
          )
        "#,
    )
    .bind(&file.id)
    .bind(&file.name)
    .bind(&file.mime_type)
    .bind(&file.drive_id)
    .bind(&file.web_view_link)
    .bind(file.source_version())
    .bind(observation_key)
    .bind(file.created_time)
    .bind(file.modified_time)
    .bind(json!(file))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    tx.commit().await?;
    Ok(updated)
}

async fn observe_delete(
    pool: &PgPool,
    credential_id: i64,
    file_id: &str,
    observation_key: &str,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.google_drive_broker_observations
        SET active = FALSE,
            observation_key = $3,
            updated_at = NOW()
        WHERE broker_credential_id = $1
          AND file_id = $2
        "#,
    )
    .bind(credential_id)
    .bind(file_id)
    .bind(observation_key)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM company_context_data.google_drive_document_access
        WHERE file_id = $1
          AND permission_id = $2
        "#,
    )
    .bind(file_id)
    .bind(format!("broker:{credential_id}"))
    .execute(&mut *tx)
    .await?;
    let remains_visible = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM company_context_system.google_drive_broker_observations
            WHERE file_id = $1
              AND active
        )
        "#,
    )
    .bind(file_id)
    .fetch_one(&mut *tx)
    .await?;
    if remains_visible {
        tx.commit().await?;
        return Ok(false);
    }
    sqlx::query(
        r#"
        INSERT INTO company_context_system.google_drive_files (
            file_id, source_version, observation_key, extraction_status,
            embedding_status, last_error, updated_at
        )
        VALUES ($1, $2, $2, 'deleted', 'deleted', '', NOW())
        ON CONFLICT (file_id) DO UPDATE
        SET source_version = EXCLUDED.source_version,
            observation_key = EXCLUDED.observation_key,
            extraction_status = 'deleted',
            embedding_status = 'deleted',
            last_error = '',
            updated_at = NOW()
        "#,
    )
    .bind(file_id)
    .bind(observation_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

async fn extract_pdf(
    state: &TaskState,
    params: ExtractParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let file = params.file;
    let observation_key = params.observation_key;
    if !observation_is_current(&state.pool, &file.id, &observation_key).await? {
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    }
    let result = async {
        if !file.is_active_user_pdf() {
            return Err(rejected(
                "extract task received a non-PDF, trashed, or Shared Drive file",
            ));
        }
        let pdf = state
            .drive
            .download_pdf(params.credential_id, &file.id)
            .await?;
        let text = extract_pdf_text(
            pdf,
            state.config.extraction_timeout,
            state.config.max_extracted_bytes,
        )
        .await?;
        let chunks = chunk_text(&text, state.config.chunk_chars);
        if chunks.is_empty() {
            return Err(rejected("PDF produced no non-empty chunks"));
        }
        let content_hash = hex_sha256(text.as_bytes());
        let mut tx = state.pool.begin().await?;
        if !lock_current_observation(&mut tx, &file.id, &observation_key).await? {
            tx.rollback().await?;
            return Ok(0);
        }
        sqlx::query(
            r#"
            UPDATE company_context_system.google_drive_files
            SET content_hash = $3,
                extraction_status = 'completed',
                embedding_status = 'pending',
                last_error = '',
                updated_at = NOW()
            WHERE file_id = $1
              AND observation_key = $2
            "#,
        )
        .bind(&file.id)
        .bind(&observation_key)
        .bind(&content_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            DELETE FROM company_context_system.google_drive_chunks
            WHERE file_id = $1
            "#,
        )
        .bind(&file.id)
        .execute(&mut *tx)
        .await?;
        for chunk in &chunks {
            sqlx::query(
                r#"
                INSERT INTO company_context_system.google_drive_chunks (
                    file_id, chunk_id, ordinal, body, content_hash
                )
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(&file.id)
            .bind(&chunk.chunk_id)
            .bind(chunk.ordinal as i32)
            .bind(&chunk.body)
            .bind(&chunk.content_hash)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        state
            .absurd
            .spawn(
                DOCUMENT_EMBED_TASK,
                EmbedParams {
                    credential_id: params.credential_id,
                    credential_revision: params.credential_revision.clone(),
                    file_id: file.id.clone(),
                    content_hash: content_hash.clone(),
                    observation_key: observation_key.clone(),
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "drive.document.embed:{}:{}:{content_hash}:{}:{}",
                        params.credential_id,
                        file.id,
                        state.embeddings.model(),
                        params.credential_revision
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
        Result::<usize>::Ok(chunks.len())
    }
    .await;

    match result {
        Ok(0) => Ok(TaskSummary {
            status: "superseded",
            files: 0,
        }),
        Ok(chunks) => {
            info!(
                event = "company_context_pdf_extracted",
                task_id = ctx.task_id(),
                file_id = file.id,
                chunks
            );
            Ok(TaskSummary {
                status: "completed",
                files: 1,
            })
        }
        Err(error) => {
            let rejected = is_rejected(&error);
            record_file_failure(
                &state.pool,
                &file.id,
                &observation_key,
                "extraction",
                rejected,
                &error,
            )
            .await;
            if rejected {
                warn!(
                    event = "company_context_pdf_rejected",
                    task_id = ctx.task_id(),
                    file_id = file.id,
                    error = %error
                );
                Ok(TaskSummary {
                    status: "rejected",
                    files: 0,
                })
            } else {
                Err(error)
            }
        }
    }
}

async fn embed_document(
    state: &TaskState,
    params: EmbedParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let Some(row) = sqlx::query(
        r#"
        SELECT name,
               mime_type,
               drive_id,
               web_view_link,
               source_version,
               source_created_at,
               source_modified_at,
               content_hash,
               metadata
        FROM company_context_system.google_drive_files
        WHERE file_id = $1
          AND observation_key = $2
          AND extraction_status = 'completed'
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.observation_key)
    .fetch_optional(&state.pool)
    .await?
    else {
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    };
    let staged_hash: String = row.try_get("content_hash")?;
    if staged_hash != params.content_hash {
        info!(
            event = "company_context_embedding_superseded",
            task_id = ctx.task_id(),
            file_id = params.file_id
        );
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    }
    let chunk_rows = sqlx::query(
        r#"
        SELECT chunk_id, body
        FROM company_context_system.google_drive_chunks
        WHERE file_id = $1
        ORDER BY ordinal
        "#,
    )
    .bind(&params.file_id)
    .fetch_all(&state.pool)
    .await?;
    if chunk_rows.is_empty() {
        let error = rejected("staged Drive file has no chunks");
        record_embedding_failure(
            &state.pool,
            &params.file_id,
            &params.observation_key,
            true,
            &error,
        )
        .await;
        return Ok(TaskSummary {
            status: "rejected",
            files: 0,
        });
    }
    let title: String = row.try_get("name")?;
    let inputs = chunk_rows
        .iter()
        .map(|row| {
            let body = row.get::<String, _>("body");
            if title.is_empty() {
                body
            } else {
                format!("{title}\n\n{body}")
            }
        })
        .collect::<Vec<_>>();
    let result = state.embeddings.embed(&inputs).await;
    let embeddings = match result {
        Ok(embeddings) => embeddings,
        Err(error) => {
            let rejected = is_rejected(&error);
            record_embedding_failure(
                &state.pool,
                &params.file_id,
                &params.observation_key,
                rejected,
                &error,
            )
            .await;
            if rejected {
                warn!(
                    event = "company_context_embedding_rejected",
                    task_id = ctx.task_id(),
                    file_id = params.file_id,
                    error = %error
                );
                return Ok(TaskSummary {
                    status: "rejected",
                    files: 0,
                });
            }
            return Err(error);
        }
    };
    let metadata: Value = row.try_get("metadata")?;
    let file: DriveFile =
        serde_json::from_value(metadata.clone()).context("decode staged Drive metadata")?;
    let mut tx = state.pool.begin().await?;
    if !lock_current_observation(&mut tx, &file.id, &params.observation_key).await? {
        tx.rollback().await?;
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    }
    replace_access(&mut tx, &file.id, &file.source_version(), &file.permissions).await?;
    let mut document_ids = Vec::with_capacity(chunk_rows.len());
    for (chunk, embedding) in chunk_rows.iter().zip(embeddings) {
        let chunk_id: String = chunk.try_get("chunk_id")?;
        let body: String = chunk.try_get("body")?;
        let content_hash = hex_sha256(format!("{}\n\n{}", file.name, body).as_bytes());
        let document_id = format!("google-drive:{}:{chunk_id}", file.id);
        document_ids.push(document_id.clone());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.google_drive_documents (
                document_id, file_id, chunk_id, document_type, mime_type, title,
                body, url, drive_id, source_created_at, source_modified_at,
                source_version, content_hash, metadata, updated_at
            )
            VALUES (
                $1, $2, $3, 'pdf', $4, $5, $6, $7, $8, $9, $10, $11, $12,
                $13, NOW()
            )
            ON CONFLICT (document_id) DO UPDATE
            SET title = EXCLUDED.title,
                body = EXCLUDED.body,
                url = EXCLUDED.url,
                drive_id = EXCLUDED.drive_id,
                source_created_at = EXCLUDED.source_created_at,
                source_modified_at = EXCLUDED.source_modified_at,
                source_version = EXCLUDED.source_version,
                content_hash = EXCLUDED.content_hash,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&document_id)
        .bind(&file.id)
        .bind(&chunk_id)
        .bind(&file.mime_type)
        .bind(&file.name)
        .bind(&body)
        .bind(&file.web_view_link)
        .bind(&file.drive_id)
        .bind(file.created_time)
        .bind(file.modified_time)
        .bind(file.source_version())
        .bind(&content_hash)
        .bind(&metadata)
        .execute(&mut *tx)
        .await?;
        let vector = serde_json::to_string(&embedding)?;
        sqlx::query(
            r#"
            INSERT INTO company_context_data.google_drive_document_embeddings (
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
        .bind(&document_id)
        .bind(state.embeddings.model())
        .bind(state.embeddings.dimensions() as i32)
        .bind(&content_hash)
        .bind(vector)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        r#"
        DELETE FROM company_context_data.google_drive_documents
        WHERE file_id = $1
          AND NOT (document_id = ANY($2::text[]))
        "#,
    )
    .bind(&file.id)
    .bind(&document_ids)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.google_drive_files
        SET embedding_status = 'completed',
            last_error = '',
            published_at = NOW(),
            updated_at = NOW()
        WHERE file_id = $1
          AND content_hash = $2
          AND observation_key = $3
        "#,
    )
    .bind(&file.id)
    .bind(&params.content_hash)
    .bind(&params.observation_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_drive_document_published",
        task_id = ctx.task_id(),
        file_id = file.id,
        chunks = document_ids.len()
    );
    Ok(TaskSummary {
        status: "completed",
        files: 1,
    })
}

async fn replace_access(
    tx: &mut Transaction<'_, Postgres>,
    file_id: &str,
    source_version: &str,
    permissions: &[Permission],
) -> Result<()> {
    sqlx::query(
        r#"
        DELETE FROM company_context_data.google_drive_document_access
        WHERE file_id = $1
          AND permission_type <> 'broker_user'
        "#,
    )
    .bind(file_id)
    .execute(&mut **tx)
    .await?;
    for permission in permissions
        .iter()
        .filter(|permission| !permission.id.is_empty())
    {
        sqlx::query(
            r#"
            INSERT INTO company_context_data.google_drive_document_access (
                file_id, permission_id, permission_type, role, email_address,
                domain, allow_file_discovery, source_version
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(file_id)
        .bind(&permission.id)
        .bind(&permission.permission_type)
        .bind(&permission.role)
        .bind(&permission.email_address)
        .bind(&permission.domain)
        .bind(permission.allow_file_discovery)
        .bind(source_version)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn delete_document(
    state: &TaskState,
    params: DeleteParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let mut tx = state.pool.begin().await?;
    if !lock_current_observation(&mut tx, &params.file_id, &params.observation_key).await? {
        tx.rollback().await?;
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    }
    let visible = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM company_context_system.google_drive_broker_observations
            WHERE file_id = $1
              AND active
        )
        "#,
    )
    .bind(&params.file_id)
    .fetch_one(&mut *tx)
    .await?;
    if visible {
        tx.rollback().await?;
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    }
    sqlx::query(
        r#"
        DELETE FROM company_context_data.google_drive_documents
        WHERE file_id = $1
        "#,
    )
    .bind(&params.file_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM company_context_data.google_drive_document_access
        WHERE file_id = $1
        "#,
    )
    .bind(&params.file_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM company_context_system.google_drive_chunks
        WHERE file_id = $1
        "#,
    )
    .bind(&params.file_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        UPDATE company_context_system.google_drive_files
        SET extraction_status = 'deleted',
            embedding_status = 'deleted',
            last_error = '',
            updated_at = NOW()
        WHERE file_id = $1
          AND observation_key = $2
        "#,
    )
    .bind(&params.file_id)
    .bind(&params.observation_key)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "company_context_drive_document_deleted",
        task_id = ctx.task_id(),
        file_id = params.file_id
    );
    Ok(TaskSummary {
        status: "completed",
        files: 1,
    })
}

struct Checkpoint {
    initial_start_page_token: String,
    initial_page_token: String,
    initial_scan_completed: bool,
    changes_page_token: String,
}

async fn ensure_checkpoint(pool: &PgPool, scope: &str) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO company_context_system.google_drive_checkpoints (scope_id)
        VALUES ($1)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(scope)
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_checkpoint(pool: &PgPool, scope: &str) -> Result<Checkpoint> {
    let row = sqlx::query(
        r#"
        SELECT initial_start_page_token,
               initial_page_token,
               initial_scan_completed,
               changes_page_token
        FROM company_context_system.google_drive_checkpoints
        WHERE scope_id = $1
        "#,
    )
    .bind(scope)
    .fetch_one(pool)
    .await?;
    Ok(Checkpoint {
        initial_start_page_token: row.try_get("initial_start_page_token")?,
        initial_page_token: row.try_get("initial_page_token")?,
        initial_scan_completed: row.try_get("initial_scan_completed")?,
        changes_page_token: row.try_get("changes_page_token")?,
    })
}

async fn observation_is_current(
    pool: &PgPool,
    file_id: &str,
    observation_key: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM company_context_system.google_drive_files
            WHERE file_id = $1
              AND observation_key = $2
        )
        "#,
    )
    .bind(file_id)
    .bind(observation_key)
    .fetch_one(pool)
    .await?)
}

async fn lock_current_observation(
    tx: &mut Transaction<'_, Postgres>,
    file_id: &str,
    observation_key: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, String>(
        r#"
        SELECT observation_key
        FROM company_context_system.google_drive_files
        WHERE file_id = $1
        FOR UPDATE
        "#,
    )
    .bind(file_id)
    .fetch_optional(&mut **tx)
    .await?
    .is_some_and(|current| current == observation_key))
}

async fn record_file_failure(
    pool: &PgPool,
    file_id: &str,
    observation_key: &str,
    stage: &str,
    rejected: bool,
    error: &anyhow::Error,
) {
    let message = bounded_error(error);
    let status_column = if stage == "extraction" {
        "extraction_status"
    } else {
        "embedding_status"
    };
    let status = if rejected { "rejected" } else { "failed" };
    let query = format!(
        r#"
        UPDATE company_context_system.google_drive_files
        SET {status_column} = $4,
            last_error = $3,
            updated_at = NOW()
        WHERE file_id = $1
          AND observation_key = $2
        "#
    );
    if let Err(db_error) = sqlx::query(&query)
        .bind(file_id)
        .bind(observation_key)
        .bind(message)
        .bind(status)
        .execute(pool)
        .await
    {
        error!(event = "company_context_failure_record_failed", file_id, error = %db_error);
    }
}

async fn record_embedding_failure(
    pool: &PgPool,
    file_id: &str,
    observation_key: &str,
    rejected: bool,
    error: &anyhow::Error,
) {
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE company_context_system.google_drive_files
        SET embedding_status = $4,
            last_error = $3,
            updated_at = NOW()
        WHERE file_id = $1
          AND observation_key = $2
        "#,
    )
    .bind(file_id)
    .bind(observation_key)
    .bind(bounded_error(error))
    .bind(if rejected { "rejected" } else { "failed" })
    .execute(pool)
    .await
    {
        error!(event = "company_context_failure_record_failed", file_id, error = %db_error);
    }
}

fn bounded_error(error: &anyhow::Error) -> String {
    error.to_string().chars().take(1_000).collect()
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn task_result<T>(result: Result<T>) -> absurd::Result<T> {
    result.map_err(|error| AbsurdError::TaskFailed(error.into_boxed_dyn_error()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_page_tokens_are_omitted() {
        assert_eq!(nonempty(""), None);
        assert_eq!(nonempty("next"), Some("next"));
    }

    #[test]
    fn errors_are_bounded() {
        let error = anyhow!("{}", "x".repeat(2_000));
        assert_eq!(bounded_error(&error).len(), 1_000);
    }
}
