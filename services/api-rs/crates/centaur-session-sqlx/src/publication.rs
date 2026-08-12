use std::time::Duration;

use centaur_session_core::{
    ThreadKey,
    development::{
        ApprovePublication, ChangeSetState, CompletePublishItem, DevelopmentPublishBatch,
        DevelopmentPublishItem, FailPublishItem, PublishBatchState, PublishItemClaim,
        PublishItemState, RetryPublication, SessionWorkspace, WorkspaceRepositorySnapshot,
        WorkspaceState,
    },
};
use sqlx::FromRow;
use time::OffsetDateTime;

use crate::{PgSessionStore, SessionStoreError, prefixed_id};

impl PgSessionStore {
    pub async fn approve_publication(
        &self,
        request: &ApprovePublication,
    ) -> Result<DevelopmentPublishBatch, SessionStoreError> {
        validate_identity(
            &request.changeset_id,
            &request.approver_principal_id,
            &request.idempotency_key,
        )?;
        let mut tx = self.pool.begin().await?;
        let context = sqlx::query_as::<_, ApprovalContext>(
            r#"
            select changeset.workspace_id, workspace.thread_key,
                   workspace.state as workspace_state,
                   changeset.state as changeset_state,
                   changeset.initiator_principal_id,
                   workspace.workspace_revision,
                   changeset.workspace_revision as changeset_workspace_revision
              from development_change_sets changeset
              join session_workspaces workspace using (workspace_id)
             where changeset.changeset_id = $1
            "#,
        )
        .bind(&request.changeset_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
            message: format!("changeset {}", request.changeset_id),
        })?;
        let thread_key = parse_thread_key(&context.thread_key)?;
        crate::lock_development_execution_boundary(&mut tx, &thread_key).await?;

        if let Some(existing) =
            load_publish_batch_by_changeset(&mut tx, &request.changeset_id).await?
        {
            if existing.idempotency_key == request.idempotency_key
                && existing.approver_principal_id == request.approver_principal_id
            {
                tx.commit().await?;
                return Ok(existing);
            }
            return Err(conflict("changeset already has a publication approval"));
        }
        if !request.is_admin && context.initiator_principal_id != request.approver_principal_id {
            return Err(SessionStoreError::DevelopmentForbidden {
                message: "only the task initiator or an administrator may publish".to_owned(),
            });
        }
        if context.workspace_state != WorkspaceState::Ready.as_ref()
            || context.changeset_state != ChangeSetState::Ready.as_ref()
        {
            return Err(conflict("changeset or workspace is not publishable"));
        }
        if has_active_execution(&mut tx, &thread_key).await? {
            return Err(conflict("session has an active execution"));
        }
        if context.workspace_revision != context.changeset_workspace_revision {
            return Err(conflict("changeset is stale"));
        }

        let items = sqlx::query_as::<_, ApprovalItem>(
            r#"
            select collected.changeset_repository_id, collected.repository_id,
                   repository.default_branch, collected.head_sha as reviewed_head_sha,
                   repository.head_sha as workspace_head_sha
              from development_change_set_repositories collected
              join session_repositories repository
                on repository.workspace_id = collected.workspace_id
               and repository.repository_id = collected.repository_id
             where collected.changeset_id = $1 and collected.state = 'changed'
             order by repository.gitlab_project_id, repository.repository_id
             for update of repository
            "#,
        )
        .bind(&request.changeset_id)
        .fetch_all(&mut *tx)
        .await?;
        if items.is_empty()
            || items.iter().any(|item| {
                item.reviewed_head_sha.is_none()
                    || item.reviewed_head_sha != item.workspace_head_sha
            })
        {
            return Err(conflict(
                "workspace no longer matches the reviewed changeset",
            ));
        }

