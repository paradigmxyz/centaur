use std::{sync::Arc, time::Duration};

use centaur_sandbox_core::{
    PreparedWorkspaceRepository, WorkspaceError, WorkspaceManager, WorkspaceMount,
    WorkspacePreparationRequest, WorkspaceRepository,
};
use centaur_session_core::{
    ExecutionStatus, ThreadKey,
    development::{
        CompleteWorkspacePreparation, FailedRepositorySnapshot, PreparedRepositorySnapshot,
        RepositoryState, WorkspaceState,
    },
};
use tracing::{error, info, warn};

use crate::{ExecuteSessionInput, SessionRuntime, SessionRuntimeError};

const WORKSPACE_PREPARATION_LEASE: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub(super) struct WorkspaceRuntime {
    pub(super) manager: Arc<dyn WorkspaceManager>,
    credential_ref: String,
    pub(super) lease_owner: String,
}

impl WorkspaceRuntime {
    fn new(manager: Arc<dyn WorkspaceManager>, credential_ref: String) -> Self {
        Self {
            manager,
            credential_ref,
            lease_owner: format!("api-rs-{}", uuid::Uuid::new_v4().simple()),
        }
    }
}

impl SessionRuntime {
    pub fn with_workspace_manager(
        mut self,
        manager: Arc<dyn WorkspaceManager>,
        credential_ref: impl Into<String>,
    ) -> Self {
        self.workspace = Some(WorkspaceRuntime::new(manager, credential_ref.into()));
        self
    }

