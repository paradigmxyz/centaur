use std::{collections::BTreeSet, time::Duration};

use centaur_sandbox_core::{ObservedSandbox, SandboxError, SandboxId, SandboxStatus};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::{RuntimeContext, SessionRuntimeError, record_idle_pause};

const COMPONENT_LABEL: &str = "centaur.ai/component";
const WORKFLOW_RUN_COMPONENT: &str = "workflow-run";

#[derive(Clone, Copy, Debug)]
pub struct SessionSandboxCleanupConfig {
    /// How often to sweep. `None` disables the cleanup worker entirely.
    pub interval: Option<Duration>,
    /// Pause session sandboxes whose latest execution has been terminal longer
    /// than this. `None` disables the idle backstop arm.
    pub idle_backstop: Option<Duration>,
}

impl SessionSandboxCleanupConfig {
    pub fn is_enabled(&self) -> bool {
        self.interval.is_some()
    }
}

#[derive(Debug, Default)]
pub struct SessionSandboxCleanupReport {
    pub retired_vacant: usize,
    pub failed_vacant_retirements: usize,
    pub stopped_orphans: usize,
    pub failed_orphans: usize,
    pub idle_pause_attempts: usize,
    pub failed_idle_pauses: usize,
}

pub struct SessionSandboxCleanupWorker {
    ctx: RuntimeContext,
    config: SessionSandboxCleanupConfig,
    pending_orphans: BTreeSet<String>,
    pending_vacant: BTreeSet<String>,
}

impl SessionSandboxCleanupWorker {
    pub(crate) fn new(ctx: RuntimeContext, config: SessionSandboxCleanupConfig) -> Self {
        Self {
            ctx,
            config,
            pending_orphans: BTreeSet::new(),
            pending_vacant: BTreeSet::new(),
        }
    }

    pub(crate) fn spawn(mut self) {
        let Some(interval_duration) = self.config.interval else {
            return;
        };
        tokio::spawn(async move {
            let mut tick = interval(interval_duration);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if let Err(error) = self.reap_once().await {
                    warn!(%error, "session sandbox cleanup worker sweep failed");
                }
            }
        });
    }

    pub(crate) async fn reap_once(
        &mut self,
    ) -> Result<SessionSandboxCleanupReport, SessionRuntimeError> {
        let mut report = SessionSandboxCleanupReport::default();
        // One observation for both sandbox arms. They do different things to
        // a sandbox, so they must not disagree about what they saw.
        let observed = self.ctx.manager.list_observed().await?;
        self.retire_vacant_sandboxes(&observed, &mut report).await?;
        self.reap_unreferenced_sandboxes(&observed, &mut report)
            .await?;
        self.pause_idle_sandboxes(&mut report).await?;
        Ok(report)
    }

    /// Pause sandboxes whose record asks for a process the backend does not
    /// have.
    ///
    /// Not counting a vacant sandbox against the running cap is the actual
    /// capacity fix; this is the other half. Left alone the record keeps
    /// requesting a replica that never arrives, so reconciliation reports
    /// drift on every pass and the CR never settles.
    ///
    /// Pausing sets `replicas: 0`, which keeps the CR, its state volume and
    /// its proxy, so the owning session resumes with its workspace intact.
    /// Stopping would delete the volume and every uncommitted change on it.
    /// This path pauses and never stops, whether or not a session still
    /// references the sandbox.
    ///
    /// Two consecutive sweeps, mirroring the orphan arm: a fresh create is
    /// briefly vacant between the CR landing and its pod being scheduled, and
    /// pausing that would fight the create it is racing.
    async fn retire_vacant_sandboxes(
        &mut self,
        observed: &[ObservedSandbox],
        report: &mut SessionSandboxCleanupReport,
    ) -> Result<(), SessionRuntimeError> {
        for sandbox_id in select_vacant_retire_candidates(observed, &mut self.pending_vacant) {
            let id = SandboxId::new(sandbox_id.clone());
            match self.ctx.manager.pause(&id).await {
                Ok(()) | Err(SandboxError::NotFound(_)) => {
                    self.ctx.sandbox_pipes.remove(&sandbox_id);
                    self.pending_vacant.remove(&sandbox_id);
                    report.retired_vacant += 1;
                    info!(
                        sandbox_id,
                        reason = "pod_absent",
                        "session sandbox cleanup worker paused vacant sandbox"
                    );
                }
                Err(error) => {
                    report.failed_vacant_retirements += 1;
                    warn!(
                        sandbox_id,
                        %error,
                        "session sandbox cleanup worker failed to pause vacant sandbox"
                    );
                }
            }
        }
        Ok(())
    }

    async fn reap_unreferenced_sandboxes(
        &mut self,
        observed: &[ObservedSandbox],
        report: &mut SessionSandboxCleanupReport,
    ) -> Result<(), SessionRuntimeError> {
        let referenced = self
            .ctx
            .store
            .list_referenced_sandbox_ids()
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let candidates =
            select_orphan_reap_candidates(observed, &referenced, &mut self.pending_orphans);

        for sandbox_id in candidates {
            let id = SandboxId::new(sandbox_id.clone());
            match self.ctx.manager.stop(&id).await {
                Ok(()) | Err(SandboxError::NotFound(_)) => {
                    self.ctx.sandbox_pipes.remove(&sandbox_id);
                    self.pending_orphans.remove(&sandbox_id);
                    report.stopped_orphans += 1;
                    info!(
                        sandbox_id,
                        reason = "unreferenced",
                        "session sandbox cleanup worker stopped orphaned sandbox"
                    );
                }
                Err(error) => {
                    report.failed_orphans += 1;
                    warn!(
                        sandbox_id,
                        %error,
                        "session sandbox cleanup worker failed to stop orphaned sandbox"
                    );
                }
            }
        }

        Ok(())
    }

    async fn pause_idle_sandboxes(
        &self,
        report: &mut SessionSandboxCleanupReport,
    ) -> Result<(), SessionRuntimeError> {
        let Some(idle_backstop) = self.config.idle_backstop else {
            return Ok(());
        };
        for candidate in self
            .ctx
            .store
            .list_idle_sandbox_candidates(idle_backstop)
            .await?
        {
            report.idle_pause_attempts += 1;
            if let Err(error) = record_idle_pause(
                &self.ctx,
                &candidate.thread_key,
                &candidate.execution_id,
                &candidate.sandbox_id,
                candidate.idle_timeout,
            )
            .await
            {
                report.failed_idle_pauses += 1;
                warn!(
                    thread_key = %candidate.thread_key,
                    execution_id = %candidate.execution_id,
                    sandbox_id = %candidate.sandbox_id,
                    %error,
                    "session sandbox cleanup worker failed to pause idle sandbox"
                );
            }
        }
        Ok(())
    }
}

