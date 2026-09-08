use std::time::Duration;

use centaur_session_sqlx::{ExpiredOutputLineBatch, PgSessionStore, SessionStoreError};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

/// Rows read per primary-key page, and the most rows one delete statement
/// removes.
const OUTPUT_LINE_RETENTION_PAGE_ROWS: i64 = 5_000;
/// Most rows one sweep deletes before waiting for the next tick, so a large
/// first backlog drains over successive sweeps rather than in one long burst.
const OUTPUT_LINE_RETENTION_MAX_DELETES_PER_SWEEP: u64 = 100_000;
/// Most pages one sweep reads. Pages of retained rows delete nothing and cost
/// only an index range read, so this is a backstop rather than the working
/// limit: it bounds the walk from the head of the table after a restart.
const OUTPUT_LINE_RETENTION_MAX_PAGES_PER_SWEEP: usize = 400;

#[derive(Clone, Copy, Debug)]
pub struct SessionOutputLineRetentionConfig {
    pub interval: Duration,
    pub retention: Duration,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SessionOutputLineRetentionReport {
    pages: usize,
    scanned_events: u64,
    deleted_events: u64,
    /// Lines collected from executions that finished after running longer than
    /// the window, which the walk had skipped.
    recovered_events: u64,
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
        page_limit: i64,
    ) -> Result<Option<ExpiredOutputLineBatch>, SessionStoreError>;

    async fn delete_expired_output_lines_of_finished_long_executions(
        &self,
        retention: Duration,
        batch_limit: i64,
    ) -> Result<Option<u64>, SessionStoreError>;
}

impl OutputLineRetentionStore for PgSessionStore {
    async fn delete_expired_output_line_events(
        &self,
        after_event_id: i64,
        retention: Duration,
        page_limit: i64,
    ) -> Result<Option<ExpiredOutputLineBatch>, SessionStoreError> {
        PgSessionStore::delete_expired_output_line_events(
            self,
            after_event_id,
            retention,
            page_limit,
        )
        .await
    }

    async fn delete_expired_output_lines_of_finished_long_executions(
        &self,
        retention: Duration,
        batch_limit: i64,
    ) -> Result<Option<u64>, SessionStoreError> {
        PgSessionStore::delete_expired_output_lines_of_finished_long_executions(
            self,
            retention,
            batch_limit,
        )
        .await
    }
}

