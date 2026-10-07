use std::{
    collections::{BTreeMap, BTreeSet},
    pin::pin,
    sync::Arc,
    time::Duration,
};

use absurd::{Client as AbsurdClient, Error as AbsurdError, SpawnOptions, TaskContext};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::{
    config::{
        Config, DOCUMENT_DELETE_TASK, DOCUMENT_EMBED_TASK, DOCUMENT_EXTRACT_TASK,
        DRIVE_CREDENTIALS_RECONCILE_TASK, DRIVE_SCAN_TASK, GOOGLE_DOC_MIME_TYPE, PDF_MIME_TYPE,
        SHARED_DRIVE_SCAN_TASK, SHARED_DRIVES_DISCOVER_TASK, SHARED_FOLDERS_BATCH_TASK,
    },
    credentials::{ConsoleCredentials, GoogleCredential},
    drive::{DriveChange, DriveClient, DriveFile},
    embeddings::EmbeddingsClient,
    errors::{is_rejected, rejected},
    extraction::{chunk_text, extract_google_doc_text, extract_pdf_text, hex_sha256},
    granola::GranolaClient,
    granola_tasks, slack_documents,
};

#[derive(Clone)]
pub struct TaskState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub absurd: AbsurdClient,
    pub credentials: Arc<ConsoleCredentials>,
    pub drive: DriveClient,
    pub granola: GranolaClient,
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
pub struct DiscoverSharedDrivesParams {
    pub credential_id: i64,
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SharedDriveScanParams {
    pub credential_id: i64,
    pub drive_id: String,
    pub bucket: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FolderBatchParams {
    pub credential_id: i64,
    pub drive_id: String,
    pub bucket: u64,
    pub folder_ids: Vec<String>,
}

/// A walked file not reached for this long is presumed deleted, moved, or
/// unshared. Walks run every scan interval, so this spans many walks.
const FOLDER_WALK_EXPIRY_SECONDS: f64 = 24.0 * 60.0 * 60.0;

/// Folders and files in one Shared Drive shared directly with a non-member.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SharedFolderRoots {
    pub folder_ids: Vec<String>,
    pub files: Vec<DriveFile>,
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
    granola_tasks::register(&state)?;
    slack_documents::register(&state)?;

    let reconcile_state = state.clone();
    state.absurd.register_task(
        DRIVE_CREDENTIALS_RECONCILE_TASK,
        move |params: ReconcileCredentialsParams, ctx| {
            let state = reconcile_state.clone();
            async move { run_task(&ctx, reconcile_credentials(&state, params, &ctx)).await }
        },
    )?;

    let scan_state = state.clone();
    state
        .absurd
        .register_task(DRIVE_SCAN_TASK, move |params: ScanParams, ctx| {
            let state = scan_state.clone();
            async move { run_task(&ctx, scan_drive(&state, params, &ctx)).await }
        })?;

    let discover_state = state.clone();
    state.absurd.register_task(
        SHARED_DRIVES_DISCOVER_TASK,
        move |params: DiscoverSharedDrivesParams, ctx| {
            let state = discover_state.clone();
            async move { run_task(&ctx, discover_shared_drives(&state, params, &ctx)).await }
        },
    )?;

    let batch_state = state.clone();
    state.absurd.register_task(
        SHARED_FOLDERS_BATCH_TASK,
        move |params: FolderBatchParams, ctx| {
            let state = batch_state.clone();
            async move { run_task(&ctx, walk_folder_batch(&state, params, &ctx)).await }
        },
    )?;

    let shared_scan_state = state.clone();
    state.absurd.register_task(
        SHARED_DRIVE_SCAN_TASK,
        move |params: SharedDriveScanParams, ctx| {
            let state = shared_scan_state.clone();
            async move { run_task(&ctx, scan_shared_drive(&state, params, &ctx)).await }
        },
    )?;

    let extract_state = state.clone();
    state
        .absurd
        .register_task(DOCUMENT_EXTRACT_TASK, move |params: ExtractParams, ctx| {
            let state = extract_state.clone();
            async move { run_task(&ctx, extract_document(&state, params, &ctx)).await }
        })?;

    let embed_state = state.clone();
    state
        .absurd
        .register_task(DOCUMENT_EMBED_TASK, move |params: EmbedParams, ctx| {
            let state = embed_state.clone();
            async move { run_task(&ctx, embed_document(&state, params, &ctx)).await }
        })?;

    let delete_client = state.absurd.clone();
    let delete_state = state;
    delete_client.register_task(DOCUMENT_DELETE_TASK, move |params: DeleteParams, ctx| {
        let state = delete_state.clone();
        async move { run_task(&ctx, delete_document(&state, params, &ctx)).await }
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
        UPDATE company_context_data.google_drive_broker_observations
        SET active = FALSE,
            updated_at = NOW()
        WHERE active
          AND NOT (broker_credential_id = ANY($1::bigint[]))
        "#,
    )
    .bind(&retained_ids)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let candidate_rows = sqlx::query(
        r#"
        SELECT files.file_id
        FROM company_context_system.google_drive_files files
        WHERE NOT EXISTS (
            SELECT 1
            FROM company_context_data.google_drive_broker_observations observations
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
                FROM company_context_data.google_drive_broker_observations
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
        observations_deactivated = stale_observations,
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
    scan_corpus(state, params.credential_id, None, ctx).await
}

async fn scan_shared_drive(
    state: &TaskState,
    params: SharedDriveScanParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    scan_corpus(state, params.credential_id, Some(&params.drive_id), ctx).await
}

/// Lists the Shared Drives a credential belongs to, revokes that credential's
/// observations of files in drives it has left, and enqueues one scan per drive.
async fn discover_shared_drives(
    state: &TaskState,
    params: DiscoverSharedDrivesParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let credential = state
        .drive
        .credentials()
        .google_credential(params.credential_id)
        .await?;
    let mut drive_ids = BTreeSet::new();
    let mut page_token: Option<String> = None;
    loop {
        let page = state
            .drive
            .list_shared_drives(credential.id, page_token.as_deref())
            .await?;
        drive_ids.extend(
            page.drives
                .into_iter()
                .map(|drive| drive.id)
                .filter(|id| !id.is_empty()),
        );
        match page.next_page_token {
            Some(next) if !next.is_empty() => page_token = Some(next),
            _ => break,
        }
    }
    let member_drive_ids: Vec<String> = drive_ids.into_iter().collect();

    // Shared Drive items shared with this user outside drive membership are
    // reachable only by walking down from the shared folders and files.
    let mut shared_with_me = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let page = state
            .drive
            .list_shared_with_me(credential.id, page_token.as_deref())
            .await?;
        shared_with_me.extend(page.files);
        match page.next_page_token {
            Some(next) if !next.is_empty() => page_token = Some(next),
            _ => break,
        }
    }
    let folder_roots = group_folder_roots(shared_with_me, &member_drive_ids);
    let folder_drive_ids: Vec<String> = folder_roots.keys().cloned().collect();
    let reachable_drive_ids: Vec<String> = member_drive_ids
        .iter()
        .chain(&folder_drive_ids)
        .cloned()
        .collect();

    // Remove access to files in drives no longer reachable, and to files in
    // walked drives that recent walks stopped reaching.
    let departed_files: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT observations.file_id
        FROM company_context_data.google_drive_broker_observations observations
        JOIN company_context_system.google_drive_files files
          ON files.file_id = observations.file_id
        WHERE observations.broker_credential_id = $1
          AND observations.active
          AND files.drive_id <> ''
          AND (
              NOT (files.drive_id = ANY($2::text[]))
              OR (
                  files.drive_id = ANY($3::text[])
                  AND observations.last_seen_at < NOW() - make_interval(secs => $4)
              )
          )
        "#,
    )
    .bind(credential.id)
    .bind(&reachable_drive_ids)
    .bind(&folder_drive_ids)
    .bind(FOLDER_WALK_EXPIRY_SECONDS)
    .fetch_all(&state.pool)
    .await?;
    let change_key = format!("shared_drives:{}", params.bucket);
    for file_id in &departed_files {
        enqueue_delete(state, credential.id, file_id.clone(), &change_key).await?;
    }

    // Forget progress for drives no longer reached this way so that returning
    // to one starts from a full scan.
    let member_scopes: Vec<String> = member_drive_ids
        .iter()
        .map(|drive_id| checkpoint_scope(credential.id, Some(drive_id)))
        .collect();
    sqlx::query(
        r#"
        DELETE FROM company_context_system.google_drive_checkpoints
        WHERE scope_id LIKE $1
          AND NOT (scope_id = ANY($2::text[]))
        "#,
    )
    .bind(format!("shared_drive:%:broker:{}", credential.id))
    .bind(&member_scopes)
    .execute(&state.pool)
    .await?;

    for drive_id in &member_drive_ids {
        state
            .absurd
            .spawn(
                SHARED_DRIVE_SCAN_TASK,
                SharedDriveScanParams {
                    credential_id: credential.id,
                    drive_id: drive_id.clone(),
                    bucket: params.bucket,
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "drive.shared_drive.scan:{}:{drive_id}:{}",
                        credential.id, params.bucket
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
    }
    for (drive_id, roots) in folder_roots {
        for file in roots.files {
            enqueue_file(state, &credential, file).await?;
        }
        spawn_folder_batches(
            state,
            credential.id,
            &drive_id,
            params.bucket,
            &roots.folder_ids,
        )
        .await?;
    }
    info!(
        event = "company_context_shared_drives_discovered",
        task_id = ctx.task_id(),
        credential_id = credential.id,
        member_drives = member_drive_ids.len(),
        shared_folder_drives = folder_drive_ids.len(),
        departed_files = departed_files.len()
    );
    Ok(TaskSummary {
        status: "completed",
        files: departed_files.len(),
    })
}

/// Groups items shared with the user by the Shared Drive they live in, keeping
/// only drives the user is not a member of; member drives are scanned whole.
fn group_folder_roots(
    shared_with_me: Vec<DriveFile>,
    member_drive_ids: &[String],
) -> BTreeMap<String, SharedFolderRoots> {
    let mut roots: BTreeMap<String, SharedFolderRoots> = BTreeMap::new();
    for file in shared_with_me {
        if file.drive_id.is_empty() || member_drive_ids.contains(&file.drive_id) {
            continue;
        }
        if file.is_active_folder() {
            roots
                .entry(file.drive_id.clone())
                .or_default()
                .folder_ids
                .push(file.id);
        } else if file.is_active_document() {
            roots
                .entry(file.drive_id.clone())
                .or_default()
                .files
                .push(file);
        }
    }
    roots
}

/// Lists the children of a batch of shared folders in a Shared Drive the user
/// is not a member of, records supported documents, and spawns batches for the subfolders.
/// Discovery spawns the root batches each interval, and its sweep removes files
/// that these walks have not reached for a while.
async fn walk_folder_batch(
    state: &TaskState,
    params: FolderBatchParams,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let credential = state
        .drive
        .credentials()
        .google_credential(params.credential_id)
        .await?;
    let mut files = 0;
    let mut child_folder_ids = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let page = state
            .drive
            .list_folder_children(credential.id, &params.folder_ids, page_token.as_deref())
            .await?;
        for child in page.files {
            if !child.belongs_to(Some(&params.drive_id)) {
                continue;
            }
            if child.is_active_folder() {
                child_folder_ids.push(child.id);
            } else if child.is_active_document() {
                files += enqueue_file(state, &credential, child).await? as usize;
            }
        }
        match page.next_page_token {
            Some(next) if !next.is_empty() => page_token = Some(next),
            _ => break,
        }
    }
    spawn_folder_batches(
        state,
        credential.id,
        &params.drive_id,
        params.bucket,
        &child_folder_ids,
    )
    .await?;
    info!(
        event = "company_context_shared_folders_batch_walked",
        task_id = ctx.task_id(),
        credential_id = credential.id,
        drive_id = params.drive_id,
        folders = params.folder_ids.len(),
        subfolders = child_folder_ids.len(),
        files_enqueued = files
    );
    Ok(TaskSummary {
        status: "completed",
        files,
    })
}

/// Spawns folder batches of at most the configured size. Keys derive from the
/// interval and the folders, so retries and repeated discovery do not respawn
/// a batch within an interval.
async fn spawn_folder_batches(
    state: &TaskState,
    credential_id: i64,
    drive_id: &str,
    bucket: u64,
    folder_ids: &[String],
) -> Result<()> {
    for chunk in folder_ids.chunks(state.config.folder_walk_batch_size) {
        let key = hex_sha256(chunk.join(",").as_bytes());
        state
            .absurd
            .spawn(
                SHARED_FOLDERS_BATCH_TASK,
                FolderBatchParams {
                    credential_id,
                    drive_id: drive_id.to_owned(),
                    bucket,
                    folder_ids: chunk.to_vec(),
                },
                SpawnOptions {
                    idempotency_key: Some(format!(
                        "drive.shared_folders.batch:{credential_id}:{drive_id}:{bucket}:{key}"
                    )),
                    ..SpawnOptions::default()
                },
            )
            .await?;
    }
    Ok(())
}

fn checkpoint_scope(credential_id: i64, shared_drive_id: Option<&str>) -> String {
    match shared_drive_id {
        Some(drive_id) => format!("shared_drive:{drive_id}:broker:{credential_id}"),
        None => format!("user:broker:{credential_id}"),
    }
}

#[derive(Debug)]
enum ChangeAction {
    Observe(Box<DriveFile>),
    Remove(String),
    Skip,
}

/// Each file is scanned by exactly one corpus: My Drive (`None`) or its Shared
/// Drive. Files in another corpus are left to that corpus's scan.
fn classify_change(change: DriveChange, shared_drive_id: Option<&str>) -> ChangeAction {
    match change.file {
        Some(file) if !file.belongs_to(shared_drive_id) => ChangeAction::Skip,
        Some(file) if !change.removed && file.is_active_document() => {
            ChangeAction::Observe(Box::new(file))
        }
        _ => ChangeAction::Remove(change.file_id),
    }
}

async fn scan_corpus(
    state: &TaskState,
    credential_id: i64,
    shared_drive_id: Option<&str>,
    ctx: &TaskContext,
) -> Result<TaskSummary> {
    let credential = state
        .drive
        .credentials()
        .google_credential(credential_id)
        .await?;
    let scope = checkpoint_scope(credential.id, shared_drive_id);
    ensure_checkpoint(&state.pool, &scope).await?;
    let mut total = 0;
    for _ in 0..state.config.max_scan_pages {
        let checkpoint = load_checkpoint(&state.pool, &scope).await?;
        if !checkpoint.initial_scan_completed {
            let start_token = if checkpoint.initial_start_page_token.is_empty() {
                let token = state
                    .drive
                    .start_page_token(credential.id, shared_drive_id)
                    .await?;
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
                .list_documents(
                    credential.id,
                    shared_drive_id,
                    state.config.scan_page_size,
                    nonempty(&checkpoint.initial_page_token),
                )
                .await?;
            total += enqueue_files(state, &credential, shared_drive_id, page.files).await?;
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
            .list_changes(
                credential.id,
                shared_drive_id,
                state.config.scan_page_size,
                &checkpoint.changes_page_token,
            )
            .await?;
        let change_key = format!("{scope}:{}", checkpoint.changes_page_token);
        for change in page.changes {
            match classify_change(change, shared_drive_id) {
                ChangeAction::Observe(file) => {
                    total += enqueue_file(state, &credential, *file).await? as usize;
                }
                ChangeAction::Remove(file_id) => {
                    enqueue_delete(state, credential.id, file_id, &change_key).await?;
                }
                ChangeAction::Skip => {}
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
        event = "company_context_drive_scan_completed",
        task_id = ctx.task_id(),
        scope,
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
    shared_drive_id: Option<&str>,
    files: Vec<DriveFile>,
) -> Result<usize> {
    let mut count = 0;
    for file in files
        .into_iter()
        .filter(|file| file.is_active_document() && file.belongs_to(shared_drive_id))
    {
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
            DOCUMENT_EXTRACT_TASK,
            ExtractParams {
                credential_id: credential.id,
                credential_revision: credential.revision.clone(),
                file: file.clone(),
                observation_key: observation_key.clone(),
            },
            SpawnOptions {
                idempotency_key: Some(format!(
                    "drive.document.extract:{}:{}:{source_version}:{}",
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
    credential_id: i64,
    file_id: String,
    change_key: &str,
) -> Result<()> {
    if file_id.is_empty() {
        return Ok(());
    }
    let observation_key = format!("delete:{credential_id}:{file_id}:{change_key}");
    if !observe_delete(&state.pool, credential_id, &file_id, &observation_key).await? {
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
                    "drive.document.delete:{credential_id}:{file_id}:{change_key}"
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
        INSERT INTO company_context_data.google_drive_broker_observations (
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
    let observed = sqlx::query(
        r#"
        UPDATE company_context_data.google_drive_broker_observations
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
    .await?
    .rows_affected()
        > 0;
    if !observed {
        // Changes also report folders and other files this credential never
        // observed; there is nothing to revoke for them.
        tx.rollback().await?;
        return Ok(false);
    }
    let remains_visible = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM company_context_data.google_drive_broker_observations
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

async fn extract_document(
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
        if !file.is_active_document() {
            return Err(rejected(
                "extract task received an unsupported or trashed file",
            ));
        }
        let text = match file.mime_type.as_str() {
            PDF_MIME_TYPE => {
                let pdf = state
                    .drive
                    .download_pdf(params.credential_id, &file.id)
                    .await?;
                extract_pdf_text(
                    pdf,
                    state.config.extraction_timeout,
                    state.config.max_extracted_bytes,
                )
                .await?
            }
            GOOGLE_DOC_MIME_TYPE => {
                let document = state
                    .drive
                    .export_google_doc(params.credential_id, &file.id)
                    .await?;
                extract_google_doc_text(document, state.config.max_extracted_bytes)?
            }
            _ => unreachable!("active documents have a supported MIME type"),
        };
        let chunks = chunk_text(&text, state.config.chunk_chars);
        if chunks.is_empty() {
            return Err(rejected("Drive document produced no non-empty chunks"));
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
                        "drive.document.embed:{}:{}:{content_hash}:{}:{}:{observation_key}",
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
                event = "company_context_drive_document_extracted",
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
                    event = "company_context_drive_document_rejected",
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
    let metadata: Value = row.try_get("metadata")?;
    let file: DriveFile =
        serde_json::from_value(metadata.clone()).context("decode staged Drive metadata")?;
    let mut chunks = Vec::with_capacity(chunk_rows.len());
    for chunk in &chunk_rows {
        let chunk_id: String = chunk.try_get("chunk_id")?;
        let body: String = chunk.try_get("body")?;
        let content_hash = hex_sha256(format!("{}\n\n{}", file.name, body).as_bytes());
        let document_id = format!("google-drive:{}:{chunk_id}", file.id);
        chunks.push((document_id, chunk_id, body, content_hash));
    }
    // Drive versions change for metadata-only edits; reuse vectors for unchanged chunk text.
    let reusable: BTreeSet<String> = sqlx::query_scalar(
        r#"
        SELECT embeddings.document_id
        FROM company_context_data.google_drive_document_embeddings embeddings
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
        .map(|(_, _, body, _)| {
            if file.name.is_empty() {
                body.clone()
            } else {
                format!("{}\n\n{body}", file.name)
            }
        })
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
    let mut tx = state.pool.begin().await?;
    if !lock_current_observation(&mut tx, &file.id, &params.observation_key).await? {
        tx.rollback().await?;
        return Ok(TaskSummary {
            status: "superseded",
            files: 0,
        });
    }
    let mut document_ids = Vec::with_capacity(chunks.len());
    for (document_id, chunk_id, body, content_hash) in &chunks {
        document_ids.push(document_id.clone());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.google_drive_documents (
                document_id, file_id, chunk_id, document_type, mime_type, title,
                body, url, drive_id, source_created_at, source_modified_at,
                source_version, content_hash, metadata, updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, NOW()
            )
            ON CONFLICT (document_id) DO UPDATE
            SET document_type = EXCLUDED.document_type,
                mime_type = EXCLUDED.mime_type,
                title = EXCLUDED.title,
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
        .bind(document_id)
        .bind(&file.id)
        .bind(chunk_id)
        .bind(
            file.document_type()
                .context("staged Drive file has unsupported MIME type")?,
        )
        .bind(&file.mime_type)
        .bind(&file.name)
        .bind(body)
        .bind(&file.web_view_link)
        .bind(&file.drive_id)
        .bind(file.created_time)
        .bind(file.modified_time)
        .bind(file.source_version())
        .bind(content_hash)
        .bind(&metadata)
        .execute(&mut *tx)
        .await?;
        if reusable.contains(document_id) {
            continue;
        }
        let embedding = embeddings
            .next()
            .context("embeddings response omitted a chunk")?;
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
        .bind(document_id)
        .bind(state.embeddings.model())
        .bind(state.embeddings.dimensions() as i32)
        .bind(content_hash)
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
            FROM company_context_data.google_drive_broker_observations
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

pub(crate) fn bounded_error(error: &anyhow::Error) -> String {
    error.to_string().chars().take(1_000).collect()
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

/// Absurd reclaims a task whose lease is not extended within the worker's
/// claim timeout (120 seconds by default), and the worker exits once a task
/// overruns it twice. Extending the lease while the task runs lets long
/// external calls, such as embeddings requests, finish.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Runs a task handler, extending its lease until it finishes.
pub(crate) async fn run_task<T>(
    ctx: &TaskContext,
    work: impl Future<Output = Result<T>>,
) -> absurd::Result<T> {
    let mut work = pin!(work);
    let mut heartbeat = interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    let result = loop {
        tokio::select! {
            result = &mut work => break result,
            _ = heartbeat.tick() => ctx.heartbeat(None).await?,
        }
    };
    result.map_err(task_error)
}

/// Keeps Absurd's control flow, such as suspending for a durable sleep, intact
/// through `anyhow` so a sleeping task is not recorded as failed.
fn task_error(error: anyhow::Error) -> AbsurdError {
    match error.downcast_ref::<AbsurdError>() {
        Some(AbsurdError::Suspend) => AbsurdError::Suspend,
        Some(AbsurdError::Cancelled) => AbsurdError::Cancelled,
        Some(AbsurdError::FailedRun) => AbsurdError::FailedRun,
        _ => AbsurdError::TaskFailed(error.into_boxed_dyn_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_page_tokens_are_omitted() {
        assert_eq!(nonempty(""), None);
        assert_eq!(nonempty("next"), Some("next"));
    }

    fn change(file_id: &str, removed: bool, file: Option<DriveFile>) -> DriveChange {
        DriveChange {
            file_id: file_id.to_owned(),
            removed,
            file,
        }
    }

    fn drive_file(id: &str, mime_type: &str, drive_id: &str) -> DriveFile {
        serde_json::from_value(json!({
            "id": id,
            "mimeType": mime_type,
            "driveId": drive_id,
        }))
        .unwrap()
    }

    fn pdf(drive_id: &str) -> DriveFile {
        drive_file("file-1", PDF_MIME_TYPE, drive_id)
    }

    fn google_doc(drive_id: &str) -> DriveFile {
        drive_file("doc-1", GOOGLE_DOC_MIME_TYPE, drive_id)
    }

    #[test]
    fn folder_roots_are_grouped_by_non_member_drive() {
        let folder = "application/vnd.google-apps.folder";
        let roots = group_folder_roots(
            vec![
                drive_file("folder-a", folder, "drive-a"),
                drive_file("pdf-a", PDF_MIME_TYPE, "drive-a"),
                drive_file("doc-a", GOOGLE_DOC_MIME_TYPE, "drive-a"),
                drive_file("folder-b", folder, "drive-b"),
                drive_file("folder-member", folder, "drive-member"),
                drive_file("folder-my-drive", folder, ""),
            ],
            &["drive-member".to_owned()],
        );
        assert_eq!(roots.keys().collect::<Vec<_>>(), ["drive-a", "drive-b"]);
        assert_eq!(roots["drive-a"].folder_ids, ["folder-a"]);
        assert_eq!(
            roots["drive-a"]
                .files
                .iter()
                .map(|file| file.id.as_str())
                .collect::<Vec<_>>(),
            ["pdf-a", "doc-a"]
        );
        assert_eq!(roots["drive-b"].folder_ids, ["folder-b"]);
        assert!(roots["drive-b"].files.is_empty());
    }

    #[test]
    fn changes_are_owned_by_the_file_corpus() {
        let shared = Some("shared-drive-1");
        assert!(matches!(
            classify_change(change("file-1", false, Some(pdf("shared-drive-1"))), shared),
            ChangeAction::Observe(_)
        ));
        assert!(matches!(
            classify_change(change("file-1", false, Some(pdf("shared-drive-1"))), None),
            ChangeAction::Skip
        ));
        assert!(matches!(
            classify_change(change("file-1", false, Some(pdf(""))), shared),
            ChangeAction::Skip
        ));
        assert!(matches!(
            classify_change(change("file-1", false, Some(pdf(""))), None),
            ChangeAction::Observe(_)
        ));
        assert!(matches!(
            classify_change(change("doc-1", false, Some(google_doc(""))), None),
            ChangeAction::Observe(_)
        ));
    }

    #[test]
    fn removed_and_inaccessible_changes_remove_the_observation() {
        let shared = Some("shared-drive-1");
        assert!(matches!(
            classify_change(change("file-1", true, None), shared),
            ChangeAction::Remove(id) if id == "file-1"
        ));
        let mut trashed = pdf("shared-drive-1");
        trashed.trashed = true;
        assert!(matches!(
            classify_change(change("file-1", false, Some(trashed)), shared),
            ChangeAction::Remove(_)
        ));
    }

    #[test]
    fn checkpoint_scopes_are_distinct_per_corpus() {
        assert_eq!(checkpoint_scope(7, None), "user:broker:7");
        assert_eq!(
            checkpoint_scope(7, Some("drive-a")),
            "shared_drive:drive-a:broker:7"
        );
    }

    #[test]
    fn suspension_is_not_a_task_failure() {
        let suspended = Err::<(), _>(AbsurdError::Suspend)
            .context("wait for Slack rate limit")
            .unwrap_err();
        assert!(matches!(task_error(suspended), AbsurdError::Suspend));
        assert!(matches!(
            task_error(anyhow!("boom")),
            AbsurdError::TaskFailed(_)
        ));
    }

    #[test]
    fn errors_are_bounded() {
        let error = anyhow!("{}", "x".repeat(2_000));
        assert_eq!(bounded_error(&error).len(), 1_000);
    }
}
