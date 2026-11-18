//! DB-backed runner tests against a fake console (RFC 0006a). Set
//! `SESSION_RUNTIME_TEST_DATABASE_URL` to a disposable Postgres with pgvector;
//! each test uses its own console schema and Absurd queues.
//!
//! Ports the scheduling assertions of Rails'
//! `test/jobs/scheduled_task_jobs_test.rb`; the console-side assertions
//! (principal provisioning, delivery policy) belong to the console's claim
//! endpoint.

mod support;

use std::sync::atomic::Ordering;

use centaur_scheduled_tasks::{TaskState, TickAction, TickOutcome, TickResult, keys};
use chrono::{DateTime, Duration, TimeZone as _, Utc};
use serde_json::json;
use support::{Clock, Harness, minute};

fn hour(time: DateTime<Utc>) -> DateTime<Utc> {
    let time = minute(time);
    time - Duration::minutes(i64::from(chrono::Timelike::minute(&time)))
}

fn tick_result(tick: &support::TickRow) -> TickResult {
    serde_json::from_value(tick.result.clone().expect("completed tick")).unwrap()
}

#[tokio::test]
async fn due_occurrence_fires_once_and_chains_the_next_tick() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let now = Utc::now();
    let scheduled_for = hour(now) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;

    // Reconcile twice: one tick for the occurrence.
    let first = h.runner.reconcile_all().await.unwrap();
    assert_eq!((first.tasks, first.created), (1, 1));
    let second = h.runner.reconcile_all().await.unwrap();
    assert_eq!(second.created, 0);
    h.drain().await;

    // The run request.
    let runs = h.runs().await;
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(
        run.key,
        format!("scheduled-task:{id}:{}", keys::key_time(scheduled_for))
    );
    assert_eq!(run.max_attempts, Some(3));
    assert_eq!(run.params["workflow_name"], "console_workflow");
    let oid = centaur_console_db::oid("tsk", id);
    assert_eq!(run.params["input"]["scheduled_task_id"], json!(oid));
    assert_eq!(run.params["input"]["prompt"], "Summarize open incidents.");
    assert_eq!(run.params["input"]["channel"], "C0123456789");
    let requests = h.spawner.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].to_workflow_request().harness_type, None);

    // The claim advanced the schedule and the result was recorded.
    let task = h.task(id).await;
    let next = hour(now) + Duration::hours(1);
    assert_eq!(task.last_enqueued_at, Some(scheduled_for));
    assert_eq!(task.next_run_at, Some(next));
    assert_eq!(task.last_run_id, Some(h.run_id(&run.task_id).await));
    assert!(task.last_run_at.is_some());
    assert_eq!(task.last_error, None);
    assert_eq!(h.fake.claim_count(), 1);

    // Chained exactly once; reconcile does not add a second tick for it.
    let tick = h.tick(&keys::tick_key(id, scheduled_for)).await.unwrap();
    assert_eq!(tick.state, "completed");
    let result = tick_result(&tick);
    assert_eq!(result.outcome, TickOutcome::Spawned);
    assert_eq!(result.run_created, Some(true));
    assert_eq!(result.next_tick, Some(keys::wire_time(next)));
    assert_eq!(h.runner.reconcile_all().await.unwrap().created, 0);
    let next_key = keys::tick_key(id, next);
    let ticks = h.ticks().await;
    assert_eq!(ticks.len(), 2);
    assert_eq!(ticks.iter().filter(|tick| tick.key == next_key).count(), 1);
    assert_eq!(h.tick(&next_key).await.unwrap().state, "sleeping");

    // Running the scheduler again does nothing (Rails: no second enqueue).
    h.drain().await;
    assert_eq!(h.runs().await.len(), 1);
    assert_eq!(h.fake.claim_count(), 1);
}

