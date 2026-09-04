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

#[derive(Debug, Default)]
struct SessionEventRetentionReport {
    deleted_events: u64,
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

    /// Delete session events past the retention window.
    ///
    /// `session_events` carries one row per harness stdout line. Bounded sweeps
    /// keep the initial backlog from monopolizing a database connection or
    /// creating one large delete transaction.
    async fn sweep_once(&self) -> Result<SessionEventRetentionReport, SessionStoreError> {
        let Some(cutoff) = SystemTime::now().checked_sub(self.config.retention) else {
            return Ok(SessionEventRetentionReport::default());
        };
        let mut report = SessionEventRetentionReport::default();
        for _ in 0..EVENT_RETENTION_MAX_BATCHES {
            let deleted = self
                .store
                .delete_events_older_than(cutoff, EVENT_RETENTION_BATCH_ROWS)
                .await?;
            report.deleted_events += deleted;
            if deleted < EVENT_RETENTION_BATCH_ROWS as u64 {
                break;
            }
        }
        if report.deleted_events > 0 {
            info!(
                component = crate::COMPONENT_SESSION_RUNTIME,
                event = "session_events_expired",
                deleted = report.deleted_events,
                retention_secs = self.config.retention.as_secs(),
                "deleted session events past the retention window"
            );
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sweep_is_bounded_so_a_large_backlog_drains_over_several() {
        assert_eq!(
            EVENT_RETENTION_BATCH_ROWS * EVENT_RETENTION_MAX_BATCHES as i64,
            100_000
        );
    }
}
