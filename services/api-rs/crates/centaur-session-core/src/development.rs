use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use strum::{AsRefStr, Display, EnumString};
use thiserror::Error;
use time::OffsetDateTime;

use crate::{HarnessType, SessionMessageInput, ThreadKey};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentChannel {
    pub platform: String,
    pub tenant_key: String,
    pub conversation_key: String,
    pub root_message_id: String,
}

impl DevelopmentChannel {
    pub fn lock_key(&self) -> String {
        format!(
            "{}:{}{}:{}{}:{}{}:{}",
            self.platform.len(),
            self.platform,
            self.tenant_key.len(),
            self.tenant_key,
            self.conversation_key.len(),
            self.conversation_key,
            self.root_message_id.len(),
            self.root_message_id,
        )
    }

    pub fn receipt_lock_key(&self, kind: &str, receipt_id: &str) -> String {
        format!(
            "receipt:{}:{}{}:{}{}:{}{}:{}",
            kind.len(),
            kind,
            self.platform.len(),
            self.platform,
            self.tenant_key.len(),
            self.tenant_key,
            receipt_id.len(),
            receipt_id,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentInitiator {
    pub principal_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptDevelopmentTask {
    pub channel: DevelopmentChannel,
    pub platform_event_id: String,
    pub platform_message_id: Option<String>,
    pub harness_type: HarnessType,
    pub initiator: DevelopmentInitiator,
    pub message: SessionMessageInput,
    #[serde(default)]
    pub session_metadata: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceptedDevelopmentTask {
    pub thread_key: ThreadKey,
    pub workspace_id: String,
    pub selection_flow_id: String,
    pub execution_id: String,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRepository {
    pub repository_id: RepositoryId,
    pub display_name: String,
    pub path_with_namespace: String,
    pub default_branch: String,
    pub clone_url: String,
    pub relative_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmRepositorySelection {
    pub selection_flow_id: String,
    pub expected_version: i32,
    pub decided_by_principal_id: String,
    pub repositories: Vec<ResolvedRepository>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositorySelectionOutcome {
    pub selection_flow_id: String,
    pub state: SelectionFlowState,
    pub version: i32,
    pub repository_ids: Vec<RepositoryId>,
    pub workspace_state: WorkspaceState,
    pub execution_blocker: Option<ExecutionBlocker>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositorySelectionDraft {
    pub selection_flow_id: String,
    pub workspace_id: String,
    pub kind: SelectionKind,
    pub state: SelectionFlowState,
    pub version: i32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RepositoryId(String);

impl RepositoryId {
    pub fn parse(value: impl Into<String>) -> Result<Self, RepositoryIdError> {
        let value = value.into();
        let project_id = value
            .strip_prefix("gitlab:")
            .ok_or(RepositoryIdError::Invalid)?
            .parse::<u64>()
            .map_err(|_| RepositoryIdError::Invalid)?;
        if project_id == 0 || value != format!("gitlab:{project_id}") {
            return Err(RepositoryIdError::Invalid);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn project_id(&self) -> u64 {
        self.0
            .strip_prefix("gitlab:")
            .expect("validated repository ID has a gitlab prefix")
            .parse()
            .expect("validated repository ID has a numeric project ID")
    }
}

impl fmt::Display for RepositoryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RepositoryId {
    type Err = RepositoryIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for RepositoryId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RepositoryId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RepositoryIdError {
    #[error("repository_id must be formatted as 'gitlab:<positive numeric project id>'")]
    Invalid,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid {entity} state transition from {from} to {to}")]
pub struct StateTransitionError {
    entity: String,
    from: String,
    to: String,
}

impl StateTransitionError {
    pub fn new(entity: impl Into<String>, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            entity: entity.into(),
            from: from.into(),
            to: to.into(),
        }
    }
}

macro_rules! state_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Eq,
            PartialEq,
            Serialize,
            Deserialize,
            AsRefStr,
            Display,
            EnumString,
        )]
        #[serde(rename_all = "snake_case")]
        #[strum(serialize_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
    };
}

state_enum!(WorkspaceState {
    AwaitingSelection,
    Provisioning,
    Ready,
    Failed,
});

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionWorkspace {
    pub workspace_id: String,
    pub thread_key: ThreadKey,
    pub state: WorkspaceState,
    pub storage_ref: Option<String>,
    pub preparation_attempt: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl WorkspaceState {
    pub fn transition_to(self, next: Self) -> Result<Self, StateTransitionError> {
        let allowed = matches!(
            (self, next),
            (Self::AwaitingSelection, Self::Provisioning)
                | (Self::Provisioning, Self::Ready | Self::Failed)
                | (Self::Failed | Self::Ready, Self::Provisioning)
        );
        allowed
            .then_some(next)
            .ok_or_else(|| StateTransitionError::new("workspace", self.as_ref(), next.as_ref()))
    }
}

state_enum!(WorkspaceRepositoryState {
    Pending,
    Provisioning,
    Ready,
    Failed,
});

impl WorkspaceRepositoryState {
    pub fn transition_to(self, next: Self) -> Result<Self, StateTransitionError> {
        let allowed = matches!(
            (self, next),
            (Self::Pending, Self::Provisioning)
                | (Self::Provisioning, Self::Ready | Self::Failed)
                | (Self::Failed, Self::Provisioning)
        );
        allowed.then_some(next).ok_or_else(|| {
            StateTransitionError::new("workspace_repository", self.as_ref(), next.as_ref())
        })
    }
}

state_enum!(SelectionFlowState {
    Pending,
    Confirmed,
    Cancelled,
});

state_enum!(SelectionKind { Initial, Add });

impl SelectionFlowState {
    pub fn transition_to(self, next: Self) -> Result<Self, StateTransitionError> {
        matches!(
            (self, next),
            (Self::Pending, Self::Confirmed | Self::Cancelled)
        )
        .then_some(next)
        .ok_or_else(|| StateTransitionError::new("selection_flow", self.as_ref(), next.as_ref()))
    }
}

state_enum!(ExecutionBlocker {
    AwaitingProjectSelection,
    WorkspaceProvisioning,
});

impl ExecutionBlocker {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AwaitingProjectSelection => "awaiting_project_selection",
            Self::WorkspaceProvisioning => "workspace_provisioning",
        }
    }
}

state_enum!(ChangeSetState {
    Collecting,
    Ready,
    NeedsAgentCompletion,
    Failed,
});

impl ChangeSetState {
    pub const fn is_publishable(self) -> bool {
        matches!(self, Self::Ready)
    }
}

state_enum!(PublishBatchState {
    Pending,
    Running,
    Succeeded,
    PartiallySucceeded,
    Failed,
});

impl PublishBatchState {
    pub fn from_items(items: impl IntoIterator<Item = PublishItemState>) -> Self {
        let mut item_count = 0;
        let mut pending = 0;
        let mut succeeded = 0;
        let mut failed = 0;
        let mut running = 0;

        for state in items {
            item_count += 1;
            match state {
                PublishItemState::Pending => pending += 1,
                PublishItemState::Succeeded => succeeded += 1,
                PublishItemState::Failed => failed += 1,
                PublishItemState::Pushing
                | PublishItemState::Pushed
                | PublishItemState::CreatingMr => running += 1,
            }
        }

        if item_count == 0 || pending == item_count {
            return Self::Pending;
        }
        if pending > 0 || running > 0 {
            Self::Running
        } else if succeeded == item_count {
            Self::Succeeded
        } else if failed == item_count {
            Self::Failed
        } else {
            Self::PartiallySucceeded
        }
    }
}

state_enum!(PublishItemState {
    Pending,
    Pushing,
    Pushed,
    CreatingMr,
    Succeeded,
    Failed,
});

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        ChangeSetState, ExecutionBlocker, PublishBatchState, PublishItemState, RepositoryId,
        SelectionFlowState, StateTransitionError, WorkspaceRepositoryState, WorkspaceState,
    };

    #[test]
    fn repository_id_accepts_only_numeric_gitlab_projects() {
        let repository_id = RepositoryId::parse("gitlab:42").unwrap();

        assert_eq!(repository_id.project_id(), 42);
        assert_eq!(repository_id.as_str(), "gitlab:42");
        assert_eq!(repository_id.to_string(), "gitlab:42");
        assert_eq!(RepositoryId::from_str("gitlab:42").unwrap(), repository_id);
        assert_eq!(
            serde_json::to_string(&repository_id).unwrap(),
            r#""gitlab:42""#
        );
        assert_eq!(
            serde_json::from_str::<RepositoryId>(r#""gitlab:42""#).unwrap(),
            repository_id
        );

        for invalid in [
            "",
            "42",
            "github:42",
            "gitlab:name",
            "gitlab:0",
            "gitlab:01",
            "gitlab:42:extra",
            " gitlab:42",
            "gitlab:42 ",
        ] {
            assert!(
                RepositoryId::parse(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn workspace_state_allows_only_recoverable_lifecycle_transitions() {
        assert_eq!(
            WorkspaceState::AwaitingSelection.transition_to(WorkspaceState::Provisioning),
            Ok(WorkspaceState::Provisioning)
        );
        assert_eq!(
            WorkspaceState::Provisioning.transition_to(WorkspaceState::Failed),
            Ok(WorkspaceState::Failed)
        );
        assert_eq!(
            WorkspaceState::Failed.transition_to(WorkspaceState::Provisioning),
            Ok(WorkspaceState::Provisioning)
        );
        assert_eq!(
            WorkspaceState::Provisioning.transition_to(WorkspaceState::Ready),
            Ok(WorkspaceState::Ready)
        );
        assert_eq!(
            WorkspaceState::Ready.transition_to(WorkspaceState::AwaitingSelection),
            Err(StateTransitionError::new(
                "workspace",
                "ready",
                "awaiting_selection"
            ))
        );
    }

    #[test]
    fn repository_and_selection_states_are_append_only_and_versionable() {
        assert_eq!(
            WorkspaceRepositoryState::Pending.transition_to(WorkspaceRepositoryState::Provisioning),
            Ok(WorkspaceRepositoryState::Provisioning)
        );
        assert_eq!(
            WorkspaceRepositoryState::Failed.transition_to(WorkspaceRepositoryState::Provisioning),
            Ok(WorkspaceRepositoryState::Provisioning)
        );
        assert!(
            WorkspaceRepositoryState::Ready
                .transition_to(WorkspaceRepositoryState::Pending)
                .is_err()
        );
        assert_eq!(
            SelectionFlowState::Pending.transition_to(SelectionFlowState::Confirmed),
            Ok(SelectionFlowState::Confirmed)
        );
        assert!(
            SelectionFlowState::Confirmed
                .transition_to(SelectionFlowState::Cancelled)
                .is_err()
        );
    }

    #[test]
    fn execution_blockers_have_stable_database_values() {
        assert_eq!(
            ExecutionBlocker::AwaitingProjectSelection.as_str(),
            "awaiting_project_selection"
        );
        assert_eq!(
            ExecutionBlocker::WorkspaceProvisioning.as_str(),
            "workspace_provisioning"
        );
    }

    #[test]
    fn changeset_state_does_not_make_incomplete_work_publishable() {
        assert!(ChangeSetState::Ready.is_publishable());
        for state in [
            ChangeSetState::Collecting,
            ChangeSetState::NeedsAgentCompletion,
            ChangeSetState::Failed,
        ] {
            assert!(!state.is_publishable());
        }
    }

    #[test]
    fn publish_batch_reduces_item_states_without_losing_partial_success() {
        assert_eq!(
            PublishBatchState::from_items([]),
            PublishBatchState::Pending
        );
        assert_eq!(
            PublishBatchState::from_items(
                [PublishItemState::Pending, PublishItemState::Succeeded,]
            ),
            PublishBatchState::Running
        );
        assert_eq!(
            PublishBatchState::from_items([
                PublishItemState::Succeeded,
                PublishItemState::Succeeded,
            ]),
            PublishBatchState::Succeeded
        );
        assert_eq!(
            PublishBatchState::from_items([PublishItemState::Succeeded, PublishItemState::Failed,]),
            PublishBatchState::PartiallySucceeded
        );
        assert_eq!(
            PublishBatchState::from_items([PublishItemState::Failed, PublishItemState::Failed,]),
            PublishBatchState::Failed
        );
    }
}
