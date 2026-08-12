use std::collections::HashSet;

use centaur_session_core::{
    ExecutionStatus, MessageRole, SessionStatus, ThreadKey,
    development::{
        AcceptDevelopmentTask, AcceptedDevelopmentTask, ConfirmRepositorySelection,
        ExecutionBlocker, RepositorySelectionDraft, RepositorySelectionOutcome, SelectionFlowState,
        SelectionKind, SessionWorkspace, WorkspaceState,
    },
};
use serde_json::{Value, json};
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
               set state = 'provisioning', updated_at = now()
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
        HarnessType, MessageRole, SessionMessageInput, ThreadKey,
        development::{
            AcceptDevelopmentTask, ConfirmRepositorySelection, DevelopmentChannel,
            DevelopmentInitiator, ExecutionBlocker, ResolvedRepository, SelectionFlowState,
            SelectionKind, WorkspaceState,
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