        let publish_batch_id = prefixed_id("pub");
        sqlx::query(
            r#"
            insert into development_publish_batches
                (publish_batch_id, changeset_id, approver_principal_id,
                 idempotency_key, state)
            values ($1, $2, $3, $4, 'pending')
            "#,
        )
        .bind(&publish_batch_id)
        .bind(&request.changeset_id)
        .bind(&request.approver_principal_id)
        .bind(&request.idempotency_key)
        .execute(&mut *tx)
        .await?;
        let source_branch = publication_branch(&context.workspace_id, &request.changeset_id);
        for item in items {
            sqlx::query(
                r#"
                insert into development_publish_items
                    (publish_item_id, publish_batch_id, changeset_repository_id,
                     repository_id, source_branch, target_branch, head_sha, state)
                values ($1, $2, $3, $4, $5, $6, $7, 'pending')
                "#,
            )
            .bind(prefixed_id("pbi"))
            .bind(&publish_batch_id)
            .bind(item.changeset_repository_id)
            .bind(item.repository_id)
            .bind(&source_branch)
            .bind(item.default_branch)
            .bind(
                item.reviewed_head_sha
                    .expect("reviewed changed item has head"),
            )
            .execute(&mut *tx)
            .await?;
        }
        insert_request(
            &mut tx,
            &publish_batch_id,
            "approve",
            &request.approver_principal_id,
            &request.idempotency_key,
        )
        .await?;
        let updated = sqlx::query(
            "update session_workspaces set state = 'publishing', updated_at = now() where workspace_id = $1 and state = 'ready'",
        )
        .bind(&context.workspace_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(conflict("workspace changed while approval was recorded"));
        }
        let batch = load_publish_batch(&mut tx, &publish_batch_id)
            .await?
            .expect("inserted publish batch exists");
        insert_publication_event(&mut tx, &batch, "development.publish_approved").await?;
        tx.commit().await?;
        Ok(batch)
    }

    pub async fn retry_failed_publication(
        &self,
        request: &RetryPublication,
    ) -> Result<DevelopmentPublishBatch, SessionStoreError> {
        validate_identity(
            &request.publish_batch_id,
            &request.requested_by_principal_id,
            &request.idempotency_key,
        )?;
        let mut tx = self.pool.begin().await?;
        let context = sqlx::query_as::<_, RetryContext>(
            r#"
            select workspace.workspace_id, workspace.thread_key,
                   workspace.state as workspace_state,
                   changeset.initiator_principal_id
              from development_publish_batches batch
              join development_change_sets changeset using (changeset_id)
              join session_workspaces workspace using (workspace_id)
             where batch.publish_batch_id = $1
            "#,
        )
        .bind(&request.publish_batch_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
            message: format!("publish batch {}", request.publish_batch_id),
        })?;
        let thread_key = parse_thread_key(&context.thread_key)?;
        crate::lock_development_execution_boundary(&mut tx, &thread_key).await?;
        if let Some(kind) = sqlx::query_scalar::<_, String>(
            "select request_kind from development_publish_requests where publish_batch_id = $1 and idempotency_key = $2",
        )
        .bind(&request.publish_batch_id)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        {
            if kind == "retry_failed" {
                let batch = load_publish_batch(&mut tx, &request.publish_batch_id)
                    .await?
                    .expect("publish request references batch");
                tx.commit().await?;
                return Ok(batch);
            }
            return Err(conflict("publication idempotency key was already used"));
        }
        if !request.is_admin && context.initiator_principal_id != request.requested_by_principal_id
        {
            return Err(SessionStoreError::DevelopmentForbidden {
                message: "only the task initiator or an administrator may retry publication"
                    .to_owned(),
            });
        }
        let batch = load_publish_batch(&mut tx, &request.publish_batch_id)
            .await?
            .expect("publish batch exists");
        if !matches!(
            batch.state,
            PublishBatchState::Failed | PublishBatchState::PartiallySucceeded
        ) || context.workspace_state != WorkspaceState::Ready.as_ref()
        {
            return Err(conflict("publication is not ready to retry"));
        }
        if has_active_execution(&mut tx, &thread_key).await? {
            return Err(conflict("session has an active execution"));
        }
        let reset = sqlx::query(
            r#"
            update development_publish_items
               set state = 'pending', failure_code = null, failure_message = null,
                   updated_at = now()
             where publish_batch_id = $1 and state = 'failed'
            "#,
        )
        .bind(&request.publish_batch_id)
        .execute(&mut *tx)
        .await?;
        if reset.rows_affected() == 0 {
            return Err(conflict("publication has no failed items"));
        }
        sqlx::query(
            "update development_publish_batches set state = 'pending', lease_owner = null, lease_expires_at = null, updated_at = now() where publish_batch_id = $1",
        )
        .bind(&request.publish_batch_id)
        .execute(&mut *tx)
        .await?;
        insert_request(
            &mut tx,
            &request.publish_batch_id,
            "retry_failed",
            &request.requested_by_principal_id,
            &request.idempotency_key,
        )
        .await?;
        let updated = sqlx::query(
            "update session_workspaces set state = 'publishing', updated_at = now() where workspace_id = $1 and state = 'ready'",
        )
        .bind(&context.workspace_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(conflict("workspace changed while retry was recorded"));
        }
        let batch = load_publish_batch(&mut tx, &request.publish_batch_id)
            .await?
            .expect("publish batch exists");
        insert_publication_event(&mut tx, &batch, "development.publish_retry_requested").await?;
        tx.commit().await?;
        Ok(batch)
    }

    pub async fn claim_publish_item(
        &self,
        publish_batch_id: &str,
        lease_owner: &str,
        lease: Duration,
    ) -> Result<Option<PublishItemClaim>, SessionStoreError> {
        if publish_batch_id.trim().is_empty() || lease_owner.trim().is_empty() || lease.is_zero() {
            return Err(invalid(
                "publish batch, lease owner, and positive lease are required",
            ));
        }
        let lease_expires_at = OffsetDateTime::now_utc()
            + time::Duration::try_from(lease)
                .map_err(|_| invalid("publication lease is too large"))?;
        let mut tx = self.pool.begin().await?;
        let claimed = sqlx::query(
            r#"
            update development_publish_batches
               set state = 'running', lease_owner = $2, lease_expires_at = $3,
                   updated_at = now()
             where publish_batch_id = $1 and state in ('pending', 'running')
               and (lease_owner is null or lease_owner = $2 or lease_expires_at < now())
            "#,
        )
        .bind(publish_batch_id)
        .bind(lease_owner)
        .bind(lease_expires_at)
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() != 1 {
            return Err(conflict("publication lease is held by another worker"));
        }
        let item_id = sqlx::query_scalar::<_, String>(
            r#"
            select publish_item_id from development_publish_items
             where publish_batch_id = $1
               and state in ('pending', 'pushing', 'pushed', 'creating_mr')
             order by repository_id, publish_item_id limit 1 for update
            "#,
        )
        .bind(publish_batch_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(item_id) = item_id else {
            finalize_batch(&mut tx, publish_batch_id).await?;
            tx.commit().await?;
            return Ok(None);
        };
        sqlx::query(
            r#"
            update development_publish_items
               set state = case when state = 'pending' then 'pushing' else state end,
                   attempt_count = case when state = 'pending' then attempt_count + 1 else attempt_count end,
                   updated_at = now()
             where publish_item_id = $1
            "#,
        )
        .bind(&item_id)
        .execute(&mut *tx)
        .await?;
        let batch = load_publish_batch(&mut tx, publish_batch_id)
            .await?
            .expect("claimed publish batch exists");
        let item = batch
            .items
            .iter()
            .find(|item| item.publish_item_id == item_id)
            .cloned()
            .expect("claimed item belongs to batch");
        let workspace = load_publish_workspace(&mut tx, publish_batch_id).await?;
        let repository = load_publish_repository(&mut tx, &workspace.workspace_id, &item).await?;
        tx.commit().await?;
        Ok(Some(PublishItemClaim {
            batch,
            item,
            workspace,
            repository,
        }))
    }

    pub async fn mark_publish_item_pushed(
        &self,
        publish_batch_id: &str,
        publish_item_id: &str,
        lease_owner: &str,
        remote_branch_sha: &str,
    ) -> Result<(), SessionStoreError> {
        if !valid_git_sha(remote_branch_sha) {
            return Err(invalid("remote branch SHA is invalid"));
        }
        let mut tx = self.pool.begin().await?;
        update_item_state(
            &mut tx,
            publish_batch_id,
            publish_item_id,
            lease_owner,
            "pushed",
            Some(remote_branch_sha),
        )
        .await?;
        let batch = load_publish_batch(&mut tx, publish_batch_id)
            .await?
            .expect("publish batch exists");
        insert_publication_event(&mut tx, &batch, "development.publish_item_pushed").await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_publish_item_creating_mr(
        &self,
        publish_batch_id: &str,
        publish_item_id: &str,
        lease_owner: &str,
    ) -> Result<(), SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        update_item_state(
            &mut tx,
            publish_batch_id,
            publish_item_id,
            lease_owner,
            "creating_mr",
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn complete_publish_item(
        &self,
        result: &CompletePublishItem,
    ) -> Result<DevelopmentPublishBatch, SessionStoreError> {
        if !valid_git_sha(&result.remote_branch_sha)
            || result.merge_request_iid <= 0
            || result.merge_request_url.trim().is_empty()
        {
            return Err(invalid("publication result is invalid"));
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            r#"
            update development_publish_items item
               set state = 'succeeded', remote_branch_sha = $4,
                   merge_request_iid = $5, merge_request_url = $6,
                   failure_code = null, failure_message = null, updated_at = now()
              from development_publish_batches batch
             where item.publish_batch_id = $1 and item.publish_item_id = $2
               and batch.publish_batch_id = item.publish_batch_id
               and batch.lease_owner = $3 and batch.state = 'running'
               and item.state in ('pushed', 'creating_mr', 'succeeded')
               and item.head_sha = $4
            "#,
        )
        .bind(&result.publish_batch_id)
        .bind(&result.publish_item_id)
        .bind(&result.lease_owner)
        .bind(&result.remote_branch_sha)
        .bind(result.merge_request_iid)
        .bind(&result.merge_request_url)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(conflict("publication item completion lease is stale"));
        }
        finalize_batch_if_done(&mut tx, &result.publish_batch_id).await?;
        let batch = load_publish_batch(&mut tx, &result.publish_batch_id)
            .await?
            .expect("publish batch exists");
        insert_publication_event(&mut tx, &batch, "development.publish_item_succeeded").await?;
        tx.commit().await?;
        Ok(batch)
    }

    pub async fn fail_publish_item(
        &self,
        result: &FailPublishItem,
    ) -> Result<DevelopmentPublishBatch, SessionStoreError> {
        if result.failure_code.trim().is_empty() || result.failure_message.trim().is_empty() {
            return Err(invalid("publication failure details are required"));
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            r#"
            update development_publish_items item
               set state = 'failed', failure_code = $4, failure_message = $5,
                   updated_at = now()
              from development_publish_batches batch
             where item.publish_batch_id = $1 and item.publish_item_id = $2
               and batch.publish_batch_id = item.publish_batch_id
               and batch.lease_owner = $3 and batch.state = 'running'
               and item.state in ('pushing', 'pushed', 'creating_mr')
            "#,
        )
        .bind(&result.publish_batch_id)
        .bind(&result.publish_item_id)
        .bind(&result.lease_owner)
        .bind(&result.failure_code)
        .bind(&result.failure_message)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(conflict("publication item failure lease is stale"));
        }
        finalize_batch_if_done(&mut tx, &result.publish_batch_id).await?;
        let batch = load_publish_batch(&mut tx, &result.publish_batch_id)
            .await?
            .expect("publish batch exists");
        insert_publication_event(&mut tx, &batch, "development.publish_item_failed").await?;
        tx.commit().await?;
        Ok(batch)
    }

    pub async fn get_publish_batch(
        &self,
        publish_batch_id: &str,
        principal_id: &str,
        is_admin: bool,
    ) -> Result<DevelopmentPublishBatch, SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        let initiator = sqlx::query_scalar::<_, String>(
            r#"
            select changeset.initiator_principal_id
              from development_publish_batches batch
              join development_change_sets changeset using (changeset_id)
             where batch.publish_batch_id = $1
            "#,
        )
        .bind(publish_batch_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
            message: format!("publish batch {publish_batch_id}"),
        })?;
        if !is_admin && initiator != principal_id {
            return Err(SessionStoreError::DevelopmentForbidden {
                message: "publish batch is not accessible to this principal".to_owned(),
            });
        }
        let batch = load_publish_batch(&mut tx, publish_batch_id)
            .await?
            .expect("authorized publish batch exists");
        tx.commit().await?;
        Ok(batch)
    }

    pub async fn list_reconcilable_publish_batch_ids(
        &self,
    ) -> Result<Vec<String>, SessionStoreError> {
        Ok(sqlx::query_scalar(
            r#"
            select publish_batch_id from development_publish_batches
             where state in ('pending', 'running')
               and (lease_owner is null or lease_expires_at < now())
             order by updated_at, publish_batch_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?)
    }
}