#[tokio::test]
async fn reconcile_endpoint_logic_reports_each_task_state() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let future = hour(Utc::now()) + Duration::hours(2);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(future))
        .await;

    let first = h.runner.reconcile_task(id).await.unwrap();
    assert_eq!(first.state, TaskState::Scheduled);
    assert_eq!(first.next_run_at, Some(future));
    assert_eq!(first.tick.as_ref().unwrap().action, TickAction::Created);
    let again = h.runner.reconcile_task(id).await.unwrap();
    assert_eq!(again.tick.as_ref().unwrap().action, TickAction::Existing);
    assert_eq!(again.tick.unwrap().task_id, first.tick.unwrap().task_id);

    h.execute(
        "update scheduled_tasks set enabled = false, next_run_at = null where id = $1",
        id,
    )
    .await;
    let disabled = h.runner.reconcile_task(id).await.unwrap();
    assert_eq!(disabled.state, TaskState::Unscheduled);
    assert_eq!(disabled.tick, None);
    // Disabled ticks are left to skip at wake; their key may be needed again.
    assert_eq!(h.ticks().await[0].state, "pending");

    h.execute("delete from scheduled_tasks where id = $1", id)
        .await;
    let deleted = h.runner.reconcile_task(id).await.unwrap();
    assert_eq!(deleted.state, TaskState::Deleted);
    assert_eq!(deleted.cancelled, 1);
    assert_eq!(h.ticks().await[0].state, "cancelled");
}

#[tokio::test]
async fn replay_after_a_crash_between_the_claim_and_its_checkpoint_claims_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(3);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.fake
        .faults
        .claim_commit_then_fail
        .store(1, Ordering::SeqCst);

    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    // The second claim is the console's replay (last_enqueued_at matches).
    assert_eq!(h.fake.claim_count(), 2);
    let task = h.task(id).await;
    assert_eq!(task.last_enqueued_at, Some(scheduled_for));
    assert_eq!(
        task.next_run_at,
        Some(hour(Utc::now()) + Duration::hours(1))
    );
    let runs = h.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].key, keys::run_key(id, scheduled_for));
    assert_eq!(task.last_run_id, Some(h.run_id(&runs[0].task_id).await));
}

/// A claim whose answer was lost must not be forgotten when the next
/// occurrence arrives first: the console only remembers the latest claim.
#[tokio::test]
async fn a_later_occurrence_waits_for_an_earlier_claim_to_be_checkpointed() {
    let Some(h) = Harness::with_config(|config| centaur_scheduled_tasks::RunnerConfig {
        // The lost claim's retry comes after the next occurrence's tick.
        tick_retry: absurd::RetryStrategy {
            kind: absurd::RetryKind::Fixed,
            base_seconds: Some(2.0),
            factor: None,
            max_seconds: None,
        },
        claim_wait: std::time::Duration::from_millis(500),
        ..config
    })
    .await
    else {
        return;
    };
    *h.fake.clock.lock().unwrap() = Clock::ScheduledFor;
    let first = hour(Utc::now()) - Duration::hours(3);
    let second = first + Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(first))
        .await;
    h.fake
        .faults
        .claim_commit_then_fail
        .store(1, Ordering::SeqCst);
    h.runner.reconcile_all().await.unwrap();
    // The first claim commits, then its answer is lost.
    h.work_one().await;
    assert_eq!(h.task(id).await.last_enqueued_at, Some(first));
    // The console now advertises the next occurrence, which is already due.
    h.runner.reconcile_all().await.unwrap();

    for _ in 0..60 {
        h.work_one().await;
        if h.runs().await.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let keys: Vec<String> = h.runs().await.into_iter().map(|run| run.key).collect();
    assert!(keys.contains(&keys::run_key(id, first)), "{keys:?}");
    assert!(keys.contains(&keys::run_key(id, second)), "{keys:?}");
    let ticks = h.ticks().await;
    let first_tick = ticks
        .iter()
        .find(|tick| tick.key == keys::tick_key(id, first))
        .unwrap();
    assert_eq!(tick_result(first_tick).outcome, TickOutcome::Spawned);
}

/// The number of `claim-wait` checkpoints the gate left behind.
async fn claim_waits(h: &Harness) -> i64 {
    sqlx::query_scalar(&format!(
        "select count(*) from absurd.\"c_{}\" where checkpoint_name like 'claim-wait%'",
        h.queue
    ))
    .fetch_one(&h.pool)
    .await
    .unwrap()
}

