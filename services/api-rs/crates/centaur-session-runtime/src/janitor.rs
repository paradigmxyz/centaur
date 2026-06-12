//! Periodic sandbox janitor.
//!
//! Two leak classes survive every other lifecycle mechanism:
//!
//! 1. Backend sandboxes with no `sessions.sandbox_id` or warm-pool reference.
//!    Desired state lives in process memory, so a control-plane restart (or a
//!    create/register race) leaves running sandboxes nothing will ever stop.
//! 2. Sessions whose idle pause never fired because the per-execution timer
//!    task (`spawn_idle_pause`) died with the process.
//!
//! The janitor closes both: it pauses sessions idle past a backstop TTL and
//! stops sandboxes that stay unreferenced across two consecutive passes. The
//! two-pass confirmation replaces a timestamp grace so freshly created
//! sandboxes that have not registered yet are never reaped.

use std::collections::HashSet;
use std::time::Duration;

use centaur_sandbox_core::{ObservedSandbox, SandboxId, SandboxStatus};
use centaur_session_core::ThreadKey;
use tracing::{info, warn};

use crate::{RuntimeContext, SessionRuntime, SessionRuntimeError, record_idle_pause};

/// Configuration for [`SessionRuntime::run_sandbox_janitor`].
#[derive(Clone, Debug)]
pub struct SandboxJanitorConfig {
    /// Time between janitor passes. Doubles as the orphan grace period: an
    /// unreferenced sandbox is only stopped after staying unreferenced for a
    /// full interval, so this must exceed the worst-case gap between a
    /// sandbox turning `Running` and its reference landing in the store
    /// (i.e. the readiness wait inside `create_running`).
    pub interval: Duration,
    /// Pause sandboxes whose latest execution finished at least this long ago
    /// and have no active execution. Backstop for lost idle-pause timers;
    /// `None` disables the idle arm.
    pub idle_backstop: Option<Duration>,
}

