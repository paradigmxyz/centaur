use std::time::Duration;

use centaur_session_sqlx::{ExpiredOutputLineBatch, PgSessionStore, SessionStoreError};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

/// Rows per delete statement, and the most statements one sweep will issue.
/// Together they bound a sweep so a large first backlog drains over successive
/// sweeps rather than in one long-running series of deletes.
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
    scanned_events: u64,
    deleted_events: u64,
    /// Another replica held the retention lock, so this sweep stopped early.
    yielded_to_peer: bool,
}

/// The slice of the store the sweep needs, so the loop can be tested without
/// Postgres.
pub(crate) trait OutputLineRetentionStore {
    async fn delete_expired_output_line_events(
        &self,
        after_event_id: i64,
        retention: Duration,
        batch_limit: i64,
    ) -> Result<Option<ExpiredOutputLineBatch>, SessionStoreError>;
}

impl OutputLineRetentionStore for PgSessionStore {
    async fn delete_expired_output_line_events(
        &self,
        after_event_id: i64,
        retention: Duration,
        batch_limit: i64,
    ) -> Result<Option<ExpiredOutputLineBatch>, SessionStoreError> {
        PgSessionStore::delete_expired_output_line_events(
            self,
            after_event_id,
            retention,
            batch_limit,
        )
        .await
    }
}

pub(crate) struct SessionOutputLineRetentionWorker<S> {
    store: S,
    config: SessionOutputLineRetentionConfig,
    /// Every `session.output.line` row at or below this id has been handled.
    /// Held in memory only: a restart walks from the head of the table again,
    /// which costs one primary-key pass over the retained lifecycle rows before
    /// the first deletable batch, and is also what picks up output lines that
    /// were skipped because their execution was still running.
    after_event_id: i64,
}

impl<S: OutputLineRetentionStore> SessionOutputLineRetentionWorker<S> {
    pub(crate) fn new(store: S, config: SessionOutputLineRetentionConfig) -> Self {
        Self {
            store,
            config,
            after_event_id: 0,
        }
    }

