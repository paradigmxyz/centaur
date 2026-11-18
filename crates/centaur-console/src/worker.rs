//! `centaur-console worker`: the replica-safe background process (RFC 0006,
//! Decision 3). It runs a set of periodic loops, each under a Postgres
//! advisory lock so only one replica works a loop at a time, and no queue.
//!
//! The loops are the Slack channel catalog discovery (hourly) and
//! membership refresh (every 10 minutes); see
//! `centaur_console_api::misc::slack`.

use std::{future::Future, pin::Pin, time::Duration};

use centaur_console_api::{AppState, misc::slack};

/// One pass of a loop.
pub type LoopFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// One periodic job.
pub struct WorkerLoop {
    /// Stable name, also hashed into the advisory lock key.
    pub name: &'static str,
    pub interval: Duration,
    /// One pass; errors are logged and the loop continues.
    pub run: fn(AppState) -> LoopFuture,
}

/// Every loop the worker runs.
pub fn loops() -> Vec<WorkerLoop> {
    vec![
        WorkerLoop {
            name: slack::REFRESH_LOOP,
            interval: slack::REFRESH_INTERVAL,
            run: |state| {
                Box::pin(async move {
                    slack::Catalog::from_state(&state)
                        .refresh()
                        .await
                        .map_err(|error| error.to_string())
                })
            },
        },
        WorkerLoop {
            name: slack::MEMBERSHIP_LOOP,
            interval: slack::MEMBERSHIP_INTERVAL,
            run: |state| {
                Box::pin(async move {
                    slack::Catalog::from_state(&state)
                        .refresh_membership(None)
                        .await
                        .map_err(|error| error.to_string())
                })
            },
        },
    ]
}

/// Runs `job` once if no other replica holds its lock.
///
/// The lock lives in a transaction held for the whole pass (see
/// [`slack::with_lock`]), so it cannot be left behind on a pooled connection
/// if the pass panics or is aborted at shutdown.
pub async fn run_once(state: &AppState, job: &WorkerLoop) -> Result<bool, sqlx::Error> {
    let outcome = slack::with_lock(&state.db, job.name, false, || (job.run)(state.clone())).await?;
    match outcome {
        None => Ok(false),
        Some(Err(error)) => {
            tracing::error!(job = job.name, %error, "worker loop failed");
            Ok(true)
        }
        Some(Ok(())) => Ok(true),
    }
}

/// Runs every loop on its interval until `shutdown` resolves.
pub async fn run(state: AppState, shutdown: impl Future<Output = ()>) {
    let jobs = loops();
    tracing::info!(loops = jobs.len(), "console worker started");
    let mut tasks = tokio::task::JoinSet::new();
    for job in jobs {
        let state = state.clone();
        tasks.spawn(async move {
            let mut ticker = tokio::time::interval(job.interval);
            loop {
                ticker.tick().await;
                if let Err(error) = run_once(&state, &job).await {
                    tracing::error!(job = job.name, %error, "worker loop could not run");
                }
            }
        });
    }
    shutdown.await;
    tasks.abort_all();
    tracing::info!("console worker stopped");
}

#[cfg(test)]
mod tests {
    use super::{loops, slack};

    #[test]
    fn the_worker_runs_both_catalog_loops() {
        let names: Vec<_> = loops().iter().map(|job| job.name).collect();
        assert_eq!(names, [slack::REFRESH_LOOP, slack::MEMBERSHIP_LOOP]);
    }
}