impl SessionRuntime {
    /// Run the sandbox janitor until the process exits.
    ///
    /// Spawn with `tokio::spawn(runtime.clone().run_sandbox_janitor(config))`.
    pub async fn run_sandbox_janitor(self, config: SandboxJanitorConfig) {
        let ctx = self.context();
        let mut pending: HashSet<String> = HashSet::new();
        let mut interval = tokio::time::interval(config.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate tick so startup creations register before the
        // first pass marks anything as pending.
        interval.tick().await;
        loop {
            interval.tick().await;
            janitor_tick(&ctx, config.idle_backstop, &mut pending).await;
        }
    }
}

async fn janitor_tick(
    ctx: &RuntimeContext,
    idle_backstop: Option<Duration>,
    pending: &mut HashSet<String>,
) {
    // The arms are independent; an idle-arm failure must not starve the
    // orphan sweep (and vice versa).
    if let Some(backstop) = idle_backstop
        && let Err(error) = pause_idle_sessions(ctx, backstop).await
    {
        warn!(
            component = "sandbox_janitor",
            event = "idle_backstop_pass_failed",
            %error,
            "idle backstop pass failed"
        );
    }
    if let Err(error) = reap_orphan_sandboxes(ctx, pending).await {
        warn!(
            component = "sandbox_janitor",
            event = "orphan_sweep_pass_failed",
            %error,
            "orphan sweep pass failed"
        );
    }
}

/// Pause sandboxes for sessions idle past the backstop TTL.
///
/// Reuses [`record_idle_pause`], which re-validates the latest execution and
/// sandbox status before acting, so racing a live turn is safe: the guard
/// refuses when the execution is no longer the latest or the sandbox is not
/// running.
async fn pause_idle_sessions(
    ctx: &RuntimeContext,
    backstop: Duration,
) -> Result<(), SessionRuntimeError> {
    let idle = ctx.store.list_idle_sandbox_sessions(backstop).await?;
    for session in idle {
        let thread_key = match ThreadKey::parse(session.thread_key.clone()) {
            Ok(thread_key) => thread_key,
            Err(error) => {
                warn!(
                    component = "sandbox_janitor",
                    event = "idle_backstop_thread_key_invalid",
                    thread_key = %session.thread_key,
                    %error,
                    "skipping idle session with unparseable thread key"
                );
                continue;
            }
        };
        if let Err(error) = record_idle_pause(
            ctx,
            &thread_key,
            &session.execution_id,
            &session.sandbox_id,
            backstop,
        )
        .await
        {
            warn!(
                component = "sandbox_janitor",
                event = "idle_backstop_pause_failed",
                thread_key = %thread_key,
                sandbox_id = %session.sandbox_id,
                %error,
                "idle backstop failed to pause sandbox; will retry next pass"
            );
        } else {
            info!(
                component = "sandbox_janitor",
                event = "idle_backstop_paused",
                thread_key = %thread_key,
                sandbox_id = %session.sandbox_id,
                "idle backstop paused sandbox"
            );
        }
    }
    Ok(())
}

/// Stop backend sandboxes that stayed unreferenced for two consecutive passes.
async fn reap_orphan_sandboxes(
    ctx: &RuntimeContext,
    pending: &mut HashSet<String>,
) -> Result<(), SessionRuntimeError> {
    let observed = ctx.manager.list_observed().await?;
    let referenced: HashSet<String> = ctx
        .store
        .list_referenced_sandbox_ids()
        .await?
        .into_iter()
        .collect();
    let (reap_now, next_pending) = orphan_reap_candidates(&observed, &referenced, pending);
    *pending = next_pending;
    for id in reap_now {
        match ctx.manager.stop(&id).await {
            Ok(()) => {
                ctx.sandbox_pipes.lock().await.remove(id.as_str());
                info!(
                    component = "sandbox_janitor",
                    event = "orphan_sandbox_reaped",
                    sandbox_id = %id.as_str(),
                    "stopped sandbox with no session or warm-pool reference"
                );
            }
            Err(error) => {
                // Keep it pending so the next pass retries the stop.
                pending.insert(id.as_str().to_owned());
                warn!(
                    component = "sandbox_janitor",
                    event = "orphan_sandbox_reap_failed",
                    sandbox_id = %id.as_str(),
                    %error,
                    "failed to stop orphan sandbox; will retry next pass"
                );
            }
        }
    }
    Ok(())
}

/// Pure selection: which observed sandboxes to stop now, and which to mark
/// pending for the next pass.
///
/// A sandbox is stopped only when it is running or suspended, unreferenced,
/// and was already unreferenced on the previous pass (`pending`). Anything
/// newly unreferenced goes into the returned pending set instead, giving
/// in-flight creations a full interval to register their reference; `Created`
/// (still scheduling/pulling) is skipped outright because creation can
/// legitimately outlast an interval before `create_running` returns.
fn orphan_reap_candidates(
    observed: &[ObservedSandbox],
    referenced: &HashSet<String>,
    pending: &HashSet<String>,
) -> (Vec<SandboxId>, HashSet<String>) {
    let mut reap_now = Vec::new();
    let mut next_pending = HashSet::new();
    for sandbox in observed {
        if sandbox.status.is_terminal() || matches!(sandbox.status, SandboxStatus::Created) {
            continue;
        }
        let id = sandbox.id.as_str();
        if referenced.contains(id) {
            continue;
        }
        if pending.contains(id) {
            reap_now.push(sandbox.id.clone());
        } else {
            next_pending.insert(id.to_owned());
        }
    }
    (reap_now, next_pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(id: &str, status: SandboxStatus) -> ObservedSandbox {
        ObservedSandbox::new(id, "test", status)
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn newly_unreferenced_sandbox_is_marked_pending_not_reaped() {
        let sandboxes = [observed("sb-1", SandboxStatus::Running)];

        let (reap, pending) = orphan_reap_candidates(&sandboxes, &set(&[]), &set(&[]));

        assert!(reap.is_empty());
        assert_eq!(pending, set(&["sb-1"]));
    }

    #[test]
    fn sandbox_unreferenced_for_two_passes_is_reaped() {
        let sandboxes = [observed("sb-1", SandboxStatus::Running)];

        let (reap, pending) = orphan_reap_candidates(&sandboxes, &set(&[]), &set(&["sb-1"]));

        assert_eq!(
            reap.iter().map(SandboxId::as_str).collect::<Vec<_>>(),
            ["sb-1"]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn referenced_sandbox_is_never_reaped_even_when_pending() {
        let sandboxes = [observed("sb-1", SandboxStatus::Running)];

        let (reap, pending) = orphan_reap_candidates(&sandboxes, &set(&["sb-1"]), &set(&["sb-1"]));

        assert!(reap.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn terminal_sandboxes_are_ignored() {
        let sandboxes = [
            observed("sb-stopped", SandboxStatus::Stopped),
            observed("sb-gone", SandboxStatus::Gone),
        ];

        let (reap, pending) = orphan_reap_candidates(&sandboxes, &set(&[]), &set(&[]));

        assert!(reap.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn suspended_unreferenced_sandbox_is_reaped() {
        let sandboxes = [observed("sb-1", SandboxStatus::Suspended)];

        let (reap, _) = orphan_reap_candidates(&sandboxes, &set(&[]), &set(&["sb-1"]));

        assert_eq!(reap.len(), 1);
    }

    #[test]
    fn created_sandbox_is_never_reaped_or_marked_pending() {
        let sandboxes = [observed("sb-new", SandboxStatus::Created)];

        let (reap, pending) = orphan_reap_candidates(&sandboxes, &set(&[]), &set(&["sb-new"]));

        assert!(reap.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_entry_for_vanished_sandbox_is_dropped() {
        let (reap, pending) = orphan_reap_candidates(&[], &set(&[]), &set(&["sb-old"]));

        assert!(reap.is_empty());
        assert!(pending.is_empty());
    }
}
