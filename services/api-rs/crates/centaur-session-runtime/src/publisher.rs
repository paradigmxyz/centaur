use std::{sync::Arc, time::Duration};

use centaur_sandbox_core::{
    GitLabMergeRequestRequest, GitLabPublisher, GitLabPushRequest, WorkspaceError,
};
use centaur_session_core::development::{
    ApprovePublication, CompletePublishItem, DevelopmentPublishBatch, FailPublishItem,
    PublishItemClaim, PublishItemState, RetryPublication,
};
use tracing::{error, info, warn};

use crate::{SessionRuntime, SessionRuntimeError};

const PUBLICATION_LEASE: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub(super) struct PublisherRuntime {
    backend: Arc<dyn GitLabPublisher>,
    credential_ref: String,
}

impl PublisherRuntime {
    fn new(backend: Arc<dyn GitLabPublisher>, credential_ref: String) -> Self {
        Self {
            backend,
            credential_ref,
        }
    }
}

impl SessionRuntime {
    pub fn with_gitlab_publisher(
        mut self,
        backend: Arc<dyn GitLabPublisher>,
        credential_ref: impl Into<String>,
    ) -> Self {
        self.publisher = Some(PublisherRuntime::new(backend, credential_ref.into()));
        self
    }

    pub fn spawn_publication_reconciliation(&self, interval: Duration) {
        let runtime = self.clone();
        tokio::spawn(async move {
            loop {
                match runtime.store.list_reconcilable_publish_batch_ids().await {
                    Ok(batch_ids) => {
                        for batch_id in batch_ids {
                            runtime.spawn_publication(batch_id);
                        }
                    }
                    Err(error) => warn!(%error, "failed to list reconcilable publish batches"),
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    pub async fn approve_publication(
        &self,
        request: &ApprovePublication,
    ) -> Result<DevelopmentPublishBatch, SessionRuntimeError> {
        self.require_publisher()?;
        let batch = self.store.approve_publication(request).await?;
        self.spawn_publication(batch.publish_batch_id.clone());
        Ok(batch)
    }

    pub async fn retry_failed_publication(
        &self,
        request: &RetryPublication,
    ) -> Result<DevelopmentPublishBatch, SessionRuntimeError> {
        self.require_publisher()?;
        let batch = self.store.retry_failed_publication(request).await?;
        self.spawn_publication(batch.publish_batch_id.clone());
        Ok(batch)
    }

    pub async fn get_publish_batch(
        &self,
        publish_batch_id: &str,
        principal_id: &str,
        is_admin: bool,
    ) -> Result<DevelopmentPublishBatch, SessionRuntimeError> {
        Ok(self
            .store
            .get_publish_batch(publish_batch_id, principal_id, is_admin)
            .await?)
    }

    fn require_publisher(&self) -> Result<&PublisherRuntime, SessionRuntimeError> {
        self.publisher.as_ref().ok_or_else(|| {
            SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                "GitLab publisher is not configured".to_owned(),
            ))
        })
    }

    fn spawn_publication(&self, publish_batch_id: String) {
        if self.publisher.is_none() {
            warn!(publish_batch_id, "GitLab publisher is not configured");
            return;
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.publish_batch(&publish_batch_id).await {
                error!(publish_batch_id, %error, "publication reconciliation failed");
            }
        });
    }

    pub async fn publish_batch(&self, publish_batch_id: &str) -> Result<(), SessionRuntimeError> {
        let publisher = self.require_publisher()?.clone();
        let lease_owner = format!("api-rs-publisher-{}", uuid::Uuid::new_v4().simple());
        loop {
            let claim = match self
                .store
                .claim_publish_item(publish_batch_id, &lease_owner, PUBLICATION_LEASE)
                .await
            {
                Ok(claim) => claim,
                Err(centaur_session_sqlx::SessionStoreError::DevelopmentConflict { .. }) => {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };
            let Some(claim) = claim else { return Ok(()) };
            if let Err(error) = self.publish_item(&publisher, &lease_owner, &claim).await {
                self.store
                    .fail_publish_item(&FailPublishItem {
                        publish_batch_id: claim.batch.publish_batch_id.clone(),
                        publish_item_id: claim.item.publish_item_id.clone(),
                        lease_owner: lease_owner.clone(),
                        failure_code: "publisher_failed".to_owned(),
                        failure_message: "GitLab publication failed".to_owned(),
                    })
                    .await?;
                warn!(
                    publish_batch_id,
                    publish_item_id = claim.item.publish_item_id,
                    %error,
                    "publication item failed"
                );
            }
        }
    }

    async fn publish_item(
        &self,
        publisher: &PublisherRuntime,
        lease_owner: &str,
        claim: &PublishItemClaim,
    ) -> Result<(), SessionRuntimeError> {
        let storage_ref = claim.workspace.storage_ref.clone().ok_or_else(|| {
            SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                "publication workspace has no storage reference".to_owned(),
            ))
        })?;
        let attempt = u32::try_from(claim.item.attempt_count).map_err(|_| {
            SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                "publication attempt is invalid".to_owned(),
            ))
        })?;
        let remote_branch_sha = match claim.item.state {
            PublishItemState::Pushing => {
                let pushed = publisher
                    .backend
                    .push(GitLabPushRequest {
                        publish_item_id: claim.item.publish_item_id.clone(),
                        attempt,
                        credential_ref: publisher.credential_ref.clone(),
                        workspace_id: claim.workspace.workspace_id.clone(),
                        storage_ref,
                        relative_path: claim.repository.relative_path.clone(),
                        clone_url: claim.repository.clone_url.clone(),
                        source_branch: claim.item.source_branch.clone(),
                        head_sha: claim.item.head_sha.clone(),
                    })
                    .await?;
                self.store
                    .mark_publish_item_pushed(
                        &claim.batch.publish_batch_id,
                        &claim.item.publish_item_id,
                        lease_owner,
                        &pushed.remote_branch_sha,
                    )
                    .await?;
                pushed.remote_branch_sha
            }
            PublishItemState::Pushed | PublishItemState::CreatingMr => {
                claim.item.remote_branch_sha.clone().ok_or_else(|| {
                    SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                        "pushed publication item has no remote SHA".to_owned(),
                    ))
                })?
            }
            state => {
                return Err(SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                    format!("publication item cannot run from state {state}"),
                )));
            }
        };

        if claim.item.state != PublishItemState::CreatingMr {
            self.store
                .mark_publish_item_creating_mr(
                    &claim.batch.publish_batch_id,
                    &claim.item.publish_item_id,
                    lease_owner,
                )
                .await?;
        }
        let merge_request = publisher
            .backend
            .ensure_merge_request(GitLabMergeRequestRequest {
                publish_item_id: claim.item.publish_item_id.clone(),
                attempt,
                credential_ref: publisher.credential_ref.clone(),
                project_id: claim.item.repository_id.project_id(),
                clone_url: claim.repository.clone_url.clone(),
                source_branch: claim.item.source_branch.clone(),
                target_branch: claim.item.target_branch.clone(),
                head_sha: claim.item.head_sha.clone(),
                remote_branch_sha: remote_branch_sha.clone(),
                changeset_id: claim.batch.changeset_id.clone(),
            })
            .await?;
        let batch = self
            .store
            .complete_publish_item(&CompletePublishItem {
                publish_batch_id: claim.batch.publish_batch_id.clone(),
                publish_item_id: claim.item.publish_item_id.clone(),
                lease_owner: lease_owner.to_owned(),
                remote_branch_sha,
                merge_request_iid: merge_request.merge_request_iid,
                merge_request_url: merge_request.merge_request_url,
            })
            .await?;
        info!(
            publish_batch_id = batch.publish_batch_id,
            publish_item_id = claim.item.publish_item_id,
            state = %batch.state,
            "publication item recorded"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use async_trait::async_trait;
    use centaur_iron_control::{IronControlError, Principal};
    use centaur_sandbox_core::{
        GitLabMergeRequestResult, GitLabPushResult, ObservedSandbox, SandboxBackend, SandboxError,
        SandboxHandle, SandboxId, SandboxIo, SandboxResult, SandboxSpec, SandboxStatus,
    };
    use centaur_session_core::{
        HarnessType, MessageRole, SessionMessageInput,
        development::{
            AcceptDevelopmentTask, CollectedChangeSetRepositoryState, CompleteChangeSetCollection,
            CompleteChangeSetRepository, CompleteWorkspacePreparation, ConfirmRepositorySelection,
            DevelopmentChannel, DevelopmentInitiator, PreparedRepositorySnapshot,
            PublishBatchState, ResolvedRepository,
        },
    };
    use centaur_session_sqlx::PgSessionStore;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{SandboxRuntime, SessionPrincipalRegistrar};

    struct UnusedBackend;

    #[async_trait]
    impl SandboxBackend for UnusedBackend {
        fn name(&self) -> &'static str {
            "publisher-test"
        }
        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            Err(SandboxError::io("unused"))
        }
        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            Err(SandboxError::io("unused"))
        }
        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(SandboxStatus::Gone)
        }
        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            Ok(ObservedSandbox::new(
                id.clone(),
                self.name(),
                SandboxStatus::Gone,
            ))
        }
        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(Vec::new())
        }
        async fn stop(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }
        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }
        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }
    }

    struct Registrar;

    #[async_trait]
    impl SessionPrincipalRegistrar for Registrar {
        async fn register_session(
            &self,
            _thread_key: &str,
            _metadata: Option<&Value>,
        ) -> Result<Principal, IronControlError> {
            Ok(principal())
        }

        async fn get_principal(&self, _principal: &str) -> Result<Principal, IronControlError> {
            Ok(principal())
        }
    }

    fn principal() -> Principal {
        Principal {
            id: "prn_publisher_test".to_owned(),
            foreign_id: None,
            name: "Publisher Test".to_owned(),
            labels: Default::default(),
            sandbox_observability_enabled: true,
            sandbox_api_server_enabled: true,
        }
    }

    #[derive(Default)]
    struct PartialPublisher {
        pushes: Mutex<BTreeMap<u64, usize>>,
    }

    impl PartialPublisher {
        fn pushes(&self, project_id: u64) -> usize {
            *self.pushes.lock().unwrap().get(&project_id).unwrap_or(&0)
        }
    }

    #[async_trait]
    impl GitLabPublisher for PartialPublisher {
        async fn push(
            &self,
            request: GitLabPushRequest,
        ) -> Result<GitLabPushResult, WorkspaceError> {
            let project_id = request
                .relative_path
                .strip_prefix("repos/")
                .and_then(|value| value.split('-').next())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap();
            let attempt = {
                let mut pushes = self.pushes.lock().unwrap();
                let count = pushes.entry(project_id).or_default();
                *count += 1;
                *count
            };
            if project_id == 84 && attempt == 1 {
                return Err(WorkspaceError::Backend("simulated push failure".to_owned()));
            }
            Ok(GitLabPushResult {
                remote_branch_sha: request.head_sha,
            })
        }

        async fn ensure_merge_request(
            &self,
            request: GitLabMergeRequestRequest,
        ) -> Result<GitLabMergeRequestResult, WorkspaceError> {
            Ok(GitLabMergeRequestResult {
                merge_request_iid: i64::try_from(request.project_id).unwrap(),
                merge_request_url: format!("http://git.example.test/mr/{}", request.project_id),
            })
        }
    }

    fn repository(project_id: u64) -> ResolvedRepository {
        ResolvedRepository {
            repository_id: format!("gitlab:{project_id}").parse().unwrap(),
            display_name: format!("Project {project_id}"),
            path_with_namespace: format!("group/project-{project_id}"),
            default_branch: "main".to_owned(),
            clone_url: format!("http://git.example.test/group/project-{project_id}.git"),
            relative_path: format!("repos/{project_id}-project-{project_id}"),
        }
    }

    #[tokio::test]
    async fn publication_worker_preserves_success_and_retries_only_failed_repository() {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return;
        };
        let store = PgSessionStore::connect(&url).await.unwrap();
        store.run_migrations().await.unwrap();
        let suffix = uuid::Uuid::new_v4();
        let accepted = store
            .accept_development_task(&AcceptDevelopmentTask {
                channel: DevelopmentChannel {
                    platform: "feishu".to_owned(),
                    tenant_key: format!("tenant-{suffix}"),
                    conversation_key: format!("chat-{suffix}"),
                    root_message_id: format!("message-{suffix}"),
                },
                platform_event_id: format!("event-{suffix}"),
                platform_message_id: Some(format!("message-{suffix}")),
                harness_type: HarnessType::Codex,
                initiator: DevelopmentInitiator {
                    principal_id: "principal-1".to_owned(),
                },
                message: SessionMessageInput {
                    client_message_id: None,
                    role: MessageRole::User,
                    parts: vec![json!({"type": "text", "text": "Fix both"})],
                    metadata: json!({}),
                },
                session_metadata: json!({}),
            })
            .await
            .unwrap();
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![repository(42), repository(84)],
            })
            .await
            .unwrap();
        let workspace = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "workspace-owner",
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        let base = "a".repeat(40);
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: workspace.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: "workspace-publisher-test".to_owned(),
                prepared: vec![42, 84]
                    .into_iter()
                    .map(|project_id| PreparedRepositorySnapshot {
                        repository_id: format!("gitlab:{project_id}").parse().unwrap(),
                        base_sha: base.clone(),
                        local_branch: "centaur/test".to_owned(),
                        head_sha: base.clone(),
                    })
                    .collect(),
                failed: Vec::new(),
            })
            .await
            .unwrap();
        store
            .complete_execution(&accepted.execution_id)
            .await
            .unwrap();
        let collecting = store
            .begin_changeset_collection(&accepted.execution_id, "collector")
            .await
            .unwrap()
            .unwrap();
        let patch = b"diff --git a/README.md b/README.md\n".to_vec();
        let completed = store
            .complete_changeset_collection(&CompleteChangeSetCollection {
                changeset_id: collecting.changeset_id,
                lease_owner: "collector".to_owned(),
                repositories: [42_u64, 84]
                    .into_iter()
                    .map(|project_id| {
                        let head = if project_id == 42 { "b" } else { "c" }.repeat(40);
                        CompleteChangeSetRepository {
                            repository_id: format!("gitlab:{project_id}").parse().unwrap(),
                            state: CollectedChangeSetRepositoryState::Changed,
                            base_sha: base.clone(),
                            recorded_head_sha: base.clone(),
                            head_sha: Some(head.clone()),
                            commit_metadata: json!([{"sha": head}]),
                            changed_file_count: 1,
                            additions: 1,
                            deletions: 0,
                            patch_hash: Some(format!(
                                "sha256:{}",
                                hex::encode(Sha256::digest(&patch))
                            )),
                            patch: patch.clone(),
                            test_evidence: json!([]),
                            failure_code: None,
                            failure_message: None,
                        }
                    })
                    .collect(),
            })
            .await
            .unwrap()
            .unwrap();
        let publisher = Arc::new(PartialPublisher::default());
        let runtime = SessionRuntime::new(
            store.clone(),
            SandboxRuntime::backend(Arc::new(UnusedBackend), SandboxSpec::new("unused")),
            Registrar,
        )
        .with_gitlab_publisher(publisher.clone(), "publisher-token");
        let approved = runtime
            .approve_publication(&ApprovePublication {
                changeset_id: completed.changeset_id,
                approver_principal_id: "principal-1".to_owned(),
                is_admin: false,
                idempotency_key: "approve".to_owned(),
            })
            .await
            .unwrap();
        runtime
            .publish_batch(&approved.publish_batch_id)
            .await
            .unwrap();
        let partial = runtime
            .get_publish_batch(&approved.publish_batch_id, "principal-1", false)
            .await
            .unwrap();
        assert_eq!(partial.state, PublishBatchState::PartiallySucceeded);
        assert_eq!(publisher.pushes(42), 1);
        assert_eq!(publisher.pushes(84), 1);

        runtime
            .retry_failed_publication(&RetryPublication {
                publish_batch_id: approved.publish_batch_id.clone(),
                requested_by_principal_id: "principal-1".to_owned(),
                is_admin: false,
                idempotency_key: "retry".to_owned(),
            })
            .await
            .unwrap();
        runtime
            .publish_batch(&approved.publish_batch_id)
            .await
            .unwrap();
        let mut succeeded = runtime
            .get_publish_batch(&approved.publish_batch_id, "principal-1", false)
            .await
            .unwrap();
        for _ in 0..100 {
            if succeeded.state == PublishBatchState::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            succeeded = runtime
                .get_publish_batch(&approved.publish_batch_id, "principal-1", false)
                .await
                .unwrap();
        }
        assert_eq!(succeeded.state, PublishBatchState::Succeeded);
        assert_eq!(publisher.pushes(42), 1);
        assert_eq!(publisher.pushes(84), 2);
        let events = store
            .list_events_after(&accepted.thread_key, 0, Some(&accepted.execution_id), 100)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "development.publish_item_failed")
        );
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "development.publish_retry_requested")
        );
    }
}