pub(crate) struct SessionOutputLineRetentionWorker<S> {
    store: S,
    config: SessionOutputLineRetentionConfig,
    /// Every row at or below this id has been read by the walk. Held in memory
    /// only: a restart walks from the head of the table again, which costs an
    /// index range read over the retained rows before the first deletable page.
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
    /// The walk pages forward through the primary key from the cursor and
    /// stops when a page reaches rows still inside the window, when the table
    /// runs out, when another replica holds the lock, or at the per-sweep
    /// caps. A second pass then collects lines the walk skipped because their
    /// execution was still running, once that execution has finished.
    async fn sweep_once(&mut self) -> Result<SessionOutputLineRetentionReport, SessionStoreError> {
        let mut report = SessionOutputLineRetentionReport::default();
        for _ in 0..OUTPUT_LINE_RETENTION_MAX_PAGES_PER_SWEEP {
            let Some(batch) = self
                .store
                .delete_expired_output_line_events(
                    self.after_event_id,
                    self.config.retention,
                    OUTPUT_LINE_RETENTION_PAGE_ROWS,
                )
                .await?
            else {
                report.yielded_to_peer = true;
                return Ok(report);
            };
            report.pages += 1;
            report.scanned_events += batch.scanned;
            report.deleted_events += batch.deleted;
            self.after_event_id = self.after_event_id.max(batch.next_after_event_id);
            if batch.reached_retained_events
                || batch.scanned < OUTPUT_LINE_RETENTION_PAGE_ROWS as u64
                || report.deleted_events >= OUTPUT_LINE_RETENTION_MAX_DELETES_PER_SWEEP
            {
                break;
            }
        }

        while report.deleted_events + report.recovered_events
            < OUTPUT_LINE_RETENTION_MAX_DELETES_PER_SWEEP
        {
            let Some(recovered) = self
                .store
                .delete_expired_output_lines_of_finished_long_executions(
                    self.config.retention,
                    OUTPUT_LINE_RETENTION_PAGE_ROWS,
                )
                .await?
            else {
                report.yielded_to_peer = true;
                break;
            };
            report.recovered_events += recovered;
            if recovered < OUTPUT_LINE_RETENTION_PAGE_ROWS as u64 {
                break;
            }
        }

        if report.deleted_events + report.recovered_events > 0 {
            info!(
                component = crate::COMPONENT_SESSION_RUNTIME,
                event = "session_output_lines_expired",
                deleted = report.deleted_events,
                recovered = report.recovered_events,
                scanned = report.scanned_events,
                pages = report.pages,
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

    /// Replays scripted results and records the cursor each page was read from.
    struct ScriptedStore {
        pages: RefCell<VecDeque<Option<ExpiredOutputLineBatch>>>,
        recoveries: RefCell<VecDeque<Option<u64>>>,
        cursors: RefCell<Vec<i64>>,
    }

    impl ScriptedStore {
        fn new(pages: impl IntoIterator<Item = Option<ExpiredOutputLineBatch>>) -> Self {
            Self {
                pages: RefCell::new(pages.into_iter().collect()),
                recoveries: RefCell::new(VecDeque::new()),
                cursors: RefCell::new(Vec::new()),
            }
        }

        fn with_recoveries(self, recoveries: impl IntoIterator<Item = Option<u64>>) -> Self {
            *self.recoveries.borrow_mut() = recoveries.into_iter().collect();
            self
        }
    }

    impl OutputLineRetentionStore for ScriptedStore {
        async fn delete_expired_output_line_events(
            &self,
            after_event_id: i64,
            _retention: Duration,
            _page_limit: i64,
        ) -> Result<Option<ExpiredOutputLineBatch>, SessionStoreError> {
            self.cursors.borrow_mut().push(after_event_id);
            Ok(self
                .pages
                .borrow_mut()
                .pop_front()
                .expect("sweep read more pages than scripted"))
        }

        async fn delete_expired_output_lines_of_finished_long_executions(
            &self,
            _retention: Duration,
            _batch_limit: i64,
        ) -> Result<Option<u64>, SessionStoreError> {
            // An unscripted recovery pass finds nothing, like a healthy deployment.
            Ok(self.recoveries.borrow_mut().pop_front().unwrap_or(Some(0)))
        }
    }

    fn full_page(next_after_event_id: i64, deleted: u64) -> Option<ExpiredOutputLineBatch> {
        Some(ExpiredOutputLineBatch {
            scanned: OUTPUT_LINE_RETENTION_PAGE_ROWS as u64,
            deleted,
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
    async fn a_sweep_stops_at_the_delete_cap_and_resumes_from_its_cursor() {
        let rows = OUTPUT_LINE_RETENTION_PAGE_ROWS;
        let pages_to_cap = OUTPUT_LINE_RETENTION_MAX_DELETES_PER_SWEEP / rows as u64;
        let pages = (1..=pages_to_cap as i64)
            .map(|n| full_page(n * rows, rows as u64))
            .chain([Some(ExpiredOutputLineBatch {
                scanned: 7,
                deleted: 7,
                next_after_event_id: pages_to_cap as i64 * rows + 7,
                reached_retained_events: false,
            })]);
        let mut worker = worker(ScriptedStore::new(pages));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.pages, pages_to_cap as usize);
        assert_eq!(
            report.deleted_events,
            OUTPUT_LINE_RETENTION_MAX_DELETES_PER_SWEEP
        );
        assert!(!report.yielded_to_peer);
        let cursors = worker.store.cursors.borrow().clone();
        assert_eq!(cursors[0], 0);
        assert_eq!(cursors[1], rows);
        assert_eq!(worker.after_event_id, pages_to_cap as i64 * rows);

        // The next sweep picks up where this one left off rather than rescanning.
        worker.sweep_once().await.unwrap();
        assert_eq!(
            *worker.store.cursors.borrow().last().unwrap(),
            pages_to_cap as i64 * rows
        );
    }

    #[tokio::test]
    async fn pages_of_retained_rows_do_not_count_against_the_delete_cap() {
        // Far more empty pages than the delete cap would allow if it counted
        // pages: the walk from the head of the table after a restart must get
        // through retained lifecycle rows in one sweep.
        let empty_pages = 100;
        let pages = (1..=empty_pages)
            .map(|n| full_page(n * OUTPUT_LINE_RETENTION_PAGE_ROWS, 0))
            .chain([Some(ExpiredOutputLineBatch {
                scanned: OUTPUT_LINE_RETENTION_PAGE_ROWS as u64,
                deleted: 1_200,
                next_after_event_id: (empty_pages + 1) * OUTPUT_LINE_RETENTION_PAGE_ROWS - 300,
                reached_retained_events: true,
            })]);
        let mut worker = worker(ScriptedStore::new(pages));

        let report = worker.sweep_once().await.unwrap();

        assert_eq!(report.pages, empty_pages as usize + 1);
        assert_eq!(report.deleted_events, 1_200);
        assert_eq!(
            report.scanned_events,
            (empty_pages as u64 + 1) * OUTPUT_LINE_RETENTION_PAGE_ROWS as u64
        );
        assert_eq!(
            worker.after_event_id,
            (empty_pages + 1) * OUTPUT_LINE_RETENTION_PAGE_ROWS - 300
        );
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

        assert_eq!(report.pages, 1);
        assert_eq!(report.deleted_events, 3);
        assert_eq!(worker.after_event_id, 42);
    }

    #[tokio::test]
    async fn a_sweep_yields_when_a_peer_holds_the_lock() {
        let mut worker = worker(ScriptedStore::new([full_page(5_000, 5_000), None]));

        let report = worker.sweep_once().await.unwrap();

        assert!(report.yielded_to_peer);
        assert_eq!(report.pages, 1);
        assert_eq!(report.recovered_events, 0);
        assert_eq!(worker.after_event_id, 5_000);
    }

    #[tokio::test]
    async fn skipped_lines_of_finished_long_executions_are_recovered_after_the_walk() {
        let rows = OUTPUT_LINE_RETENTION_PAGE_ROWS as u64;
        let mut worker = worker(
            ScriptedStore::new([Some(ExpiredOutputLineBatch {
                scanned: 10,
                deleted: 4,
                next_after_event_id: 10,
                reached_retained_events: true,
            })])
            .with_recoveries([Some(rows), Some(12), Some(999)]),
        );

        let report = worker.sweep_once().await.unwrap();

        // A full recovery batch is followed by another; a short one ends the pass.
        assert_eq!(report.deleted_events, 4);
        assert_eq!(report.recovered_events, rows + 12);
        assert_eq!(worker.store.recoveries.borrow().len(), 1);
    }
}
