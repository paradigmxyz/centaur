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