#[derive(FromRow)]
struct ApprovalContext {
    workspace_id: String,
    thread_key: String,
    workspace_state: String,
    changeset_state: String,
    initiator_principal_id: String,
    workspace_revision: i64,
    changeset_workspace_revision: i64,
}

#[derive(FromRow)]
struct ApprovalItem {
    changeset_repository_id: String,
    repository_id: String,
    default_branch: String,
    reviewed_head_sha: Option<String>,
    workspace_head_sha: Option<String>,
}

#[derive(FromRow)]
struct RetryContext {
    workspace_id: String,
    thread_key: String,
    workspace_state: String,
    initiator_principal_id: String,
}

#[derive(FromRow)]
struct PublishBatchRow {
    publish_batch_id: String,
    changeset_id: String,
    approver_principal_id: String,
    idempotency_key: String,
    state: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(FromRow)]
struct PublishItemRow {
    publish_item_id: String,
    changeset_repository_id: String,
    repository_id: String,
    source_branch: String,
    target_branch: String,
    head_sha: String,
    state: String,
    attempt_count: i32,
    remote_branch_sha: Option<String>,
    merge_request_iid: Option<i64>,
    merge_request_url: Option<String>,
    failure_code: Option<String>,
    failure_message: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

async fn load_publish_batch_by_changeset(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    changeset_id: &str,
) -> Result<Option<DevelopmentPublishBatch>, SessionStoreError> {
    let id = sqlx::query_scalar::<_, String>(
        "select publish_batch_id from development_publish_batches where changeset_id = $1",
    )
    .bind(changeset_id)
    .fetch_optional(&mut **tx)
    .await?;
    match id {
        Some(id) => load_publish_batch(tx, &id).await,
        None => Ok(None),
    }
}

async fn load_publish_batch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publish_batch_id: &str,
) -> Result<Option<DevelopmentPublishBatch>, SessionStoreError> {
    let row = sqlx::query_as::<_, PublishBatchRow>(
        r#"
        select publish_batch_id, changeset_id, approver_principal_id,
               idempotency_key, state, created_at, updated_at
          from development_publish_batches where publish_batch_id = $1
        "#,
    )
    .bind(publish_batch_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let items = sqlx::query_as::<_, PublishItemRow>(
        r#"
        select publish_item_id, changeset_repository_id, repository_id,
               source_branch, target_branch, head_sha, state, attempt_count,
               remote_branch_sha, merge_request_iid, merge_request_url,
               failure_code, failure_message, created_at, updated_at
          from development_publish_items where publish_batch_id = $1
         order by repository_id, publish_item_id
        "#,
    )
    .bind(publish_batch_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(Some(DevelopmentPublishBatch {
        publish_batch_id: row.publish_batch_id,
        changeset_id: row.changeset_id,
        approver_principal_id: row.approver_principal_id,
        idempotency_key: row.idempotency_key,
        state: parse_value(row.state)?,
        items: items
            .into_iter()
            .map(|item| {
                Ok(DevelopmentPublishItem {
                    publish_item_id: item.publish_item_id,
                    changeset_repository_id: item.changeset_repository_id,
                    repository_id: item.repository_id.parse().map_err(|error| {
                        SessionStoreError::InvalidPersistedValue(format!("{error}"))
                    })?,
                    source_branch: item.source_branch,
                    target_branch: item.target_branch,
                    head_sha: item.head_sha,
                    state: parse_value(item.state)?,
                    attempt_count: item.attempt_count,
                    remote_branch_sha: item.remote_branch_sha,
                    merge_request_iid: item.merge_request_iid,
                    merge_request_url: item.merge_request_url,
                    failure_code: item.failure_code,
                    failure_message: item.failure_message,
                    created_at: item.created_at,
                    updated_at: item.updated_at,
                })
            })
            .collect::<Result<Vec<_>, SessionStoreError>>()?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

async fn load_publish_workspace(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publish_batch_id: &str,
) -> Result<SessionWorkspace, SessionStoreError> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            i32,
            OffsetDateTime,
            OffsetDateTime,
        ),
    >(
        r#"
        select workspace.workspace_id, workspace.thread_key, workspace.state,
               workspace.storage_ref, workspace.preparation_attempt,
               workspace.created_at, workspace.updated_at
          from development_publish_batches batch
          join development_change_sets changeset using (changeset_id)
          join session_workspaces workspace using (workspace_id)
         where batch.publish_batch_id = $1
        "#,
    )
    .bind(publish_batch_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(SessionWorkspace {
        workspace_id: row.0,
        thread_key: parse_thread_key(&row.1)?,
        state: parse_value(row.2)?,
        storage_ref: row.3,
        preparation_attempt: row.4,
        created_at: row.5,
        updated_at: row.6,
    })
}

async fn load_publish_repository(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: &str,
    item: &DevelopmentPublishItem,
) -> Result<WorkspaceRepositorySnapshot, SessionStoreError> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        r#"
        select repository_id, display_name, path_with_namespace, default_branch,
               clone_url, relative_path, state, base_sha, local_branch, head_sha
          from session_repositories where workspace_id = $1 and repository_id = $2
        "#,
    )
    .bind(workspace_id)
    .bind(item.repository_id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| {
        SessionStoreError::InvalidPersistedValue("publish item repository is missing".to_owned())
    })?;
    Ok(WorkspaceRepositorySnapshot {
        repository_id: row
            .0
            .parse()
            .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
        display_name: row.1,
        path_with_namespace: row.2,
        default_branch: row.3,
        clone_url: row.4,
        relative_path: row.5,
        state: parse_value(row.6)?,
        base_sha: row.7,
        local_branch: row.8,
        head_sha: row.9,
    })
}

async fn insert_request(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publish_batch_id: &str,
    kind: &str,
    principal_id: &str,
    idempotency_key: &str,
) -> Result<(), SessionStoreError> {
    sqlx::query(
        r#"
        insert into development_publish_requests
            (publish_request_id, publish_batch_id, request_kind,
             requested_by_principal_id, idempotency_key)
        values ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(prefixed_id("pbr"))
    .bind(publish_batch_id)
    .bind(kind)
    .bind(principal_id)
    .bind(idempotency_key)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_publication_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    batch: &DevelopmentPublishBatch,
    event_type: &str,
) -> Result<(), SessionStoreError> {
    sqlx::query(
        r#"
        insert into session_events (thread_key, execution_id, event_type, payload)
        select workspace.thread_key, changeset.execution_id, $2, $3
          from development_publish_batches stored_batch
          join development_change_sets changeset using (changeset_id)
          join session_workspaces workspace using (workspace_id)
         where stored_batch.publish_batch_id = $1
        "#,
    )
    .bind(&batch.publish_batch_id)
    .bind(event_type)
    .bind(serde_json::json!({
        "publish_batch_id": batch.publish_batch_id,
        "changeset_id": batch.changeset_id,
        "state": batch.state,
        "items": batch.items,
    }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn update_item_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publish_batch_id: &str,
    publish_item_id: &str,
    lease_owner: &str,
    next_state: &str,
    remote_branch_sha: Option<&str>,
) -> Result<(), SessionStoreError> {
    let allowed_from = if next_state == "pushed" {
        ["pushing", "pushed"]
    } else {
        ["pushed", "creating_mr"]
    };
    let updated = sqlx::query(
        r#"
        update development_publish_items item
           set state = $4, remote_branch_sha = coalesce($5, remote_branch_sha),
               updated_at = now()
          from development_publish_batches batch
         where item.publish_batch_id = $1 and item.publish_item_id = $2
           and batch.publish_batch_id = item.publish_batch_id
           and batch.lease_owner = $3 and batch.state = 'running'
           and item.state = any($6) and ($4 <> 'pushed' or item.head_sha = $5)
        "#,
    )
    .bind(publish_batch_id)
    .bind(publish_item_id)
    .bind(lease_owner)
    .bind(next_state)
    .bind(remote_branch_sha)
    .bind(&allowed_from[..])
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(conflict("publication item state lease is stale"));
    }
    Ok(())
}

async fn finalize_batch_if_done(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publish_batch_id: &str,
) -> Result<(), SessionStoreError> {
    let active = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from development_publish_items where publish_batch_id = $1 and state not in ('succeeded', 'failed'))",
    )
    .bind(publish_batch_id)
    .fetch_one(&mut **tx)
    .await?;
    if !active {
        finalize_batch(tx, publish_batch_id).await?;
    }
    Ok(())
}

async fn finalize_batch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    publish_batch_id: &str,
) -> Result<(), SessionStoreError> {
    let states = sqlx::query_scalar::<_, String>(
        "select state from development_publish_items where publish_batch_id = $1",
    )
    .bind(publish_batch_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(parse_value::<PublishItemState>)
    .collect::<Result<Vec<_>, _>>()?;
    let state = PublishBatchState::from_items(states);
    if matches!(
        state,
        PublishBatchState::Pending | PublishBatchState::Running
    ) {
        return Err(SessionStoreError::InvalidPersistedValue(
            "publication finalized with active items".to_owned(),
        ));
    }
    let workspace_id = sqlx::query_scalar::<_, String>(
        r#"
        update development_publish_batches batch
           set state = $2, lease_owner = null, lease_expires_at = null,
               updated_at = now()
          from development_change_sets changeset
         where batch.publish_batch_id = $1
           and changeset.changeset_id = batch.changeset_id
        returning changeset.workspace_id
        "#,
    )
    .bind(publish_batch_id)
    .bind(state.as_ref())
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "update session_workspaces set state = 'ready', updated_at = now() where workspace_id = $1 and state = 'publishing'",
    )
    .bind(workspace_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn has_active_execution(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    thread_key: &ThreadKey,
) -> Result<bool, SessionStoreError> {
    Ok(sqlx::query_scalar(
        "select exists(select 1 from session_executions where thread_key = $1 and status in ('queued', 'running'))",
    )
    .bind(thread_key.as_str())
    .fetch_one(&mut **tx)
    .await?)
}

fn publication_branch(workspace_id: &str, changeset_id: &str) -> String {
    let workspace = short_id(workspace_id);
    let changeset = short_id(changeset_id);
    format!("centaur/{workspace}/{changeset}")
}

fn short_id(value: &str) -> String {
    value
        .rsplit_once('_')
        .map_or(value, |(_, suffix)| suffix)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(12)
        .collect()
}

fn validate_identity(
    resource: &str,
    principal: &str,
    idempotency: &str,
) -> Result<(), SessionStoreError> {
    if resource.trim().is_empty() || principal.trim().is_empty() || idempotency.trim().is_empty() {
        return Err(invalid(
            "publication resource, principal, and idempotency key are required",
        ));
    }
    Ok(())
}

fn parse_thread_key(value: &str) -> Result<ThreadKey, SessionStoreError> {
    value
        .parse()
        .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))
}

fn parse_value<T>(value: String) -> Result<T, SessionStoreError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))
}

