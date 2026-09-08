use std::time::Duration;

use centaur_session_sqlx::{PgSessionStore, SessionStoreError};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

/// Rows per delete statement, and the most statements one sweep will issue.
/// Together they bound a sweep at 100k rows, so a large first backlog drains
/// over successive sweeps rather than in one long burst.
const OUTPUT_LINE_RETENTION_BATCH_ROWS: i64 = 5_000;
const OUTPUT_LINE_RETENTION_MAX_BATCHES: usize = 20;

#[derive(Clone, Copy, Debug)]
pub struct SessionOutputLineRetentionConfig {
    pub interval: Duration,
    pub retention: Duration,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SessionOutputLineRetentionReport {
    batches: usize,
    deleted_events: u64,
    /// Another replica held the retention lock, so this sweep stopped early.
    yielded_to_peer: bool,
}

/// The slice of the store the sweep needs, so the loop can be tested without
/// Postgres.
pub(crate) trait OutputLineRetentionStore {
    async fn delete_expired_output_line_events(
        &self,
        retention: Duration,
        batch_limit: i64,
    ) -> Result<Option<u64>, SessionStoreError>;
}

impl OutputLineRetentionStore for PgSessionStore {
    async fn delete_expired_output_line_events(
        &self,
        retention: Duration,
        batch_limit: i64,
    ) -> Result<Option<u64>, SessionStoreError> {
        PgSessionStore::delete_expired_output_line_events(self, retention, batch_limit).await
    }
}

pub(crate) struct SessionOutputLineRetentionWorker<S> {
    store: S,
    config: SessionOutputLineRetentionConfig,
}

impl<S: OutputLineRetentionStore> SessionOutputLineRetentionWorker<S> {
    pub(crate) fn new(store: S, config: SessionOutputLineRetentionConfig) -> Self {
        Self { store, config }
    }

    /// Delete `session.output.line` events past the retention window, oldest
    /// first, until a short batch says the backlog is drained, another replica
    /// holds the lock, or the per-sweep cap is reached.
    async fn sweep_once(&mut self) -> Result<SessionOutputLineRetentionReport, SessionStoreError> {
        let mut report = SessionOutputLineRetentionReport::default();
        for _ in 0..OUTPUT_LINE_RETENTION_MAX_BATCHES {
            let Some(deleted) = self
                .store
                .delete_expired_output_line_events(
                    self.config.retention,
                    OUTPUT_LINE_RETENTION_BATCH_ROWS,
                )
                .await?
            else {
                report.yielded_to_peer = true;
                break;
            };
            report.batches += 1;
            report.deleted_events += deleted;
            if deleted < OUTPUT_LINE_RETENTION_BATCH_ROWS as u64 {
                break;
            }
        }
        if report.deleted_events > 0 {
            info!(
                component = crate::COMPONENT_SESSION_RUNTIME,
                event = "session_output_lines_expired",
                deleted = report.deleted_events,
                batches = report.batches,
                retention_secs = self.config.retention.as_secs(),
                "deleted session output lines past the retention window"
            );
        }
        Ok(report)
    }
}

impl SessionOutputLineRetentionWorker<PgSessionStore> {
    pub(crate) fn spawn(mut self) {
        tokio::spawn(async move {
            let mut tick = interval(self.config.interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = self.sweep_once().await {
                    warn!(%error, "session output line retention sweep failed");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque};

    use super::*;

    /// Replays scripted batch results.
    struct ScriptedStore {
        batches: RefCell<VecDeque<Option<u64>>>,
    }

    impl ScriptedStore {
        fn new(batches: impl IntoIterator<Item = Option<u64>>) -> Self {
            Self {
                batches: RefCell::new(batches.into_iter().collect()),
            }
        }
    }

    impl OutputLineRetentionStore for ScriptedStore {
        async fn delete_expired_output_line_events(
            &self,
            _retention: Duration,
            _batch_limit: i64,
        ) -> Result<Option<u64>, SessionStoreError> {
            Ok(self
                .batches
                .borrow_mut()
                .pop_front()
                .expect("sweep issued more batches than scripted"))
        }
    }

    fn worker(store: ScriptedStore) -> SessionOutputLineRetentionWorker<ScriptedStore> {
        SessionOutputLineRetentionWorker::new(
            store,
            SessionOutputLineRetentionConfig {
                interval: Duration::from_secs(300),
                retention: Duration::from_secs(7 * 24 * 60 * 60),
            },
        )
    }

    #[tokio::test]
    async fn a_sweep_stops_at_the_batch_cap_so_a_backlog_drains_over_several() {
        let full = OUTPUT_LINE_RETENTION_BATCH_ROWS as u64;
        let mut worker = worker(ScriptedStore::new(
            std::iter::repeat_n(Some(full), OUTPUT_LINE_RETENTION_MAX_BATCHES).chain([Some(3)]),
        ));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.batches, OUTPUT_LINE_RETENTION_MAX_BATCHES);
        assert_eq!(
            report.deleted_events,
            full * OUTPUT_LINE_RETENTION_MAX_BATCHES as u64
        );
        assert!(!report.yielded_to_peer);

        let report = worker.sweep_once().await.unwrap();
        assert_eq!(report.batches, 1);
        assert_eq!(report.deleted_events, 3);
    }

    #[tokio::test]
    async fn a_sweep_ends_on_a_short_batch() {
        let full = OUTPUT_LINE_RETENTION_BATCH_ROWS as u64;
        let mut worker = worker(ScriptedStore::new([Some(full), Some(full), Some(1_200)]));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.batches, 3);
        assert_eq!(report.deleted_events, 2 * full + 1_200);
    }

    #[tokio::test]
    async fn a_sweep_yields_when_a_peer_holds_the_lock() {
        let full = OUTPUT_LINE_RETENTION_BATCH_ROWS as u64;
        let mut worker = worker(ScriptedStore::new([Some(full), None]));

        let report = worker.sweep_once().await.unwrap();

        assert!(report.yielded_to_peer);
        assert_eq!(report.batches, 1);
        assert_eq!(report.deleted_events, full);
    }
}
