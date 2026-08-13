use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{EnvVar, Mount, MountKind, SandboxSpec};

pub const WORKSPACE_MOUNT_PATH: &str = "/workspace";
pub const WORKSPACE_ROOT_ENV: &str = "CENTAUR_WORKSPACE_ROOT";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRepository {
    pub repository_id: String,
    pub display_name: String,
    pub path_with_namespace: String,
    pub default_branch: String,
    pub clone_url: String,
    pub relative_path: String,
    /// Present for an append operation so the manager can preserve existing
    /// repository state while rebuilding the manifest.
    pub existing: Option<PreparedWorkspaceRepository>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePreparationRequest {
    pub workspace_id: String,
    pub thread_key: String,
    pub attempt: u32,
    /// Opaque reference interpreted only by the concrete provisioner.
    pub credential_ref: String,
    pub repositories: Vec<WorkspaceRepository>,
}

impl WorkspacePreparationRequest {
    pub fn new(
        workspace_id: impl Into<String>,
        thread_key: impl Into<String>,
        attempt: u32,
        credential_ref: impl Into<String>,
        repositories: Vec<WorkspaceRepository>,
    ) -> Result<Self, WorkspaceError> {
        let request = Self {
            workspace_id: workspace_id.into(),
            thread_key: thread_key.into(),
            attempt,
            credential_ref: credential_ref.into(),
            repositories,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), WorkspaceError> {
        for (name, value) in [
            ("workspace_id", self.workspace_id.as_str()),
            ("thread_key", self.thread_key.as_str()),
            ("credential_ref", self.credential_ref.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(WorkspaceError::Invalid(format!("{name} must not be empty")));
            }
        }
        if self.attempt == 0 {
            return Err(WorkspaceError::Invalid(
                "workspace preparation attempt must be positive".to_owned(),
            ));
        }
        let mut repository_ids = HashSet::with_capacity(self.repositories.len());
        let mut relative_paths = HashSet::with_capacity(self.repositories.len());
        for repository in &self.repositories {
            let project_id = gitlab_project_id(&repository.repository_id)?;
            let expected_path =
                repository_relative_path(project_id, &repository.path_with_namespace);
            if repository.relative_path != expected_path {
                return Err(WorkspaceError::Invalid(format!(
                    "repository {} must use relative path {expected_path}",
                    repository.repository_id
                )));
            }
            if repository.display_name.trim().is_empty()
                || repository.path_with_namespace.trim().is_empty()
                || repository.default_branch.trim().is_empty()
                || repository.clone_url.trim().is_empty()
            {
                return Err(WorkspaceError::Invalid(format!(
                    "repository {} has incomplete provider metadata",
                    repository.repository_id
                )));
            }
            if !repository_ids.insert(repository.repository_id.as_str()) {
                return Err(WorkspaceError::Invalid(format!(
                    "repository {} appears more than once",
                    repository.repository_id
                )));
            }
            if !relative_paths.insert(repository.relative_path.as_str()) {
                return Err(WorkspaceError::Invalid(format!(
                    "workspace repository path {} appears more than once",
                    repository.relative_path
                )));
            }
            if let Some(existing) = &repository.existing
                && existing.repository_id != repository.repository_id
            {
                return Err(WorkspaceError::Invalid(format!(
                    "existing preparation does not match repository {}",
                    repository.repository_id
                )));
            }
        }
        Ok(())
    }

    pub fn manifest_json(&self) -> Result<String, WorkspaceError> {
        let manifest = WorkspaceManifest {
            version: 1,
            workspace_id: &self.workspace_id,
            thread_key: &self.thread_key,
            repositories: self
                .repositories
                .iter()
                .map(|repository| WorkspaceManifestRepository {
                    repository_id: &repository.repository_id,
                    display_name: &repository.display_name,
                    path_with_namespace: &repository.path_with_namespace,
                    default_branch: &repository.default_branch,
                    relative_path: &repository.relative_path,
                    base_sha: repository
                        .existing
                        .as_ref()
                        .map(|value| value.base_sha.as_str()),
                    local_branch: repository
                        .existing
                        .as_ref()
                        .map(|value| value.local_branch.as_str()),
                    head_sha: repository
                        .existing
                        .as_ref()
                        .map(|value| value.head_sha.as_str()),
                })
                .collect(),
        };
        serde_json::to_string_pretty(&manifest)
            .map_err(|error| WorkspaceError::Invalid(format!("workspace manifest: {error}")))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedWorkspaceRepository {
    pub repository_id: String,
    pub base_sha: String,
    pub local_branch: String,
    pub head_sha: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FailedWorkspaceRepository {
    pub repository_id: String,
    pub failure_code: String,
    pub failure_message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePreparation {
    pub workspace_id: String,
    pub storage_ref: String,
    #[serde(default)]
    pub prepared: Vec<PreparedWorkspaceRepository>,
    #[serde(default)]
    pub failed: Vec<FailedWorkspaceRepository>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCollectionRequest {
    pub workspace_id: String,
    pub execution_id: String,
    pub storage_ref: String,
    pub repositories: Vec<WorkspaceCollectionRepository>,
}

impl WorkspaceCollectionRequest {
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        for (name, value) in [
            ("workspace_id", self.workspace_id.as_str()),
            ("execution_id", self.execution_id.as_str()),
            ("storage_ref", self.storage_ref.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(WorkspaceError::Invalid(format!("{name} must not be empty")));
            }
        }
        let mut repository_ids = HashSet::with_capacity(self.repositories.len());
        let mut relative_paths = HashSet::with_capacity(self.repositories.len());
        for repository in &self.repositories {
            let project_id = gitlab_project_id(&repository.repository_id)?;
            if repository.relative_path
                != repository_relative_path(project_id, &repository.path_with_namespace)
            {
                return Err(WorkspaceError::Invalid(format!(
                    "repository {} has an invalid collection path",
                    repository.repository_id
                )));
            }
            validate_git_sha(&repository.base_sha)?;
            validate_git_sha(&repository.recorded_head_sha)?;
            if repository.local_branch.trim().is_empty()
                || !repository_ids.insert(repository.repository_id.as_str())
                || !relative_paths.insert(repository.relative_path.as_str())
            {
                return Err(WorkspaceError::Invalid(
                    "workspace collection contains invalid repository metadata".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCollectionRepository {
    pub repository_id: String,
    pub path_with_namespace: String,
    pub relative_path: String,
    pub base_sha: String,
    pub recorded_head_sha: String,
    pub local_branch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCollection {
    pub workspace_id: String,
    pub execution_id: String,
    pub repositories: Vec<CollectedWorkspaceRepository>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitLabPushRequest {
    pub publish_item_id: String,
    pub attempt: u32,
    /// Opaque reference interpreted only by the concrete publisher.
    pub credential_ref: String,
    pub workspace_id: String,
    pub storage_ref: String,
    pub relative_path: String,
    pub clone_url: String,
    pub source_branch: String,
    pub head_sha: String,
}

impl GitLabPushRequest {
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        validate_publish_fields(&[
            ("publish_item_id", &self.publish_item_id),
            ("credential_ref", &self.credential_ref),
            ("workspace_id", &self.workspace_id),
            ("storage_ref", &self.storage_ref),
            ("relative_path", &self.relative_path),
            ("clone_url", &self.clone_url),
            ("source_branch", &self.source_branch),
        ])?;
        if self.attempt == 0 {
            return Err(WorkspaceError::Invalid(
                "publication attempt must be positive".to_owned(),
            ));
        }
        validate_publish_relative_path(&self.relative_path)?;
        validate_clone_url(&self.clone_url)?;
        validate_source_branch(&self.source_branch)?;
        validate_git_sha(&self.head_sha)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitLabPushResult {
    pub remote_branch_sha: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitLabMergeRequestRequest {
    pub publish_item_id: String,
    pub attempt: u32,
    /// Opaque reference interpreted only by the concrete publisher.
    pub credential_ref: String,
    pub project_id: u64,
    pub clone_url: String,
    pub source_branch: String,
    pub target_branch: String,
    pub head_sha: String,
    pub remote_branch_sha: String,
    pub changeset_id: String,
}

impl GitLabMergeRequestRequest {
    pub fn validate(&self) -> Result<(), WorkspaceError> {
        validate_publish_fields(&[
            ("publish_item_id", &self.publish_item_id),
            ("credential_ref", &self.credential_ref),
            ("clone_url", &self.clone_url),
            ("source_branch", &self.source_branch),
            ("target_branch", &self.target_branch),
            ("changeset_id", &self.changeset_id),
        ])?;
        if self.attempt == 0 || self.project_id == 0 {
            return Err(WorkspaceError::Invalid(
                "publication attempt and GitLab project ID must be positive".to_owned(),
            ));
        }
        validate_clone_url(&self.clone_url)?;
        validate_source_branch(&self.source_branch)?;
        validate_source_branch(&self.target_branch)?;
        validate_git_sha(&self.head_sha)?;
        validate_git_sha(&self.remote_branch_sha)?;
        if self.remote_branch_sha != self.head_sha {
            return Err(WorkspaceError::Invalid(
                "remote branch must match the reviewed commit".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GitLabMergeRequestResult {
    pub merge_request_iid: i64,
    pub merge_request_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollectedWorkspaceRepository {
    pub repository_id: String,
    pub state: WorkspaceCollectionState,
    pub base_sha: String,
    pub recorded_head_sha: String,
    pub head_sha: Option<String>,
    #[serde(default)]
    pub commit_metadata: Vec<serde_json::Value>,
    pub changed_file_count: u32,
    pub additions: u32,
    pub deletions: u32,
    pub patch_hash: Option<String>,
    pub patch_base64: Option<String>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCollectionState {
    Unchanged,
    Changed,
    NeedsAgentCompletion,
    Failed,
}

impl WorkspacePreparation {
    pub fn is_ready(&self) -> bool {
        self.failed.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceMount {
    pub storage_ref: String,
}

impl WorkspaceMount {
    pub fn new(storage_ref: impl Into<String>) -> Self {
        Self {
            storage_ref: storage_ref.into(),
        }
    }

    pub fn apply_to(&self, mut spec: SandboxSpec) -> SandboxSpec {
        spec.mounts
            .retain(|mount| mount.target_path != WORKSPACE_MOUNT_PATH);
        spec.env.retain(|env| env.name != WORKSPACE_ROOT_ENV);
        spec.env
            .push(EnvVar::new(WORKSPACE_ROOT_ENV, WORKSPACE_MOUNT_PATH));
        spec.mounts.push(Mount::new(
            MountKind::NamedVolume(self.storage_ref.clone()),
            WORKSPACE_MOUNT_PATH,
        ));
        spec.working_dir = Some(WORKSPACE_MOUNT_PATH.to_owned());
        spec
    }
}

#[async_trait]
pub trait WorkspaceManager: Send + Sync {
    async fn prepare(
        &self,
        request: WorkspacePreparationRequest,
    ) -> Result<WorkspacePreparation, WorkspaceError>;

    async fn collect(
        &self,
        request: WorkspaceCollectionRequest,
    ) -> Result<WorkspaceCollection, WorkspaceError>;
}

#[async_trait]
pub trait GitLabPublisher: Send + Sync {
    async fn push(&self, request: GitLabPushRequest) -> Result<GitLabPushResult, WorkspaceError>;

    async fn ensure_merge_request(
        &self,
        request: GitLabMergeRequestRequest,
    ) -> Result<GitLabMergeRequestResult, WorkspaceError>;
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("invalid workspace preparation: {0}")]
    Invalid(String),
    #[error("workspace preparation backend failed: {0}")]
    Backend(String),
}

#[derive(Serialize)]
struct WorkspaceManifest<'a> {
    version: u8,
    workspace_id: &'a str,
    thread_key: &'a str,
    repositories: Vec<WorkspaceManifestRepository<'a>>,
}

#[derive(Serialize)]
struct WorkspaceManifestRepository<'a> {
    repository_id: &'a str,
    display_name: &'a str,
    path_with_namespace: &'a str,
    default_branch: &'a str,
    relative_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_sha: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_branch: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_sha: Option<&'a str>,
}

fn gitlab_project_id(repository_id: &str) -> Result<u64, WorkspaceError> {
    let project_id = repository_id
        .strip_prefix("gitlab:")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            WorkspaceError::Invalid(format!(
                "repository_id {repository_id:?} is not a GitLab project ID"
            ))
        })?;
    if repository_id != format!("gitlab:{project_id}") {
        return Err(WorkspaceError::Invalid(format!(
            "repository_id {repository_id:?} is not canonical"
        )));
    }
    Ok(project_id)
}

fn validate_git_sha(value: &str) -> Result<(), WorkspaceError> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorkspaceError::Invalid(
            "workspace collection contains an invalid Git SHA".to_owned(),
        ));
    }
    Ok(())
}

fn validate_publish_fields(fields: &[(&str, &String)]) -> Result<(), WorkspaceError> {
    for (name, value) in fields {
        if value.trim().is_empty() || value.contains(['\0', '\n', '\r']) {
            return Err(WorkspaceError::Invalid(format!(
                "publication {name} is invalid"
            )));
        }
    }
    Ok(())
}

fn validate_publish_relative_path(value: &str) -> Result<(), WorkspaceError> {
    if !value.starts_with("repos/")
        || value.starts_with('/')
        || value
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err(WorkspaceError::Invalid(
            "publication repository path is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_clone_url(value: &str) -> Result<(), WorkspaceError> {
    let Some((scheme, authority_and_path)) = value.split_once("://") else {
        return Err(WorkspaceError::Invalid(
            "publication clone URL must be HTTP(S)".to_owned(),
        ));
    };
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    if !matches!(scheme, "http" | "https")
        || authority.is_empty()
        || authority.contains('@')
        || !authority_and_path.contains('/')
    {
        return Err(WorkspaceError::Invalid(
            "publication clone URL must be credential-free HTTP(S)".to_owned(),
        ));
    }
    Ok(())
}

fn validate_source_branch(value: &str) -> Result<(), WorkspaceError> {
    let invalid = value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("@{")
        || value.contains("//")
        || value.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        });
    if invalid {
        return Err(WorkspaceError::Invalid(
            "publication branch name is invalid".to_owned(),
        ));
    }
    Ok(())
}

pub fn repository_relative_path(project_id: u64, path_with_namespace: &str) -> String {
    let slug = path_with_namespace
        .rsplit('/')
        .next()
        .map(sanitize_path_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repository".to_owned());
    format!("repos/{project_id}-{slug}")
}

fn sanitize_path_component(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            separator = false;
        } else if !separator && !output.is_empty() {
            output.push('-');
            separator = true;
        }
        if output.len() >= 48 {
            break;
        }
    }
    output.trim_end_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(id: u64, relative_path: &str) -> WorkspaceRepository {
        WorkspaceRepository {
            repository_id: format!("gitlab:{id}"),
            display_name: format!("Project {id}"),
            path_with_namespace: "platform/project".to_owned(),
            default_branch: "main".to_owned(),
            clone_url: format!("http://git.example.test:82/platform/project-{id}.git"),
            relative_path: relative_path.to_owned(),
            existing: None,
        }
    }

    #[test]
    fn workspace_plan_requires_deterministic_unique_repository_paths() {
        let valid = WorkspacePreparationRequest::new(
            "wsp_123",
            "development:123",
            1,
            "gitlab-catalog",
            vec![repository(42, "repos/42-project")],
        )
        .unwrap();
        assert_eq!(valid.repositories[0].relative_path, "repos/42-project");

        for repositories in [
            vec![repository(42, "../escape")],
            vec![repository(42, "/workspace/repos/42-project")],
            vec![repository(42, "repos/7-project")],
            vec![
                repository(42, "repos/42-project"),
                repository(43, "repos/42-project"),
            ],
        ] {
            assert!(
                WorkspacePreparationRequest::new(
                    "wsp_123",
                    "development:123",
                    1,
                    "gitlab-catalog",
                    repositories,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn workspace_manifest_and_mount_expose_no_git_credentials() {
        let request = WorkspacePreparationRequest::new(
            "wsp_123",
            "development:123",
            1,
            "secret/team-gitlab-token",
            vec![repository(42, "repos/42-project")],
        )
        .unwrap();
        let manifest = request.manifest_json().unwrap();
        assert!(manifest.contains("gitlab:42"));
        assert!(!manifest.contains("clone_url"));
        assert!(!manifest.contains("git.example.test"));
        assert!(!manifest.contains("secret/team-gitlab-token"));

        let mount = WorkspaceMount::new("workspace-wsp-123");
        let spec = mount.apply_to(mount.apply_to(SandboxSpec::new("agent")));
        assert_eq!(spec.working_dir.as_deref(), Some("/workspace"));
        assert_eq!(spec.mounts.len(), 1);
        assert_eq!(
            spec.mounts[0],
            Mount::new(
                MountKind::NamedVolume("workspace-wsp-123".to_owned()),
                "/workspace"
            )
        );
        assert_eq!(
            spec.env
                .iter()
                .filter(|env| env.name == "CENTAUR_WORKSPACE_ROOT")
                .map(|env| env.value.as_str())
                .collect::<Vec<_>>(),
            vec!["/workspace"]
        );
        let serialized = serde_json::to_string(&spec).unwrap();
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("credential"));
    }

    #[test]
    fn workspace_collection_requires_exact_repository_snapshot_metadata() {
        let valid = WorkspaceCollectionRequest {
            workspace_id: "wsp_123".to_owned(),
            execution_id: "exe_123".to_owned(),
            storage_ref: "workspace-wsp-123".to_owned(),
            repositories: vec![WorkspaceCollectionRepository {
                repository_id: "gitlab:42".to_owned(),
                path_with_namespace: "platform/project".to_owned(),
                relative_path: "repos/42-project".to_owned(),
                base_sha: "a".repeat(40),
                recorded_head_sha: "b".repeat(40),
                local_branch: "centaur/wsp-123".to_owned(),
            }],
        };
        assert!(valid.validate().is_ok());

        for invalid in [
            WorkspaceCollectionRequest {
                repositories: vec![WorkspaceCollectionRepository {
                    base_sha: "not-a-sha".to_owned(),
                    ..valid.repositories[0].clone()
                }],
                ..valid.clone()
            },
            WorkspaceCollectionRequest {
                repositories: vec![WorkspaceCollectionRepository {
                    relative_path: "../escape".to_owned(),
                    ..valid.repositories[0].clone()
                }],
                ..valid.clone()
            },
            WorkspaceCollectionRequest {
                repositories: vec![valid.repositories[0].clone(), valid.repositories[0].clone()],
                ..valid.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn publisher_requests_reject_unsafe_or_mismatched_git_inputs() {
        let push = GitLabPushRequest {
            publish_item_id: "pbi_123".to_owned(),
            attempt: 1,
            credential_ref: "gitlab-token".to_owned(),
            workspace_id: "wsp_123".to_owned(),
            storage_ref: "workspace-wsp-123".to_owned(),
            relative_path: "repos/42-project".to_owned(),
            clone_url: "https://git.example.test/group/project.git".to_owned(),
            source_branch: "centaur/123/456".to_owned(),
            head_sha: "a".repeat(40),
        };
        assert!(push.validate().is_ok());

        let mut unsafe_url = push.clone();
        unsafe_url.clone_url =
            "https://oauth2:secret@git.example.test/group/project.git".to_owned();
        assert!(unsafe_url.validate().is_err());

        let mut symbolic_sha = push.clone();
        symbolic_sha.head_sha = "HEAD".to_owned();
        assert!(symbolic_sha.validate().is_err());

        let merge_request = GitLabMergeRequestRequest {
            publish_item_id: push.publish_item_id.clone(),
            attempt: push.attempt,
            credential_ref: push.credential_ref.clone(),
            project_id: 42,
            clone_url: push.clone_url.clone(),
            source_branch: push.source_branch.clone(),
            target_branch: "main".to_owned(),
            head_sha: push.head_sha.clone(),
            remote_branch_sha: push.head_sha.clone(),
            changeset_id: "chg_456".to_owned(),
        };
        assert!(merge_request.validate().is_ok());

        let mut mismatched = merge_request;
        mismatched.remote_branch_sha = "b".repeat(40);
        assert!(mismatched.validate().is_err());
    }
}
