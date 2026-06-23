//! SQLx-backed session repository.

use std::str::FromStr;

use centaur_session_core::{
    ExecutionStatus, HarnessType, Session, SessionEvent, SessionExecution, SessionMessage,
    SessionMessageInput, SessionStatus, ThreadKey, empty_object,
};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{
    FromRow, PgPool,
    postgres::{PgListener, PgPoolOptions},
};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

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
            select thread_key, sandbox_id, harness_type, harness_thread_id, persona_id, status, iron_control_principal, created_at, updated_at
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

    pub async fn update_sandbox_id(
        &self,
        thread_key: &ThreadKey,
        sandbox_id: Option<&str>,
    ) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            update sessions
            set sandbox_id = $2, updated_at = now()
            where thread_key = $1
            returning thread_key, sandbox_id, harness_type, harness_thread_id, persona_id, status, iron_control_principal, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(sandbox_id)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
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
                status = $3,
                updated_at = now()
            where thread_key = $1
            returning thread_key, sandbox_id, harness_type, harness_thread_id, persona_id, status, iron_control_principal, created_at, updated_at
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
            returning thread_key, sandbox_id, harness_type, harness_thread_id, persona_id, status, iron_control_principal, created_at, updated_at
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
            returning thread_key, sandbox_id, harness_type, harness_thread_id, persona_id, status, iron_control_principal, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(harness_thread_id)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
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
    sandbox_id: Option<String>,
    harness_type: String,
    harness_thread_id: Option<String>,
    persona_id: Option<String>,
    status: String,
    iron_control_principal: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<SessionRow> for Session {
    type Error = SessionStoreError;

    fn try_from(row: SessionRow) -> Result<Self, Self::Error> {
        Ok(Self {
            thread_key: parse_persisted(row.thread_key)?,
            sandbox_id: row.sandbox_id,
            harness_type: parse_persisted(row.harness_type)?,
            harness_thread_id: row.harness_thread_id,
            persona_id: row.persona_id,
            status: parse_persisted(row.status)?,
            iron_control_principal: row.iron_control_principal,
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

#[cfg(test)]
mod tests {
    use super::SessionEventNotification;

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
}
