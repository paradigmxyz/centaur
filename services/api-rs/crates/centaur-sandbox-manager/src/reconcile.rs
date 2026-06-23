use centaur_sandbox_core::{DesiredSandboxState, ObservedSandbox, SandboxStatus};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconcileAction {
    None,
    Pause,
    Resume,
    Stop,
    ReportDrift(DriftReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DriftReason {
    NoDesiredState,
    MissingWhileRunning,
    MissingWhileSuspended,
    UnknownObservedState(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconcilePlan {
    pub action: ReconcileAction,
}

impl ReconcilePlan {
    pub fn for_state(desired: &DesiredSandboxState, observed: &ObservedSandbox) -> Self {
        let action = match desired {
            DesiredSandboxState::Running(_) => match &observed.status {
                SandboxStatus::Running | SandboxStatus::Created => ReconcileAction::None,
                SandboxStatus::Suspended => ReconcileAction::Resume,
                SandboxStatus::Stopped | SandboxStatus::Gone => {
                    ReconcileAction::ReportDrift(DriftReason::MissingWhileRunning)
                }
                SandboxStatus::Unknown(value) => {
                    ReconcileAction::ReportDrift(DriftReason::UnknownObservedState(value.clone()))
                }
            },
            DesiredSandboxState::Suspended(_) => match &observed.status {
                SandboxStatus::Suspended => ReconcileAction::None,
                SandboxStatus::Running | SandboxStatus::Created => ReconcileAction::Pause,
                SandboxStatus::Stopped | SandboxStatus::Gone => {
                    ReconcileAction::ReportDrift(DriftReason::MissingWhileSuspended)
                }
                SandboxStatus::Unknown(value) => {
                    ReconcileAction::ReportDrift(DriftReason::UnknownObservedState(value.clone()))
                }
            },
            DesiredSandboxState::Stopped => match &observed.status {
                SandboxStatus::Stopped | SandboxStatus::Gone => ReconcileAction::None,
                SandboxStatus::Created | SandboxStatus::Running | SandboxStatus::Suspended => {
                    ReconcileAction::Stop
                }
                SandboxStatus::Unknown(value) => {
                    ReconcileAction::ReportDrift(DriftReason::UnknownObservedState(value.clone()))
                }
            },
        };

        Self { action }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReconcileOutcome {
    Noop,
    Paused,
    Resumed,
    Stopped,
    Drift(DriftReason),
}
