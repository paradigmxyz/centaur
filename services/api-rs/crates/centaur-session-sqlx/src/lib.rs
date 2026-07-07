//! SQLx-backed session repository.

use std::{str::FromStr, time::Duration};

use centaur_session_core::{
    ExecutionStatus, HarnessType, MessageRole, SandboxCapabilities, Session, SessionEvent,
    SessionExecution, SessionMessage, SessionMessageInput, SessionStatus, ThreadKey, empty_object,
};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{
    FromRow, PgPool,
    postgres::{PgListener, PgPoolOptions},
};
use thiserror::Error;
use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

// The API binary embeds these migrations at compile time.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub const SESSION_EVENTS_CHANNEL: &str = "centaur_session_events";
const DEFAULT_MAX_CONNECTIONS: u32 = 500;

#[derive(Clone, Debug)]
pub struct CreateExecutionResult {
    pub execution: SessionExecution,
    pub created: bool,
}

#[derive(Clone, Debug)]
pub struct ClaimExecutionResult {
    pub execution: SessionExecution,
    /// True only when this call transitioned the execution from `queued` to
    /// `running`. False means another request already claimed it (or it is
    /// terminal), so the caller must not drive the execution.
    pub claimed: bool,
}

/// An active execution whose stdout-owner lease was released by
/// [`PgSessionStore::release_stdout_owned_executions`].
#[derive(Clone, Debug)]
pub struct ReleasedExecution {
    pub execution_id: String,
    pub thread_key: ThreadKey,
}

/// An active execution together with its stdout-owner lease state, as
/// returned by [`PgSessionStore::list_active_executions_with_ownership`].
/// The lease snapshot is advisory — only the conditional
/// `claim_expired_stdout_owner` update decides ownership — but it lets an
/// adoption scan skip executions with a live owner without touching the
/// session row or the sandbox backend.
#[derive(Clone, Debug)]
pub struct ActiveExecutionOwnership {
    pub execution: SessionExecution,
    pub stdout_owner_id: Option<String>,
    /// True when a stdout-owner lease exists and has not expired yet.
    pub stdout_owner_lease_active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdleSandboxCandidate {
    pub thread_key: ThreadKey,
    pub sandbox_id: String,
    pub execution_id: String,
    pub idle_timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxCapacityCandidate {
    pub thread_key: ThreadKey,
    pub sandbox_id: String,
    pub latest_execution_id: Option<String>,
    pub last_active_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowOwnedSandbox {
    pub thread_key: ThreadKey,
    pub sandbox_id: String,
}

#[derive(Clone)]
pub struct PgSessionStore {
    pool: PgPool,
}

impl PgSessionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> Result<Self, SessionStoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(DEFAULT_MAX_CONNECTIONS)
            .connect(database_url)
            .await?;
        Ok(Self::new(pool))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn run_migrations(&self) -> Result<(), SessionStoreError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    pub async fn listen_session_events(&self) -> Result<SessionEventListener, SessionStoreError> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen(SESSION_EVENTS_CHANNEL).await?;
        Ok(SessionEventListener { listener })
    }

    pub async fn create_or_get_session(
        &self,
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
        persona_id: Option<&str>,
        metadata: Value,
    ) -> Result<Session, SessionStoreError> {
        sqlx::query(
            r#"
            insert into sessions (thread_key, harness_type, persona_id, status, metadata)
            values ($1, $2, $3, $4, $5)
            on conflict (thread_key) do nothing
            "#,
        )
        .bind(thread_key.as_str())
        .bind(harness_type.as_ref())
        .bind(persona_id)
        .bind(SessionStatus::Idle.as_ref())
        .bind(metadata)
        .execute(&self.pool)
        .await?;

        let session = self.get_session(thread_key).await?;
        if session.harness_type != *harness_type {
            return Err(SessionStoreError::HarnessConflict {
                thread_key: thread_key.as_str().to_owned(),
                existing: session.harness_type.to_string(),
                requested: harness_type.as_ref().to_owned(),
            });
        }
        if session.persona_id.as_deref() != persona_id {
            return Err(SessionStoreError::PersonaConflict {
                thread_key: thread_key.as_str().to_owned(),
                existing: session.persona_id,
                requested: persona_id.map(str::to_owned),
            });
        }
        Ok(session)
    }

    pub async fn get_session(&self, thread_key: &ThreadKey) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            select thread_key, title, sandbox_id, sandbox_repo_cache_enabled, sandbox_observability_enabled, sandbox_api_server_enabled, harness_type, harness_thread_id, persona_id, status, iron_control_principal, sandbox_last_active_at, created_at, updated_at
            from sessions
            where thread_key = $1
            "#,
        )
        .bind(thread_key.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| SessionStoreError::NotFound {
            thread_key: thread_key.as_str().to_owned(),
        })?;

