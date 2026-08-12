use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use centaur_session_core::{
    ExecutionStatus, MessageRole, SessionStatus, ThreadKey,
    development::{
        AcceptDevelopmentTask, AcceptedDevelopmentTask, ChangeSetCollectionClaim,
        ChangeSetRepositoryState, ChangeSetState, CompleteChangeSetCollection,
        CompleteWorkspacePreparation, CompletedDevelopmentExecution, ConfirmRepositorySelection,
        DevelopmentChangeSet, DevelopmentChangeSetRepository, ExecutionBlocker,
        RepositorySelectionDraft, RepositorySelectionOutcome, RepositoryState, SelectionFlowState,
        SelectionKind, SessionWorkspace, WorkspacePreparationClaim, WorkspaceRepositorySnapshot,
        WorkspaceState,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use time::OffsetDateTime;

use crate::{
    CreateExecutionResult, CreateExecutionRow, PgSessionStore, SessionStoreError, prefixed_id,
};

impl PgSessionStore {
    pub async fn accept_development_task(
        &self,
        request: &AcceptDevelopmentTask,
    ) -> Result<AcceptedDevelopmentTask, SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        validate_development_task(request)?;
        let mut lock_keys = vec![
            format!("channel:{}", request.channel.lock_key()),
            request
                .channel
                .receipt_lock_key("event", &request.platform_event_id),
        ];
        if let Some(message_id) = request.platform_message_id.as_deref() {
            lock_keys.push(request.channel.receipt_lock_key("message", message_id));
        }
        lock_keys.sort();
        lock_keys.dedup();
        for lock_key in lock_keys {
            sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(lock_key)
                .execute(&mut *tx)
                .await?;
        }

        if let Some(existing) = accepted_task_for_receipt(&mut tx, request).await? {
            tx.commit().await?;
            return Ok(existing);
        }
        let binding_exists = sqlx::query_scalar::<_, bool>(
            r#"
            select exists(
                select 1 from development_channel_bindings
                 where platform = $1 and tenant_key = $2
                   and conversation_key = $3 and root_message_id = $4
                   and active
            )
            "#,
        )
        .bind(&request.channel.platform)
        .bind(&request.channel.tenant_key)
        .bind(&request.channel.conversation_key)
        .bind(&request.channel.root_message_id)
        .fetch_one(&mut *tx)
        .await?;
        if binding_exists {
            return Err(SessionStoreError::DevelopmentConflict {
                message: "channel already has an active development task".to_owned(),
            });
        }

        let thread_key = ThreadKey::parse(format!("development:{}", uuid::Uuid::new_v4().simple()))
            .expect("generated development thread key is valid");
        let binding_id = prefixed_id("bind");
        let workspace_id = prefixed_id("wsp");
        let message_id = prefixed_id("msg");
        let execution_id = prefixed_id("exe");
        let selection_flow_id = prefixed_id("sel");

        sqlx::query(
            r#"
            insert into sessions
                (thread_key, harness_type, status, metadata)
            values ($1, $2, $3, $4)
            "#,
        )
        .bind(thread_key.as_str())
        .bind(request.harness_type.as_ref())
        .bind(SessionStatus::Idle.as_ref())
        .bind(request.session_metadata.clone())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            insert into development_channel_bindings
                (binding_id, platform, tenant_key, conversation_key,
                 root_message_id, thread_key, initiator_principal_id)
            values ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(binding_id)
        .bind(&request.channel.platform)
        .bind(&request.channel.tenant_key)
        .bind(&request.channel.conversation_key)
        .bind(&request.channel.root_message_id)
        .bind(thread_key.as_str())
        .bind(&request.initiator.principal_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "insert into session_workspaces (workspace_id, thread_key, state) values ($1, $2, $3)",
        )
        .bind(&workspace_id)
        .bind(thread_key.as_str())
        .bind(WorkspaceState::AwaitingSelection.as_ref())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            insert into session_messages
                (message_id, thread_key, client_message_id, role, parts, metadata)
            values ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(message_id)
        .bind(thread_key.as_str())
        .bind(
            request
                .platform_message_id
                .as_deref()
                .or(request.message.client_message_id.as_deref()),
        )
        .bind(MessageRole::User.as_ref())
        .bind(Value::Array(request.message.parts.clone()))
        .bind(request.message.metadata.clone())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            insert into session_executions
                (execution_id, thread_key, idempotency_key, status,
                 blocking_reason, metadata)
            values ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(&execution_id)
        .bind(thread_key.as_str())
        .bind(&request.platform_event_id)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionBlocker::AwaitingProjectSelection.as_str())
        .bind(json!({
            "source": request.channel.platform,
            "platform_event_id": request.platform_event_id,
            "development_input_line": {
                "type": "user",
                "thread_key": thread_key.as_str(),
                "client_user_message_id": request.message.client_message_id,
                "trace_metadata": {
                    "source": request.channel.platform,
                    "action": "execute",
                    "platform_event_id": request.platform_event_id,
                    "message_metadata": request.message.metadata,
                },
                "message": {
                    "role": request.message.role.as_ref(),
                    "content": request.message.parts,
                },
            },
        }))
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            insert into development_selection_flows
                (selection_flow_id, workspace_id, execution_id, kind, state)
            values ($1, $2, $3, 'initial', 'pending')
            "#,
        )
        .bind(&selection_flow_id)
        .bind(&workspace_id)
        .bind(&execution_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            insert into development_platform_events
                (platform, tenant_key, event_id, message_id, thread_key)
            values ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(&request.channel.platform)
        .bind(&request.channel.tenant_key)
        .bind(&request.platform_event_id)
        .bind(request.platform_message_id.as_deref())
        .bind(thread_key.as_str())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(AcceptedDevelopmentTask {
            thread_key,
            workspace_id,
            selection_flow_id,
            execution_id,
            created: true,
        })
    }

    pub async fn confirm_repository_selection(
        &self,
        request: &ConfirmRepositorySelection,
    ) -> Result<RepositorySelectionOutcome, SessionStoreError> {
        if request.expected_version < 1 {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "selection version must be positive".to_owned(),
            });
        }
        let mut unique_ids = HashSet::with_capacity(request.repositories.len());
        for repository in &request.repositories {
            if !unique_ids.insert(repository.repository_id.clone()) {
                return Err(SessionStoreError::InvalidDevelopmentRequest {
                    message: format!(
                        "repository {} appears more than once",
                        repository.repository_id
                    ),
                });
            }
        }

        let mut tx = self.pool.begin().await?;
        let flow = lock_selection_flow(&mut tx, &request.selection_flow_id).await?;
        ensure_pending_selection_version(&flow, request.expected_version)?;
        ensure_selection_decider(&flow, &request.decided_by_principal_id)?;
        let kind = parse_selection_kind(&flow.kind)?;
        match kind {
            SelectionKind::Initial if flow.execution_id.is_none() => {
                return Err(SessionStoreError::DevelopmentConflict {
                    message: "initial selection has no blocked execution".to_owned(),
                });
            }
            SelectionKind::Add if request.repositories.is_empty() => {
                return Err(SessionStoreError::InvalidDevelopmentRequest {
                    message: "add-project selection requires at least one repository".to_owned(),
                });
            }
            SelectionKind::Add => ensure_workspace_accepts_additions(&mut tx, &flow).await?,
            SelectionKind::Initial => {}
        }

        for repository in &request.repositories {
            let existing = sqlx::query_scalar::<_, bool>(
                r#"
                select exists(
                    select 1 from session_repositories
                     where workspace_id = $1 and repository_id = $2
                )
                "#,
            )
            .bind(&flow.workspace_id)
            .bind(repository.repository_id.as_str())
            .fetch_one(&mut *tx)
            .await?;
            if existing {
                return Err(SessionStoreError::DevelopmentConflict {
                    message: format!(
                        "repository {} is already in the workspace",
                        repository.repository_id
                    ),
                });
            }
        }

        for repository in &request.repositories {
            let project_id =
                i64::try_from(repository.repository_id.project_id()).map_err(|_| {
                    SessionStoreError::InvalidDevelopmentRequest {
                        message: format!(
                            "repository {} exceeds the supported GitLab project ID range",
                            repository.repository_id
                        ),
                    }
                })?;
            sqlx::query(
                r#"
                insert into session_repositories
                    (workspace_id, repository_id, gitlab_project_id,
                     display_name, path_with_namespace, default_branch,
                     clone_url, relative_path, selection_flow_id,
                     added_by_principal_id, state)
                values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending')
                "#,
            )
            .bind(&flow.workspace_id)
            .bind(repository.repository_id.as_str())
            .bind(project_id)
            .bind(&repository.display_name)
            .bind(&repository.path_with_namespace)
            .bind(&repository.default_branch)
            .bind(&repository.clone_url)
            .bind(&repository.relative_path)
            .bind(&request.selection_flow_id)
            .bind(&request.decided_by_principal_id)
            .execute(&mut *tx)
            .await?;
        }

        let repository_ids = request
            .repositories
            .iter()
            .map(|repository| repository.repository_id.clone())
            .collect::<Vec<_>>();
        let selected_repository_ids = Value::Array(
            repository_ids
                .iter()
                .map(|repository_id| Value::String(repository_id.to_string()))
                .collect(),
        );
        let version = next_selection_version(request.expected_version)?;
        sqlx::query(
            r#"
            update development_selection_flows
               set state = 'confirmed', version = $2,
                   selected_repository_ids = $3,
                   decided_by_principal_id = $4,
                   updated_at = now()
             where selection_flow_id = $1
            "#,
        )
        .bind(&request.selection_flow_id)
        .bind(version)
        .bind(selected_repository_ids)
        .bind(&request.decided_by_principal_id)
        .execute(&mut *tx)
        .await?;

        let workspace_updated = sqlx::query(
            r#"
            update session_workspaces
               set state = 'provisioning', workspace_revision = workspace_revision + 1,
                   updated_at = now()
             where workspace_id = $1 and state = $2
            "#,
        )
        .bind(&flow.workspace_id)
        .bind(match kind {
            SelectionKind::Initial => WorkspaceState::AwaitingSelection.as_ref(),
            SelectionKind::Add => WorkspaceState::Ready.as_ref(),
        })
        .execute(&mut *tx)
        .await?;
        if workspace_updated.rows_affected() != 1 {
            return Err(SessionStoreError::DevelopmentConflict {
                message: "workspace state changed while confirming selection".to_owned(),
            });
        }

        let execution_blocker = if let Some(execution_id) = flow.execution_id.as_deref() {
            let execution_updated = sqlx::query(
                r#"
                update session_executions
                   set blocking_reason = $2, updated_at = now()
                 where execution_id = $1
                   and status = 'queued'
                   and blocking_reason = $3
                "#,
            )
            .bind(execution_id)
            .bind(ExecutionBlocker::WorkspaceProvisioning.as_str())
            .bind(ExecutionBlocker::AwaitingProjectSelection.as_str())
            .execute(&mut *tx)
            .await?;
            if execution_updated.rows_affected() != 1 {
                return Err(SessionStoreError::DevelopmentConflict {
                    message: "blocked execution is no longer awaiting project selection".to_owned(),
                });
            }
            Some(ExecutionBlocker::WorkspaceProvisioning)
        } else {
            None
        };

        tx.commit().await?;
        Ok(RepositorySelectionOutcome {
            selection_flow_id: request.selection_flow_id.clone(),
            workspace_id: flow.workspace_id,
            state: SelectionFlowState::Confirmed,
            version,
            repository_ids,
            workspace_state: WorkspaceState::Provisioning,
            execution_blocker,
        })
    }

    pub async fn create_add_repository_selection(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<RepositorySelectionDraft, SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        let workspace = sqlx::query_as::<_, (String, String)>(
            r#"
            select workspace_id, state from session_workspaces
             where thread_key = $1 for update
            "#,
        )
        .bind(thread_key.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
            message: format!("workspace for session {thread_key}"),
        })?;
        if workspace.1 != WorkspaceState::Ready.as_ref() {
            return Err(SessionStoreError::DevelopmentConflict {
                message: format!("workspace is {} and cannot accept projects", workspace.1),
            });
        }
        ensure_session_has_no_active_work(&mut tx, thread_key).await?;

        if let Some(row) = sqlx::query_as::<_, RepositorySelectionDraftRow>(
            r#"
            select selection_flow_id, workspace_id, kind, state, version
              from development_selection_flows
             where workspace_id = $1 and state = 'pending'
             limit 1
            "#,
        )
        .bind(&workspace.0)
        .fetch_optional(&mut *tx)
        .await?
        {
            tx.commit().await?;
            return row.try_into();
        }

        let row = sqlx::query_as::<_, RepositorySelectionDraftRow>(
            r#"
            insert into development_selection_flows
                (selection_flow_id, workspace_id, kind, state)
            values ($1, $2, 'add', 'pending')
            returning selection_flow_id, workspace_id, kind, state, version
            "#,
        )
        .bind(prefixed_id("sel"))
        .bind(&workspace.0)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        row.try_into()
    }

    pub async fn cancel_repository_selection(
        &self,
        selection_flow_id: &str,
        expected_version: i32,
        decided_by_principal_id: &str,
    ) -> Result<RepositorySelectionOutcome, SessionStoreError> {
        if expected_version < 1 {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "selection version must be positive".to_owned(),
            });
        }
        let mut tx = self.pool.begin().await?;
        let flow = lock_selection_flow(&mut tx, selection_flow_id).await?;
        ensure_pending_selection_version(&flow, expected_version)?;
        ensure_selection_decider(&flow, decided_by_principal_id)?;
        let kind = parse_selection_kind(&flow.kind)?;
        let version = next_selection_version(expected_version)?;

        sqlx::query(
            r#"
            update development_selection_flows
               set state = 'cancelled', version = $2,
                   decided_by_principal_id = $3, updated_at = now()
             where selection_flow_id = $1
            "#,
        )
        .bind(selection_flow_id)
        .bind(version)
        .bind(decided_by_principal_id)
        .execute(&mut *tx)
        .await?;
        if let Some(execution_id) = flow.execution_id.as_deref() {
            let execution = sqlx::query(
                r#"
                update session_executions
                   set status = 'cancelled', blocking_reason = null,
                       error = 'project selection cancelled',
                       completed_at = now(), updated_at = now()
                 where execution_id = $1
                   and status = 'queued'
                   and blocking_reason = $2
                "#,
            )
            .bind(execution_id)
            .bind(ExecutionBlocker::AwaitingProjectSelection.as_str())
            .execute(&mut *tx)
            .await?;
            if execution.rows_affected() != 1 {
                return Err(SessionStoreError::DevelopmentConflict {
                    message: "blocked execution is no longer awaiting project selection".to_owned(),
                });
            }
        } else if kind != SelectionKind::Add {
            return Err(SessionStoreError::DevelopmentConflict {
                message: "initial selection has no blocked execution".to_owned(),
            });
        }

        tx.commit().await?;
        Ok(RepositorySelectionOutcome {
            selection_flow_id: selection_flow_id.to_owned(),
            workspace_id: flow.workspace_id,
            state: SelectionFlowState::Cancelled,
            version,
            repository_ids: Vec::new(),
            workspace_state: flow.workspace_state,
            execution_blocker: None,
        })
    }

    pub async fn create_or_get_workspace(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<SessionWorkspace, SessionStoreError> {
        let workspace_id = prefixed_id("wsp");
        let row = sqlx::query_as::<_, SessionWorkspaceRow>(
            r#"
            insert into session_workspaces (workspace_id, thread_key, state)
            values ($1, $2, $3)
            on conflict (thread_key) do update set thread_key = excluded.thread_key
            returning workspace_id, thread_key, state, storage_ref,
                      preparation_attempt, created_at, updated_at
            "#,
        )
        .bind(workspace_id)
        .bind(thread_key.as_str())
        .bind(WorkspaceState::AwaitingSelection.as_ref())
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn create_blocked_execution(
        &self,
        thread_key: &ThreadKey,
        idempotency_key: &str,
        blocker: ExecutionBlocker,
        metadata: Value,
    ) -> Result<CreateExecutionResult, SessionStoreError> {
        let execution_id = prefixed_id("exe");
        let row = sqlx::query_as::<_, CreateExecutionRow>(
            r#"
            insert into session_executions
                (execution_id, thread_key, idempotency_key, status,
                 blocking_reason, metadata)
            values ($1, $2, $3, 'queued', $4, $5)
            on conflict (thread_key, idempotency_key)
                where idempotency_key is not null
            do update set idempotency_key = excluded.idempotency_key
            returning
                execution_id = $1 as created,
                execution_id,
                idempotency_key,
                thread_key,
                status,
                blocking_reason,
                metadata,
                error,
                created_at,
                updated_at,
                started_at,
                completed_at
            "#,
        )
        .bind(execution_id)
        .bind(thread_key.as_str())
        .bind(idempotency_key)
        .bind(blocker.as_str())
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn claim_workspace_preparation(
        &self,
        workspace_id: &str,
        lease_owner: &str,
        lease: Duration,
    ) -> Result<WorkspacePreparationClaim, SessionStoreError> {
        if workspace_id.trim().is_empty() || lease_owner.trim().is_empty() || lease.is_zero() {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "workspace_id, lease_owner, and positive lease are required".to_owned(),
            });
        }
        let mut tx = self.pool.begin().await?;
        let lease_expires_at = OffsetDateTime::now_utc()
            + time::Duration::try_from(lease).map_err(|_| {
                SessionStoreError::InvalidDevelopmentRequest {
                    message: "workspace preparation lease is too large".to_owned(),
                }
            })?;
        let row = sqlx::query_as::<_, SessionWorkspaceRow>(
            r#"
            update session_workspaces
               set preparation_attempt = preparation_attempt + case
                       when preparation_attempt = 0 or exists(
                           select 1 from session_repositories repository
                            where repository.workspace_id = session_workspaces.workspace_id
                              and repository.state in ('pending', 'failed')
                       ) then 1 else 0
                   end,
                   lease_owner = $2,
                   lease_expires_at = $3,
                   updated_at = now()
             where workspace_id = $1
               and state = 'provisioning'
               and (lease_owner is null or lease_owner = $2 or lease_expires_at < now())
            returning workspace_id, thread_key, state, storage_ref,
                      preparation_attempt, created_at, updated_at
            "#,
        )
        .bind(workspace_id)
        .bind(lease_owner)
        .bind(lease_expires_at)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentConflict {
            message: format!("workspace {workspace_id} is not claimable for preparation"),
        })?;
        sqlx::query(
            r#"
            update session_repositories
               set state = 'provisioning', provisioning_attempt = provisioning_attempt + 1,
                   failure_code = null, failure_message = null, updated_at = now()
             where workspace_id = $1 and state in ('pending', 'failed')
            "#,
        )
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;
        let repositories = sqlx::query_as::<_, WorkspaceRepositorySnapshotRow>(
            r#"
            select repository_id, display_name, path_with_namespace, default_branch,
                   clone_url, relative_path, state, base_sha, local_branch, head_sha
              from session_repositories
             where workspace_id = $1
             order by gitlab_project_id, repository_id
             for update
            "#,
        )
        .bind(workspace_id)
        .fetch_all(&mut *tx)
        .await?;
        let execution_id = sqlx::query_scalar::<_, String>(
            r#"
            select flow.execution_id
              from development_selection_flows flow
              join session_executions execution on execution.execution_id = flow.execution_id
             where flow.workspace_id = $1 and flow.kind = 'initial'
               and execution.status = 'queued'
               and execution.blocking_reason = 'workspace_provisioning'
             order by flow.created_at, flow.selection_flow_id
             limit 1
            "#,
        )
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(WorkspacePreparationClaim {
            workspace: row.try_into()?,
            execution_id,
            repositories: repositories
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    pub async fn complete_workspace_preparation(
        &self,
        result: &CompleteWorkspacePreparation,
    ) -> Result<SessionWorkspace, SessionStoreError> {
        if result.storage_ref.trim().is_empty() || result.lease_owner.trim().is_empty() {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "storage_ref and lease_owner are required".to_owned(),
            });
        }
        let mut result_ids = HashSet::new();
        for repository in &result.prepared {
            if !result_ids.insert(repository.repository_id.clone())
                || repository.base_sha.trim().is_empty()
                || repository.head_sha.trim().is_empty()
                || repository.local_branch.trim().is_empty()
            {
                return Err(SessionStoreError::InvalidDevelopmentRequest {
                    message: "workspace preparation contains an invalid prepared repository"
                        .to_owned(),
                });
            }
        }
        for repository in &result.failed {
            if !result_ids.insert(repository.repository_id.clone())
                || repository.failure_code.trim().is_empty()
            {
                return Err(SessionStoreError::InvalidDevelopmentRequest {
                    message: "workspace preparation contains an invalid failed repository"
                        .to_owned(),
                });
            }
        }
        let mut tx = self.pool.begin().await?;
        let expected_ids = sqlx::query_scalar::<_, String>(
            "select repository_id from session_repositories where workspace_id = $1 for update",
        )
        .bind(&result.workspace_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|value| value.parse())
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?;
        if expected_ids != result_ids {
            return Err(SessionStoreError::DevelopmentConflict {
                message: "workspace preparation result does not match the repository set"
                    .to_owned(),
            });
        }
        for repository in &result.prepared {
            sqlx::query(
                r#"
                update session_repositories
                   set state = 'ready', base_sha = $3, local_branch = $4, head_sha = $5,
                       failure_code = null, failure_message = null, updated_at = now()
                 where workspace_id = $1 and repository_id = $2
                "#,
            )
            .bind(&result.workspace_id)
            .bind(repository.repository_id.as_str())
            .bind(&repository.base_sha)
            .bind(&repository.local_branch)
            .bind(&repository.head_sha)
            .execute(&mut *tx)
            .await?;
        }
        for repository in &result.failed {
            sqlx::query(
                r#"
                update session_repositories
                   set state = 'failed', failure_code = $3, failure_message = $4,
                       updated_at = now()
                 where workspace_id = $1 and repository_id = $2
                "#,
            )
            .bind(&result.workspace_id)
            .bind(repository.repository_id.as_str())
            .bind(&repository.failure_code)
            .bind(&repository.failure_message)
            .execute(&mut *tx)
            .await?;
        }
        let target_state = if result.failed.is_empty() {
            WorkspaceState::Ready
        } else {
            WorkspaceState::Failed
        };
        let workspace = sqlx::query_as::<_, SessionWorkspaceRow>(
            r#"
            update session_workspaces
               set state = $4, storage_ref = $5, lease_owner = null,
                   lease_expires_at = null, updated_at = now()
             where workspace_id = $1 and preparation_attempt = $2 and lease_owner = $3
               and state = 'provisioning'
            returning workspace_id, thread_key, state, storage_ref,
                      preparation_attempt, created_at, updated_at
            "#,
        )
        .bind(&result.workspace_id)
        .bind(result.attempt)
        .bind(&result.lease_owner)
        .bind(target_state.as_ref())
        .bind(&result.storage_ref)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentConflict {
            message: "workspace preparation lease or attempt is stale".to_owned(),
        })?;
        if target_state == WorkspaceState::Ready {
            sqlx::query(
                r#"
                update session_executions execution
                   set blocking_reason = null, updated_at = now()
                  from development_selection_flows flow
                 where flow.workspace_id = $1
                   and flow.execution_id = execution.execution_id
                   and execution.status = 'queued'
                   and execution.blocking_reason = 'workspace_provisioning'
                "#,
            )
            .bind(&result.workspace_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        workspace.try_into()
    }

    pub async fn fail_workspace_preparation(
        &self,
        workspace_id: &str,
        attempt: i32,
        lease_owner: &str,
        failure_code: &str,
        failure_message: &str,
    ) -> Result<SessionWorkspace, SessionStoreError> {
        if workspace_id.trim().is_empty()
            || attempt < 1
            || lease_owner.trim().is_empty()
            || failure_code.trim().is_empty()
        {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "workspace failure requires an ID, attempt, owner, and code".to_owned(),
            });
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            update session_repositories
               set state = 'failed', failure_code = $2, failure_message = $3,
                   updated_at = now()
             where workspace_id = $1 and state = 'provisioning'
            "#,
        )
        .bind(workspace_id)
        .bind(failure_code)
        .bind(failure_message)
        .execute(&mut *tx)
        .await?;
        let workspace = sqlx::query_as::<_, SessionWorkspaceRow>(
            r#"
            update session_workspaces
               set state = 'failed', lease_owner = null, lease_expires_at = null,
                   updated_at = now()
             where workspace_id = $1 and preparation_attempt = $2 and lease_owner = $3
               and state = 'provisioning'
            returning workspace_id, thread_key, state, storage_ref,
                      preparation_attempt, created_at, updated_at
            "#,
        )
        .bind(workspace_id)
        .bind(attempt)
        .bind(lease_owner)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentConflict {
            message: "workspace preparation lease or attempt is stale".to_owned(),
        })?;
        tx.commit().await?;
        workspace.try_into()
    }

    pub async fn workspace_for_session(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<SessionWorkspace>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionWorkspaceRow>(
            r#"
            select workspace_id, thread_key, state, storage_ref,
                   preparation_attempt, created_at, updated_at
              from session_workspaces where thread_key = $1
            "#,
        )
        .bind(thread_key.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn list_provisioning_workspace_ids(&self) -> Result<Vec<String>, SessionStoreError> {
        Ok(sqlx::query_scalar(
            r#"
            select workspace_id
              from session_workspaces
             where state = 'provisioning'
               and (lease_owner is null or lease_expires_at < now())
             order by updated_at, workspace_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn begin_changeset_collection(
        &self,
        execution_id: &str,
        owner_id: &str,
    ) -> Result<Option<DevelopmentChangeSet>, SessionStoreError> {
        if execution_id.trim().is_empty() || owner_id.trim().is_empty() {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "execution_id and collection owner are required".to_owned(),
            });
        }
        let mut tx = self.pool.begin().await?;
        crate::lock_development_execution_boundary_for_update(&mut tx, execution_id).await?;
        #[derive(FromRow)]
        struct ContextRow {
            workspace_id: String,
            initiator_principal_id: String,
            execution_status: String,
            workspace_state: String,
        }
        let context = sqlx::query_as::<_, ContextRow>(
            r#"
            select workspace.workspace_id,
                   binding.initiator_principal_id,
                   execution.status as execution_status,
                   workspace.state as workspace_state
              from session_executions execution
              join session_workspaces workspace using (thread_key)
              join development_channel_bindings binding
                on binding.thread_key = execution.thread_key and binding.active
             where execution.execution_id = $1
             for update of execution, workspace
            "#,
        )
        .bind(execution_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(context) = context else {
            tx.commit().await?;
            return Ok(None);
        };
        if context.execution_status != ExecutionStatus::Completed.as_ref() {
            return Err(SessionStoreError::DevelopmentConflict {
                message: "only a completed execution can be collected".to_owned(),
            });
        }
        if let Some(existing) = load_changeset_by_execution(&mut tx, execution_id).await? {
            tx.commit().await?;
            return Ok(Some(existing));
        }
        if context.workspace_state != WorkspaceState::Ready.as_ref() {
            return Err(SessionStoreError::DevelopmentConflict {
                message: format!(
                    "workspace is {} and cannot begin collection",
                    context.workspace_state
                ),
            });
        }
        let changeset_id = prefixed_id("chg");
        sqlx::query(
            r#"
            insert into development_change_sets
                (changeset_id, workspace_id, execution_id, initiator_principal_id,
                 state, lease_owner, lease_expires_at)
            values ($1, $2, $3, $4, 'collecting', $5, now() + interval '10 minutes')
            "#,
        )
        .bind(&changeset_id)
        .bind(&context.workspace_id)
        .bind(execution_id)
        .bind(&context.initiator_principal_id)
        .bind(owner_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update session_workspaces set state = 'collecting', updated_at = now() where workspace_id = $1 and state = 'ready'",
        )
        .bind(&context.workspace_id)
        .execute(&mut *tx)
        .await?;
        let changeset = load_changeset(&mut tx, &changeset_id)
            .await?
            .expect("inserted changeset is visible");
        tx.commit().await?;
        Ok(Some(changeset))
    }

    pub async fn complete_development_execution_and_begin_collection(
        &self,
        execution_id: &str,
        stdout_owner_id: &str,
        collection_owner_id: &str,
    ) -> Result<Option<CompletedDevelopmentExecution>, SessionStoreError> {
        if execution_id.trim().is_empty()
            || stdout_owner_id.trim().is_empty()
            || collection_owner_id.trim().is_empty()
        {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "execution and collection owners are required".to_owned(),
            });
        }
        let mut tx = self.pool.begin().await?;
        crate::lock_development_execution_boundary_for_update(&mut tx, execution_id).await?;
        let row = sqlx::query_as::<_, crate::SessionExecutionRow>(
            r#"
            update session_executions
               set status = 'completed', completed_at = coalesce(completed_at, now()),
                   stdout_owner_id = null, stdout_owner_lease_expires_at = null,
                   updated_at = now()
             where execution_id = $1 and status in ('queued', 'running')
               and stdout_owner_id = $2
            returning execution_id, idempotency_key, thread_key, status, blocking_reason,
                      metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(stdout_owner_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let workspace = sqlx::query_as::<_, (String, String, String)>(
            r#"
            select workspace.workspace_id, workspace.state, binding.initiator_principal_id
              from session_workspaces workspace
              join development_channel_bindings binding
                on binding.thread_key = workspace.thread_key and binding.active
             where workspace.thread_key = $1
             for update of workspace
            "#,
        )
        .bind(&row.thread_key)
        .fetch_optional(&mut *tx)
        .await?;
        let mut changeset_id = None;
        if let Some((workspace_id, workspace_state, initiator)) = workspace {
            if workspace_state != WorkspaceState::Ready.as_ref() {
                return Err(SessionStoreError::DevelopmentConflict {
                    message: format!("workspace is {workspace_state} when execution completed"),
                });
            }
            let id = prefixed_id("chg");
            sqlx::query(
                r#"
                insert into development_change_sets
                    (changeset_id, workspace_id, execution_id, initiator_principal_id,
                     state, lease_owner, lease_expires_at)
                values ($1, $2, $3, $4, 'collecting', $5, now() + interval '10 minutes')
                "#,
            )
            .bind(&id)
            .bind(&workspace_id)
            .bind(execution_id)
            .bind(initiator)
            .bind(collection_owner_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "update session_workspaces set state = 'collecting', updated_at = now() where workspace_id = $1",
            )
            .bind(workspace_id)
            .execute(&mut *tx)
            .await?;
            changeset_id = Some(id);
        }
        sqlx::query(
            "update sessions set status = 'idle', updated_at = now() where thread_key = $1",
        )
        .bind(&row.thread_key)
        .execute(&mut *tx)
        .await?;
        let execution = row.try_into()?;
        tx.commit().await?;
        Ok(Some(CompletedDevelopmentExecution {
            execution,
            changeset_id,
        }))
    }

    pub async fn claim_changeset_collection(
        &self,
        changeset_id: &str,
        lease_owner: &str,
        lease: Duration,
    ) -> Result<ChangeSetCollectionClaim, SessionStoreError> {
        if changeset_id.trim().is_empty() || lease_owner.trim().is_empty() || lease.is_zero() {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "changeset_id, lease_owner, and positive lease are required".to_owned(),
            });
        }
        let lease_expires_at = OffsetDateTime::now_utc()
            + time::Duration::try_from(lease).map_err(|_| {
                SessionStoreError::InvalidDevelopmentRequest {
                    message: "changeset collection lease is too large".to_owned(),
                }
            })?;
        let mut tx = self.pool.begin().await?;
        let changeset_id = sqlx::query_scalar::<_, String>(
            r#"
            update development_change_sets
               set lease_owner = $2, lease_expires_at = $3, updated_at = now()
             where changeset_id = $1 and state = 'collecting'
               and (lease_owner is null or lease_owner = $2 or lease_expires_at < now())
            returning changeset_id
            "#,
        )
        .bind(changeset_id)
        .bind(lease_owner)
        .bind(lease_expires_at)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentConflict {
            message: "changeset is not claimable for collection".to_owned(),
        })?;
        let changeset = load_changeset(&mut tx, &changeset_id)
            .await?
            .expect("claimed changeset exists");
        let workspace = sqlx::query_as::<_, SessionWorkspaceRow>(
            r#"
            select workspace_id, thread_key, state, storage_ref,
                   preparation_attempt, created_at, updated_at
              from session_workspaces where workspace_id = $1
            "#,
        )
        .bind(&changeset.workspace_id)
        .fetch_one(&mut *tx)
        .await?
        .try_into()?;
        let repositories = load_workspace_repositories(&mut tx, &changeset.workspace_id).await?;
        let execution_metadata = sqlx::query_scalar::<_, Value>(
            "select metadata from session_executions where execution_id = $1",
        )
        .bind(&changeset.execution_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ChangeSetCollectionClaim {
            changeset,
            workspace,
            repositories,
            execution_metadata,
        })
    }

    pub async fn complete_changeset_collection(
        &self,
        result: &CompleteChangeSetCollection,
    ) -> Result<Option<DevelopmentChangeSet>, SessionStoreError> {
        validate_complete_changeset(result)?;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, (String, String)>(
            r#"
            select changeset.workspace_id, changeset.state
              from development_change_sets changeset
             where changeset.changeset_id = $1 and changeset.lease_owner = $2
               and changeset.state = 'collecting'
             for update
            "#,
        )
        .bind(&result.changeset_id)
        .bind(&result.lease_owner)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| SessionStoreError::DevelopmentConflict {
            message: "changeset collection lease is stale".to_owned(),
        })?;
        let expected_repositories = sqlx::query_as::<_, (String, Option<String>, Option<String>, String)>(
            "select repository_id, base_sha, head_sha, state from session_repositories where workspace_id = $1 order by repository_id for update",
        )
        .bind(&row.0)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(repository_id, base_sha, head_sha, state)| {
            let repository_id = repository_id.parse::<centaur_session_core::development::RepositoryId>().map_err(|error| {
                SessionStoreError::InvalidPersistedValue(format!("{error}"))
            })?;
            Ok((repository_id, (base_sha, head_sha, state)))
        })
        .collect::<Result<HashMap<_, _>, SessionStoreError>>()?;
        let result_ids = result
            .repositories
            .iter()
            .map(|repository| repository.repository_id.clone())
            .collect::<HashSet<_>>();
        if expected_repositories.len() != result_ids.len()
            || !result_ids
                .iter()
                .all(|repository_id| expected_repositories.contains_key(repository_id))
        {
            return Err(SessionStoreError::DevelopmentConflict {
                message: "changeset result does not match the workspace repository set".to_owned(),
            });
        }
        for repository in &result.repositories {
            let (expected_base, expected_head, state) =
                &expected_repositories[&repository.repository_id];
            if state != RepositoryState::Ready.as_ref()
                || expected_base.as_deref() != Some(repository.base_sha.as_str())
                || expected_head.as_deref() != Some(repository.recorded_head_sha.as_str())
            {
                return Err(SessionStoreError::DevelopmentConflict {
                    message: format!(
                        "repository {} no longer matches the collection input",
                        repository.repository_id
                    ),
                });
            }
        }
        let changed_count = result
            .repositories
            .iter()
            .filter(|repository| {
                !matches!(
                    repository.state,
                    centaur_session_core::development::CollectedChangeSetRepositoryState::Unchanged
                )
            })
            .count();
        if changed_count == 0 {
            sqlx::query("delete from development_change_sets where changeset_id = $1")
                .bind(&result.changeset_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "update session_workspaces set state = 'ready', updated_at = now() where workspace_id = $1 and state = 'collecting'",
            )
            .bind(&row.0)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(None);
        }

        let mut has_needs_completion = false;
        let mut has_failure = false;
        for repository in &result.repositories {
            use centaur_session_core::development::CollectedChangeSetRepositoryState;
            if repository.state == CollectedChangeSetRepositoryState::Unchanged {
                continue;
            }
            let persisted_state = match repository.state {
                CollectedChangeSetRepositoryState::Changed => ChangeSetRepositoryState::Changed,
                CollectedChangeSetRepositoryState::NeedsAgentCompletion => {
                    has_needs_completion = true;
                    ChangeSetRepositoryState::NeedsAgentCompletion
                }
                CollectedChangeSetRepositoryState::Failed => {
                    has_failure = true;
                    ChangeSetRepositoryState::Failed
                }
                CollectedChangeSetRepositoryState::Unchanged => unreachable!(),
            };
            let artifact_ref = if let (Some(hash), false) = (
                repository.patch_hash.as_deref(),
                repository.patch.is_empty(),
            ) {
                Some(
                    put_development_artifact(&mut tx, hash, "text/x-diff", &repository.patch)
                        .await?,
                )
            } else {
                None
            };
            sqlx::query(
                r#"
                insert into development_change_set_repositories
                    (changeset_repository_id, changeset_id, workspace_id, repository_id,
                     state, base_sha, recorded_head_sha, head_sha, commit_metadata,
                     changed_file_count, additions, deletions, patch_hash,
                     patch_artifact_ref, test_evidence, failure_code, failure_message)
                values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                        $13, $14, $15, $16, $17)
                "#,
            )
            .bind(prefixed_id("cgr"))
            .bind(&result.changeset_id)
            .bind(&row.0)
            .bind(repository.repository_id.as_str())
            .bind(persisted_state.as_ref())
            .bind(&repository.base_sha)
            .bind(&repository.recorded_head_sha)
            .bind(&repository.head_sha)
            .bind(&repository.commit_metadata)
            .bind(repository.changed_file_count)
            .bind(repository.additions)
            .bind(repository.deletions)
            .bind(&repository.patch_hash)
            .bind(&artifact_ref)
            .bind(&repository.test_evidence)
            .bind(&repository.failure_code)
            .bind(&repository.failure_message)
            .execute(&mut *tx)
            .await?;
            if repository.state == CollectedChangeSetRepositoryState::Changed {
                let updated = sqlx::query(
                    r#"
                    update session_repositories
                       set head_sha = $3, updated_at = now()
                     where workspace_id = $1 and repository_id = $2
                       and head_sha = $4 and state = 'ready'
                    "#,
                )
                .bind(&row.0)
                .bind(repository.repository_id.as_str())
                .bind(&repository.head_sha)
                .bind(&repository.recorded_head_sha)
                .execute(&mut *tx)
                .await?;
                if updated.rows_affected() != 1 {
                    return Err(SessionStoreError::DevelopmentConflict {
                        message: format!(
                            "repository {} changed while its changeset was collected",
                            repository.repository_id
                        ),
                    });
                }
            }
        }
        let final_state = if has_failure {
            ChangeSetState::Failed
        } else if has_needs_completion {
            ChangeSetState::NeedsAgentCompletion
        } else {
            ChangeSetState::Ready
        };
        let summary = format!("{changed_count} repository result(s)");
        sqlx::query(
            r#"
            update development_change_sets
               set state = $2, summary = $3, lease_owner = null,
                   lease_expires_at = null, updated_at = now()
             where changeset_id = $1
            "#,
        )
        .bind(&result.changeset_id)
        .bind(final_state.as_ref())
        .bind(summary)
        .execute(&mut *tx)
        .await?;
        let changed_head_count = result
            .repositories
            .iter()
            .filter(|repository| {
                repository.state
                    == centaur_session_core::development::CollectedChangeSetRepositoryState::Changed
            })
            .count();
        let workspace_revision = sqlx::query_scalar::<_, i64>(
            r#"
            update session_workspaces
               set state = 'ready',
                   workspace_revision = workspace_revision + case when $2 > 0 then 1 else 0 end,
                   updated_at = now()
             where workspace_id = $1 and state = 'collecting'
            returning workspace_revision
            "#,
        )
        .bind(&row.0)
        .bind(i64::try_from(changed_head_count).unwrap_or(i64::MAX))
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "update development_change_sets set workspace_revision = $2 where changeset_id = $1",
        )
        .bind(&result.changeset_id)
        .bind(workspace_revision)
        .execute(&mut *tx)
        .await?;
        let changeset = load_changeset(&mut tx, &result.changeset_id)
            .await?
            .expect("completed changeset exists");
        tx.commit().await?;
        Ok(Some(changeset))
    }

    pub async fn get_changeset(
        &self,
        changeset_id: &str,
        principal_id: &str,
        is_admin: bool,
    ) -> Result<DevelopmentChangeSet, SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        let changeset = load_changeset(&mut tx, changeset_id)
            .await?
            .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
                message: format!("changeset {changeset_id}"),
            })?;
        if !is_admin && changeset.initiator_principal_id != principal_id {
            return Err(SessionStoreError::DevelopmentForbidden {
                message: "changeset is not accessible to this principal".to_owned(),
            });
        }
        tx.commit().await?;
        Ok(changeset)
    }

    pub async fn get_changeset_artifact(
        &self,
        changeset_id: &str,
        artifact_ref: &str,
        principal_id: &str,
        is_admin: bool,
    ) -> Result<Vec<u8>, SessionStoreError> {
        let changeset = self
            .get_changeset(changeset_id, principal_id, is_admin)
            .await?;
        if !changeset
            .repositories
            .iter()
            .any(|repository| repository.patch_artifact_ref.as_deref() == Some(artifact_ref))
        {
            return Err(SessionStoreError::DevelopmentNotFound {
                message: "changeset artifact".to_owned(),
            });
        }
        sqlx::query_scalar("select content from development_artifacts where artifact_ref = $1")
            .bind(artifact_ref)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
                message: "changeset artifact".to_owned(),
            })
    }

    pub async fn list_collecting_changeset_ids(&self) -> Result<Vec<String>, SessionStoreError> {
        Ok(sqlx::query_scalar(
            r#"
            select changeset_id from development_change_sets
             where state = 'collecting'
               and (lease_owner is null or lease_expires_at < now())
             order by updated_at, changeset_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?)
    }
}