/// A tick that exhausted its attempts before checkpointing its claim is not
/// pending: the console decides whether a later occurrence is still due.
#[tokio::test]
async fn a_failed_earlier_tick_does_not_block_the_next_occurrence() {
    let Some(h) = Harness::with_config(|config| centaur_scheduled_tasks::RunnerConfig {
        tick_max_attempts: 1,
        claim_wait: std::time::Duration::from_millis(10),
        claim_wait_attempts: 2,
        ..config
    })
    .await
    else {
        return;
    };
    let first = hour(Utc::now()) - Duration::hours(1);
    let second = first + Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(first))
        .await;
    h.fake.faults.claim_unavailable.store(1, Ordering::SeqCst);
    h.runner.reconcile_all().await.unwrap();
    h.drain().await;
    assert_eq!(
        h.tick(&keys::tick_key(id, first)).await.unwrap().state,
        "failed"
    );

    // The next occurrence is already due; its gate sees only a failed tick.
    h.runner.ensure_tick(id, second).await.unwrap();
    h.drain().await;
    let tick = h.tick(&keys::tick_key(id, second)).await.unwrap();
    assert_eq!(tick.state, "completed");
    assert_eq!(
        tick_result(&tick).reason.as_deref(),
        Some("not_due"),
        "the console, not the gate, decides"
    );
    assert_eq!(
        claim_waits(&h).await,
        0,
        "a failed tick must not be waited on"
    );
}

/// A tick that never checkpoints (here: parked mid-run) delays the next
/// occurrence, but only for the bounded number of waits.
#[tokio::test]
async fn a_stuck_earlier_tick_delays_the_next_occurrence_only_for_the_bound() {
    let Some(h) = Harness::with_config(|config| centaur_scheduled_tasks::RunnerConfig {
        tick_max_attempts: 1,
        claim_wait: std::time::Duration::from_millis(10),
        claim_wait_attempts: 2,
        ..config
    })
    .await
    else {
        return;
    };
    let first = hour(Utc::now()) - Duration::hours(1);
    let second = first + Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(first))
        .await;
    h.runner.reconcile_all().await.unwrap();
    // Park the first tick: it is neither finished nor ever checkpointing, so
    // no worker picks it up again.
    sqlx::query(&format!(
        "update absurd.\"t_{}\" set state = 'running' where idempotency_key = $1",
        h.queue
    ))
    .bind(keys::tick_key(id, first))
    .execute(&h.pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "update absurd.\"r_{}\" set state = 'running' \
         where task_id = (select task_id from absurd.\"t_{}\" where idempotency_key = $1)",
        h.queue, h.queue
    ))
    .bind(keys::tick_key(id, first))
    .execute(&h.pool)
    .await
    .unwrap();
    // The console already advanced past the first occurrence.
    h.set_next_run_at(id, Some(second)).await;

    h.runner.ensure_tick(id, second).await.unwrap();
    for _ in 0..50 {
        h.work_one().await;
        let done = h
            .tick(&keys::tick_key(id, second))
            .await
            .is_some_and(|tick| tick.state == "completed");
        if done {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let runs: Vec<String> = h.runs().await.into_iter().map(|run| run.key).collect();
    assert_eq!(runs, vec![keys::run_key(id, second)]);
    assert_eq!(claim_waits(&h).await, 2, "the wait is bounded");
    assert_eq!(
        h.tick(&keys::tick_key(id, first)).await.unwrap().state,
        "running",
        "the stuck tick is left alone"
    );
}

#[tokio::test]
async fn replay_after_a_crash_between_the_spawn_and_its_checkpoint_keeps_one_run() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.spawner.faults.fail_after_spawn.store(1, Ordering::SeqCst);

    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    assert_eq!(h.spawner.requests.lock().unwrap().len(), 2);
    let runs = h.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(h.fake.claim_count(), 1);
    let task = h.task(id).await;
    assert_eq!(task.last_run_id, Some(h.run_id(&runs[0].task_id).await));
    // The failed attempt's error was recorded, then cleared by the success.
    assert_eq!(task.last_error, None);
    let results = h.fake.results();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].error.as_deref(), Some("internal server error"));
    let tick = tick_result(&h.tick(&keys::tick_key(id, scheduled_for)).await.unwrap());
    assert_eq!(tick.run_created, Some(false));
}

#[tokio::test]
async fn console_5xx_is_retried_and_report_failures_do_not_respawn() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.fake.faults.claim_unavailable.store(3, Ordering::SeqCst);
    h.fake.faults.result_unavailable.store(2, Ordering::SeqCst);

    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    assert_eq!(h.fake.claim_count(), 4);
    assert_eq!(h.fake.results().len(), 3);
    assert_eq!(h.spawner.requests.lock().unwrap().len(), 1);
    let runs = h.runs().await;
    assert_eq!(runs.len(), 1);
    let task = h.task(id).await;
    assert_eq!(task.last_run_id, Some(h.run_id(&runs[0].task_id).await));
    assert_eq!(task.last_enqueued_at, Some(scheduled_for));
}

