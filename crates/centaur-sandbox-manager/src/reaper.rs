//! Background garbage collection for leaked sandboxes.
//!
//! Sessions pause idle sandboxes (replicas to zero), but paused sandboxes and
//! sandboxes whose sessions never go idle still need a restart-surviving
//! backstop. The reaper sweeps the backend's observed sandboxes and stops any
//! that exceed the configured max lifetime, releasing the sandbox, its proxy
//! resources, and its node pod slots. That includes sandboxes whose agent
//! exited (`Stopped`): sessions replace rather than reuse them, so nothing
//! else ever releases their backend resources. A warm-pool sandbox's lifetime
//! starts when a session claims it, not when the pool created it, and a
//! sandbox whose session has a queued or running execution is left for a later
//! sweep. Each sweep also deletes iron-proxy
//! resources whose sandbox no longer has a live Sandbox, the orphan class no
//! observed-sandbox path can reach.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use centaur_sandbox_core::{ObservedSandbox, SandboxResult, SandboxStatus};
use centaur_session_sqlx::{PgSessionStore, SandboxReapState};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::SandboxManager;

#[derive(Clone, Copy, Debug)]
pub struct SandboxReaperConfig {
    /// How often to sweep.
    pub interval: Duration,
    /// Minimum age of an iron-proxy resource whose Sandbox no longer exists
    /// before the orphan sweep may delete it.
    pub orphan_sweep_grace: Duration,
    /// Stop any idle sandbox older than this, measured from its warm-pool
    /// claim when it has one. `None` disables the max-lifetime sweep.
    pub max_lifetime: Option<Duration>,
}

impl SandboxReaperConfig {
    /// The orphan sweep runs whenever a sweep interval is configured, so the
    /// reaper is enabled even when the max-lifetime sweep is disabled.
    pub fn is_enabled(&self) -> bool {
        self.interval > Duration::ZERO || self.max_lifetime.is_some()
    }
}

pub struct SandboxReaper {
    manager: Arc<SandboxManager>,
    store: PgSessionStore,
    config: SandboxReaperConfig,
}

impl SandboxReaper {
    pub fn new(
        manager: Arc<SandboxManager>,
        store: PgSessionStore,
        config: SandboxReaperConfig,
    ) -> Self {
        Self {
            manager,
            store,
            config,
        }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            // Orphans left by a dead control plane are reached sooner when
            // the first sweep runs at startup rather than after the interval.
            if let Err(error) = self.reap_once().await {
                warn!(%error, "initial sandbox reaper sweep failed");
            }
            let mut tick = interval(self.config.interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = self.reap_once().await {
                    warn!(%error, "sandbox reaper sweep failed");
                }
            }
        });
    }

    /// Sweep once and return how many sandboxes were stopped. A failed stop is
    /// logged and skipped so one wedged sandbox cannot stall the sweep.
    pub async fn reap_once(&self) -> SandboxResult<usize> {
        let now = SystemTime::now();
        let mut reaped = 0;
        for observed in self.manager.list_observed().await? {
            // Durable state can only defer a reap, so skip the lookup for
            // sandboxes that are not expired by backend age alone.
            if reap_reason(&observed, &SandboxReapState::default(), now, &self.config).is_none() {
                continue;
            }
            let state = match self.store.sandbox_reap_state(observed.id.as_str()).await {
                Ok(state) => state,
                Err(error) => {
                    warn!(
                        sandbox_id = %observed.id.as_str(),
                        %error,
                        "failed to load sandbox reap state; skipping"
                    );
                    continue;
                }
            };
            let Some(reason) = reap_reason(&observed, &state, now, &self.config) else {
                continue;
            };
            match self.manager.stop(&observed.id).await {
                Ok(()) => {
                    reaped += 1;
                    info!(
                        sandbox_id = %observed.id.as_str(),
                        reason,
                        "reaped expired sandbox"
                    );
                }
                Err(error) => {
                    warn!(
                        sandbox_id = %observed.id.as_str(),
                        reason,
                        %error,
                        "failed to reap expired sandbox"
                    );
                }
            }
        }
        let orphaned = self
            .manager
            .reap_orphan_iron_proxy_resources(self.config.orphan_sweep_grace)
            .await?;
        if !orphaned.is_empty() {
            info!(?orphaned, "reaped orphaned iron-proxy resources");
        }
        Ok(reaped)
    }
}