#[derive(Debug, FromRow)]
struct DevelopmentChangeSetRow {
    changeset_id: String,
    workspace_id: String,
    execution_id: String,
    initiator_principal_id: String,
    state: String,
    summary: Option<String>,
    failure_code: Option<String>,
    failure_message: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct DevelopmentChangeSetRepositoryRow {
    changeset_repository_id: String,
    repository_id: String,
    display_name: String,
    path_with_namespace: String,
    default_branch: String,
    state: String,
    base_sha: String,
    recorded_head_sha: String,
    head_sha: Option<String>,
    commit_metadata: Value,
    changed_file_count: i32,
    additions: i32,
    deletions: i32,
    patch_hash: Option<String>,
    patch_artifact_ref: Option<String>,
    test_evidence: Value,
    failure_code: Option<String>,
    failure_message: Option<String>,
}

async fn load_changeset_by_execution(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    execution_id: &str,
) -> Result<Option<DevelopmentChangeSet>, SessionStoreError> {
    let changeset_id = sqlx::query_scalar::<_, String>(
        "select changeset_id from development_change_sets where execution_id = $1",
    )
    .bind(execution_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(changeset_id) = changeset_id else {
        return Ok(None);
    };
    load_changeset(tx, &changeset_id).await
}

async fn load_changeset(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    changeset_id: &str,
) -> Result<Option<DevelopmentChangeSet>, SessionStoreError> {
    let row = sqlx::query_as::<_, DevelopmentChangeSetRow>(
        r#"
        select changeset_id, workspace_id, execution_id, initiator_principal_id,
               state, summary, failure_code, failure_message, created_at, updated_at
          from development_change_sets where changeset_id = $1
        "#,
    )
    .bind(changeset_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let repositories = sqlx::query_as::<_, DevelopmentChangeSetRepositoryRow>(
        r#"
        select collected.changeset_repository_id, collected.repository_id,
               repository.display_name, repository.path_with_namespace,
               repository.default_branch, collected.state, collected.base_sha,
               collected.recorded_head_sha, collected.head_sha,
               collected.commit_metadata, collected.changed_file_count,
               collected.additions, collected.deletions, collected.patch_hash,
               collected.patch_artifact_ref, collected.test_evidence,
               collected.failure_code, collected.failure_message
          from development_change_set_repositories collected
          join session_repositories repository
            on repository.workspace_id = collected.workspace_id
           and repository.repository_id = collected.repository_id
         where collected.changeset_id = $1
         order by repository.gitlab_project_id, repository.repository_id
        "#,
    )
    .bind(changeset_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(Some(DevelopmentChangeSet {
        changeset_id: row.changeset_id,
        workspace_id: row.workspace_id,
        execution_id: row.execution_id,
        initiator_principal_id: row.initiator_principal_id,
        state: parse_development_value(row.state)?,
        summary: row.summary,
        failure_code: row.failure_code,
        failure_message: row.failure_message,
        repositories: repositories
            .into_iter()
            .map(|repository| {
                Ok(DevelopmentChangeSetRepository {
                    changeset_repository_id: repository.changeset_repository_id,
                    repository_id: parse_development_value(repository.repository_id)?,
                    display_name: repository.display_name,
                    path_with_namespace: repository.path_with_namespace,
                    default_branch: repository.default_branch,
                    state: parse_development_value(repository.state)?,
                    base_sha: repository.base_sha,
                    recorded_head_sha: repository.recorded_head_sha,
                    head_sha: repository.head_sha,
                    commit_metadata: repository.commit_metadata,
                    changed_file_count: repository.changed_file_count,
                    additions: repository.additions,
                    deletions: repository.deletions,
                    patch_hash: repository.patch_hash,
                    patch_artifact_ref: repository.patch_artifact_ref,
                    test_evidence: repository.test_evidence,
                    failure_code: repository.failure_code,
                    failure_message: repository.failure_message,
                })
            })
            .collect::<Result<Vec<_>, SessionStoreError>>()?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

async fn load_workspace_repositories(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: &str,
) -> Result<Vec<WorkspaceRepositorySnapshot>, SessionStoreError> {
    sqlx::query_as::<_, WorkspaceRepositorySnapshotRow>(
        r#"
        select repository_id, display_name, path_with_namespace, default_branch,
               clone_url, relative_path, state, base_sha, local_branch, head_sha
          from session_repositories where workspace_id = $1
         order by gitlab_project_id, repository_id
        "#,
    )
    .bind(workspace_id)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

async fn put_development_artifact(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    expected_hash: &str,
    media_type: &str,
    content: &[u8],
) -> Result<String, SessionStoreError> {
    let actual_hash = format!("sha256:{}", hex::encode(Sha256::digest(content)));
    if actual_hash != expected_hash {
        return Err(SessionStoreError::InvalidDevelopmentRequest {
            message: "changeset patch hash does not match its content".to_owned(),
        });
    }
    let byte_length =
        i32::try_from(content.len()).map_err(|_| SessionStoreError::InvalidDevelopmentRequest {
            message: "changeset patch exceeds the artifact size limit".to_owned(),
        })?;
    let artifact_ref = format!("artifact:{actual_hash}");
    sqlx::query(
        r#"
        insert into development_artifacts
            (artifact_ref, sha256, media_type, byte_length, content)
        values ($1, $2, $3, $4, $5)
        on conflict do nothing
        "#,
    )
    .bind(&artifact_ref)
    .bind(&actual_hash)
    .bind(media_type)
    .bind(byte_length)
    .bind(content)
    .execute(&mut **tx)
    .await?;
    let stored = sqlx::query_as::<_, (String, String, i32, Vec<u8>)>(
        "select sha256, media_type, byte_length, content from development_artifacts where artifact_ref = $1",
    )
    .bind(&artifact_ref)
    .fetch_optional(&mut **tx)
    .await?;
    if stored.as_ref()
        != Some(&(
            actual_hash,
            media_type.to_owned(),
            byte_length,
            content.to_vec(),
        ))
    {
        return Err(SessionStoreError::InvalidPersistedValue(
            "content-addressed development artifact does not match its reference".to_owned(),
        ));
    }
    Ok(artifact_ref)
}

fn validate_complete_changeset(
    result: &CompleteChangeSetCollection,
) -> Result<(), SessionStoreError> {
    use centaur_session_core::development::CollectedChangeSetRepositoryState;
    if result.changeset_id.trim().is_empty() || result.lease_owner.trim().is_empty() {
        return Err(SessionStoreError::InvalidDevelopmentRequest {
            message: "changeset_id and lease_owner are required".to_owned(),
        });
    }
    let mut ids = HashSet::with_capacity(result.repositories.len());
    for repository in &result.repositories {
        if !ids.insert(repository.repository_id.clone())
            || repository.changed_file_count < 0
            || repository.additions < 0
            || repository.deletions < 0
            || !valid_git_sha(&repository.base_sha)
            || !valid_git_sha(&repository.recorded_head_sha)
            || repository
                .head_sha
                .as_deref()
                .is_some_and(|sha| !valid_git_sha(sha))
        {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "changeset contains invalid repository metadata".to_owned(),
            });
        }
        let valid_state = match repository.state {
            CollectedChangeSetRepositoryState::Unchanged => {
                repository.head_sha.is_some()
                    && repository.patch_hash.is_none()
                    && repository.patch.is_empty()
                    && repository.failure_code.is_none()
            }
            CollectedChangeSetRepositoryState::Changed => {
                repository.head_sha.is_some()
                    && repository.patch_hash.is_some()
                    && !repository.patch.is_empty()
                    && repository.changed_file_count > 0
                    && repository.failure_code.is_none()
            }
            CollectedChangeSetRepositoryState::NeedsAgentCompletion
            | CollectedChangeSetRepositoryState::Failed => {
                repository.patch_hash.is_none()
                    && repository.patch.is_empty()
                    && repository
                        .failure_code
                        .as_deref()
                        .is_some_and(|code| !code.trim().is_empty())
            }
        };
        if !valid_state {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "changeset repository state does not match its artifacts".to_owned(),
            });
        }
        if let Some(hash) = &repository.patch_hash {
            let actual = format!("sha256:{}", hex::encode(Sha256::digest(&repository.patch)));
            if hash != &actual {
                return Err(SessionStoreError::InvalidDevelopmentRequest {
                    message: "changeset patch hash does not match its content".to_owned(),
                });
            }
        }
        if !repository.test_evidence.is_array() {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: "changeset test evidence must be an array".to_owned(),
            });
        }
    }
    Ok(())
}

fn valid_git_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_development_value<T>(value: String) -> Result<T, SessionStoreError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))
}