    pub fn spawn_workspace_reconciliation(&self, interval: Duration) {
        let runtime = self.clone();
        tokio::spawn(async move {
            loop {
                match runtime.store.list_provisioning_workspace_ids().await {
                    Ok(workspace_ids) => {
                        for workspace_id in workspace_ids {
                            runtime.spawn_workspace_preparation(workspace_id);
                        }
                    }
                    Err(error) => warn!(%error, "failed to list provisioning workspaces"),
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    pub(super) fn spawn_workspace_preparation(&self, workspace_id: String) {
        if self.workspace.is_none() {
            warn!(workspace_id, "workspace manager is not configured");
            return;
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.prepare_workspace(&workspace_id).await {
                error!(workspace_id, %error, "workspace preparation failed");
            }
        });
    }

    pub async fn prepare_workspace(&self, workspace_id: &str) -> Result<(), SessionRuntimeError> {
        let workspace = self.workspace.as_ref().ok_or_else(|| {
            SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                "workspace manager is not configured".to_owned(),
            ))
        })?;
        let claim = self
            .store
            .claim_workspace_preparation(
                workspace_id,
                &workspace.lease_owner,
                WORKSPACE_PREPARATION_LEASE,
            )
            .await?;
        self.stop_assigned_sandbox(&claim.workspace.thread_key)
            .await?;
        let request = WorkspacePreparationRequest::new(
            &claim.workspace.workspace_id,
            claim.workspace.thread_key.as_str(),
            u32::try_from(claim.workspace.preparation_attempt).map_err(|_| {
                SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                    "workspace attempt is invalid".to_owned(),
                ))
            })?,
            &workspace.credential_ref,
            claim
                .repositories
                .iter()
                .map(|repository| WorkspaceRepository {
                    repository_id: repository.repository_id.to_string(),
                    display_name: repository.display_name.clone(),
                    path_with_namespace: repository.path_with_namespace.clone(),
                    default_branch: repository.default_branch.clone(),
                    clone_url: repository.clone_url.clone(),
                    relative_path: repository.relative_path.clone(),
                    existing: existing_repository(repository),
                })
                .collect(),
        )?;
        let result = match workspace.manager.prepare(request).await {
            Ok(result) => result,
            Err(error) => {
                self.store
                    .fail_workspace_preparation(
                        workspace_id,
                        claim.workspace.preparation_attempt,
                        &workspace.lease_owner,
                        "workspace_backend_failed",
                        "workspace preparation failed",
                    )
                    .await?;
                return Err(error.into());
            }
        };
        let completed = self
            .store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: result.workspace_id,
                attempt: claim.workspace.preparation_attempt,
                lease_owner: workspace.lease_owner.clone(),
                storage_ref: result.storage_ref,
                prepared: result
                    .prepared
                    .into_iter()
                    .map(|repository| {
                        Ok(PreparedRepositorySnapshot {
                            repository_id: repository.repository_id.parse().map_err(|error| {
                                SessionRuntimeError::Workspace(WorkspaceError::Invalid(format!(
                                    "workspace result repository ID: {error}"
                                )))
                            })?,
                            base_sha: repository.base_sha,
                            local_branch: repository.local_branch,
                            head_sha: repository.head_sha,
                        })
                    })
                    .collect::<Result<Vec<_>, SessionRuntimeError>>()?,
                failed: result
                    .failed
                    .into_iter()
                    .map(|repository| {
                        Ok(FailedRepositorySnapshot {
                            repository_id: repository.repository_id.parse().map_err(|error| {
                                SessionRuntimeError::Workspace(WorkspaceError::Invalid(format!(
                                    "workspace result repository ID: {error}"
                                )))
                            })?,
                            failure_code: repository.failure_code,
                            failure_message: repository.failure_message,
                        })
                    })
                    .collect::<Result<Vec<_>, SessionRuntimeError>>()?,
            })
            .await?;
        info!(
            workspace_id = completed.workspace_id,
            state = %completed.state,
            "workspace preparation recorded"
        );
        if completed.state == WorkspaceState::Ready
            && let Some(execution_id) = claim.execution_id
        {
            self.drive_development_execution(&claim.workspace.thread_key, &execution_id)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn workspace_mount(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<Option<WorkspaceMount>, SessionRuntimeError> {
        let Some(workspace) = self.store.workspace_for_session(thread_key).await? else {
            return Ok(None);
        };
        match workspace.state {
            WorkspaceState::Ready => workspace
                .storage_ref
                .map(WorkspaceMount::new)
                .map(Some)
                .ok_or_else(|| {
                    SessionRuntimeError::Workspace(WorkspaceError::Invalid(format!(
                        "ready workspace {} has no storage reference",
                        workspace.workspace_id
                    )))
                }),
            state => Err(SessionRuntimeError::Workspace(WorkspaceError::Invalid(
                format!("workspace {} is {state}", workspace.workspace_id),
            ))),
        }
    }

    async fn stop_assigned_sandbox(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<(), SessionRuntimeError> {
        let session = self.store.get_session(thread_key).await?;
        let Some(sandbox_id) = session.sandbox_id else {
            return Ok(());
        };
        self.sandbox_pipes.remove(&sandbox_id);
        match self
            .sandbox_runtime
            .manager
            .stop(&centaur_sandbox_core::SandboxId::new(&sandbox_id))
            .await
        {
            Ok(()) | Err(centaur_sandbox_core::SandboxError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
        self.store.update_sandbox_id(thread_key, None).await?;
        Ok(())
    }

    pub(super) async fn drive_development_execution(
        &self,
        thread_key: &ThreadKey,
        execution_id: &str,
    ) -> Result<(), SessionRuntimeError> {
        let execution = self
            .store
            .latest_execution_for_thread(thread_key)
            .await?
            .filter(|execution| execution.execution_id == execution_id)
            .ok_or_else(|| {
                SessionRuntimeError::BadRequest(format!(
                    "development execution {execution_id} is not current"
                ))
            })?;
        if execution.status != ExecutionStatus::Queued || execution.blocking_reason.is_some() {
            return Ok(());
        }
        let input_line = execution
            .metadata
            .get("development_input_line")
            .cloned()
            .ok_or_else(|| {
                SessionRuntimeError::BadRequest(
                    "development execution has no durable input line".to_owned(),
                )
            })?;
        let input_line = serde_json::to_string(&input_line)?;
        self.execute_session(
            thread_key,
            ExecuteSessionInput {
                idempotency_key: execution.idempotency_key,
                metadata: Some(execution.metadata),
                input_lines: vec![input_line],
                idle_timeout_ms: None,
                max_duration_ms: None,
            },
        )
        .await?;
        Ok(())
    }
}

fn existing_repository(
    repository: &centaur_session_core::development::WorkspaceRepositorySnapshot,
) -> Option<PreparedWorkspaceRepository> {
    if repository.state != RepositoryState::Ready {
        return None;
    }
    Some(PreparedWorkspaceRepository {
        repository_id: repository.repository_id.to_string(),
        base_sha: repository.base_sha.clone()?,
        local_branch: repository.local_branch.clone()?,
        head_sha: repository.head_sha.clone()?,
    })
}

#[cfg(test)]
mod workspace_preparation_tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use centaur_iron_control::{IronControlError, Principal};
    use centaur_sandbox_core::{
        ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
        SandboxResult, SandboxSpec, SandboxStatus, WorkspaceCollection, WorkspaceCollectionRequest,
        WorkspaceManager, WorkspacePreparation, WorkspacePreparationRequest,
    };
    use centaur_session_core::{
        HarnessType, MessageRole, SessionMessageInput,
        development::{
            AcceptDevelopmentTask, ConfirmRepositorySelection, DevelopmentChannel,
            DevelopmentInitiator, ResolvedRepository, WorkspaceState,
        },
    };
    use centaur_session_sqlx::PgSessionStore;
    use serde_json::{Value, json};

    use crate::{
        EnsureSessionSandboxRequest, SandboxRuntime, SessionPrincipalRegistrar, SessionRuntime,
    };

    #[derive(Default)]
    struct FakeWorkspaceManager {
        requests: Mutex<Vec<WorkspacePreparationRequest>>,
    }

    #[async_trait]
    impl WorkspaceManager for FakeWorkspaceManager {
        async fn prepare(
            &self,
            request: WorkspacePreparationRequest,
        ) -> Result<WorkspacePreparation, centaur_sandbox_core::WorkspaceError> {
            let prepared = request
                .repositories
                .iter()
                .map(
                    |repository| centaur_sandbox_core::PreparedWorkspaceRepository {
                        repository_id: repository.repository_id.clone(),
                        base_sha: "a".repeat(40),
                        local_branch: "centaur/test".to_owned(),
                        head_sha: "a".repeat(40),
                    },
                )
                .collect();
            self.requests.lock().unwrap().push(request.clone());
            Ok(WorkspacePreparation {
                workspace_id: request.workspace_id,
                storage_ref: "workspace-test-pvc".to_owned(),
                prepared,
                failed: Vec::new(),
            })
        }

        async fn collect(
            &self,
            request: WorkspaceCollectionRequest,
        ) -> Result<WorkspaceCollection, centaur_sandbox_core::WorkspaceError> {
            Ok(WorkspaceCollection {
                workspace_id: request.workspace_id,
                execution_id: request.execution_id,
                repositories: Vec::new(),
            })
        }
    }

    #[derive(Default)]
    struct RecordingBackend {
        specs: Mutex<Vec<SandboxSpec>>,
    }

    #[async_trait]
    impl SandboxBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            "workspace-recording"
        }

        async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            self.specs.lock().unwrap().push(spec);
            Ok(SandboxHandle::new(
                SandboxId::new("workspace-sandbox"),
                self.name(),
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            Err(SandboxError::io("not used"))
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(SandboxStatus::Running)
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            Ok(ObservedSandbox::new(
                id.clone(),
                self.name(),
                SandboxStatus::Running,
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

    #[derive(Clone, Copy)]
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
            id: "prn_workspace_test".to_owned(),
            foreign_id: None,
            name: "Workspace Test".to_owned(),
            labels: Default::default(),
            sandbox_observability_enabled: true,
            sandbox_api_server_enabled: true,
        }
    }

    async fn store() -> Option<PgSessionStore> {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return None;
        };
        let store = PgSessionStore::connect(&url).await.unwrap();
        store.run_migrations().await.unwrap();
        Some(store)
    }

    #[tokio::test]
    async fn workspace_preparation_persists_fake_manager_result() {
        let Some(store) = store().await else {
            return;
        };
        let suffix = uuid::Uuid::new_v4();
        let task = AcceptDevelopmentTask {
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
                client_message_id: Some(format!("message-{suffix}")),
                role: MessageRole::User,
                parts: vec![json!({"type": "text", "text": "Fix it"})],
                metadata: json!({}),
            },
            session_metadata: json!({}),
        };
        let accepted = store.accept_development_task(&task).await.unwrap();
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: accepted.selection_flow_id,
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![ResolvedRepository {
                    repository_id: "gitlab:42".parse().unwrap(),
                    display_name: "Project".to_owned(),
                    path_with_namespace: "platform/project".to_owned(),
                    default_branch: "main".to_owned(),
                    clone_url: "http://git.example.test:82/platform/project.git".to_owned(),
                    relative_path: "repos/42-project".to_owned(),
                }],
            })
            .await
            .unwrap();
        sqlx::query(
            "update session_executions set status = 'cancelled', blocking_reason = null where execution_id = $1",
        )
        .bind(&accepted.execution_id)
        .execute(store.pool())
        .await
        .unwrap();

        let manager = Arc::new(FakeWorkspaceManager::default());
        let runtime = SessionRuntime::new(
            store.clone(),
            SandboxRuntime::backend(
                Arc::new(RecordingBackend::default()),
                SandboxSpec::new("test"),
            ),
            Registrar,
        )
        .with_workspace_manager(manager.clone(), "gitlab-token-secret");
        runtime
            .prepare_workspace(&accepted.workspace_id)
            .await
            .unwrap();

        let request = manager.requests.lock().unwrap().first().unwrap().clone();
        assert_eq!(request.credential_ref, "gitlab-token-secret");
        assert_eq!(request.repositories[0].relative_path, "repos/42-project");
        let workspace = store
            .workspace_for_session(&accepted.thread_key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(workspace.state, WorkspaceState::Ready);
        assert_eq!(workspace.storage_ref.as_deref(), Some("workspace-test-pvc"));
    }

    #[tokio::test]
    async fn ready_workspace_mounts_pvc_without_gitlab_credentials() {
        let Some(store) = store().await else {
            return;
        };
        let thread_key = format!("api:workspace-mount-{}", uuid::Uuid::new_v4())
            .parse()
            .unwrap();
        store
            .create_or_get_session(
                &thread_key,
                &HarnessType::Codex,
                None,
                json!({}),
                Default::default(),
            )
            .await
            .unwrap();
        let workspace = store.create_or_get_workspace(&thread_key).await.unwrap();
        sqlx::query(
            "update session_workspaces set state = 'ready', storage_ref = 'workspace-safe-pvc' where workspace_id = $1",
        )
        .bind(&workspace.workspace_id)
        .execute(store.pool())
        .await
        .unwrap();

        let backend = Arc::new(RecordingBackend::default());
        let runtime = SessionRuntime::new(
            store,
            SandboxRuntime::backend(backend.clone(), SandboxSpec::new("test")),
            Registrar,
        );
        runtime
            .ensure_session_sandbox(EnsureSessionSandboxRequest {
                thread_key: &thread_key,
                harness_type: &HarnessType::Codex,
                persona_id: None,
                existing_sandbox_id: None,
                existing_sandbox_capabilities: None,
                iron_control_principal: None,
                proxy_labels: &Default::default(),
                desired_capabilities: &centaur_session_core::SandboxCapabilities::default_enabled(),
                execution_id: "exe_workspace_mount",
            })
            .await
            .unwrap();

        let spec = backend.specs.lock().unwrap().first().unwrap().clone();
        assert_eq!(spec.working_dir.as_deref(), Some("/workspace"));
        assert!(spec.mounts.iter().any(|mount| {
            mount.target_path == "/workspace"
                && mount.kind
                    == centaur_sandbox_core::MountKind::NamedVolume("workspace-safe-pvc".to_owned())
        }));
        let rendered = serde_json::to_string(&spec).unwrap();
        assert!(!rendered.contains("gitlab-token"));
        assert!(!rendered.contains("GITLAB"));
    }
}
