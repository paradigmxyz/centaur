use std::time::{Duration, SystemTime};

use centaur_session_sqlx::{PgSessionStore, SessionStoreError};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

/// Rows per delete statement, and the most statements one sweep will issue.
/// Together they bound a sweep at 100k rows, which drains a large backlog over
/// successive sweeps rather than in one long-running transaction.
const EVENT_RETENTION_BATCH_ROWS: i64 = 5_000;
const EVENT_RETENTION_MAX_BATCHES: usize = 20;

#[derive(Clone, Copy, Debug)]
pub struct SessionEventRetentionConfig {
    pub interval: Duration,
    pub retention: Duration,
}

pub(crate) struct SessionEventRetentionWorker {
    store: PgSessionStore,
    config: SessionEventRetentionConfig,
}

impl SessionEventRetentionWorker {
    pub(crate) fn new(store: PgSessionStore, config: SessionEventRetentionConfig) -> Self {
        Self { store, config }
    }

    pub(crate) fn spawn(self) {
        tokio::spawn(async move {
            let mut tick = interval(self.config.interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = self.sweep_once().await {
                    warn!(%error, "session event retention sweep failed");
                }
            }
        });
    }

    /// Delete stdout output-line events past the retention window.
    ///
    /// `session.output.line` carries one row per harness stdout line. Bounded sweeps
    /// keep the initial backlog from monopolizing a database connection or
    /// creating one large delete transaction.
    async fn sweep_once(&self) -> Result<u64, SessionStoreError> {
        if !self.store.stdout_retention_index_is_valid().await? {
            warn!(
                component = crate::COMPONENT_SESSION_RUNTIME,
                event = "session_events_retention_skipped",
                index = "session_events_stdout_created_at_idx",
                "skipping retention: create or repair the stdout retention index using the manual migration; the index is missing or invalid"
            );
            return Ok(0);
        }
        let Some(cutoff) = SystemTime::now().checked_sub(self.config.retention) else {
            return Ok(0);
        };
        let mut deleted_events = 0;
        for _ in 0..EVENT_RETENTION_MAX_BATCHES {
            let deleted = self
                .store
                .delete_stdout_events_older_than(cutoff, EVENT_RETENTION_BATCH_ROWS)
                .await?;
            deleted_events += deleted;
            if deleted < EVENT_RETENTION_BATCH_ROWS as u64 {
                break;
            }
        }
        if deleted_events > 0 {
            info!(
                component = crate::COMPONENT_SESSION_RUNTIME,
                event = "session_events_expired",
                deleted = deleted_events,
                retention_secs = self.config.retention.as_secs(),
                "deleted stdout output-line events past the retention window"
            );
        }
        Ok(deleted_events)
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, str::FromStr};

    use centaur_session_core::{HarnessType, ThreadKey};
    use serde_json::json;
    use sqlx::{PgPool, postgres::PgConnectOptions};
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn retention_skips_missing_or_invalid_index_and_resumes_after_repair()
    -> Result<(), Box<dyn Error>> {
        let Ok(database_url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return Ok(());
        };
        let admin = PgPool::connect(&database_url).await?;
        let database = format!("retention_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("create database {database}"))
            .execute(&admin)
            .await?;
        let options = PgConnectOptions::from_str(&database_url)?.database(&database);
        let pool = PgPool::connect_with(options).await?;
        let store = PgSessionStore::new(pool.clone());
        let result = check_index_recovery(store).await;
        pool.close().await;
        sqlx::query(&format!("drop database {database}"))
            .execute(&admin)
            .await?;
        admin.close().await;
        result
    }

    async fn check_index_recovery(store: PgSessionStore) -> Result<(), Box<dyn Error>> {
        store.run_migrations().await?;
        let thread_key = ThreadKey::parse("test:retention-index")?;
        store
            .create_or_get_session(
                &thread_key,
                &HarnessType::Codex,
                None,
                json!({}),
                Default::default(),
            )
            .await?;
        let output = store
            .append_event(&thread_key, None, "session.output.line", json!("output"))
            .await?;
        let terminal = store
            .append_event(&thread_key, None, "session.execution_completed", json!({}))
            .await?;
        sqlx::query("update session_events set created_at = now() - interval '2 days'")
            .execute(store.pool())
            .await?;
        let worker = SessionEventRetentionWorker::new(
            store.clone(),
            SessionEventRetentionConfig {
                interval: Duration::from_secs(300),
                retention: Duration::from_secs(24 * 60 * 60),
            },
        );

        // Startup migrations succeed without the optional index. Retention
        // leaves even eligible output intact until the operator installs it.
        assert_eq!(worker.sweep_once().await?, 0);
        assert_eq!(
            store
                .list_events_after(&thread_key, 0, None, 10)
                .await?
                .len(),
            2
        );

        // Hold a writer open so the concurrent build creates its catalog entry
        // and then times out waiting for that writer, leaving a real invalid index.
        let mut writer = store.pool().begin().await?;
        sqlx::query("update session_events set payload = payload where event_id = $1")
            .bind(output.event_id)
            .execute(&mut *writer)
            .await?;
        let mut builder = store.pool().acquire().await?;
        sqlx::query("set statement_timeout = '100ms'")
            .execute(&mut *builder)
            .await?;
        let failure = sqlx::raw_sql(include_str!(
            "../../centaur-session-sqlx/manual-migrations/session_events_retention_index.sql"
        ))
        .execute(&mut *builder)
        .await
        .expect_err("concurrent index build should time out behind the writer");
        writer.rollback().await?;
        sqlx::query("set statement_timeout = 0")
            .execute(&mut *builder)
            .await?;
        assert_eq!(
            failure
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("57014")
        );
        let valid: bool = sqlx::query_scalar(
            "select indisvalid from pg_index where indexrelid = 'session_events_stdout_created_at_idx'::regclass",
        )
        .fetch_one(&mut *builder)
        .await?;
        assert!(!valid);
        assert_eq!(worker.sweep_once().await?, 0);
        assert_eq!(
            store
                .list_events_after(&thread_key, 0, None, 10)
                .await?
                .len(),
            2
        );

        // Use the documented operator recovery and the same worker instance.
        sqlx::raw_sql("reindex index concurrently session_events_stdout_created_at_idx")
            .execute(&mut *builder)
            .await?;
        assert_eq!(worker.sweep_once().await?, 1);
        let remaining = store.list_events_after(&thread_key, 0, None, 10).await?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_id, terminal.event_id);

        // Index readiness must be rechecked, not cached after a successful sweep.
        sqlx::raw_sql("drop index concurrently session_events_stdout_created_at_idx")
            .execute(&mut *builder)
            .await?;
        store
            .append_event(
                &thread_key,
                None,
                "session.output.line",
                json!("more output"),
            )
            .await?;
        sqlx::query("update session_events set created_at = now() - interval '2 days'")
            .execute(store.pool())
            .await?;
        assert_eq!(worker.sweep_once().await?, 0);
        assert_eq!(
            store
                .list_events_after(&thread_key, 0, None, 10)
                .await?
                .len(),
            2
        );
        Ok(())
    }
}