fn select_vacant_retire_candidates(
    observed: &[ObservedSandbox],
    pending_vacant: &mut BTreeSet<String>,
) -> Vec<String> {
    let current = observed
        .iter()
        .filter(|sandbox| matches!(sandbox.status, SandboxStatus::Vacant))
        .map(|sandbox| sandbox.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    let candidates = current
        .intersection(pending_vacant)
        .cloned()
        .collect::<Vec<_>>();
    *pending_vacant = current;
    candidates
}

fn select_orphan_reap_candidates(
    observed: &[ObservedSandbox],
    referenced: &BTreeSet<String>,
    pending_orphans: &mut BTreeSet<String>,
) -> Vec<String> {
    let current_orphans = observed
        .iter()
        .filter(|sandbox| orphan_reap_eligible(sandbox, referenced))
        .map(|sandbox| sandbox.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();

    let candidates = current_orphans
        .intersection(pending_orphans)
        .cloned()
        .collect::<Vec<_>>();
    *pending_orphans = current_orphans;
    candidates
}

fn orphan_reap_eligible(sandbox: &ObservedSandbox, referenced: &BTreeSet<String>) -> bool {
    if referenced.contains(sandbox.id.as_str()) {
        return false;
    }
    if sandbox.labels.get(COMPONENT_LABEL).map(String::as_str) == Some(WORKFLOW_RUN_COMPONENT) {
        return false;
    }
    // Vacant belongs to the retire arm, which pauses rather than stops so the
    // state volume survives. Once that arm has run the CR reads Suspended,
    // and this arm can reap it on the usual two passes if nothing references
    // it by then.
    !matches!(
        sandbox.status,
        SandboxStatus::Created
            | SandboxStatus::Stopped
            | SandboxStatus::Gone
            | SandboxStatus::Vacant
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn observed(id: &str, status: SandboxStatus) -> ObservedSandbox {
        ObservedSandbox::new(SandboxId::new(id), "test", status)
    }

    fn workflow_observed(id: &str, status: SandboxStatus) -> ObservedSandbox {
        ObservedSandbox::new(SandboxId::new(id), "test", status).with_labels(BTreeMap::from([(
            COMPONENT_LABEL.to_owned(),
            WORKFLOW_RUN_COMPONENT.to_owned(),
        )]))
    }

    fn referenced(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn vacant_retire_requires_two_consecutive_passes() {
        // A fresh create is briefly vacant between the CR landing and its pod
        // being scheduled; one pass must not pause the create it is racing.
        let observed = [observed("asbx-1", SandboxStatus::Vacant)];
        let mut pending = BTreeSet::new();

        assert_eq!(
            select_vacant_retire_candidates(&observed, &mut pending),
            Vec::<String>::new()
        );
        assert_eq!(
            select_vacant_retire_candidates(&observed, &mut pending),
            vec!["asbx-1".to_owned()]
        );
    }

    #[test]
    fn a_sandbox_that_gets_its_pod_back_is_not_retired() {
        let mut pending = BTreeSet::new();
        select_vacant_retire_candidates(&[observed("asbx-1", SandboxStatus::Vacant)], &mut pending);
        assert_eq!(
            select_vacant_retire_candidates(
                &[observed("asbx-1", SandboxStatus::Running)],
                &mut pending
            ),
            Vec::<String>::new()
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn only_vacant_sandboxes_are_retired() {
        let observed = [
            observed("asbx-running", SandboxStatus::Running),
            observed("asbx-created", SandboxStatus::Created),
            observed("asbx-suspended", SandboxStatus::Suspended),
            observed("asbx-vacant", SandboxStatus::Vacant),
        ];
        let mut pending = BTreeSet::new();
        select_vacant_retire_candidates(&observed, &mut pending);
        assert_eq!(
            select_vacant_retire_candidates(&observed, &mut pending),
            vec!["asbx-vacant".to_owned()]
        );
    }

    #[test]
    fn vacant_sandboxes_are_left_to_the_retire_arm() {
        // The orphan arm stops, which deletes the state volume. A vacant
        // sandbox is paused instead, and only becomes reapable here once it
        // reads Suspended.
        let observed = [observed("asbx-vacant", SandboxStatus::Vacant)];
        let mut pending = BTreeSet::new();

        select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending);
        assert_eq!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending),
            Vec::<String>::new()
        );
    }

    #[test]
    fn orphan_reap_requires_two_consecutive_passes() {
        let observed = [observed("asbx-1", SandboxStatus::Running)];
        let mut pending = BTreeSet::new();

        assert_eq!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending),
            Vec::<String>::new()
        );
        assert_eq!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending),
            vec!["asbx-1".to_owned()]
        );
    }

    #[test]
    fn referenced_sandbox_rescues_pending_orphan() {
        let observed = [observed("asbx-1", SandboxStatus::Running)];
        let mut pending = BTreeSet::new();

        select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending);
        assert_eq!(
            select_orphan_reap_candidates(&observed, &referenced(&["asbx-1"]), &mut pending),
            Vec::<String>::new()
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn created_and_terminal_sandboxes_are_not_reaped() {
        let observed = [
            observed("asbx-created", SandboxStatus::Created),
            observed("asbx-stopped", SandboxStatus::Stopped),
            observed("asbx-gone", SandboxStatus::Gone),
        ];
        let mut pending = BTreeSet::from([
            "asbx-created".to_owned(),
            "asbx-stopped".to_owned(),
            "asbx-gone".to_owned(),
        ]);

        assert_eq!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending),
            Vec::<String>::new()
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn failed_stop_stays_pending_for_retry() {
        let observed = [observed("asbx-1", SandboxStatus::Running)];
        let mut pending = BTreeSet::from(["asbx-1".to_owned()]);

        assert_eq!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending),
            vec!["asbx-1".to_owned()]
        );
        assert!(pending.contains("asbx-1"));
    }

    #[test]
    fn vanished_pending_orphan_is_dropped() {
        let mut pending = BTreeSet::from(["asbx-1".to_owned()]);

        assert_eq!(
            select_orphan_reap_candidates(&[], &referenced(&[]), &mut pending),
            Vec::<String>::new()
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn workflow_sandbox_is_not_owned_by_session_cleanup() {
        let observed = [workflow_observed("asbx-workflow", SandboxStatus::Running)];
        let mut pending = BTreeSet::new();

        assert!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending).is_empty()
        );
        assert!(
            select_orphan_reap_candidates(&observed, &referenced(&[]), &mut pending).is_empty()
        );
        assert!(pending.is_empty());
    }
}