#[tokio::test]
async fn an_exhausted_tick_is_rearmed_by_reconcile_until_the_console_is_back() {
    let Some(h) = Harness::with_config(|mut config| {
        config.tick_max_attempts = 2;
        config
    })
    .await
    else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.fake.faults.claim_unavailable.store(4, Ordering::SeqCst);

    h.runner.reconcile_all().await.unwrap();
    h.drain().await;
    let key = keys::tick_key(id, scheduled_for);
    assert_eq!(h.tick(&key).await.unwrap().state, "failed");
    assert!(h.runs().await.is_empty());
    assert_eq!(h.task(id).await.next_run_at, Some(scheduled_for));

    // One more attempt per reconcile pass, through the single-task path too.
    assert_eq!(h.runner.reconcile_all().await.unwrap().rearmed, 1);
    h.drain().await;
    assert_eq!(h.tick(&key).await.unwrap().state, "failed");
    let ensured = h.runner.reconcile_task(id).await.unwrap();
    assert_eq!(ensured.tick.unwrap().action, TickAction::Rearmed);
    h.drain().await;
    assert_eq!(h.tick(&key).await.unwrap().state, "failed");
    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    assert_eq!(h.tick(&key).await.unwrap().state, "completed");
    assert_eq!(h.fake.claim_count(), 5);
    assert_eq!(h.runs().await.len(), 1);
    assert_eq!(h.task(id).await.last_enqueued_at, Some(scheduled_for));
}

#[tokio::test]
async fn a_task_disabled_after_its_tick_was_armed_is_skipped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.runner.reconcile_all().await.unwrap();
    // ScheduledTask#refresh_next_run_at clears next_run_at when disabled.
    h.execute(
        "update scheduled_tasks set enabled = false, next_run_at = null where id = $1",
        id,
    )
    .await;

    h.drain().await;

    // A task disabled after it was enqueued is skipped.
    assert!(h.runs().await.is_empty());
    assert_eq!(h.fake.claim_count(), 0);
    let task = h.task(id).await;
    assert_eq!(task.last_run_id, None);
    assert_eq!(task.last_enqueued_at, None);
    let ticks = h.ticks().await;
    assert_eq!(ticks.len(), 1);
    let result = tick_result(&ticks[0]);
    assert_eq!(result.outcome, TickOutcome::Skipped);
    assert_eq!(result.reason.as_deref(), Some("task_disabled"));
    assert_eq!(result.next_tick, None);

    // Re-enabled with the same occurrence: the tick key is still usable.
    h.execute(
        "update scheduled_tasks set enabled = true where id = $1",
        id,
    )
    .await;
    let future = hour(Utc::now()) + Duration::hours(1);
    h.set_next_run_at(id, Some(future)).await;
    assert_eq!(h.runner.reconcile_all().await.unwrap().created, 1);
}

#[tokio::test]
async fn deleted_tasks_skip_and_lose_their_pending_ticks() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let due = hour(Utc::now()) - Duration::hours(1);
    let due_id = h.insert_task(Some("0 * * * *"), "Etc/UTC", Some(due)).await;
    let future = hour(Utc::now()) + Duration::hours(1);
    let future_id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(future))
        .await;
    h.runner.reconcile_all().await.unwrap();
    h.execute("delete from scheduled_tasks where id = $1", due_id)
        .await;
    h.execute("delete from scheduled_tasks where id = $1", future_id)
        .await;

    // A task deleted after it was enqueued is dropped.
    h.drain().await;
    let due_tick = h.tick(&keys::tick_key(due_id, due)).await.unwrap();
    let result = tick_result(&due_tick);
    assert_eq!(result.outcome, TickOutcome::Skipped);
    assert_eq!(result.reason.as_deref(), Some("task_not_found"));
    assert_eq!(h.fake.claim_count(), 0);
    assert!(h.runs().await.is_empty());

    let future_key = keys::tick_key(future_id, future);
    assert_eq!(h.tick(&future_key).await.unwrap().state, "sleeping");
    assert_eq!(h.runner.reconcile_all().await.unwrap().cancelled, 1);
    assert_eq!(h.tick(&future_key).await.unwrap().state, "cancelled");
}

