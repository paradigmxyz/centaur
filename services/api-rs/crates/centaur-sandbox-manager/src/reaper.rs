//! Background garbage collection for leaked sandboxes.
//!
//! Sessions pause idle sandboxes (replicas to zero) but nothing stops them:
//! the pause timer lives in process memory and dies with the deploy, and
//! sandboxes whose sessions never go idle are retained forever. The reaper is
//! the restart-surviving backstop: it sweeps the backend's observed sandboxes
//! and stops any that outlived their welcome, releasing the sandbox, its
//! proxy resources, and its node pod slots.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use centaur_sandbox_core::{ObservedSandbox, SandboxResult, SandboxStatus};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::SandboxManager;

#[derive(Clone, Copy, Debug)]
pub struct SandboxReaperConfig {
    /// How often to sweep.
    pub interval: Duration,
    /// Stop sandboxes that have been suspended longer than this. `None`
    /// disables the idle sweep.
    pub idle_ttl: Option<Duration>,
    /// Stop any sandbox older than this regardless of status. `None` disables
    /// the max-lifetime sweep.
    pub max_lifetime: Option<Duration>,
}

impl SandboxReaperConfig {
    pub fn is_enabled(&self) -> bool {
        self.idle_ttl.is_some() || self.max_lifetime.is_some()
    }
}

pub struct SandboxReaper {
    manager: Arc<SandboxManager>,
    config: SandboxReaperConfig,
}

impl SandboxReaper {
    pub fn new(manager: Arc<SandboxManager>, config: SandboxReaperConfig) -> Self {
        Self { manager, config }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
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
            let Some(reason) = reap_reason(&observed, now, &self.config) else {
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
        Ok(reaped)
    }
}

fn reap_reason(
    observed: &ObservedSandbox,
    now: SystemTime,
    config: &SandboxReaperConfig,
) -> Option<&'static str> {
    if observed.status.is_terminal() {
        return None;
    }
    if let (Some(idle_ttl), Some(suspended_since)) = (config.idle_ttl, observed.suspended_since)
        && matches!(observed.status, SandboxStatus::Suspended)
        && now
            .duration_since(suspended_since)
            .is_ok_and(|age| age >= idle_ttl)
    {
        return Some("idle_ttl");
    }
    if let (Some(max_lifetime), Some(created_at)) = (config.max_lifetime, observed.created_at)
        && now
            .duration_since(created_at)
            .is_ok_and(|age| age >= max_lifetime)
    {
        return Some("max_lifetime");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(idle_ttl: Option<Duration>, max_lifetime: Option<Duration>) -> SandboxReaperConfig {
        SandboxReaperConfig {
            interval: Duration::from_secs(60),
            idle_ttl,
            max_lifetime,
        }
    }

    fn observed(status: SandboxStatus) -> ObservedSandbox {
        ObservedSandbox::new("sandbox-1", "fake", status)
    }

    #[test]
    fn reaps_suspended_sandbox_past_idle_ttl() {
        let now = SystemTime::now();
        let sandbox = observed(SandboxStatus::Suspended)
            .with_suspended_since(Some(now - Duration::from_secs(7200)));

        let reason = reap_reason(
            &sandbox,
            now,
            &config(Some(Duration::from_secs(3600)), None),
        );

        assert_eq!(reason, Some("idle_ttl"));
    }

    #[test]
    fn keeps_suspended_sandbox_within_idle_ttl() {
        let now = SystemTime::now();
        let sandbox = observed(SandboxStatus::Suspended)
            .with_suspended_since(Some(now - Duration::from_secs(60)));

        let reason = reap_reason(
            &sandbox,
            now,
            &config(Some(Duration::from_secs(3600)), None),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn keeps_suspended_sandbox_without_pause_timestamp() {
        let now = SystemTime::now();
        let sandbox = observed(SandboxStatus::Suspended);

        let reason = reap_reason(
            &sandbox,
            now,
            &config(Some(Duration::from_secs(3600)), None),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn reaps_running_sandbox_past_max_lifetime() {
        let now = SystemTime::now();
        let sandbox = observed(SandboxStatus::Running)
            .with_created_at(Some(now - Duration::from_secs(100_000)));

        let reason = reap_reason(
            &sandbox,
            now,
            &config(None, Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, Some("max_lifetime"));
    }

    #[test]
    fn keeps_running_sandbox_within_max_lifetime() {
        let now = SystemTime::now();
        let sandbox =
            observed(SandboxStatus::Running).with_created_at(Some(now - Duration::from_secs(60)));

        let reason = reap_reason(
            &sandbox,
            now,
            &config(None, Some(Duration::from_secs(86_400))),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn ignores_terminal_sandboxes() {
        let now = SystemTime::now();
        let sandbox =
            observed(SandboxStatus::Gone).with_created_at(Some(now - Duration::from_secs(100_000)));

        let reason = reap_reason(
            &sandbox,
            now,
            &config(
                Some(Duration::from_secs(3600)),
                Some(Duration::from_secs(86_400)),
            ),
        );

        assert_eq!(reason, None);
    }

    #[test]
    fn disabled_config_reaps_nothing() {
        let now = SystemTime::now();
        let sandbox = observed(SandboxStatus::Suspended)
            .with_created_at(Some(now - Duration::from_secs(100_000)))
            .with_suspended_since(Some(now - Duration::from_secs(100_000)));
        let config = config(None, None);

        assert!(!config.is_enabled());
        assert_eq!(reap_reason(&sandbox, now, &config), None);
    }
}
