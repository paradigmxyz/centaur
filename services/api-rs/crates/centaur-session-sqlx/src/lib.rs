//! SQLx-backed session repository.

use std::str::FromStr;

use centaur_session_core::{
    ExecutionStatus, HarnessType, Session, SessionEvent, SessionExecution, SessionMessage,
    SessionMessageInput, SessionStatus, ThreadKey, empty_object,
};
use serde_json::Value;
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

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
            .max_connections(50)
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

    pub async fn create_or_get_session(
        &self,
        thread_key: &ThreadKey,
        harness_type: &HarnessType,
        metadata: Value,
    ) -> Result<Session, SessionStoreError> {
        sqlx::query(
            r#"
            insert into sessions (thread_key, harness_type, status, metadata)
            values ($1, $2, $3, $4)
            on conflict (thread_key) do nothing
            "#,
        )
        .bind(thread_key.as_str())
        .bind(harness_type.as_ref())
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
        Ok(session)
    }

    pub async fn get_session(&self, thread_key: &ThreadKey) -> Result<Session, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            select thread_key, sandbox_id, harness_type, harness_thread_id, status, created_at, updated_at
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
            sqlx::query(
                r#"
                insert into session_messages (message_id, thread_key, role, parts, metadata)
                values ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(&message_id)
            .bind(thread_key.as_str())
            .bind(message.role.as_ref())
            .bind(parts)
            .bind(message.metadata.clone())
            .execute(&mut *tx)
            .await?;
            message_ids.push(message_id);
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
            select message_id, thread_key, role, parts, metadata, created_at
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
        metadata: Value,
    ) -> Result<SessionExecution, SessionStoreError> {
        let execution_id = prefixed_id("exe");
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            insert into session_executions (execution_id, thread_key, status, metadata)
            values ($1, $2, $3, $4)
            returning execution_id, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(&execution_id)
        .bind(thread_key.as_str())
        .bind(ExecutionStatus::Queued.as_ref())
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn mark_execution_running(
        &self,
        execution_id: &str,
    ) -> Result<SessionExecution, SessionStoreError> {
        let row = sqlx::query_as::<_, SessionExecutionRow>(
            r#"
            update session_executions
            set status = $2, started_at = coalesce(started_at, now()), updated_at = now()
            where execution_id = $1
            returning execution_id, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
            "#,
        )
        .bind(execution_id)
        .bind(ExecutionStatus::Running.as_ref())
        .fetch_one(&self.pool)
        .await?;

        self.set_session_status(&row.thread_key, SessionStatus::Executing)
            .await?;
        row.try_into()
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
            returning execution_id, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
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
            returning execution_id, thread_key, status, metadata, error, created_at, updated_at, started_at, completed_at
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
        limit: i64,
    ) -> Result<Vec<SessionEvent>, SessionStoreError> {
        let rows = sqlx::query_as::<_, SessionEventRow>(
            r#"
            select event_id, thread_key, execution_id, event_type, payload, created_at
            from session_events
            where thread_key = $1 and event_id > $2
            order by event_id
            limit $3
            "#,
        )
        .bind(thread_key.as_str())
        .bind(after_event_id)
        .bind(limit)
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
            set sandbox_id = $2, updated_at = now()
            where thread_key = $1
            returning thread_key, sandbox_id, harness_type, harness_thread_id, status, created_at, updated_at
            "#,
        )
        .bind(thread_key.as_str())
        .bind(sandbox_id)
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
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
            returning thread_key, sandbox_id, harness_type, harness_thread_id, status, created_at, updated_at
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
    #[error("invalid persisted value: {0}")]
    InvalidPersistedValue(String),
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
    status: String,
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
            status: parse_persisted(row.status)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct SessionMessageRow {
    message_id: String,
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