#[tokio::test]
async fn a_rescheduled_task_fires_only_at_its_new_time() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let old = hour(Utc::now()) - Duration::hours(1);
    let id = h.insert_task(Some("0 * * * *"), "Etc/UTC", Some(old)).await;
    h.runner.reconcile_all().await.unwrap();
    let new = hour(Utc::now()) + Duration::hours(5);
    h.set_next_run_at(id, Some(new)).await;
    assert_eq!(h.runner.reconcile_all().await.unwrap().created, 1);

    h.drain().await;

    let stale = tick_result(&h.tick(&keys::tick_key(id, old)).await.unwrap());
    assert_eq!(stale.reason.as_deref(), Some("not_due"));
    // The stale tick chains to the current occurrence, which already exists.
    assert_eq!(stale.next_tick, Some(keys::wire_time(new)));
    let ticks = h.ticks().await;
    assert_eq!(ticks.len(), 2);
    assert_eq!(
        h.tick(&keys::tick_key(id, new)).await.unwrap().state,
        "sleeping"
    );
    assert_eq!(h.fake.claim_count(), 0);
    assert!(h.runs().await.is_empty());
    assert_eq!(h.task(id).await.next_run_at, Some(new));
}

#[tokio::test]
async fn manual_runs_use_now_and_leave_the_schedule_alone() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let next = hour(Utc::now()) + Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(next))
        .await;
    let now = Utc.with_ymd_and_hms(2026, 8, 19, 12, 5, 0).unwrap() + Duration::milliseconds(750);

    let queued = h.runner.run_now(id, now).await.unwrap();
    assert!(queued.created);
    assert_eq!(
        queued.scheduled_for,
        Utc.with_ymd_and_hms(2026, 8, 19, 12, 5, 0).unwrap()
    );
    let again = h.runner.run_now(id, now).await.unwrap();
    assert!(!again.created);
    assert_eq!(again.task_id, queued.task_id);
    h.drain().await;

    let runs = h.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].key,
        format!("scheduled-task:{id}:2026-08-19T12:05:00Z")
    );
    let task = h.task(id).await;
    assert_eq!(task.next_run_at, Some(next));
    assert_eq!(task.last_enqueued_at, None);
    assert_eq!(task.last_run_id, Some(h.run_id(&runs[0].task_id).await));
    let claims = h.fake.claims.lock().unwrap().clone();
    assert!(claims[0].1.manual);
    // A manual tick does not chain.
    assert_eq!(h.ticks().await.len(), 1);
    let key = keys::manual_tick_key(id, queued.scheduled_for);
    assert_eq!(tick_result(&h.tick(&key).await.unwrap()).next_tick, None);

    // Disabled: the claim skips it silently.
    h.execute(
        "update scheduled_tasks set enabled = false, next_run_at = null where id = $1",
        id,
    )
    .await;
    h.runner
        .run_now(id, now + Duration::seconds(5))
        .await
        .unwrap();
    h.drain().await;
    assert_eq!(h.runs().await.len(), 1);
}

#[tokio::test]
async fn a_manual_run_in_the_same_second_as_the_schedule_does_not_suppress_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.runner.run_now(id, scheduled_for).await.unwrap();
    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    // One run (Rails: the same run key), and the occurrence was claimed.
    assert_eq!(h.runs().await.len(), 1);
    let task = h.task(id).await;
    assert_eq!(task.last_enqueued_at, Some(scheduled_for));
    assert!(task.next_run_at.unwrap() > Utc::now());
}

#[tokio::test]
async fn spawn_failures_are_recorded_and_stop_after_five_attempts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.spawner.faults.fail.store(100, Ordering::SeqCst);

    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    assert_eq!(h.spawner.requests.lock().unwrap().len(), 5);
    assert!(h.runs().await.is_empty());
    let results = h.fake.results();
    assert_eq!(results.len(), 5);
    assert!(
        results
            .iter()
            .all(|r| r.error.as_deref() == Some("internal server error"))
    );
    let task = h.task(id).await;
    assert_eq!(task.last_error.as_deref(), Some("internal server error"));
    assert_eq!(task.last_run_id, None);
    let result = tick_result(&h.tick(&keys::tick_key(id, scheduled_for)).await.unwrap());
    assert_eq!(result.outcome, TickOutcome::SpawnFailed);
    // The claim already advanced the schedule, so the next occurrence runs.
    assert_eq!(result.next_tick, task.next_run_at.map(keys::wire_time));
    assert!(result.next_tick.is_some());
}