fn reap_reason(
    observed: &ObservedSandbox,
    state: &SandboxReapState,
    now: SystemTime,
    config: &SandboxReaperConfig,
) -> Option<&'static str> {
    if observed.status == SandboxStatus::Gone || state.has_active_execution {
        return None;
    }
    let started_at = state.claimed_at.or(observed.created_at);
    if let (Some(max_lifetime), Some(started_at)) = (config.max_lifetime, started_at)
        && now
            .duration_since(started_at)
            .is_ok_and(|age| age >= max_lifetime)
    {
        return Some("max_lifetime");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_lifetime: Option<Duration>) -> SandboxReaperConfig {
        SandboxReaperConfig {
            interval: Duration::from_secs(60),
            orphan_sweep_grace: Duration::from_secs(600),
            max_lifetime,
        }
    }

    fn observed(status: centaur_sandbox_core::SandboxStatus) -> ObservedSandbox {
        ObservedSandbox::new("sandbox-1", "fake", status)
    }

    #[test]
    fn reaps_running_sandbox_past_max_lifetime() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Running)
            .with_created_at(Some(now - Duration::from_secs(100_000)));

        let reason = reap_reason(
            &sandbox,
            &SandboxReapState::default(),
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, Some("max_lifetime"));
    }

    #[test]
    fn reaps_suspended_sandbox_past_max_lifetime() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Suspended)
            .with_created_at(Some(now - Duration::from_secs(100_000)))
            .with_suspended_since(Some(now - Duration::from_secs(60)));

        let reason = reap_reason(
            &sandbox,
            &SandboxReapState::default(),
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, Some("max_lifetime"));
    }

    #[test]
    fn keeps_running_sandbox_within_max_lifetime() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Running)
            .with_created_at(Some(now - Duration::from_secs(60)));

        let reason = reap_reason(
            &sandbox,
            &SandboxReapState::default(),
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn reaps_stopped_sandbox_past_max_lifetime() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Stopped)
            .with_created_at(Some(now - Duration::from_secs(100_000)));

        let reason = reap_reason(
            &sandbox,
            &SandboxReapState::default(),
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, Some("max_lifetime"));
    }

    #[test]
    fn ignores_gone_sandboxes() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Gone)
            .with_created_at(Some(now - Duration::from_secs(100_000)));

        let reason = reap_reason(
            &sandbox,
            &SandboxReapState::default(),
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn disabled_max_lifetime_reaps_nothing_by_age() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Suspended)
            .with_created_at(Some(now - Duration::from_secs(100_000)))
            .with_suspended_since(Some(now - Duration::from_secs(100_000)));
        let config = config(None);

        assert_eq!(
            reap_reason(&sandbox, &SandboxReapState::default(), now, &config),
            None
        );
    }

    #[test]
    fn warm_sandbox_lifetime_starts_at_claim() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Running)
            .with_created_at(Some(now - Duration::from_secs(100_000)));
        let state = SandboxReapState {
            claimed_at: Some(now - Duration::from_secs(60)),
            has_active_execution: false,
        };

        let reason = reap_reason(
            &sandbox,
            &state,
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn keeps_expired_sandbox_with_active_execution() {
        let now = SystemTime::now();
        let sandbox = observed(centaur_sandbox_core::SandboxStatus::Running)
            .with_created_at(Some(now - Duration::from_secs(100_000)));
        let state = SandboxReapState {
            claimed_at: Some(now - Duration::from_secs(100_000)),
            has_active_execution: true,
        };

        let reason = reap_reason(
            &sandbox,
            &state,
            now,
            &config(Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn orphan_sweep_keeps_the_reaper_enabled_without_max_lifetime() {
        let config = config(None);
        assert!(config.is_enabled());
    }
}