#[derive(Debug)]
struct LockedSelectionFlow {
    workspace_id: String,
    execution_id: Option<String>,
    kind: String,
    state: String,
    version: i32,
    workspace_state: WorkspaceState,
    initiator_principal_id: String,
}

async fn lock_selection_flow(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    selection_flow_id: &str,
) -> Result<LockedSelectionFlow, SessionStoreError> {
    #[derive(FromRow)]
    struct Row {
        workspace_id: String,
        execution_id: Option<String>,
        kind: String,
        state: String,
        version: i32,
        workspace_state: String,
        initiator_principal_id: String,
    }

    let row = sqlx::query_as::<_, Row>(
        r#"
        select flow.workspace_id, flow.execution_id, flow.kind, flow.state, flow.version,
               workspace.state as workspace_state, binding.initiator_principal_id
          from development_selection_flows flow
          join session_workspaces workspace using (workspace_id)
          join development_channel_bindings binding
            on binding.thread_key = workspace.thread_key and binding.active
         where flow.selection_flow_id = $1
         for update of flow, workspace
        "#,
    )
    .bind(selection_flow_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| SessionStoreError::DevelopmentNotFound {
        message: format!("selection flow {selection_flow_id}"),
    })?;

    Ok(LockedSelectionFlow {
        workspace_id: row.workspace_id,
        execution_id: row.execution_id,
        kind: row.kind,
        state: row.state,
        version: row.version,
        workspace_state: row
            .workspace_state
            .parse()
            .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
        initiator_principal_id: row.initiator_principal_id,
    })
}