#[tokio::test]
async fn an_undeliverable_task_records_the_error_and_keeps_its_schedule() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = hour(Utc::now()) - Duration::hours(1);
    let id = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    let other = h
        .insert_task(Some("0 * * * *"), "Etc/UTC", Some(scheduled_for))
        .await;
    h.fake.undeliverable.lock().unwrap().insert(id);

    h.runner.reconcile_all().await.unwrap();
    h.drain().await;

    // Other tasks still run after a channel task's bot leaves, and that
    // task is refused.
    let runs = h.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].key, keys::run_key(other, scheduled_for));
    let task = h.task(id).await;
    assert_eq!(
        task.last_error.as_deref(),
        Some("Slack delivery destination is no longer available to the task owner")
    );
    assert_eq!(task.last_enqueued_at, Some(scheduled_for));
    let next = hour(Utc::now()) + Duration::hours(1);
    assert_eq!(task.next_run_at, Some(next));
    let result = tick_result(&h.tick(&keys::tick_key(id, scheduled_for)).await.unwrap());
    assert_eq!(result.outcome, TickOutcome::Failed);
    assert_eq!(
        result.reason.as_deref(),
        Some("delivery_destination_unavailable")
    );
    assert_eq!(result.next_tick, Some(keys::wire_time(next)));
}

/// Runs `n` ticks of a chain whose claims happen exactly on time and returns
/// the run keys' times.
async fn dst_chain(
    cron: &str,
    timezone: &str,
    first: DateTime<Utc>,
    n: usize,
) -> Option<Vec<String>> {
    let h = Harness::new().await?;
    *h.fake.clock.lock().unwrap() = Clock::ScheduledFor;
    let id = h.insert_task(Some(cron), timezone, Some(first)).await;
    h.runner.reconcile_all().await.unwrap();
    for _ in 0..n {
        h.work_one().await;
    }
    let prefix = format!("scheduled-task:{id}:");
    let mut times: Vec<String> = h
        .runs()
        .await
        .into_iter()
        .map(|run| run.key.strip_prefix(&prefix).unwrap().to_owned())
        .collect();
    times.sort();
    Some(times)
}

#[tokio::test]
async fn dst_spring_forward_keeps_the_chain_advancing() {
    // 02:30 does not exist on 2024-03-10 in Los Angeles; the chain runs at the
    // next valid time and continues the next day.
    let first = Utc.with_ymd_and_hms(2024, 3, 8, 10, 30, 0).unwrap();
    let Some(times) = dst_chain("30 2 * * *", "America/Los_Angeles", first, 4).await else {
        return;
    };
    assert_eq!(
        times,
        [
            "2024-03-08T10:30:00Z",
            "2024-03-09T10:30:00Z",
            "2024-03-10T10:00:00Z",
            "2024-03-11T09:30:00Z"
        ]
    );
}

#[tokio::test]
async fn dst_fall_back_fires_once_for_the_repeated_hour() {
    // 01:30 happens twice on 2024-11-03 in Los Angeles; the task runs once.
    let first = Utc.with_ymd_and_hms(2024, 11, 2, 8, 30, 0).unwrap();
    let Some(times) = dst_chain("30 1 * * *", "America/Los_Angeles", first, 4).await else {
        return;
    };
    assert_eq!(
        times,
        [
            "2024-11-02T08:30:00Z",
            "2024-11-03T08:30:00Z",
            "2024-11-04T09:30:00Z",
            "2024-11-05T09:30:00Z"
        ]
    );
}

#[tokio::test]
async fn dst_in_another_zone_keeps_its_local_time() {
    // "30 2 * * *" in Europe/London across the March change.
    let first = Utc.with_ymd_and_hms(2024, 3, 29, 2, 30, 0).unwrap();
    let Some(times) = dst_chain("30 2 * * *", "Europe/London", first, 4).await else {
        return;
    };
    assert_eq!(
        times,
        [
            "2024-03-29T02:30:00Z",
            "2024-03-30T02:30:00Z",
            "2024-03-31T01:30:00Z",
            "2024-04-01T01:30:00Z"
        ]
    );
}

#[tokio::test]
async fn a_task_without_a_cron_but_with_next_run_at_fires_once() {
    // Due tasks are selected without looking at cron_expression.
    let Some(h) = Harness::new().await else {
        return;
    };
    let scheduled_for = minute(Utc::now()) - Duration::minutes(1);
    let id = h.insert_task(None, "Etc/UTC", Some(scheduled_for)).await;
    h.runner.reconcile_all().await.unwrap();
    h.drain().await;
    assert_eq!(h.runs().await.len(), 1);
    let task = h.task(id).await;
    assert_eq!(task.next_run_at, None);
    assert_eq!(tick_result(&h.ticks().await[0]).next_tick, None);
}