        row.try_into()
    }

    pub async fn get_session_title(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<String>, SessionStoreError> {
        let title = sqlx::query_scalar::<_, Option<String>>(
            r#"
            select title
            from sessions
            where thread_key = $1
            "#,
        )
        .bind(thread_key.as_str())
        .fetch_optional(&self.pool)
        .await?
        .flatten();

        Ok(title)
    }

    pub async fn append_messages(
        &self,
        thread_key: &ThreadKey,
        messages: &[SessionMessageInput],
    ) -> Result<Vec<String>, SessionStoreError> {
        let mut tx = self.pool.begin().await?;
        let mut message_ids = Vec::with_capacity(messages.len());

        for message in messages {
            let message_id = prefixed_id("msg");
            let parts = Value::Array(message.parts.clone());
            let persisted_message_id = sqlx::query_scalar::<_, String>(
                r#"
                insert into session_messages
                    (message_id, thread_key, client_message_id, role, parts, metadata)
                values ($1, $2, $3, $4, $5, $6)
                on conflict (thread_key, client_message_id)
                    where client_message_id is not null
                do update set client_message_id = excluded.client_message_id
                returning message_id
                "#,
            )
            .bind(&message_id)
            .bind(thread_key.as_str())
            .bind(message.client_message_id.as_deref())
            .bind(message.role.as_ref())
            .bind(parts)
            .bind(message.metadata.clone())
            .fetch_one(&mut *tx)
            .await?;
            message_ids.push(persisted_message_id);
        }

        tx.commit().await?;
        Ok(message_ids)
    }

    pub async fn title_generation_candidate(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<Vec<Value>>, SessionStoreError> {
        let rows = sqlx::query_scalar::<_, Value>(
            r#"
            select m.parts
            from sessions s
            join session_messages m on m.thread_key = s.thread_key
            where s.thread_key = $1 and s.title is null
                and m.role = $2
            order by m.created_at, m.message_id
            "#,
        )
        .bind(thread_key.as_str())
        .bind(MessageRole::User.as_ref())
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let parts = rows
            .into_iter()
            .flat_map(|parts| match parts {
                Value::Array(parts) => parts,
                other => vec![other],
            })
            .collect();
        Ok(Some(parts))
    }

    pub async fn set_session_title_if_empty(
        &self,
        thread_key: &ThreadKey,
        title: &str,
    ) -> Result<bool, SessionStoreError> {
        let result = sqlx::query(
            r#"
            update sessions
            set title = $2, updated_at = now()
            where thread_key = $1 and title is null
            "#,
        )
        .bind(thread_key.as_str())
        .bind(title)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_messages(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Vec<SessionMessage>, SessionStoreError> {
        let rows = sqlx::query_as::<_, SessionMessageRow>(
            r#"
            select message_id, client_message_id, thread_key, role, parts, metadata, created_at
            from session_messages
            where thread_key = $1
            order by created_at, message_id
            "#,
        )
        .bind(thread_key.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn create_execution(
        &self,
        thread_key: &ThreadKey,
        idempotency_key: Option<&str>,
        metadata: Value,
    ) -> Result<CreateExecutionResult, SessionStoreError> {
        let execution_id = prefixed_id("exe");
        let row = sqlx::query_as::<_, CreateExecutionRow>(
            r#"
            insert into session_executions
                (execution_id, thread_key, idempotency_key, status, metadata)
            values ($1, $2, $3, $4, $5)
            on conflict (thread_key, idempotency_key)
                where idempotency_key is not null
            do update set idempotency_key = excluded.idempotency_key
            returning
                execution_id = $1 as created,
                execution_id,
                idempotency_key,
                thread_key,
                status,
                metadata,
                error,
                created_at,
                updated_at,
                started_at,
                completed_at
            "#,
        )
        .bind(&execution_id)
        .bind(thread_key.as_str())
        .bind(idempotency_key)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn active_execution_for_thread(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            select execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            from session_executions
            where thread_key = $1 and status in ($2, $3)
            order by created_at desc, execution_id desc
            limit 1
            "#,
        )
        .bind(thread_key.as_str())
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    /// Lists every execution still marked queued or running. Used at startup
    /// to adopt executions orphaned by a previous control plane process.
    pub async fn list_active_executions(&self) -> Result<Vec<SessionExecution>, SessionStoreError> {
        let rows = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            select execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            from session_executions
            where status in ($1, $2)
            order by created_at, execution_id
            "#,
        )
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn list_active_executions_with_ownership(
        &self,
    ) -> Result<Vec<ActiveExecutionOwnership>, SessionStoreError> {
        let rows = sqlx::query_as::<_, ActiveExecutionOwnershipRow>(
            r#"
            select execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at,
                   stdout_owner_id,
                   coalesce(stdout_owner_lease_expires_at > now(), false) as stdout_owner_lease_active
            from session_executions
            where status in ($1, $2)
            order by created_at, execution_id
            "#,
        )
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(ActiveExecutionOwnership {
                    execution: row.execution.try_into()?,
                    stdout_owner_id: row.stdout_owner_id,
                    stdout_owner_lease_active: row.stdout_owner_lease_active,
                })
            })
            .collect()
    }

    pub async fn latest_execution_for_thread(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            select execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            from session_executions
            where thread_key = $1
            order by created_at desc, execution_id desc
            limit 1
            "#,
        )
        .bind(thread_key.as_str())
        .fetch_optional(&self.pool)
        .await?;

        row.map(TryInto::try_into).transpose()
    }

    pub async fn mark_execution_running(
        &self,
        execution_id: &str,
    ) -> Result<ClaimExecutionResult, SessionStoreError> {
        let maybe_row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2, started_at = coalesce(started_at, now()), updated_at = now()
            where execution_id = $1 and status = $3
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Running.as_ref())
        .bind(ExecutionStatus::Queued.as_ref())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = maybe_row else {
            // The execution was not queued: a concurrent request already
            // claimed it or it reached a terminal state. Report the current
            // row without taking ownership.
            let row = sqlx::query_as::<_, SessionExecutionRow>(
                r#"
                select execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
                from session_executions
                where execution_id = $1
                "#,
            )
            .bind(execution_id)
            .fetch_one(&self.pool)
            .await?;
            return Ok(ClaimExecutionResult {
                execution: row.try_into()?,
                claimed: false,
            });
        };

        self.set_session_status(&row.thread_key, SessionStatus::Executing)
            .await?;
        Ok(ClaimExecutionResult {
            execution: row.try_into()?,
            claimed: true,
        })
    }

    pub async fn claim_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, SessionStoreError> {
        let lease_expires_at = stdout_lease_expires_at(lease);
        let result = sqlx::query(
            r#"
            update session_executions
            set stdout_owner_id = $2,
                stdout_owner_lease_expires_at = $3,
                updated_at = now()
            where execution_id = $1
              and status in ($4, $5)
              and (
                stdout_owner_id is null
                or stdout_owner_id = $2
                or stdout_owner_lease_expires_at < now()
              )
            "#,
        )
        .bind(execution_id)
        .bind(owner_id)
        .bind(lease_expires_at)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn claim_expired_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, SessionStoreError> {
        let lease_expires_at = stdout_lease_expires_at(lease);
        let result = sqlx::query(
            r#"
            update session_executions
            set stdout_owner_id = $2,
                stdout_owner_lease_expires_at = $3,
                updated_at = now()
            where execution_id = $1
              and status in ($4, $5)
              and (
                stdout_owner_id is null
                or stdout_owner_lease_expires_at < now()
              )
            "#,
        )
        .bind(execution_id)
        .bind(owner_id)
        .bind(lease_expires_at)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn renew_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
        lease: Duration,
    ) -> Result<bool, SessionStoreError> {
        let lease_expires_at = stdout_lease_expires_at(lease);
        let result = sqlx::query(
            r#"
            update session_executions
            set stdout_owner_lease_expires_at = $3,
                updated_at = now()
            where execution_id = $1
              and stdout_owner_id = $2
              and status in ($4, $5)
            "#,
        )
        .bind(execution_id)
        .bind(owner_id)
        .bind(lease_expires_at)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn release_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
    ) -> Result<bool, SessionStoreError> {
        let result = sqlx::query(
            r#"
            update session_executions
            set stdout_owner_id = null,
                stdout_owner_lease_expires_at = null,
                updated_at = now()
            where execution_id = $1 and stdout_owner_id = $2
            "#,
        )
        .bind(execution_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn count_executions_with_stdout_owner(
        &self,
        owner_id: &str,
    ) -> Result<u64, SessionStoreError> {
        let count = sqlx::query_scalar::<_, i64>(
            r#"
            select count(*)
            from session_executions
            where stdout_owner_id = $1 and status in ($2, $3)
            "#,
        )
        .bind(owner_id)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_one(&self.pool)
        .await?;

        Ok(u64::try_from(count).unwrap_or_default())
    }

    /// Releases every active stdout-owner lease held by `owner_id` in one
    /// statement, returning the affected executions. Used by a clean
    /// control-plane shutdown so a peer's adoption scan can claim the
    /// executions immediately instead of waiting out the lease TTL.
    pub async fn release_stdout_owned_executions(
        &self,
        owner_id: &str,
    ) -> Result<Vec<ReleasedExecution>, SessionStoreError> {
        let rows = sqlx::query_as::<_, (String, String)>(
            r#"
            update session_executions
            set stdout_owner_id = null,
                stdout_owner_lease_expires_at = null,
                updated_at = now()
            where stdout_owner_id = $1 and status in ($2, $3)
            returning execution_id, thread_key
            "#,
        )
        .bind(owner_id)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|(execution_id, thread_key)| {
                Ok(ReleasedExecution {
                    execution_id,
                    thread_key: parse_persisted(thread_key)?,
                })
            })
            .collect()
    }

    pub async fn complete_execution(
        &self,
        execution_id: &str,
    ) -> Result<SessionExecution, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2, completed_at = coalesce(completed_at, now()), updated_at = now()
            where execution_id = $1
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Completed.as_ref())
        .fetch_one(&self.pool)
        .await?;

        self.set_session_status(&row.thread_key, SessionStatus::Idle)
            .await?;
        row.try_into()
    }

    pub async fn complete_execution_if_active(
        &self,
        execution_id: &str,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2, completed_at = coalesce(completed_at, now()), updated_at = now()
            where execution_id = $1 and status in ($3, $4)
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Completed.as_ref())
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        self.set_session_status(&row.thread_key, SessionStatus::Idle)
            .await?;
        row.try_into().map(Some)
    }

    pub async fn complete_execution_if_active_and_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2,
                completed_at = coalesce(completed_at, now()),
                stdout_owner_id = null,
                stdout_owner_lease_expires_at = null,
                updated_at = now()
            where execution_id = $1
              and status in ($3, $4)
              and stdout_owner_id = $5
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Completed.as_ref())
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        self.set_session_status(&row.thread_key, SessionStatus::Idle)
            .await?;
        row.try_into().map(Some)
    }

    pub async fn fail_execution(
        &self,
        execution_id: &str,
        error: &str,
    ) -> Result<SessionExecution, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2, error = $3, completed_at = coalesce(completed_at, now()), updated_at = now()
            where execution_id = $1
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Failed.as_ref())
        .bind(error)
        .fetch_one(&self.pool)
        .await?;

        self.set_session_status(&row.thread_key, SessionStatus::Failed)
            .await?;
        row.try_into()
    }

    pub async fn fail_execution_if_active(
        &self,
        execution_id: &str,
        error: &str,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2, error = $3, completed_at = coalesce(completed_at, now()), updated_at = now()
            where execution_id = $1 and status in ($4, $5)
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Failed.as_ref())
        .bind(error)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        self.set_session_status(&row.thread_key, SessionStatus::Failed)
            .await?;
        row.try_into().map(Some)
    }

    pub async fn fail_execution_if_active_and_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
        error: &str,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2,
                error = $3,
                completed_at = coalesce(completed_at, now()),
                stdout_owner_id = null,
                stdout_owner_lease_expires_at = null,
                updated_at = now()
            where execution_id = $1
              and status in ($4, $5)
              and stdout_owner_id = $6
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Failed.as_ref())
        .bind(error)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        self.set_session_status(&row.thread_key, SessionStatus::Failed)
            .await?;
        row.try_into().map(Some)
    }

    pub async fn cancel_execution_if_active_and_stdout_owner(
        &self,
        execution_id: &str,
        owner_id: &str,
        reason: &str,
    ) -> Result<Option<SessionExecution>, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2,
                error = $3,
                completed_at = coalesce(completed_at, now()),
                stdout_owner_id = null,
                stdout_owner_lease_expires_at = null,
                updated_at = now()
            where execution_id = $1
              and status in ($4, $5)
              and stdout_owner_id = $6
            returning execution_id, idempotency_key, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Cancelled.as_ref())
        .bind(reason)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        self.set_session_status(&row.thread_key, SessionStatus::Idle)
            .await?;
        row.try_into().map(Some)
    }

    pub async fn append_event(
        &self,
        thread_key: &ThreadKey,
        execution_id: Option<&str>,
        event_type: &str,
        payload: Value,
    ) -> Result<SessionEvent, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionEventRow>(
            r#"
            insert into session_events (thread_key, execution_id, event_type, payload)
            values ($1, $2, $3, $4)
            returning event_id, thread_key, execution_id, event_type, payload, created_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(execution_id)
        .bind(event_type)
        .bind(payload)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn append_event_if_stdout_owner(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
        owner_id: &str,
        lease: Duration,
        event_type: &str,
        payload: Value,
    ) -> Result<Option<SessionEvent>, SessionStoreError> {
        let lease_expires_at = stdout_lease_expires_at(lease);
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            update session_executions
            set stdout_owner_lease_expires_at = $3,
                updated_at = now()
            where execution_id = $1
              and stdout_owner_id = $2
              and status in ($4, $5)
              and thread_key = $6
            "#,
        )
        .bind(execution_id)
        .bind(owner_id)
        .bind(lease_expires_at)
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(ExecutionStatus::Running.as_ref())
        .bind(thread_key.as_str())
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }

        let row = sqlx::query_as::<_, SessionEventRow>(
            r#"
            insert into session_events (thread_key, execution_id, event_type, payload)
            values ($1, $2, $3, $4)
            returning event_id, thread_key, execution_id, event_type, payload, created_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(execution_id)
        .bind(event_type)
        .bind(payload)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        row.try_into().map(Some)
    }

    pub async fn list_events_after(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
        execution_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SessionEvent>, SessionStoreError> {
        let rows = sqlx::query_as::<_, SessionEventRow>(
            r#"
            select event_id, thread_key, execution_id, event_type, payload, created_at
            from session_events
            where thread_key = $1
              and event_id > $2
              and ($3::text is null or execution_id = $3)
            order by event_id
            limit $4
            "#,
        )
        .bind(thread_key.as_str())
        .bind(after_event_id)
        .bind(execution_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn execution_event_exists(
        &self,
        execution_id: &str,
        event_type: &str,
    ) -> Result<bool, SessionStoreError> {
        let exists = sqlx::query_scalar::<_, bool>(
            r#"
            select exists (
                select 1
                from session_events
                where execution_id = $1
                  and event_type = $2
                limit 1
            )
            "#,
        )
        .bind(execution_id)
        .bind(event_type)
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    pub async fn list_referenced_sandbox_ids(&self) -> Result<Vec<String>, SessionStoreError> {
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            select sandbox_id
            from sessions
            where sandbox_id is not null

            union

            select sandbox_id
            from session_warm_sandboxes
            where status in ('ready', 'claimed', 'evicting')
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows)
    }

    pub async fn list_idle_sandbox_candidates(
        &self,
        idle_backstop: Duration,
    ) -> Result<Vec<IdleSandboxCandidate>, SessionStoreError> {
        let rows = sqlx::query_as::<_, IdleSandboxCandidateRow>(
            r#"
            with latest as (
                select distinct on (thread_key)
                    execution_id,
                    thread_key,
                    status,
                    completed_at,
                    metadata
                from session_executions
                order by thread_key, created_at desc, execution_id desc
            )
            select
                s.thread_key,
                s.sandbox_id as sandbox_id,
                latest.execution_id,
                latest.completed_at,
                latest.metadata
            from sessions s
            join latest on latest.thread_key = s.thread_key
            where s.sandbox_id is not null
              and latest.status in ('completed', 'failed', 'cancelled')
              and latest.completed_at is not null
              and not exists (
                  select 1
                  from session_executions active
                  where active.thread_key = s.thread_key
                    and active.status in ('queued', 'running')
              )
            order by latest.completed_at, s.thread_key
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let now = OffsetDateTime::now_utc();
        rows.into_iter()
            .filter_map(|row| idle_candidate_from_row(row, idle_backstop, now).transpose())
            .collect()
    }

    pub async fn list_sandbox_capacity_candidates(
        &self,
        excluded_thread_key: Option<&ThreadKey>,
        hot_idle_grace: std::time::Duration,
        limit: i64,
    ) -> Result<Vec<SandboxCapacityCandidate>, SessionStoreError> {
        let rows = sqlx::query_as::<_, SandboxCapacityCandidateRow>(
            r#"
            with latest as (
                select distinct on (thread_key)
                    execution_id,
                    thread_key,
                    completed_at
                from session_executions
                order by thread_key, created_at desc, execution_id desc
            )
            select
                s.thread_key,
                s.sandbox_id as sandbox_id,
                latest.execution_id as latest_execution_id,
                coalesce(
                    s.sandbox_last_active_at,
                    latest.completed_at,
                    s.updated_at,
                    s.created_at
                ) as last_active_at
            from sessions s
            left join latest on latest.thread_key = s.thread_key
            where s.sandbox_id is not null
              and ($1::text is null or s.thread_key != $1)
              and not exists (
                  select 1
                  from lateral (
                      select e.event_type
                      from session_events e
                      where e.thread_key = s.thread_key
                        and e.payload->>'sandbox_id' = s.sandbox_id
                        and e.event_type in (
                            'session.sandbox_paused',
                            'session.sandbox_ready',
                            'session.sandbox_resumed'
                        )
                      order by e.created_at desc, e.event_id desc
                      limit 1
                  ) latest_sandbox_event
                  where latest_sandbox_event.event_type = 'session.sandbox_paused'
              )
              and coalesce(
                    s.sandbox_last_active_at,
                    latest.completed_at,
                    s.updated_at,
                    s.created_at
                  ) <= now() - ($2::float8 * interval '1 second')
              and not exists (
                  select 1
                  from session_executions active
                  where active.thread_key = s.thread_key
                    and active.status in ('queued', 'running')
              )
            order by last_active_at, s.thread_key
            limit $3
            "#,
        )
        .bind(excluded_thread_key.map(ThreadKey::as_str))
        .bind(hot_idle_grace.as_secs_f64())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn list_workflow_owned_sandboxes(
        &self,
        workflow_run_id: &str,
    ) -> Result<Vec<WorkflowOwnedSandbox>, SessionStoreError> {
        let rows = sqlx::query_as::<_, WorkflowOwnedSandboxRow>(
            r#"
            select thread_key, sandbox_id as sandbox_id
            from sessions
            where sandbox_id is not null
              and metadata->>'workflow_owned_thread' = 'true'
              and metadata->>'workflow_run_id' = $1
            order by thread_key
            "#,
        )
        .bind(workflow_run_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub async fn update_sandbox_id(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: Option<&str>,
    ) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            update sessions
            set
                sandbox_id = $2,
                sandbox_repo_cache_enabled = null,
                sandbox_observability_enabled = null,
                sandbox_api_server_enabled = null,
                sandbox_last_active_at = case
                    when $2::text is null then null
                    else now()
                end,
                updated_at = now()
            where thread_key = $1
            returning thread_key, title, sandbox_id, sandbox_repo_cache_enabled, sandbox_observability_enabled, sandbox_api_server_enabled, harness_type, harness_thread_id, persona_id, status, iron_control_principal, sandbox_last_active_at, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(sandbox_id)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn update_sandbox_assignment(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: &str,
        capabilities: &SandboxCapabilities,
    ) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            update sessions
            set
                sandbox_id = $2,
                sandbox_repo_cache_enabled = $3,
                sandbox_observability_enabled = $4,
                sandbox_api_server_enabled = $5,
                sandbox_last_active_at = now(),
                updated_at = now()
            where thread_key = $1
            returning thread_key, title, sandbox_id, sandbox_repo_cache_enabled, sandbox_observability_enabled, sandbox_api_server_enabled, harness_type, harness_thread_id, persona_id, status, iron_control_principal, sandbox_last_active_at, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(sandbox_id)
        .bind(capabilities.repo_cache_enabled)
        .bind(capabilities.observability_enabled)
        .bind(capabilities.api_server_enabled)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn clear_sandbox_id_if_matches(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: &str,
    ) -> Result<bool, SessionStoreError> {
        let result = sqlx::query(
            r#"
            update sessions
            set
                sandbox_id = null,
                sandbox_repo_cache_enabled = null,
                sandbox_observability_enabled = null,
                sandbox_api_server_enabled = null,
                sandbox_last_active_at = null,
                updated_at = now()
            where thread_key = $1 and sandbox_id = $2
            "#,
        )
        .bind(thread_key.as_str())
        .bind(sandbox_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Move an existing session onto a different harness. Clears the sandbox
    /// and harness thread state (they belong to the old harness) and resets
    /// the session to idle; messages and events are preserved.
    pub async fn switch_session_harness(
        &self,
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
    ) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            update sessions
            set harness_type = $2,
                harness_thread_id = null,
                sandbox_id = null,
                sandbox_repo_cache_enabled = null,
                sandbox_observability_enabled = null,
                sandbox_api_server_enabled = null,
                sandbox_last_active_at = null,
                status = $3,
                updated_at = now()
            where thread_key = $1
            returning thread_key, title, sandbox_id, sandbox_repo_cache_enabled, sandbox_observability_enabled, sandbox_api_server_enabled, harness_type, harness_thread_id, persona_id, status, iron_control_principal, sandbox_last_active_at, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(harness_type.as_ref())
        .bind(SessionStatus::Idle.as_ref())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| SessionStoreError::NotFound {
            thread_key: thread_key.as_str().to_owned(),
        })?;

        row.try_into()
    }

    pub async fn set_iron_control_principal(
        &self,
        thread_key: &ThreadKey,
        iron_control_principal: Option<&str>,
    ) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            update sessions
            set iron_control_principal = $2, updated_at = now()
            where thread_key = $1
            returning thread_key, title, sandbox_id, sandbox_repo_cache_enabled, sandbox_observability_enabled, sandbox_api_server_enabled, harness_type, harness_thread_id, persona_id, status, iron_control_principal, sandbox_last_active_at, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(iron_control_principal)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn insert_ready_warm_sandbox(
        &self,
        sandbox_id: &str,
        workload_key: &str,
    ) -> Result<(), SessionStoreError> {
        sqlx::query(
            r#"
            insert into session_warm_sandboxes (sandbox_id, workload_key, status)
            values ($1, $2, 'ready')
            "#,
        )
        .bind(sandbox_id)
        .bind(workload_key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn count_ready_warm_sandboxes(
        &self,
        workload_key: &str,
    ) -> Result<i64, SessionStoreError> {
        let count = sqlx::query_scalar::<_, i64>(
            r#"
            select count(*)::bigint
            from session_warm_sandboxes
            where workload_key = $1 and status = 'ready'
            "#,
        )
        .bind(workload_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    pub async fn list_ready_warm_sandbox_ids(&self) -> Result<Vec<String>, SessionStoreError> {
        let sandbox_ids = sqlx::query_scalar::<_, String>(
            r#"
            select sandbox_id
            from session_warm_sandboxes
            where status = 'ready'
            order by created_at, sandbox_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(sandbox_ids)
    }

    pub async fn claim_ready_warm_sandbox(
        &self,
        workload_key: &str,
        thread_key: &str,
    ) -> Result<Option<String>, SessionStoreError> {
        let sandbox_id = sqlx::query_scalar::<_, String>(
            r#"
            with candidate as (
                select sandbox_id
                from session_warm_sandboxes
                where workload_key = $1 and status = 'ready'
                order by created_at, sandbox_id
                for update skip locked
                limit 1
            )
            update session_warm_sandboxes warm
            set
                status = 'claimed',
                claimed_thread_key = $2,
                claimed_at = now(),
                updated_at = now()
            from candidate
            where warm.sandbox_id = candidate.sandbox_id
            returning warm.sandbox_id
            "#,
        )
        .bind(workload_key)
        .bind(thread_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(sandbox_id)
    }

    pub async fn reserve_ready_warm_sandboxes_for_eviction(
        &self,
        limit: i64,
    ) -> Result<Vec<String>, SessionStoreError> {
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            with candidates as (
                select sandbox_id
                from session_warm_sandboxes
                where status = 'ready'
                order by created_at, sandbox_id
                for update skip locked
                limit $1
            )
            update session_warm_sandboxes warm
            set
                status = 'evicting',
                updated_at = now()
            from candidates
            where warm.sandbox_id = candidates.sandbox_id
            returning warm.sandbox_id
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn list_stale_evicting_warm_sandbox_ids(
        &self,
        min_age: Duration,
    ) -> Result<Vec<String>, SessionStoreError> {
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            select sandbox_id
            from session_warm_sandboxes
            where status = 'evicting'
              and updated_at <= now() - ($1::float8 * interval '1 second')
            order by updated_at, sandbox_id
            "#,
        )
        .bind(min_age.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn mark_warm_sandbox_failed(
        &self,
        sandbox_id: &str,
        error: &str,
    ) -> Result<(), SessionStoreError> {
        sqlx::query(
            r#"
            update session_warm_sandboxes
            set status = 'failed', last_error = $2, updated_at = now()
            where sandbox_id = $1
            "#,
        )
        .bind(sandbox_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_harness_thread_id(
        &self,
        thread_key: &ThreadKey,
        harness_thread_id: Option<&str>,
    ) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            update sessions
            set harness_thread_id = $2, updated_at = now()
            where thread_key = $1
            returning thread_key, title, sandbox_id, sandbox_repo_cache_enabled, sandbox_observability_enabled, sandbox_api_server_enabled, harness_type, harness_thread_id, persona_id, status, iron_control_principal, sandbox_last_active_at, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(harness_thread_id)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn touch_session_sandbox_activity(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<bool, SessionStoreError> {
        let result = sqlx::query(
            r#"
            update sessions
            set sandbox_last_active_at = now()
            where thread_key = $1 and sandbox_id is not null
            "#,
        )
        .bind(thread_key.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn touch_sandbox_activity(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: &str,
    ) -> Result<bool, SessionStoreError> {
        let result = sqlx::query(
            r#"
            update sessions
            set sandbox_last_active_at = now()
            where thread_key = $1 and sandbox_id = $2
            "#,
        )
        .bind(thread_key.as_str())
        .bind(sandbox_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    async fn set_session_status(
        &self,
        thread_key: &str,
        status: SessionStatus,
    ) -> Result<(), SessionStoreError> {
        sqlx::query(
            r#"
            update sessions
            set status = $2, updated_at = now()
            where thread_key = $1
            "#,
        )
        .bind(thread_key)
        .bind(status.as_ref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

pub struct SessionEventListener {
    listener: PgListener,
}

impl SessionEventListener {
    pub async fn recv(&mut self) -> Result<SessionEventNotification, SessionStoreError> {
        loop {
            let notification = self.listener.recv().await?;
            if notification.channel() != SESSION_EVENTS_CHANNEL {
                continue;
            }

            let payload = notification.payload();
            return serde_json::from_str(payload).map_err(|error| {
                SessionStoreError::InvalidNotification {
                    channel: notification.channel().to_owned(),
                    payload: payload.to_owned(),
                    error,
                }
            });
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct SessionEventNotification {
    pub thread_key: String,
    pub event_id: i64,
}

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("session not found for thread_key {thread_key}")]
    NotFound { thread_key: String },
    #[error(
        "session {thread_key} already exists with harness_type {existing}, requested {requested}"
    )]
    HarnessConflict {
        thread_key: String,
        existing: String,
        requested: String,
    },
    #[error(
        "session {thread_key} already exists with persona_id {existing:?}, requested {requested:?}"
    )]
    PersonaConflict {
        thread_key: String,
        existing: Option<String>,
        requested: Option<String>,
    },
    #[error("invalid persisted value: {0}")]
    InvalidPersistedValue(String),
    #[error("invalid notification payload on {channel}: {payload}: {error}")]
    InvalidNotification {
        channel: String,
        payload: String,
        error: serde_json::Error,
    },
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

#[derive(Debug, FromRow)]
struct SessionRow {
    thread_key: String,
    title: Option<String>,
    sandbox_id: Option<String>,
    sandbox_repo_cache_enabled: Option<bool>,
    sandbox_observability_enabled: Option<bool>,
    sandbox_api_server_enabled: Option<bool>,
    harness_type: String,
    harness_thread_id: Option<String>,
    persona_id: Option<String>,
    status: String,
    iron_control_principal: Option<String>,
    sandbox_last_active_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<SessionRow> for Session {
    type Error = SessionStoreError;

    fn try_from(row: SessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            thread_key: parse_persisted(row.thread_key)?,
            title: row.title,
            sandbox_id: row.sandbox_id,
            sandbox_capabilities: match (
                row.sandbox_repo_cache_enabled,
                row.sandbox_observability_enabled,
                row.sandbox_api_server_enabled,
            ) {
                (
                    Some(repo_cache_enabled),
                    Some(observability_enabled),
                    Some(api_server_enabled),
                ) => Some(SandboxCapabilities {
                    repo_cache_enabled,
                    observability_enabled,
                    api_server_enabled,
                }),
                _ => None,
            },
            harness_type: parse_persisted(row.harness_type)?,
            harness_thread_id: row.harness_thread_id,
            persona_id: row.persona_id,
            status: parse_persisted(row.status)?,
            iron_control_principal: row.iron_control_principal,
            sandbox_last_active_at: row.sandbox_last_active_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct SessionMessageRow {
    message_id: String,
    client_message_id: Option<String>,
    thread_key: String,
    role: String,
    parts: Value,
    metadata: Value,
    created_at: OffsetDateTime,
}

impl TryFrom<SessionMessageRow> for SessionMessage {
    type Error = SessionStoreError;

    fn try_from(row: SessionMessageRow) -> Result<Self, Self::Error> {
        let parts = match row.parts {
            Value::Array(parts) => parts,
            other => vec![other],
        };
        Ok(Self {
            message_id: row.message_id,
            client_message_id: row.client_message_id,
            thread_key: parse_persisted(row.thread_key)?,
            role: parse_persisted(row.role)?,
            parts,
            metadata: row.metadata,
            created_at: row.created_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct SessionExecutionRow {
    execution_id: String,
    idempotency_key: Option<String>,
    thread_key: String,
    status: String,
    metadata: Value,
    error: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    completed_at: Option<OffsetDateTime>,
}

#[derive(Debug, FromRow)]
struct ActiveExecutionOwnershipRow {
    #[sqlx(flatten)]
    execution: SessionExecutionRow,
    stdout_owner_id: Option<String>,
    stdout_owner_lease_active: bool,
}

#[derive(Debug, FromRow)]
struct IdleSandboxCandidateRow {
    thread_key: String,
    sandbox_id: String,
    execution_id: String,
    completed_at: OffsetDateTime,
    metadata: Value,
}

fn idle_candidate_from_row(
    row: IdleSandboxCandidateRow,
    idle_backstop: Duration,
    now: OffsetDateTime,
) -> Result<Option<IdleSandboxCandidate>, SessionStoreError> {
    let idle_timeout = effective_idle_timeout(&row.metadata, idle_backstop);
    if !idle_deadline_elapsed(row.completed_at, idle_timeout, now) {
        return Ok(None);
    }
    Ok(Some(IdleSandboxCandidate {
        thread_key: parse_persisted(row.thread_key)?,
        sandbox_id: row.sandbox_id,
        execution_id: row.execution_id,
        idle_timeout,
    }))
}

fn effective_idle_timeout(metadata: &Value, idle_backstop: Duration) -> Duration {
    metadata
        .get("idle_timeout_ms")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| std::cmp::max(idle_backstop, Duration::from_millis(1)))
}

fn idle_deadline_elapsed(
    completed_at: OffsetDateTime,
    idle_timeout: Duration,
    now: OffsetDateTime,
) -> bool {
    let elapsed = now - completed_at;
    if elapsed.is_negative() {
        return false;
    }
    elapsed.whole_nanoseconds() >= idle_timeout.as_nanos() as i128
}

#[derive(Debug, FromRow)]
struct SandboxCapacityCandidateRow {
    thread_key: String,
    sandbox_id: String,
    latest_execution_id: Option<String>,
    last_active_at: OffsetDateTime,
}

impl TryFrom<SandboxCapacityCandidateRow> for SandboxCapacityCandidate {
    type Error = SessionStoreError;

    fn try_from(row: SandboxCapacityCandidateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            thread_key: parse_persisted(row.thread_key)?,
            sandbox_id: row.sandbox_id,
            latest_execution_id: row.latest_execution_id,
            last_active_at: row.last_active_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct WorkflowOwnedSandboxRow {
    thread_key: String,
    sandbox_id: String,
}

impl TryFrom<WorkflowOwnedSandboxRow> for WorkflowOwnedSandbox {
    type Error = SessionStoreError;

    fn try_from(row: WorkflowOwnedSandboxRow) -> Result<Self, Self::Error> {
        Ok(Self {
            thread_key: parse_persisted(row.thread_key)?,
            sandbox_id: row.sandbox_id,
        })
    }
}

impl TryFrom<SessionExecutionRow> for SessionExecution {
    type Error = SessionStoreError;

    fn try_from(row: SessionExecutionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            execution_id: row.execution_id,
            idempotency_key: row.idempotency_key,
            thread_key: parse_persisted(row.thread_key)?,
            status: parse_persisted(row.status)?,
            metadata: row.metadata,
            error: row.error,
            created_at: row.created_at,
            updated_at: row.updated_at,
            started_at: row.started_at,
            completed_at: row.completed_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct CreateExecutionRow {
    created: bool,
    execution_id: String,
    idempotency_key: Option<String>,
    thread_key: String,
    status: String,
    metadata: Value,
    error: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    started_at: Option<OffsetDateTime>,
    completed_at: Option<OffsetDateTime>,
}

impl TryFrom<CreateExecutionRow> for CreateExecutionResult {
    type Error = SessionStoreError;

    fn try_from(row: CreateExecutionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            created: row.created,
            execution: SessionExecutionRow {
                execution_id: row.execution_id,
                idempotency_key: row.idempotency_key,
                thread_key: row.thread_key,
                status: row.status,
                metadata: row.metadata,
                error: row.error,
                created_at: row.created_at,
                updated_at: row.updated_at,
                started_at: row.started_at,
                completed_at: row.completed_at,
            }
            .try_into()?,
        })
    }
}

#[derive(Debug, FromRow)]
struct SessionEventRow {
    event_id: i64,
    thread_key: String,
    execution_id: Option<String>,
    event_type: String,
    payload: Value,
    created_at: OffsetDateTime,
}

impl TryFrom<SessionEventRow> for SessionEvent {
    type Error = SessionStoreError;

    fn try_from(row: SessionEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            event_id: row.event_id,
            thread_key: parse_persisted(row.thread_key)?,
            execution_id: row.execution_id,
            event_type: row.event_type,
            payload: row.payload,
            created_at: row.created_at,
        })
    }
}

fn parse_persisted<T>(value: String) -> Result<T, SessionStoreError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|err: T::Err| SessionStoreError::InvalidPersistedValue(err.to_string()))
}

fn prefixed_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

pub fn default_metadata(metadata: Option<Value>) -> Value {
    metadata.unwrap_or_else(empty_object)
}

fn stdout_lease_expires_at(lease: Duration) -> OffsetDateTime {
    let seconds = i64::try_from(lease.as_secs()).unwrap_or(i64::MAX);
    OffsetDateTime::now_utc() + TimeDuration::new(seconds, lease.subsec_nanos() as i32)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use centaur_session_core::{HarnessType, ThreadKey};
    use serde_json::json;
    use time::{Duration as TimeDuration, OffsetDateTime};
    use uuid::Uuid;

    use super::{IdleSandboxCandidateRow, PgSessionStore, SessionEventNotification};

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

    #[test]
    fn parses_session_event_notification_payload() {
        let notification: SessionEventNotification =
            serde_json::from_str(r#"{"thread_key":"cli:test","event_id":42}"#).unwrap();

        assert_eq!(
            notification,
            SessionEventNotification {
                thread_key: "cli:test".to_owned(),
                event_id: 42,
            }
        );
    }

    fn idle_row(
        metadata: serde_json::Value,
        completed_at: OffsetDateTime,
    ) -> IdleSandboxCandidateRow {
        IdleSandboxCandidateRow {
            thread_key: "test:idle-row".to_owned(),
            sandbox_id: "sbx-idle-row".to_owned(),
            execution_id: "exe-idle-row".to_owned(),
            completed_at,
            metadata,
        }
    }

    #[test]
    fn idle_candidate_uses_persisted_timeout_deadline() {
        let now = OffsetDateTime::now_utc();
        let candidate = super::idle_candidate_from_row(
            idle_row(
                json!({"idle_timeout_ms": 1000}),
                now - TimeDuration::seconds(2),
            ),
            Duration::from_secs(3600),
            now,
        )
        .unwrap()
        .expect("candidate should use persisted timeout");

        assert_eq!(candidate.idle_timeout, Duration::from_secs(1));
    }

    #[test]
    fn idle_candidate_waits_for_persisted_timeout_even_when_backstop_elapsed() {
        let now = OffsetDateTime::now_utc();
        let candidate = super::idle_candidate_from_row(
            idle_row(
                json!({"idle_timeout_ms": 10_000}),
                now - TimeDuration::seconds(2),
            ),
            Duration::from_secs(1),
            now,
        )
        .unwrap();

        assert!(candidate.is_none());
    }

    #[test]
    fn idle_candidate_falls_back_to_backstop_for_missing_or_invalid_timeout() {
        let now = OffsetDateTime::now_utc();
        let candidate = super::idle_candidate_from_row(
            idle_row(
                json!({"idle_timeout_ms": "not-a-number"}),
                now - TimeDuration::seconds(2),
            ),
            Duration::from_secs(1),
            now,
        )
        .unwrap()
        .expect("candidate should use backstop");

        assert_eq!(candidate.idle_timeout, Duration::from_secs(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_candidates_use_persisted_execution_idle_timeout() {
        let Some(store) = test_store().await else {
            return;
        };
        let thread_key = ThreadKey::parse(format!("test:idle-cleanup-{}", Uuid::new_v4())).unwrap();
        let sandbox_id = format!("sbx-idle-{}", Uuid::new_v4());
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        store
            .update_sandbox_id(&thread_key, Some(&sandbox_id))
            .await
            .expect("set sandbox id");
        let execution_id = store
            .create_execution(&thread_key, None, json!({"idle_timeout_ms": 1000}))
            .await
            .expect("create execution")
            .execution
            .execution_id;
        store
            .complete_execution(&execution_id)
            .await
            .expect("complete execution");
        sqlx::query(
            r#"
            update session_executions
            set completed_at = now() - interval '2 seconds', updated_at = now()
            where execution_id = $1
            "#,
        )
        .bind(&execution_id)
        .execute(store.pool())
        .await
        .expect("age execution");

        let candidates = store
            .list_idle_sandbox_candidates(Duration::from_secs(3600))
            .await
            .expect("list idle sandbox candidates");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.thread_key == thread_key)
            .expect("candidate should use execution idle timeout, not backstop");

        assert_eq!(candidate.sandbox_id, sandbox_id);
        assert_eq!(candidate.execution_id, execution_id);
        assert_eq!(candidate.idle_timeout, Duration::from_secs(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdout_owner_fences_output_and_terminal_updates() {
        let Some(store) = test_store().await else {
            return;
        };
        let thread_key = ThreadKey::parse(format!("test:stdout-owner-{}", Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create session");
        let execution_id = store
            .create_execution(&thread_key, None, json!({}))
            .await
            .expect("create execution")
            .execution
            .execution_id;
        store
            .mark_execution_running(&execution_id)
            .await
            .expect("mark running");

        assert!(
            store
                .claim_stdout_owner(&execution_id, "owner-a", Duration::from_millis(25))
                .await
                .expect("owner-a claims stdout")
        );
        assert!(
            store
                .append_event_if_stdout_owner(
                    &thread_key,
                    &execution_id,
                    "owner-a",
                    Duration::from_millis(25),
                    "session.output.line",
                    json!("line-from-owner-a"),
                )
                .await
                .expect("owner-a appends")
                .is_some()
        );
        assert!(
            store
                .append_event_if_stdout_owner(
                    &thread_key,
                    &execution_id,
                    "owner-b",
                    Duration::from_millis(25),
                    "session.output.line",
                    json!("line-from-stale-owner-b"),
                )
                .await
                .expect("owner-b append is fenced")
                .is_none()
        );
        assert!(
            store
                .complete_execution_if_active_and_stdout_owner(&execution_id, "owner-b")
                .await
                .expect("owner-b terminal update is fenced")
                .is_none()
        );

        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            store
                .claim_expired_stdout_owner(&execution_id, "owner-b", Duration::from_secs(5))
                .await
                .expect("owner-b claims after lease expiry")
        );
        assert!(
            store
                .append_event_if_stdout_owner(
                    &thread_key,
                    &execution_id,
                    "owner-a",
                    Duration::from_secs(5),
                    "session.output.line",
                    json!("line-from-expired-owner-a"),
                )
                .await
                .expect("expired owner-a append is fenced")
                .is_none()
        );
        let completed = store
            .complete_execution_if_active_and_stdout_owner(&execution_id, "owner-b")
            .await
            .expect("owner-b completes")
            .expect("completion should be recorded");
        assert_eq!(
            completed.status,
            centaur_session_core::ExecutionStatus::Completed
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn releases_all_stdout_leases_held_by_one_owner() {
        let Some(store) = test_store().await else {
            return;
        };
        let owner = format!("owner-{}", Uuid::new_v4().simple());
        let peer = format!("peer-{}", Uuid::new_v4().simple());
        let mut owned = Vec::new();
        for label in ["a", "b"] {
            let thread_key =
                ThreadKey::parse(format!("test:handoff-{label}-{}", Uuid::new_v4())).unwrap();
            store
                .create_or_get_session(&thread_key, &HarnessType::Codex, None, json!({}))
                .await
                .expect("create session");
            let execution_id = store
                .create_execution(&thread_key, None, json!({}))
                .await
                .expect("create execution")
                .execution
                .execution_id;
            store
                .mark_execution_running(&execution_id)
                .await
                .expect("mark running");
            assert!(
                store
                    .claim_stdout_owner(&execution_id, &owner, Duration::from_secs(60))
                    .await
                    .expect("claim stdout owner")
            );
            owned.push((execution_id, thread_key));
        }
        // A bystander owner's lease must survive the release untouched.
        let bystander_thread =
            ThreadKey::parse(format!("test:handoff-bystander-{}", Uuid::new_v4())).unwrap();
        store
            .create_or_get_session(&bystander_thread, &HarnessType::Codex, None, json!({}))
            .await
            .expect("create bystander session");
        let bystander_execution = store
            .create_execution(&bystander_thread, None, json!({}))
            .await
            .expect("create bystander execution")
            .execution
            .execution_id;
        store
            .mark_execution_running(&bystander_execution)
            .await
            .expect("mark bystander running");
        let bystander = format!("bystander-{}", Uuid::new_v4().simple());
        assert!(
            store
                .claim_stdout_owner(&bystander_execution, &bystander, Duration::from_secs(60))
                .await
                .expect("claim bystander lease")
        );
        assert_eq!(
            store
                .count_executions_with_stdout_owner(&owner)
                .await
                .expect("count owned"),
            2
        );

        let released = store
            .release_stdout_owned_executions(&owner)
            .await
            .expect("release owned leases");
        assert_eq!(released.len(), 2);
        for (execution_id, thread_key) in &owned {
            assert!(
                released.iter().any(|execution| {
                    execution.execution_id == *execution_id && execution.thread_key == *thread_key
                }),
                "released set must include {execution_id}"
            );
        }
        assert_eq!(
            store
                .count_executions_with_stdout_owner(&owner)
                .await
                .expect("count after release"),
            0
        );

        // Released leases are immediately claimable by a peer, without
        // waiting for expiry.
        assert!(
            store
                .claim_stdout_owner(&owned[0].0, &peer, Duration::from_secs(60))
                .await
                .expect("peer claims released lease")
        );

        assert_eq!(
            store
                .count_executions_with_stdout_owner(&bystander)
                .await
                .expect("count bystander"),
            1,
            "release must be scoped to the requested owner"
        );
        store
            .fail_execution_if_active(&bystander_execution, "test cleanup")
            .await
            .expect("terminalize bystander");

        // Terminal executions are never part of a release, even if a lease
        // column is still populated.
        for (execution_id, _) in &owned {
            store
                .fail_execution_if_active(execution_id, "test cleanup")
                .await
                .expect("terminalize execution");
        }
        assert!(
            store
                .release_stdout_owned_executions(&peer)
                .await
                .expect("release for peer")
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn warm_eviction_reservation_blocks_later_claims() {
        let Some(store) = test_store().await else {
            return;
        };
        let sandbox_id = format!("sbx-warm-evict-{}", Uuid::new_v4());
        let workload_key = format!("workload-warm-evict-{}", Uuid::new_v4());
        store
            .insert_ready_warm_sandbox(&sandbox_id, &workload_key)
            .await
            .expect("insert warm sandbox");
        sqlx::query(
            r#"
            update session_warm_sandboxes
            set created_at = now() - interval '100 years'
            where sandbox_id = $1
            "#,
        )
        .bind(&sandbox_id)
        .execute(store.pool())
        .await
        .expect("age warm sandbox");

        let reserved = store
            .reserve_ready_warm_sandboxes_for_eviction(1)
            .await
            .expect("reserve warm sandbox");

        assert_eq!(reserved, vec![sandbox_id.clone()]);
        assert_eq!(
            store
                .claim_ready_warm_sandbox(&workload_key, "test-thread")
                .await
                .expect("claim after reservation"),
            None
        );
        assert!(
            store
                .list_referenced_sandbox_ids()
                .await
                .expect("list referenced sandboxes")
                .contains(&sandbox_id)
        );

        store
            .mark_warm_sandbox_failed(&sandbox_id, "test cleanup")
            .await
            .expect("mark reserved warm sandbox failed");
        assert!(
            !store
                .list_referenced_sandbox_ids()
                .await
                .expect("list referenced sandboxes")
                .contains(&sandbox_id)
        );
    }
}