    /// Delete `session.output.line` events past the retention window.
    ///
    /// Each batch walks the primary key forward from the cursor. The sweep ends
    /// when a batch reaches rows still inside the window, when the table runs
    /// out, when another replica holds the lock, or at the per-sweep batch cap.
    async fn sweep_once(&mut self) -> Result<SessionOutputLineRetentionReport, SessionStoreError> {
        let mut report = SessionOutputLineRetentionReport::default();
        for _ in 0..OUTPUT_LINE_RETENTION_MAX_BATCHES {
            let Some(batch) = self
                .store
                .delete_expired_output_line_events(
                    self.after_event_id,
                    self.config.retention,
                    OUTPUT_LINE_RETENTION_BATCH_ROWS,
                )
                .await?
            else {
                report.yielded_to_peer = true;
                break;
            };
            report.batches += 1;
            report.scanned_events += batch.scanned;
            report.deleted_events += batch.deleted;
            self.after_event_id = self.after_event_id.max(batch.next_after_event_id);
            if batch.reached_retained_events
                || batch.scanned < OUTPUT_LINE_RETENTION_BATCH_ROWS as u64
            {
                break;
            }
        }
        if report.deleted_events > 0 {
            info!(
                component = crate::COMPONENT_SESSION_RUNTIME,
                event = "session_output_lines_expired",
                deleted = report.deleted_events,
                scanned = report.scanned_events,
                batches = report.batches,
                after_event_id = self.after_event_id,
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

    /// Replays scripted batches and records the cursor each call was made with.
    struct ScriptedStore {
        batches: RefCell<VecDeque<Option<ExpiredOutputLineBatch>>>,
        cursors: RefCell<Vec<i64>>,
    }

    impl ScriptedStore {
        fn new(batches: impl IntoIterator<Item = Option<ExpiredOutputLineBatch>>) -> Self {
            Self {
                batches: RefCell::new(batches.into_iter().collect()),
                cursors: RefCell::new(Vec::new()),
            }
        }
    }

    impl OutputLineRetentionStore for ScriptedStore {
        async fn delete_expired_output_line_events(
            &self,
            after_event_id: i64,
            _retention: Duration,
            _batch_limit: i64,
        ) -> Result<Option<ExpiredOutputLineBatch>, SessionStoreError> {
            self.cursors.borrow_mut().push(after_event_id);
            Ok(self
                .batches
                .borrow_mut()
                .pop_front()
                .expect("sweep issued more batches than scripted"))
        }
    }

    fn full_batch(next_after_event_id: i64) -> Option<ExpiredOutputLineBatch> {
        Some(ExpiredOutputLineBatch {
            scanned: OUTPUT_LINE_RETENTION_BATCH_ROWS as u64,
            deleted: OUTPUT_LINE_RETENTION_BATCH_ROWS as u64,
            next_after_event_id,
            reached_retained_events: false,
        })
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
    async fn a_sweep_stops_at_the_batch_cap_and_resumes_from_its_cursor() {
        let cap = OUTPUT_LINE_RETENTION_MAX_BATCHES as i64;
        let batches = (1..=cap)
            .map(|n| full_batch(n * OUTPUT_LINE_RETENTION_BATCH_ROWS))
            .chain([Some(ExpiredOutputLineBatch {
                scanned: 7,
                deleted: 7,
                next_after_event_id: cap * OUTPUT_LINE_RETENTION_BATCH_ROWS + 7,
                reached_retained_events: false,
            })]);
        let mut worker = worker(ScriptedStore::new(batches));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.batches, OUTPUT_LINE_RETENTION_MAX_BATCHES);
        assert_eq!(
            report.deleted_events,
            (OUTPUT_LINE_RETENTION_BATCH_ROWS * OUTPUT_LINE_RETENTION_MAX_BATCHES as i64) as u64
        );
        assert!(!report.yielded_to_peer);
        let cursors = worker.store.cursors.borrow().clone();
        assert_eq!(cursors[0], 0);
        assert_eq!(cursors[1], OUTPUT_LINE_RETENTION_BATCH_ROWS);
        assert_eq!(
            worker.after_event_id,
            OUTPUT_LINE_RETENTION_BATCH_ROWS * OUTPUT_LINE_RETENTION_MAX_BATCHES as i64
        );

        // The next sweep picks up where this one left off rather than rescanning.
        worker.sweep_once().await.unwrap();
        assert_eq!(
            *worker.store.cursors.borrow().last().unwrap(),
            OUTPUT_LINE_RETENTION_BATCH_ROWS * OUTPUT_LINE_RETENTION_MAX_BATCHES as i64
        );
    }

    #[tokio::test]
    async fn a_sweep_ends_when_it_reaches_retained_rows() {
        let mut worker = worker(ScriptedStore::new([
            full_batch(5_000),
            Some(ExpiredOutputLineBatch {
                scanned: OUTPUT_LINE_RETENTION_BATCH_ROWS as u64,
                deleted: 1_200,
                next_after_event_id: 6_199,
                reached_retained_events: true,
            }),
        ]));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.deleted_events, 6_200);
        assert_eq!(worker.after_event_id, 6_199);
    }

    #[tokio::test]
    async fn a_sweep_ends_when_the_table_runs_out() {
        let mut worker = worker(ScriptedStore::new([Some(ExpiredOutputLineBatch {
            scanned: 3,
            deleted: 3,
            next_after_event_id: 42,
            reached_retained_events: false,
        })]));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.batches, 1);
        assert_eq!(report.deleted_events, 3);
        assert_eq!(worker.after_event_id, 42);
    }

    #[tokio::test]
    async fn a_sweep_yields_when_a_peer_holds_the_lock() {
        let mut worker = worker(ScriptedStore::new([full_batch(5_000), None]));

        let report = worker.sweep_once().await.unwrap();

        assert!(report.yielded_to_peer);
        assert_eq!(report.batches, 1);
        assert_eq!(worker.after_event_id, 5_000);
    }
}
