use centaur_session_core::{
    ThreadKey,
    development::{ExecutionBlocker, SessionWorkspace, WorkspaceState},
};
use serde_json::Value;
use sqlx::FromRow;
use time::OffsetDateTime;

use crate::{
    CreateExecutionResult, CreateExecutionRow, PgSessionStore, SessionStoreError, prefixed_id,
};

impl PgSessionStore {
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
        HarnessType, ThreadKey,
        development::{ExecutionBlocker, WorkspaceState},
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
        let thread_key = legacy_session(&store, "uniqueness").await;
        let workspace = store
            .create_or_get_workspace(&thread_key)
            .await
            .expect("create workspace");

        sqlx::query(
            r#"
            insert into development_platform_events
                (platform, tenant_key, event_id, message_id, thread_key)
            values ('feishu', 'tenant-1', 'event-1', 'message-1', $1)
            "#,
        )
        .bind(thread_key.as_str())
        .execute(store.pool())
        .await
        .expect("insert platform event");
        let duplicate_event = sqlx::query(
            r#"
            insert into development_platform_events
                (platform, tenant_key, event_id, message_id, thread_key)
            values ('feishu', 'tenant-1', 'event-1', 'message-2', $1)
            "#,
        )
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
}