fn ensure_selection_decider(
    flow: &LockedSelectionFlow,
    decided_by_principal_id: &str,
) -> Result<(), SessionStoreError> {
    if flow.initiator_principal_id != decided_by_principal_id {
        return Err(SessionStoreError::DevelopmentForbidden {
            message: "only the task initiator may decide repository selection".to_owned(),
        });
    }
    Ok(())
}

fn validate_development_task(request: &AcceptDevelopmentTask) -> Result<(), SessionStoreError> {
    for (name, value) in [
        ("platform", request.channel.platform.as_str()),
        ("tenant_key", request.channel.tenant_key.as_str()),
        (
            "conversation_key",
            request.channel.conversation_key.as_str(),
        ),
        ("root_message_id", request.channel.root_message_id.as_str()),
        ("platform_event_id", request.platform_event_id.as_str()),
        (
            "initiator principal_id",
            request.initiator.principal_id.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            return Err(SessionStoreError::InvalidDevelopmentRequest {
                message: format!("{name} must not be empty"),
            });
        }
    }
    if request.message.role != MessageRole::User {
        return Err(SessionStoreError::InvalidDevelopmentRequest {
            message: "development task message role must be user".to_owned(),
        });
    }
    if let (Some(platform_message_id), Some(client_message_id)) = (
        request.platform_message_id.as_deref(),
        request.message.client_message_id.as_deref(),
    ) && platform_message_id != client_message_id
    {
        return Err(SessionStoreError::InvalidDevelopmentRequest {
            message: "platform_message_id and client_message_id must match".to_owned(),
        });
    }
    Ok(())
}

