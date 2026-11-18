//! The `scheduled_task.tick` task (RFC 0006a "Tick task").

use std::sync::Arc;

use absurd::TaskContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::{
    Inner,
    console::{ClaimOutcome, ClaimRequest, ConsoleError, ResultRequest},
    keys, spawner, store,
};

/// Params of a tick.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TickParams {
    pub scheduled_task_id: i64,
    pub scheduled_task_oid: String,
    /// Wire time (RFC 0006a "Time format").
    pub scheduled_for: String,
    #[serde(default)]
    pub manual: bool,
}

/// `outcome` of a finished tick.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TickOutcome {
    Spawned,
    SpawnFailed,
    Skipped,
    Failed,
}

/// The completed payload of a tick.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TickResult {
    pub scheduled_task_id: i64,
    pub scheduled_for: String,
    pub manual: bool,
    pub outcome: TickOutcome,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub run_id: Option<String>,
    pub run_created: Option<bool>,
    pub next_tick: Option<String>,
}

/// The `claim` checkpoint: the console's answer, or the pre-check's skip.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ClaimStep {
    outcome: ClaimOutcome,
    reason: Option<String>,
    error: Option<String>,
    /// `api_input` as JSON text (the checkpoint format in-flight ticks use).
    api_input: Option<String>,
    enabled: bool,
    next_run_at: Option<String>,
    /// `false` when the task no longer exists.
    found: bool,
}