fn valid_git_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn conflict(message: &str) -> SessionStoreError {
    SessionStoreError::DevelopmentConflict {
        message: message.to_owned(),
    }
}

fn invalid(message: &str) -> SessionStoreError {
    SessionStoreError::InvalidDevelopmentRequest {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use centaur_session_core::{
        HarnessType, MessageRole, SessionMessageInput,
        development::{
            AcceptDevelopmentTask, ApprovePublication, CollectedChangeSetRepositoryState,
            CompleteChangeSetCollection, CompleteChangeSetRepository, CompletePublishItem,
            CompleteWorkspacePreparation, ConfirmRepositorySelection, DevelopmentChannel,
            DevelopmentInitiator, FailPublishItem, PreparedRepositorySnapshot, PublishBatchState,
            PublishItemState, ResolvedRepository, RetryPublication, WorkspaceState,
        },
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use crate::{PgSessionStore, SessionStoreError};

    async fn test_store() -> Option<PgSessionStore> {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return None;
        };
        let store = PgSessionStore::connect(&url)
            .await
            .expect("connect test db");
        store.run_migrations().await.expect("run migrations");
        Some(store)
    }

    async fn ready_changeset(
        store: &PgSessionStore,
        project_ids: &[u64],
    ) -> (
        centaur_session_core::development::AcceptedDevelopmentTask,
        String,
    ) {
        let suffix = Uuid::new_v4();
        let message_id = format!("message-{suffix}");
        let accepted = store
            .accept_development_task(&AcceptDevelopmentTask {
                channel: DevelopmentChannel {
                    platform: "feishu".to_owned(),
                    tenant_key: format!("tenant-{suffix}"),
                    conversation_key: format!("chat-{suffix}"),
                    root_message_id: message_id.clone(),
                },
                platform_event_id: format!("event-{suffix}"),
                platform_message_id: Some(message_id.clone()),
                harness_type: HarnessType::Codex,
                initiator: DevelopmentInitiator {
                    principal_id: "principal-1".to_owned(),
                },
                message: SessionMessageInput {
                    client_message_id: Some(message_id),
                    role: MessageRole::User,
                    parts: vec![json!({"type": "text", "text": "Implement it"})],
                    metadata: json!({}),
                },
                session_metadata: json!({}),
            })
            .await
            .expect("accept task");
        let repositories = project_ids
            .iter()
            .map(|project_id| repository(*project_id))
            .collect::<Vec<_>>();
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id.clone(),
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories,
            })
            .await
            .expect("confirm repositories");
        let claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "workspace-owner",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("claim workspace");
        let base_sha = "a".repeat(40);
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id.clone(),
                attempt: claim.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: format!("workspace-{suffix}"),
                prepared: project_ids
                    .iter()
                    .map(|project_id| PreparedRepositorySnapshot {
                        repository_id: format!("gitlab:{project_id}").parse().unwrap(),
                        base_sha: base_sha.clone(),
                        local_branch: format!("centaur/{suffix}"),
                        head_sha: base_sha.clone(),
                    })
                    .collect(),
                failed: Vec::new(),
            })
            .await
            .expect("complete workspace");
        store
            .complete_execution(&accepted.execution_id)
            .await
            .expect("complete execution");
        let collecting = store
            .begin_changeset_collection(&accepted.execution_id, "collector")
            .await
            .expect("begin changeset")
            .expect("development execution creates changeset");
        let patch = b"diff --git a/README.md b/README.md\n".to_vec();
        let patch_hash = format!("sha256:{}", hex::encode(Sha256::digest(&patch)));
        let completed = store
            .complete_changeset_collection(&CompleteChangeSetCollection {
                changeset_id: collecting.changeset_id,
                lease_owner: "collector".to_owned(),
                repositories: project_ids
                    .iter()
                    .enumerate()
                    .map(|(index, project_id)| {
                        let head_sha = format!("{:x}", index + 11).repeat(40);
                        let head_sha = head_sha.chars().take(40).collect::<String>();
                        CompleteChangeSetRepository {
                            repository_id: format!("gitlab:{project_id}").parse().unwrap(),
                            state: CollectedChangeSetRepositoryState::Changed,
                            base_sha: base_sha.clone(),
                            recorded_head_sha: base_sha.clone(),
                            head_sha: Some(head_sha.clone()),
                            commit_metadata: json!([{"sha": head_sha}]),
                            changed_file_count: 1,
                            additions: 1,
                            deletions: 0,
                            patch_hash: Some(patch_hash.clone()),
                            patch: patch.clone(),
                            test_evidence: json!([]),
                            failure_code: None,
                            failure_message: None,
                        }
                    })
                    .collect(),
            })
            .await
            .expect("complete changeset")
            .expect("changed repositories create changeset");
        (accepted, completed.changeset_id)
    }

    fn repository(project_id: u64) -> ResolvedRepository {
        ResolvedRepository {
            repository_id: format!("gitlab:{project_id}").parse().unwrap(),
            display_name: format!("Project {project_id}"),
            path_with_namespace: format!("group/project-{project_id}"),
            default_branch: "main".to_owned(),
            clone_url: format!("http://git.example.test:82/group/project-{project_id}.git"),
            relative_path: format!("repos/{project_id}-project-{project_id}"),
        }
    }

    #[tokio::test]
    async fn publication_authorization_leases_partial_retry_and_idempotency_are_durable() {
        let Some(store) = test_store().await else {
            return;
        };
        let (accepted, changeset_id) = ready_changeset(&store, &[42, 84]).await;
        let approval = ApprovePublication {
            changeset_id: changeset_id.clone(),
            approver_principal_id: "principal-1".to_owned(),
            is_admin: false,
            idempotency_key: "approve-1".to_owned(),
        };
        assert!(matches!(
            store
                .approve_publication(&ApprovePublication {
                    approver_principal_id: "principal-2".to_owned(),
                    ..approval.clone()
                })
                .await,
            Err(SessionStoreError::DevelopmentForbidden { .. })
        ));
        let approved = store.approve_publication(&approval).await.unwrap();
        assert_eq!(approved.state, PublishBatchState::Pending);
        assert_eq!(approved.items.len(), 2);
        assert_eq!(
            store
                .approve_publication(&approval)
                .await
                .unwrap()
                .publish_batch_id,
            approved.publish_batch_id
        );
        assert!(matches!(
            store
                .create_execution(&accepted.thread_key, None, json!({}))
                .await,
            Err(SessionStoreError::DevelopmentConflict { .. })
        ));

        let first = store
            .claim_publish_item(
                &approved.publish_batch_id,
                "publisher-a",
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.item.repository_id.to_string(), "gitlab:42");
        assert_eq!(first.item.state, PublishItemState::Pushing);
        assert!(matches!(
            store
                .claim_publish_item(
                    &approved.publish_batch_id,
                    "publisher-b",
                    std::time::Duration::from_secs(30),
                )
                .await,
            Err(SessionStoreError::DevelopmentConflict { .. })
        ));
        store
            .mark_publish_item_pushed(
                &approved.publish_batch_id,
                &first.item.publish_item_id,
                "publisher-a",
                &first.item.head_sha,
            )
            .await
            .unwrap();
        store
            .mark_publish_item_creating_mr(
                &approved.publish_batch_id,
                &first.item.publish_item_id,
                "publisher-a",
            )
            .await
            .unwrap();
        store
            .complete_publish_item(&CompletePublishItem {
                publish_batch_id: approved.publish_batch_id.clone(),
                publish_item_id: first.item.publish_item_id,
                lease_owner: "publisher-a".to_owned(),
                remote_branch_sha: first.item.head_sha,
                merge_request_iid: 7,
                merge_request_url: "http://git.example.test:82/group/project-42/-/merge_requests/7"
                    .to_owned(),
            })
            .await
            .unwrap();

        let second = store
            .claim_publish_item(
                &approved.publish_batch_id,
                "publisher-a",
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.item.repository_id.to_string(), "gitlab:84");
        let partial = store
            .fail_publish_item(&FailPublishItem {
                publish_batch_id: approved.publish_batch_id.clone(),
                publish_item_id: second.item.publish_item_id.clone(),
                lease_owner: "publisher-a".to_owned(),
                failure_code: "gitlab_unavailable".to_owned(),
                failure_message: "publication failed".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(partial.state, PublishBatchState::PartiallySucceeded);
        assert_eq!(
            store
                .workspace_for_session(&accepted.thread_key)
                .await
                .unwrap()
                .unwrap()
                .state,
            WorkspaceState::Ready
        );
        assert_eq!(partial.items[0].merge_request_iid, Some(7));

        let retry = RetryPublication {
            publish_batch_id: approved.publish_batch_id.clone(),
            requested_by_principal_id: "principal-1".to_owned(),
            is_admin: false,
            idempotency_key: "retry-1".to_owned(),
        };
        let retried = store.retry_failed_publication(&retry).await.unwrap();
        assert_eq!(retried.state, PublishBatchState::Pending);
        assert_eq!(retried.items[0].state, PublishItemState::Succeeded);
        assert_eq!(retried.items[0].merge_request_iid, Some(7));
        assert_eq!(retried.items[1].state, PublishItemState::Pending);
        assert_eq!(
            store.retry_failed_publication(&retry).await.unwrap().items[0].merge_request_iid,
            Some(7)
        );
        let retried_item = store
            .claim_publish_item(
                &approved.publish_batch_id,
                "publisher-a",
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retried_item.item.repository_id.to_string(), "gitlab:84");
        assert_eq!(retried_item.item.attempt_count, 2);
        store
            .mark_publish_item_pushed(
                &approved.publish_batch_id,
                &retried_item.item.publish_item_id,
                "publisher-a",
                &retried_item.item.head_sha,
            )
            .await
            .unwrap();
        let succeeded = store
            .complete_publish_item(&CompletePublishItem {
                publish_batch_id: approved.publish_batch_id.clone(),
                publish_item_id: retried_item.item.publish_item_id,
                lease_owner: "publisher-a".to_owned(),
                remote_branch_sha: retried_item.item.head_sha,
                merge_request_iid: 8,
                merge_request_url: "http://git.example.test:82/group/project-84/-/merge_requests/8"
                    .to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(succeeded.state, PublishBatchState::Succeeded);
        assert_eq!(succeeded.items[0].merge_request_iid, Some(7));
        assert_eq!(succeeded.items[1].merge_request_iid, Some(8));
        assert!(matches!(
            store
                .get_publish_batch(&approved.publish_batch_id, "principal-2", false)
                .await,
            Err(SessionStoreError::DevelopmentForbidden { .. })
        ));
        assert!(
            store
                .get_publish_batch(&approved.publish_batch_id, "admin-1", true)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn publication_rejects_changeset_after_workspace_membership_advances() {
        let Some(store) = test_store().await else {
            return;
        };
        let (accepted, changeset_id) = ready_changeset(&store, &[42]).await;
        let draft = store
            .create_add_repository_selection(&accepted.thread_key)
            .await
            .unwrap();
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: draft.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![repository(84)],
            })
            .await
            .unwrap();
        let claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "workspace-owner",
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap();
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: claim.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: claim.workspace.storage_ref.unwrap(),
                prepared: claim
                    .repositories
                    .iter()
                    .map(|repository| PreparedRepositorySnapshot {
                        repository_id: repository.repository_id.clone(),
                        base_sha: repository
                            .base_sha
                            .clone()
                            .unwrap_or_else(|| "c".repeat(40)),
                        local_branch: repository
                            .local_branch
                            .clone()
                            .unwrap_or_else(|| "centaur/add".to_owned()),
                        head_sha: repository
                            .head_sha
                            .clone()
                            .unwrap_or_else(|| "c".repeat(40)),
                    })
                    .collect(),
                failed: Vec::new(),
            })
            .await
            .unwrap();
        assert!(matches!(
            store
                .approve_publication(&ApprovePublication {
                    changeset_id,
                    approver_principal_id: "principal-1".to_owned(),
                    is_admin: false,
                    idempotency_key: "stale-approval".to_owned(),
                })
                .await,
            Err(SessionStoreError::DevelopmentConflict { .. })
        ));
    }

    #[test]
    fn publication_branch_is_deterministic_and_bounded() {
        let branch = super::publication_branch(
            "wsp_0123456789abcdef0123456789abcdef",
            "chg_fedcba9876543210fedcba9876543210",
        );
        assert_eq!(branch, "centaur/0123456789ab/fedcba987654");
        assert!(branch.len() < 64);
    }
}