fn parse_selection_kind(value: &str) -> Result<SelectionKind, SessionStoreError> {
    value
        .parse()
        .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))
}

async fn ensure_workspace_accepts_additions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    flow: &LockedSelectionFlow,
) -> Result<(), SessionStoreError> {
    if flow.workspace_state != WorkspaceState::Ready {
        return Err(SessionStoreError::DevelopmentConflict {
            message: format!(
                "workspace is {} and cannot accept projects",
                flow.workspace_state
            ),
        });
    }
    let thread_key = sqlx::query_scalar::<_, String>(
        "select thread_key from session_workspaces where workspace_id = $1",
    )
    .bind(&flow.workspace_id)
    .fetch_one(&mut **tx)
    .await?
    .parse()
    .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?;
    ensure_session_has_no_active_work(tx, &thread_key).await
}

async fn ensure_session_has_no_active_work(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    thread_key: &ThreadKey,
) -> Result<(), SessionStoreError> {
    let active = sqlx::query_scalar::<_, bool>(
        r#"
        select
            exists(
                select 1 from session_executions
                 where thread_key = $1 and status in ('queued', 'running')
            )
            or exists(
                select 1
                  from development_publish_batches batch
                  join development_change_sets changeset using (changeset_id)
                  join session_workspaces workspace using (workspace_id)
                 where workspace.thread_key = $1
                   and batch.state in ('pending', 'running', 'partially_succeeded')
            )
        "#,
    )
    .bind(thread_key.as_str())
    .fetch_one(&mut **tx)
    .await?;
    if active {
        return Err(SessionStoreError::DevelopmentConflict {
            message: "session has active execution or publication".to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct RepositorySelectionDraftRow {
    selection_flow_id: String,
    workspace_id: String,
    kind: String,
    state: String,
    version: i32,
}

impl TryFrom<RepositorySelectionDraftRow> for RepositorySelectionDraft {
    type Error = SessionStoreError;

    fn try_from(row: RepositorySelectionDraftRow) -> Result<Self, Self::Error> {
        Ok(Self {
            selection_flow_id: row.selection_flow_id,
            workspace_id: row.workspace_id,
            kind: row
                .kind
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            state: row
                .state
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            version: row.version,
        })
    }
}

fn ensure_pending_selection_version(
    flow: &LockedSelectionFlow,
    expected_version: i32,
) -> Result<(), SessionStoreError> {
    if flow.state != SelectionFlowState::Pending.as_ref() || flow.version != expected_version {
        return Err(SessionStoreError::DevelopmentConflict {
            message: format!(
                "selection is {} at version {}, expected pending version {}",
                flow.state, flow.version, expected_version
            ),
        });
    }
    Ok(())
}

fn next_selection_version(current: i32) -> Result<i32, SessionStoreError> {
    current
        .checked_add(1)
        .ok_or_else(|| SessionStoreError::InvalidDevelopmentRequest {
            message: "selection version is too large".to_owned(),
        })
}

async fn accepted_task_for_receipt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &AcceptDevelopmentTask,
) -> Result<Option<AcceptedDevelopmentTask>, SessionStoreError> {
    let row = sqlx::query_as::<_, AcceptedDevelopmentTaskRow>(
        r#"
        select event.thread_key, workspace.workspace_id,
               flow.selection_flow_id, flow.execution_id
          from development_platform_events event
          join session_workspaces workspace using (thread_key)
          join development_selection_flows flow
            on flow.workspace_id = workspace.workspace_id
           and flow.kind = 'initial'
         where event.platform = $1
           and event.tenant_key = $2
           and (
                event.event_id = $3
                or ($4::text is not null and event.message_id = $4)
           )
         order by flow.created_at, flow.selection_flow_id
         limit 1
        "#,
    )
    .bind(&request.channel.platform)
    .bind(&request.channel.tenant_key)
    .bind(&request.platform_event_id)
    .bind(request.platform_message_id.as_deref())
    .fetch_optional(&mut **tx)
    .await?;

    row.map(TryInto::try_into).transpose()
}

#[derive(Debug, FromRow)]
struct AcceptedDevelopmentTaskRow {
    thread_key: String,
    workspace_id: String,
    selection_flow_id: String,
    execution_id: String,
}

impl TryFrom<AcceptedDevelopmentTaskRow> for AcceptedDevelopmentTask {
    type Error = SessionStoreError;

    fn try_from(row: AcceptedDevelopmentTaskRow) -> Result<Self, Self::Error> {
        Ok(Self {
            thread_key: row
                .thread_key
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            workspace_id: row.workspace_id,
            selection_flow_id: row.selection_flow_id,
            execution_id: row.execution_id,
            created: false,
        })
    }
}

#[derive(Debug, FromRow)]
struct SessionWorkspaceRow {
    workspace_id: String,
    thread_key: String,
    state: String,
    storage_ref: Option<String>,
    preparation_attempt: i32,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(Debug, FromRow)]
struct WorkspaceRepositorySnapshotRow {
    repository_id: String,
    display_name: String,
    path_with_namespace: String,
    default_branch: String,
    clone_url: String,
    relative_path: String,
    state: String,
    base_sha: Option<String>,
    local_branch: Option<String>,
    head_sha: Option<String>,
}

impl TryFrom<WorkspaceRepositorySnapshotRow> for WorkspaceRepositorySnapshot {
    type Error = SessionStoreError;

    fn try_from(row: WorkspaceRepositorySnapshotRow) -> Result<Self, Self::Error> {
        Ok(Self {
            repository_id: row
                .repository_id
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            display_name: row.display_name,
            path_with_namespace: row.path_with_namespace,
            default_branch: row.default_branch,
            clone_url: row.clone_url,
            relative_path: row.relative_path,
            state: row
                .state
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            base_sha: row.base_sha,
            local_branch: row.local_branch,
            head_sha: row.head_sha,
        })
    }
}

impl TryFrom<SessionWorkspaceRow> for SessionWorkspace {
    type Error = SessionStoreError;

    fn try_from(row: SessionWorkspaceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            workspace_id: row.workspace_id,
            thread_key: row
                .thread_key
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            state: row
                .state
                .parse()
                .map_err(|error| SessionStoreError::InvalidPersistedValue(format!("{error}")))?,
            storage_ref: row.storage_ref,
            preparation_attempt: row.preparation_attempt,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use centaur_session_core::{
        ExecutionStatus, HarnessType, MessageRole, SessionMessageInput, ThreadKey,
        development::{
            AcceptDevelopmentTask, ChangeSetState, CompleteWorkspacePreparation,
            ConfirmRepositorySelection, DevelopmentChannel, DevelopmentInitiator, ExecutionBlocker,
            FailedRepositorySnapshot, PreparedRepositorySnapshot, ResolvedRepository,
            SelectionFlowState, SelectionKind, WorkspaceState,
        },
    };
    use serde_json::json;
    use uuid::Uuid;

    use crate::PgSessionStore;

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

    async fn legacy_session(store: &PgSessionStore, label: &str) -> ThreadKey {
        let thread_key = ThreadKey::parse(format!("test:{label}-{}", Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(
                &thread_key,
                &HarnessType::Codex,
                None,
                json!({}),
                Default::default(),
            )
            .await
            .expect("create legacy session");
        thread_key
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn legacy_session_can_attach_one_empty_workspace_idempotently() {
        let Some(store) = test_store().await else {
            return;
        };
        let thread_key = legacy_session(&store, "workspace").await;

        let first = store
            .create_or_get_workspace(&thread_key)
            .await
            .expect("create workspace");
        let replay = store
            .create_or_get_workspace(&thread_key)
            .await
            .expect("get workspace");

        assert_eq!(first, replay);
        assert_eq!(first.thread_key, thread_key);
        assert_eq!(first.state, WorkspaceState::AwaitingSelection);
        assert_eq!(first.preparation_attempt, 0);
        assert!(first.storage_ref.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_execution_round_trips_and_cannot_be_claimed() {
        let Some(store) = test_store().await else {
            return;
        };
        let thread_key = legacy_session(&store, "blocked-execution").await;

        let created = store
            .create_blocked_execution(
                &thread_key,
                "platform-event-1",
                ExecutionBlocker::AwaitingProjectSelection,
                json!({"source": "test"}),
            )
            .await
            .expect("create blocked execution");
        let claimed = store
            .mark_execution_running(&created.execution.execution_id)
            .await
            .expect("attempt claim");

        assert!(created.created);
        assert_eq!(
            created.execution.blocking_reason,
            Some(ExecutionBlocker::AwaitingProjectSelection)
        );
        assert!(!claimed.claimed);
        assert_eq!(
            claimed.execution.blocking_reason,
            Some(ExecutionBlocker::AwaitingProjectSelection)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn schema_rejects_duplicate_repository_and_platform_event_bindings() {
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = Uuid::new_v4();
        let tenant_key = format!("tenant-{suffix}");
        let event_id = format!("event-{suffix}");
        let first_message_id = format!("message-a-{suffix}");
        let second_message_id = format!("message-b-{suffix}");
        let thread_key = legacy_session(&store, "uniqueness").await;
        let workspace = store
            .create_or_get_workspace(&thread_key)
            .await
            .expect("create workspace");

        sqlx::query(
            r#"
            insert into development_platform_events
                (platform, tenant_key, event_id, message_id, thread_key)
            values ('feishu', $1, $2, $3, $4)
            "#,
        )
        .bind(&tenant_key)
        .bind(&event_id)
        .bind(&first_message_id)
        .bind(thread_key.as_str())
        .execute(store.pool())
        .await
        .expect("insert platform event");
        let duplicate_event = sqlx::query(
            r#"
            insert into development_platform_events
                (platform, tenant_key, event_id, message_id, thread_key)
            values ('feishu', $1, $2, $3, $4)
            "#,
        )
        .bind(&tenant_key)
        .bind(&event_id)
        .bind(&second_message_id)
        .bind(thread_key.as_str())
        .execute(store.pool())
        .await;
        assert!(duplicate_event.is_err());

        let insert_repository = || {
            sqlx::query(
                r#"
                insert into session_repositories
                    (workspace_id, repository_id, gitlab_project_id,
                     display_name, path_with_namespace, default_branch,
                     clone_url, relative_path, state, added_by_principal_id)
                values ($1, 'gitlab:42', 42, 'project', 'group/project', 'main',
                        'http://git.example.internal:82/group/project.git',
                        'repos/42-project', 'pending', 'principal-1')
                "#,
            )
            .bind(&workspace.workspace_id)
        };
        insert_repository()
            .execute(store.pool())
            .await
            .expect("insert repository");
        assert!(insert_repository().execute(store.pool()).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accept_development_task_is_atomic_and_idempotent() {
        let Some(store) = test_store().await else {
            return;
        };
        let suffix = Uuid::new_v4();
        let event_id = format!("event-{suffix}");
        let message_id = format!("message-{suffix}");
        let request = AcceptDevelopmentTask {
            channel: DevelopmentChannel {
                platform: "feishu".to_owned(),
                tenant_key: format!("tenant-{suffix}"),
                conversation_key: format!("chat-{suffix}"),
                root_message_id: message_id.clone(),
            },
            platform_event_id: event_id,
            platform_message_id: Some(message_id.clone()),
            harness_type: HarnessType::Codex,
            initiator: DevelopmentInitiator {
                principal_id: "principal-1".to_owned(),
            },
            message: SessionMessageInput {
                client_message_id: Some(message_id),
                role: MessageRole::User,
                parts: vec![json!({"type": "text", "text": "Fix the failing test"})],
                metadata: json!({"source": "feishu"}),
            },
            session_metadata: json!({"source": "feishu"}),
        };

        let accepted = store
            .accept_development_task(&request)
            .await
            .expect("accept task");
        let replay = store
            .accept_development_task(&request)
            .await
            .expect("replay task");

        assert!(accepted.created);
        assert!(!replay.created);
        assert_eq!(accepted.thread_key, replay.thread_key);
        assert_eq!(accepted.workspace_id, replay.workspace_id);
        assert_eq!(accepted.selection_flow_id, replay.selection_flow_id);
        assert_eq!(accepted.execution_id, replay.execution_id);

        let counts = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            r#"
            select
                (select count(*) from sessions where thread_key = $1),
                (select count(*) from session_workspaces where thread_key = $1),
                (select count(*) from session_messages where thread_key = $1),
                (select count(*) from session_executions where thread_key = $1),
                (select count(*)
                   from development_selection_flows flow
                   join session_workspaces workspace using (workspace_id)
                  where workspace.thread_key = $1)
            "#,
        )
        .bind(accepted.thread_key.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count intake rows");
        assert_eq!(counts, (1, 1, 1, 1, 1));

        let execution = store
            .active_execution_for_thread(&accepted.thread_key)
            .await
            .expect("load execution")
            .expect("active execution");
        assert_eq!(
            execution.blocking_reason,
            Some(ExecutionBlocker::AwaitingProjectSelection)
        );
        assert!(
            !store
                .mark_execution_running(&accepted.execution_id)
                .await
                .expect("try to claim blocked execution")
                .claimed
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_development_task_receipts_create_one_intake() {
        let Some(store) = test_store().await else {
            return;
        };
        let request = development_task("concurrent-intake");
        let (first, second) = tokio::join!(
            store.accept_development_task(&request),
            store.accept_development_task(&request),
        );
        let first = first.expect("first intake");
        let second = second.expect("second intake");

        assert_ne!(first.created, second.created);
        assert_eq!(first.thread_key, second.thread_key);
        assert_eq!(first.workspace_id, second.workspace_id);
        assert_eq!(first.selection_flow_id, second.selection_flow_id);
        assert_eq!(first.execution_id, second.execution_id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirm_selection_binds_resolved_repositories_and_advances_blocker_once() {
        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("confirm-selection"))
            .await
            .expect("accept task");
        let request = ConfirmRepositorySelection {
            selection_flow_id: accepted.selection_flow_id.clone(),
            expected_version: 1,
            decided_by_principal_id: "principal-1".to_owned(),
            repositories: vec![resolved_repository(42), resolved_repository(84)],
        };

        let unauthorized = store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id.clone(),
                expected_version: 1,
                decided_by_principal_id: "principal-2".to_owned(),
                repositories: vec![resolved_repository(42)],
            })
            .await;
        assert!(matches!(
            unauthorized,
            Err(crate::SessionStoreError::DevelopmentForbidden { .. })
        ));

        let confirmed = store
            .confirm_repository_selection(&request)
            .await
            .expect("confirm selection");
        assert_eq!(confirmed.state, SelectionFlowState::Confirmed);
        assert_eq!(confirmed.version, 2);
        assert_eq!(confirmed.repository_ids.len(), 2);
        assert_eq!(confirmed.workspace_state, WorkspaceState::Provisioning);
        assert_eq!(
            confirmed.execution_blocker,
            Some(ExecutionBlocker::WorkspaceProvisioning)
        );

        let stale = store.confirm_repository_selection(&request).await;
        assert!(matches!(
            stale,
            Err(crate::SessionStoreError::DevelopmentConflict { .. })
        ));
        let repository_count = sqlx::query_scalar::<_, i64>(
            "select count(*) from session_repositories where workspace_id = $1",
        )
        .bind(&accepted.workspace_id)
        .fetch_one(store.pool())
        .await
        .expect("count repositories");
        assert_eq!(repository_count, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_project_confirms_empty_selection_and_cancel_terminalizes_execution() {
        let Some(store) = test_store().await else {
            return;
        };
        let no_project = store
            .accept_development_task(&development_task("no-project"))
            .await
            .expect("accept no-project task");
        let confirmed = store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: no_project.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: Vec::new(),
            })
            .await
            .expect("confirm no project");
        assert!(confirmed.repository_ids.is_empty());
        assert_eq!(confirmed.workspace_state, WorkspaceState::Provisioning);

        let cancelled = store
            .accept_development_task(&development_task("cancel-selection"))
            .await
            .expect("accept cancelled task");
        let cancellation = store
            .cancel_repository_selection(&cancelled.selection_flow_id, 1, "principal-1")
            .await
            .expect("cancel selection");
        assert_eq!(cancellation.state, SelectionFlowState::Cancelled);
        assert_eq!(cancellation.version, 2);
        let execution = store
            .latest_execution_for_thread(&cancelled.thread_key)
            .await
            .expect("load execution")
            .expect("cancelled execution");
        assert_eq!(
            execution.status,
            centaur_session_core::ExecutionStatus::Cancelled
        );
        assert!(execution.blocking_reason.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn add_repository_selection_requires_idle_ready_workspace_and_is_append_only() {
        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("add-project"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![resolved_repository(42)],
            })
            .await
            .expect("confirm initial repository");
        sqlx::query(
            "update session_executions set status = 'completed', blocking_reason = null, completed_at = now() where execution_id = $1",
        )
        .bind(&accepted.execution_id)
        .execute(store.pool())
        .await
        .expect("complete initial execution");
        sqlx::query("update session_workspaces set state = 'ready' where workspace_id = $1")
            .bind(&accepted.workspace_id)
            .execute(store.pool())
            .await
            .expect("mark workspace ready");

        let draft = store
            .create_add_repository_selection(&accepted.thread_key)
            .await
            .expect("create add-project selection");
        assert_eq!(draft.kind, SelectionKind::Add);
        assert_eq!(draft.state, SelectionFlowState::Pending);
        assert_eq!(draft.version, 1);
        assert_eq!(
            store
                .create_add_repository_selection(&accepted.thread_key)
                .await
                .expect("reuse pending add-project selection"),
            draft
        );

        let duplicate = store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: draft.selection_flow_id.clone(),
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![resolved_repository(42)],
            })
            .await;
        assert!(matches!(
            duplicate,
            Err(crate::SessionStoreError::DevelopmentConflict { .. })
        ));

        let added = store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: draft.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![resolved_repository(84)],
            })
            .await
            .expect("confirm added repository");
        assert_eq!(added.repository_ids, vec!["gitlab:84".parse().unwrap()]);
        assert_eq!(added.workspace_state, WorkspaceState::Provisioning);
        assert_eq!(added.execution_blocker, None);
    }

    #[tokio::test]
    async fn workspace_preparation_claim_and_completion_release_only_the_exact_execution() {
        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("workspace-preparation"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![resolved_repository(42)],
            })
            .await
            .expect("confirm repository");

        let claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "api-rs-test",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("claim preparation");
        assert_eq!(claim.workspace.preparation_attempt, 1);
        assert_eq!(
            claim.execution_id.as_deref(),
            Some(accepted.execution_id.as_str())
        );
        assert_eq!(claim.repositories.len(), 1);
        assert_eq!(claim.repositories[0].state.as_ref(), "provisioning");

        let stale = store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id.clone(),
                attempt: 0,
                lease_owner: "api-rs-test".to_owned(),
                storage_ref: "workspace-test".to_owned(),
                prepared: vec![PreparedRepositorySnapshot {
                    repository_id: "gitlab:42".parse().unwrap(),
                    base_sha: "a".repeat(40),
                    local_branch: "centaur/test".to_owned(),
                    head_sha: "a".repeat(40),
                }],
                failed: Vec::new(),
            })
            .await;
        assert!(matches!(
            stale,
            Err(crate::SessionStoreError::DevelopmentConflict { .. })
        ));

        let ready = store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: claim.workspace.preparation_attempt,
                lease_owner: "api-rs-test".to_owned(),
                storage_ref: "workspace-test".to_owned(),
                prepared: vec![PreparedRepositorySnapshot {
                    repository_id: "gitlab:42".parse().unwrap(),
                    base_sha: "a".repeat(40),
                    local_branch: "centaur/test".to_owned(),
                    head_sha: "a".repeat(40),
                }],
                failed: Vec::new(),
            })
            .await
            .expect("complete preparation");
        assert_eq!(ready.state, WorkspaceState::Ready);
        assert_eq!(ready.storage_ref.as_deref(), Some("workspace-test"));
        let execution = store
            .latest_execution_for_thread(&accepted.thread_key)
            .await
            .unwrap()
            .unwrap();
        assert!(execution.blocking_reason.is_none());
    }

    #[tokio::test]
    async fn failed_workspace_preparation_keeps_execution_blocked() {
        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("workspace-failure"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![resolved_repository(42)],
            })
            .await
            .expect("confirm repository");
        let claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "api-rs-test",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("claim preparation");
        let failed = store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: claim.workspace.preparation_attempt,
                lease_owner: "api-rs-test".to_owned(),
                storage_ref: "workspace-test".to_owned(),
                prepared: Vec::new(),
                failed: vec![FailedRepositorySnapshot {
                    repository_id: "gitlab:42".parse().unwrap(),
                    failure_code: "clone_failed".to_owned(),
                    failure_message: "clone failed".to_owned(),
                }],
            })
            .await
            .expect("record failure");
        assert_eq!(failed.state, WorkspaceState::Failed);
        let execution = store
            .latest_execution_for_thread(&accepted.thread_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            execution.blocking_reason,
            Some(ExecutionBlocker::WorkspaceProvisioning)
        );
    }

    #[tokio::test]
    async fn workspace_backend_failure_is_durable_without_repositories() {
        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("workspace-backend-failure"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: Vec::new(),
            })
            .await
            .expect("confirm without repositories");
        let claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "api-rs-test",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("claim preparation");

        let failed = store
            .fail_workspace_preparation(
                &accepted.workspace_id,
                claim.workspace.preparation_attempt,
                "api-rs-test",
                "workspace_backend_failed",
                "workspace preparation failed",
            )
            .await
            .expect("record backend failure");

        assert_eq!(failed.state, WorkspaceState::Failed);
        let execution = store
            .latest_execution_for_thread(&accepted.thread_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            execution.blocking_reason,
            Some(ExecutionBlocker::WorkspaceProvisioning)
        );
    }

    #[tokio::test]
    async fn changeset_collection_is_leased_and_persists_immutable_artifacts() {
        use centaur_session_core::development::{
            CollectedChangeSetRepositoryState, CompleteChangeSetCollection,
            CompleteChangeSetRepository,
        };
        use sha2::{Digest, Sha256};

        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("changeset-persist"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![resolved_repository(42)],
            })
            .await
            .expect("confirm repository");
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
                storage_ref: "workspace-test".to_owned(),
                prepared: vec![PreparedRepositorySnapshot {
                    repository_id: "gitlab:42".parse().unwrap(),
                    base_sha: base_sha.clone(),
                    local_branch: "centaur/test".to_owned(),
                    head_sha: base_sha.clone(),
                }],
                failed: Vec::new(),
            })
            .await
            .expect("ready workspace");
        store
            .complete_execution(&accepted.execution_id)
            .await
            .expect("complete execution");
        let collecting = store
            .begin_changeset_collection(&accepted.execution_id, "collector-a")
            .await
            .expect("begin collection")
            .expect("development execution has workspace");
        let duplicate = store
            .begin_changeset_collection(&accepted.execution_id, "collector-b")
            .await
            .expect("idempotent begin")
            .expect("same changeset");
        assert_eq!(collecting.changeset_id, duplicate.changeset_id);
        assert_eq!(collecting.state, ChangeSetState::Collecting);
        assert!(matches!(
            store
                .claim_changeset_collection(
                    &collecting.changeset_id,
                    "collector-b",
                    std::time::Duration::from_secs(30),
                )
                .await,
            Err(crate::SessionStoreError::DevelopmentConflict { .. })
        ));
        let claim = store
            .claim_changeset_collection(
                &collecting.changeset_id,
                "collector-a",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("renew owned collection");
        assert_eq!(claim.repositories.len(), 1);

        let head_sha = "b".repeat(40);
        let patch = b"diff --git a/README.md b/README.md\n".to_vec();
        let patch_hash = format!("sha256:{}", hex::encode(Sha256::digest(&patch)));
        let changed_repository = CompleteChangeSetRepository {
            repository_id: "gitlab:42".parse().unwrap(),
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
            test_evidence: json!([{"command": "cargo test", "status": "passed"}]),
            failure_code: None,
            failure_message: None,
        };
        let mut mismatched_repository = changed_repository.clone();
        mismatched_repository.base_sha = "c".repeat(40);
        assert!(matches!(
            store
                .complete_changeset_collection(&CompleteChangeSetCollection {
                    changeset_id: collecting.changeset_id.clone(),
                    lease_owner: "collector-a".to_owned(),
                    repositories: vec![mismatched_repository],
                })
                .await,
            Err(crate::SessionStoreError::DevelopmentConflict { .. })
        ));
        let completed = store
            .complete_changeset_collection(&CompleteChangeSetCollection {
                changeset_id: collecting.changeset_id.clone(),
                lease_owner: "collector-a".to_owned(),
                repositories: vec![changed_repository],
            })
            .await
            .expect("complete collection")
            .expect("changed workspace creates review");
        assert_eq!(completed.state, ChangeSetState::Ready);
        assert_eq!(completed.repositories.len(), 1);
        assert_eq!(
            completed.repositories[0].patch_hash.as_deref(),
            Some(patch_hash.as_str())
        );
        assert_eq!(
            completed.repositories[0].test_evidence,
            json!([{"command": "cargo test", "status": "passed"}])
        );
        let artifact_ref = completed.repositories[0]
            .patch_artifact_ref
            .as_deref()
            .unwrap();
        assert_eq!(
            store
                .get_changeset_artifact(
                    &completed.changeset_id,
                    artifact_ref,
                    "principal-1",
                    false,
                )
                .await
                .unwrap(),
            patch
        );
        assert!(matches!(
            store
                .get_changeset(&completed.changeset_id, "principal-2", false)
                .await,
            Err(crate::SessionStoreError::DevelopmentForbidden { .. })
        ));
        assert_eq!(
            store
                .workspace_for_session(&accepted.thread_key)
                .await
                .unwrap()
                .unwrap()
                .state,
            WorkspaceState::Ready
        );
    }

    #[tokio::test]
    async fn execution_completion_atomically_starts_collection_and_fences_next_execution() {
        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("changeset-atomic"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: Vec::new(),
            })
            .await
            .expect("confirm no repository");
        let workspace_claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "workspace-owner",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("claim workspace");
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: workspace_claim.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: "workspace-atomic".to_owned(),
                prepared: Vec::new(),
                failed: Vec::new(),
            })
            .await
            .expect("ready workspace");
        assert!(
            store
                .mark_execution_running(&accepted.execution_id)
                .await
                .expect("mark running")
                .claimed
        );
        assert!(
            store
                .claim_stdout_owner(
                    &accepted.execution_id,
                    "stdout-owner",
                    std::time::Duration::from_secs(30),
                )
                .await
                .expect("claim stdout")
        );

        let completed = store
            .complete_development_execution_and_begin_collection(
                &accepted.execution_id,
                "stdout-owner",
                "collector-owner",
            )
            .await
            .expect("complete and collect")
            .expect("owned execution completes");
        let changeset_id = completed
            .changeset_id
            .expect("development execution creates changeset");
        assert_eq!(completed.execution.status, ExecutionStatus::Completed);
        assert_eq!(
            store
                .get_changeset(&changeset_id, "principal-1", false)
                .await
                .expect("load changeset")
                .state,
            ChangeSetState::Collecting
        );
        assert_eq!(
            store
                .workspace_for_session(&accepted.thread_key)
                .await
                .unwrap()
                .unwrap()
                .state,
            WorkspaceState::Collecting
        );
        assert!(matches!(
            store
                .create_execution(&accepted.thread_key, None, json!({}))
                .await,
            Err(crate::SessionStoreError::DevelopmentConflict { .. })
        ));
    }

    #[tokio::test]
    async fn unchanged_collection_creates_no_review_and_releases_workspace() {
        use centaur_session_core::development::{
            CollectedChangeSetRepositoryState, CompleteChangeSetCollection,
            CompleteChangeSetRepository,
        };

        let Some(store) = test_store().await else {
            return;
        };
        let accepted = store
            .accept_development_task(&development_task("changeset-empty"))
            .await
            .expect("accept task");
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: Vec::new(),
            })
            .await
            .expect("confirm no repository");
        let workspace_claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "workspace-owner",
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("claim workspace");
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: workspace_claim.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: "workspace-empty".to_owned(),
                prepared: Vec::new(),
                failed: Vec::new(),
            })
            .await
            .expect("ready empty workspace");
        store
            .complete_execution(&accepted.execution_id)
            .await
            .expect("complete execution");
        let changeset = store
            .begin_changeset_collection(&accepted.execution_id, "collector")
            .await
            .unwrap()
            .unwrap();
        let completed = store
            .complete_changeset_collection(&CompleteChangeSetCollection {
                changeset_id: changeset.changeset_id.clone(),
                lease_owner: "collector".to_owned(),
                repositories: Vec::<CompleteChangeSetRepository>::new(),
            })
            .await
            .expect("complete empty collection");
        assert!(completed.is_none());
        assert!(matches!(
            store
                .get_changeset(&changeset.changeset_id, "principal-1", false)
                .await,
            Err(crate::SessionStoreError::DevelopmentNotFound { .. })
        ));
        assert_eq!(
            store
                .workspace_for_session(&accepted.thread_key)
                .await
                .unwrap()
                .unwrap()
                .state,
            WorkspaceState::Ready
        );
        let _ = CollectedChangeSetRepositoryState::Unchanged;
    }

    fn development_task(label: &str) -> AcceptDevelopmentTask {
        let suffix = Uuid::new_v4();
        let message_id = format!("message-{label}-{suffix}");
        AcceptDevelopmentTask {
            channel: DevelopmentChannel {
                platform: "feishu".to_owned(),
                tenant_key: format!("tenant-{suffix}"),
                conversation_key: format!("chat-{suffix}"),
                root_message_id: message_id.clone(),
            },
            platform_event_id: format!("event-{label}-{suffix}"),
            platform_message_id: Some(message_id.clone()),
            harness_type: HarnessType::Codex,
            initiator: DevelopmentInitiator {
                principal_id: "principal-1".to_owned(),
            },
            message: SessionMessageInput {
                client_message_id: Some(message_id),
                role: MessageRole::User,
                parts: vec![json!({"type": "text", "text": "Fix the failing test"})],
                metadata: json!({"source": "feishu"}),
            },
            session_metadata: json!({"source": "feishu"}),
        }
    }

    fn resolved_repository(project_id: u64) -> ResolvedRepository {
        ResolvedRepository {
            repository_id: format!("gitlab:{project_id}").parse().unwrap(),
            display_name: format!("project-{project_id}"),
            path_with_namespace: format!("group/project-{project_id}"),
            default_branch: "main".to_owned(),
            clone_url: format!("http://git.example.internal:82/group/project-{project_id}.git"),
            relative_path: format!("repos/{project_id}-project-{project_id}"),
        }
    }
}