impl ClaimStep {
    fn skip(reason: &str, found: bool, enabled: bool, next: Option<DateTime<Utc>>) -> Self {
        Self {
            outcome: ClaimOutcome::Skipped,
            reason: Some(reason.to_owned()),
            error: None,
            api_input: None,
            enabled,
            next_run_at: next.map(keys::wire_time),
            found,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SpawnStep {
    Spawned {
        run_id: String,
        created: bool,
    },
    Failed {
        error: String,
        #[serde(default)]
        retryable: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReportStep {
    found: bool,
    enabled: bool,
    next_run_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum TickError {
    #[error("invalid tick params: {0}")]
    Params(String),
    #[error(transparent)]
    Console(#[from] ConsoleError),
    #[error("console database read failed: {0}")]
    ConsoleDb(#[from] sqlx::Error),
    #[error("chaining the next tick failed: {0}")]
    Chain(#[source] crate::RunnerError),
}

fn failed(error: TickError) -> absurd::Error {
    absurd::Error::TaskFailed(Box::new(error))
}

fn log_console_error(params: &TickParams, step: &str, error: &ConsoleError) {
    if error.is_client_error() {
        tracing::error!(
            scheduled_task_id = params.scheduled_task_id,
            step,
            %error,
            "console rejected a scheduled task runner call; check the runner JWT and the console version"
        );
    } else {
        warn!(
            scheduled_task_id = params.scheduled_task_id,
            step,
            %error,
            "console scheduled task call failed; retrying"
        );
    }
}

pub(crate) async fn run(
    inner: &Arc<Inner>,
    params: TickParams,
    ctx: TaskContext,
) -> absurd::Result<Value> {
    let scheduled_for = keys::parse_wire_time(&params.scheduled_for).ok_or_else(|| {
        failed(TickError::Params(format!(
            "scheduled_for {:?}",
            params.scheduled_for
        )))
    })?;
    ctx.sleep_until("wait", scheduled_for).await?;

    let claim = claim_step(inner, &params, scheduled_for, &ctx).await?;
    let mut result = TickResult {
        scheduled_task_id: params.scheduled_task_id,
        scheduled_for: params.scheduled_for.clone(),
        manual: params.manual,
        outcome: match claim.outcome {
            ClaimOutcome::Claimed => TickOutcome::Spawned,
            ClaimOutcome::Skipped => TickOutcome::Skipped,
            ClaimOutcome::Failed => TickOutcome::Failed,
        },
        reason: claim.reason.clone(),
        error: claim.error.clone(),
        run_id: None,
        run_created: None,
        next_tick: None,
    };

    let mut report = None;
    if claim.outcome == ClaimOutcome::Claimed {
        let spawn = spawn_step(inner, &params, scheduled_for, &claim, &ctx).await?;
        let request = ResultRequest {
            scheduled_for: params.scheduled_for.clone(),
            manual: params.manual,
            run_id: None,
            error: None,
        };
        let request = match &spawn {
            SpawnStep::Spawned { run_id, created } => {
                result.run_id = Some(run_id.clone());
                result.run_created = Some(*created);
                ResultRequest {
                    run_id: Some(run_id.clone()),
                    ..request
                }
            }
            SpawnStep::Failed { error, .. } => {
                result.outcome = TickOutcome::SpawnFailed;
                result.error = Some(error.clone());
                ResultRequest {
                    error: Some(error.clone()),
                    ..request
                }
            }
        };
        let console = inner.console.clone();
        let oid = params.scheduled_task_oid.clone();
        let step_params = params.clone();
        report = Some(
            ctx.step("report", || async move {
                match console.result(&oid, &request).await {
                    Ok(Some(response)) => Ok(ReportStep {
                        found: true,
                        enabled: response.enabled,
                        next_run_at: response.next_run_at,
                    }),
                    Ok(None) => Ok(ReportStep {
                        found: false,
                        enabled: false,
                        next_run_at: None,
                    }),
                    Err(error) => {
                        log_console_error(&step_params, "report", &error);
                        Err(failed(error.into()))
                    }
                }
            })
            .await?,
        );
    }

    // Chain from the latest answer: the report's, else the claim's.
    let (found, enabled, next_run_at) = match &report {
        Some(report) => (report.found, report.enabled, report.next_run_at.clone()),
        None => (claim.found, claim.enabled, claim.next_run_at.clone()),
    };
    let next = next_run_at.as_deref().and_then(keys::parse_wire_time);
    if !params.manual
        && found
        && enabled
        && let Some(next) = next.filter(|next| *next > scheduled_for)
    {
        let chain_inner = inner.clone();
        let id = params.scheduled_task_id;
        result.next_tick = ctx
            .step("chain", || async move {
                crate::spawn_tick(&chain_inner, id, next, crate::TickKind::Scheduled)
                    .await
                    .map(|_| Some(keys::wire_time(next)))
                    .map_err(|error| failed(TickError::Chain(error)))
            })
            .await?;
    }

    info!(
        scheduled_task_id = result.scheduled_task_id,
        scheduled_for = %result.scheduled_for,
        manual = result.manual,
        outcome = ?result.outcome,
        reason = ?result.reason,
        run_id = ?result.run_id,
        next_tick = ?result.next_tick,
        "scheduled task tick finished"
    );
    serde_json::to_value(result).map_err(absurd::Error::from)
}

async fn claim_step(
    inner: &Arc<Inner>,
    params: &TickParams,
    scheduled_for: DateTime<Utc>,
    ctx: &TaskContext,
) -> absurd::Result<ClaimStep> {
    if !params.manual {
        wait_for_earlier_claims(inner, params, scheduled_for, ctx).await?;
    }
    let handle = ctx.begin_step::<ClaimStep>("claim").await?;
    if handle.done {
        return handle
            .state
            .ok_or_else(|| absurd::Error::InvalidOptions("completed step missing state".into()));
    }

    // Read-only pre-check: skip stale ticks without calling the console. The
    // console's compare-and-set stays authoritative.
    let row = store::find_task(&inner.console_db, params.scheduled_task_id)
        .await
        .map_err(|error| failed(error.into()))?;
    let precheck = match &row {
        None => Some(ClaimStep::skip("task_not_found", false, false, None)),
        Some(row) if !row.enabled => Some(ClaimStep::skip(
            "task_disabled",
            true,
            false,
            row.next_run_at,
        )),
        Some(row)
            if !params.manual
                && row.next_run_at != Some(scheduled_for)
                && row.last_enqueued_at != Some(scheduled_for) =>
        {
            Some(ClaimStep::skip("not_due", true, true, row.next_run_at))
        }
        Some(_) => None,
    };
    let step = match precheck {
        Some(skip) => skip,
        None => {
            let request = ClaimRequest {
                scheduled_for: params.scheduled_for.clone(),
                manual: params.manual,
            };
            match inner
                .console
                .claim(&params.scheduled_task_oid, &request)
                .await
            {
                Ok(Some(response)) => {
                    if response.outcome == ClaimOutcome::Claimed && response.api_input.is_none() {
                        let error = ConsoleError::InvalidBody("claimed without api_input".into());
                        log_console_error(params, "claim", &error);
                        return Err(failed(error.into()));
                    }
                    ClaimStep {
                        outcome: response.outcome,
                        reason: response.reason,
                        error: response.error,
                        api_input: response.api_input.map(|input| input.to_string()),
                        enabled: response.enabled,
                        next_run_at: response.next_run_at,
                        found: true,
                    }
                }
                Ok(None) => ClaimStep::skip("task_not_found", false, false, None),
                Err(error) => {
                    log_console_error(params, "claim", &error);
                    return Err(failed(error.into()));
                }
            }
        }
    };
    ctx.complete_step(handle, step).await
}

/// Holds this occurrence back while an earlier one of the same task has not
/// checkpointed its claim (see [`store::earlier_claim_pending`]).
///
/// The wait is bounded: the gate is only an ordering nicety, since the
/// console's compare-and-set decides every claim, so a later occurrence may
/// proceed once the bound expires (and says so). Each wait is a durable
/// sleep, releasing the worker so the earlier tick's own retry can run, and
/// the bound keeps the checkpoints it leaves behind finite.
async fn wait_for_earlier_claims(
    inner: &Arc<Inner>,
    params: &TickParams,
    scheduled_for: DateTime<Utc>,
    ctx: &TaskContext,
) -> absurd::Result<()> {
    let max_waits = inner.config.claim_wait_attempts;
    for wait in 0..max_waits {
        let pending = store::earlier_claim_pending(
            inner.client.pool(),
            &inner.config.queue_name,
            crate::TICK_TASK,
            params.scheduled_task_id,
            scheduled_for,
            ctx.task_id(),
        )
        .await
        .map_err(|error| failed(error.into()))?;
        if !pending {
            return Ok(());
        }
        info!(
            scheduled_task_id = params.scheduled_task_id,
            scheduled_for = %params.scheduled_for,
            wait = wait + 1,
            max_waits,
            "an earlier occurrence has not checkpointed its claim; waiting"
        );
        ctx.sleep_for("claim-wait", inner.config.claim_wait).await?;
    }
    if store::earlier_claim_pending(
        inner.client.pool(),
        &inner.config.queue_name,
        crate::TICK_TASK,
        params.scheduled_task_id,
        scheduled_for,
        ctx.task_id(),
    )
    .await
    .map_err(|error| failed(error.into()))?
    {
        // The earlier tick is stuck: claiming is still safe, the console
        // decides whether this occurrence is still due.
        warn!(
            scheduled_task_id = params.scheduled_task_id,
            scheduled_for = %params.scheduled_for,
            waits = max_waits,
            "an earlier occurrence never checkpointed its claim; claiming anyway"
        );
    }
    Ok(())
}

/// Spawns the run, retrying failures as Rails' `retry_on CentaurApiClient::Error,
/// wait: :polynomially_longer, attempts: 5` did: each failure is reported
/// (`last_error`), then the tick sleeps durably before the next attempt.
async fn spawn_step(
    inner: &Arc<Inner>,
    params: &TickParams,
    scheduled_for: DateTime<Utc>,
    claim: &ClaimStep,
    ctx: &TaskContext,
) -> absurd::Result<SpawnStep> {
    let max = inner.config.spawn_max_attempts.max(1);
    for attempt in 1..=max {
        let step_inner = inner.clone();
        let step_params = params.clone();
        let input = match claim.api_input.as_deref().map(serde_json::from_str) {
            Some(Ok(input)) => input,
            Some(Err(error)) => return Err(absurd::Error::Json(error)),
            None => serde_json::Value::Null,
        };
        let last = attempt == max;
        // One checkpoint per attempt ("spawn", "spawn#2", ...). A crash after
        // the spawn but before its checkpoint re-runs only this attempt, and
        // the run's idempotency key returns the same run.
        let outcome = ctx
            .step("spawn", || async move {
                let request = spawner::run_request(
                    input,
                    keys::run_key(step_params.scheduled_task_id, scheduled_for),
                );
                match step_inner.spawner.spawn_run(request).await {
                    Ok(run) => Ok(SpawnStep::Spawned {
                        run_id: run.run_id,
                        created: run.created,
                    }),
                    Err(error) => {
                        warn!(
                            scheduled_task_id = step_params.scheduled_task_id,
                            spawn_attempt = attempt,
                            retryable = error.retryable,
                            error = %error.message,
                            "scheduled task workflow run spawn failed"
                        );
                        if error.retryable && !last {
                            // The run records last_error on every
                            // failed attempt. Best effort: the final error is
                            // reported durably by the report step.
                            let request = ResultRequest {
                                scheduled_for: step_params.scheduled_for.clone(),
                                manual: step_params.manual,
                                run_id: None,
                                error: Some(error.message.clone()),
                            };
                            if let Err(report_error) = step_inner
                                .console
                                .result(&step_params.scheduled_task_oid, &request)
                                .await
                            {
                                log_console_error(&step_params, "report", &report_error);
                            }
                        }
                        Ok(SpawnStep::Failed {
                            error: error.message,
                            retryable: error.retryable,
                        })
                    }
                }
            })
            .await?;
        match outcome {
            SpawnStep::Failed {
                retryable: true, ..
            } if !last => {
                ctx.sleep_for("spawn-retry", (inner.config.spawn_retry_delay)(attempt))
                    .await?;
            }
            outcome => return Ok(outcome),
        }
    }
    unreachable!("the last attempt returns")
}

/// `polynomially_longer` without its random jitter: `executions**4 + 2`
/// seconds after the `executions`th failure (3 s, 18 s, 83 s, 258 s).
pub fn polynomially_longer(executions: i32) -> std::time::Duration {
    let executions = u64::try_from(executions.max(0)).unwrap_or(0);
    std::time::Duration::from_secs(executions.pow(4) + 2)
}
