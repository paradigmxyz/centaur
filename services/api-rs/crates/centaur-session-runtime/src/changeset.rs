#[cfg(test)]
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use centaur_sandbox_core::{
    WorkspaceCollectionRepository, WorkspaceCollectionRequest, WorkspaceCollectionState,
};
#[cfg(test)]
use centaur_session_core::development::WorkspaceRepositorySnapshot;
use centaur_session_core::development::{
    ChangeSetState, CollectedChangeSetRepositoryState, CompleteChangeSetCollection,
    CompleteChangeSetRepository, RepositoryState,
};
#[cfg(test)]
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use sha2::{Digest, Sha256};
#[cfg(test)]
use thiserror::Error;
#[cfg(test)]
use tokio::{process::Command, time::timeout};
use tracing::{error, info, warn};

use crate::{RuntimeContext, SessionRuntime, SessionRuntimeError};

#[cfg(test)]
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const MAX_GIT_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
#[cfg(test)]
const FIELD_SEPARATOR: char = '\u{1f}';
#[cfg(test)]
const RECORD_SEPARATOR: char = '\u{1e}';
const CHANGESET_COLLECTION_LEASE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl SessionRuntime {
    pub async fn get_changeset(
        &self,
        changeset_id: &str,
        principal_id: &str,
        is_admin: bool,
    ) -> Result<centaur_session_core::development::DevelopmentChangeSet, SessionRuntimeError> {
        Ok(self
            .store
            .get_changeset(changeset_id, principal_id, is_admin)
            .await?)
    }

    pub async fn get_changeset_artifact(
        &self,
        changeset_id: &str,
        artifact_ref: &str,
        principal_id: &str,
        is_admin: bool,
    ) -> Result<Vec<u8>, SessionRuntimeError> {
        Ok(self
            .store
            .get_changeset_artifact(changeset_id, artifact_ref, principal_id, is_admin)
            .await?)
    }

    pub fn spawn_changeset_reconciliation(&self, interval: std::time::Duration) {
        let runtime = self.clone();
        tokio::spawn(async move {
            loop {
                match runtime.store.list_collecting_changeset_ids().await {
                    Ok(changeset_ids) => {
                        for changeset_id in changeset_ids {
                            runtime.spawn_changeset_collection(changeset_id);
                        }
                    }
                    Err(error) => warn!(%error, "failed to list collecting changesets"),
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    pub fn spawn_changeset_collection(&self, changeset_id: String) {
        Self::spawn_changeset_collection_from_context(self.context(), changeset_id);
    }

    pub(super) fn spawn_changeset_collection_from_context(
        ctx: RuntimeContext,
        changeset_id: String,
    ) {
        if ctx.workspace.is_none() {
            warn!(
                changeset_id,
                "workspace manager is not configured for changeset collection"
            );
            return;
        }
        tokio::spawn(async move {
            if let Err(error) = collect_changeset(&ctx, &changeset_id).await {
                error!(changeset_id, %error, "changeset collection failed");
            }
        });
    }
}

async fn collect_changeset(
    ctx: &RuntimeContext,
    changeset_id: &str,
) -> Result<(), SessionRuntimeError> {
    let workspace_runtime = ctx.workspace.as_ref().ok_or_else(|| {
        SessionRuntimeError::Workspace(centaur_sandbox_core::WorkspaceError::Invalid(
            "workspace manager is not configured".to_owned(),
        ))
    })?;
    let claim = ctx
        .store
        .claim_changeset_collection(
            changeset_id,
            &workspace_runtime.lease_owner,
            CHANGESET_COLLECTION_LEASE,
        )
        .await?;
    let storage_ref = claim.workspace.storage_ref.clone().ok_or_else(|| {
        SessionRuntimeError::Workspace(centaur_sandbox_core::WorkspaceError::Invalid(
            "changeset workspace has no storage reference".to_owned(),
        ))
    })?;
    let repositories = claim
        .repositories
        .iter()
        .map(|repository| {
            if repository.state != RepositoryState::Ready {
                return Err(SessionRuntimeError::BadRequest(format!(
                    "repository {} is not ready for collection",
                    repository.repository_id
                )));
            }
            Ok(WorkspaceCollectionRepository {
                repository_id: repository.repository_id.to_string(),
                path_with_namespace: repository.path_with_namespace.clone(),
                relative_path: repository.relative_path.clone(),
                base_sha: repository.base_sha.clone().ok_or_else(|| {
                    SessionRuntimeError::BadRequest("repository has no base SHA".to_owned())
                })?,
                recorded_head_sha: repository.head_sha.clone().ok_or_else(|| {
                    SessionRuntimeError::BadRequest(
                        "repository has no recorded head SHA".to_owned(),
                    )
                })?,
                local_branch: repository.local_branch.clone().ok_or_else(|| {
                    SessionRuntimeError::BadRequest("repository has no local branch".to_owned())
                })?,
            })
        })
        .collect::<Result<Vec<_>, SessionRuntimeError>>()?;
    let collected = workspace_runtime
        .manager
        .collect(WorkspaceCollectionRequest {
            workspace_id: claim.workspace.workspace_id.clone(),
            execution_id: claim.changeset.execution_id.clone(),
            storage_ref,
            repositories,
        })
        .await?;
    let completed = ctx
        .store
        .complete_changeset_collection(&CompleteChangeSetCollection {
            changeset_id: changeset_id.to_owned(),
            lease_owner: workspace_runtime.lease_owner.clone(),
            repositories: collected
                .repositories
                .into_iter()
                .map(|repository| {
                    let patch = repository
                        .patch_base64
                        .as_deref()
                        .map(|encoded| {
                            STANDARD.decode(encoded).map_err(|_| {
                                SessionRuntimeError::BadRequest(
                                    "changeset collector returned invalid patch data".to_owned(),
                                )
                            })
                        })
                        .transpose()?
                        .unwrap_or_default();
                    Ok(CompleteChangeSetRepository {
                        repository_id: repository.repository_id.parse().map_err(|error| {
                            SessionRuntimeError::BadRequest(format!(
                                "changeset repository ID is invalid: {error}"
                            ))
                        })?,
                        state: match repository.state {
                            WorkspaceCollectionState::Unchanged => {
                                CollectedChangeSetRepositoryState::Unchanged
                            }
                            WorkspaceCollectionState::Changed => {
                                CollectedChangeSetRepositoryState::Changed
                            }
                            WorkspaceCollectionState::NeedsAgentCompletion => {
                                CollectedChangeSetRepositoryState::NeedsAgentCompletion
                            }
                            WorkspaceCollectionState::Failed => {
                                CollectedChangeSetRepositoryState::Failed
                            }
                        },
                        base_sha: repository.base_sha,
                        recorded_head_sha: repository.recorded_head_sha,
                        head_sha: repository.head_sha,
                        commit_metadata: serde_json::Value::Array(repository.commit_metadata),
                        changed_file_count: i32::try_from(repository.changed_file_count).map_err(
                            |_| {
                                SessionRuntimeError::BadRequest(
                                    "changeset file count is too large".to_owned(),
                                )
                            },
                        )?,
                        additions: i32::try_from(repository.additions).map_err(|_| {
                            SessionRuntimeError::BadRequest(
                                "changeset additions count is too large".to_owned(),
                            )
                        })?,
                        deletions: i32::try_from(repository.deletions).map_err(|_| {
                            SessionRuntimeError::BadRequest(
                                "changeset deletions count is too large".to_owned(),
                            )
                        })?,
                        patch_hash: repository.patch_hash,
                        patch,
                        test_evidence: test_evidence_for_repository(
                            &claim.execution_metadata,
                            &repository.repository_id,
                        ),
                        failure_code: repository.failure_code,
                        failure_message: repository.failure_message,
                    })
                })
                .collect::<Result<Vec<_>, SessionRuntimeError>>()?,
        })
        .await?;
    let (event_type, payload) = match completed {
        Some(changeset) => {
            let event_type = match changeset.state {
                ChangeSetState::Ready => "development.changeset_ready",
                ChangeSetState::NeedsAgentCompletion => {
                    "development.changeset_needs_agent_completion"
                }
                ChangeSetState::Failed => "development.changeset_failed",
                ChangeSetState::Collecting => {
                    return Err(SessionRuntimeError::BadRequest(
                        "completed changeset remained collecting".to_owned(),
                    ));
                }
            };
            (
                event_type,
                serde_json::json!({
                    "changeset_id": changeset.changeset_id,
                    "workspace_id": changeset.workspace_id,
                    "execution_id": changeset.execution_id,
                    "state": changeset.state,
                    "repository_count": changeset.repositories.len(),
                }),
            )
        }
        None => (
            "development.changeset_empty",
            serde_json::json!({
                "workspace_id": claim.workspace.workspace_id,
                "execution_id": claim.changeset.execution_id,
            }),
        ),
    };
    ctx.store
        .append_event(
            &claim.workspace.thread_key,
            Some(&claim.changeset.execution_id),
            event_type,
            payload,
        )
        .await?;
    info!(changeset_id, event_type, "changeset collection recorded");
    Ok(())
}

fn test_evidence_for_repository(metadata: &Value, repository_id: &str) -> Value {
    let Some(evidence) = metadata.get("test_evidence").and_then(Value::as_array) else {
        return serde_json::json!([]);
    };
    Value::Array(
        evidence
            .iter()
            .filter(|item| item.get("repository_id").and_then(Value::as_str) == Some(repository_id))
            .cloned()
            .collect(),
    )
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectedRepositoryState {
    Unchanged,
    Changed,
    NeedsAgentCompletion,
    Failed,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollectedRepository {
    pub repository_id: String,
    pub state: CollectedRepositoryState,
    pub base_sha: String,
    pub recorded_head_sha: String,
    pub head_sha: Option<String>,
    pub commit_metadata: Vec<Value>,
    pub changed_file_count: u32,
    pub additions: u32,
    pub deletions: u32,
    pub patch_hash: Option<String>,
    #[serde(with = "hex_bytes")]
    pub patch: Vec<u8>,
    pub failure_code: Option<String>,
    pub failure_message: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Error)]
pub enum ChangeSetCollectionError {
    #[error("invalid repository collection request: {0}")]
    Invalid(String),
    #[error("git command failed: {0}")]
    Git(String),
}

#[cfg(test)]
pub async fn collect_repository(
    repository_path: &Path,
    repository: &WorkspaceRepositorySnapshot,
) -> Result<CollectedRepository, ChangeSetCollectionError> {
    let base_sha = repository.base_sha.clone().ok_or_else(|| {
        ChangeSetCollectionError::Invalid(format!(
            "repository {} has no recorded base SHA",
            repository.repository_id
        ))
    })?;
    let recorded_head_sha = repository.head_sha.clone().ok_or_else(|| {
        ChangeSetCollectionError::Invalid(format!(
            "repository {} has no recorded head SHA",
            repository.repository_id
        ))
    })?;
    validate_sha(&base_sha)?;
    validate_sha(&recorded_head_sha)?;
    let canonical_path = repository_path.canonicalize().map_err(|_| {
        ChangeSetCollectionError::Invalid("repository path is unavailable".to_owned())
    })?;
    if !canonical_path.join(".git").is_dir() {
        return Err(ChangeSetCollectionError::Invalid(
            "repository path is not a Git worktree".to_owned(),
        ));
    }
    let git = ReadOnlyGit::new(canonical_path);
    if !git.object_exists(&base_sha).await? {
        return Ok(failed_repository(
            repository,
            base_sha,
            recorded_head_sha,
            "base_object_missing",
            "recorded base commit is unavailable",
        ));
    }
    if !git.object_exists(&recorded_head_sha).await? {
        return Ok(failed_repository(
            repository,
            base_sha,
            recorded_head_sha,
            "recorded_head_object_missing",
            "recorded repository head is unavailable",
        ));
    }
    let head_sha = git
        .output(&["rev-parse", "--verify", "HEAD^{commit}"])
        .await?;
    let head_sha = String::from_utf8(head_sha)
        .map_err(|_| ChangeSetCollectionError::Git("git returned invalid text".to_owned()))?
        .trim()
        .to_owned();
    validate_sha(&head_sha)?;
    if !git.is_ancestor(&base_sha, &head_sha).await? {
        return Ok(failed_repository_with_head(
            repository,
            base_sha,
            recorded_head_sha,
            head_sha,
            "head_not_descendant",
            "repository head does not descend from the recorded base",
        ));
    }
    if !git.is_ancestor(&recorded_head_sha, &head_sha).await? {
        return Ok(failed_repository_with_head(
            repository,
            base_sha,
            recorded_head_sha,
            head_sha,
            "recorded_history_rewritten",
            "repository head rewrites previously recorded workspace history",
        ));
    }
    if !git
        .output(&["status", "--porcelain=v1", "-z"])
        .await?
        .is_empty()
    {
        return Ok(needs_completion(
            repository,
            base_sha,
            recorded_head_sha,
            head_sha,
            "working_tree_dirty",
            "repository has staged, unstaged, or untracked changes",
        ));
    }
    if recorded_head_sha == head_sha {
        return Ok(CollectedRepository {
            repository_id: repository.repository_id.to_string(),
            state: CollectedRepositoryState::Unchanged,
            base_sha,
            recorded_head_sha,
            head_sha: Some(head_sha),
            commit_metadata: Vec::new(),
            changed_file_count: 0,
            additions: 0,
            deletions: 0,
            patch_hash: None,
            patch: Vec::new(),
            failure_code: None,
            failure_message: None,
        });
    }

    let range = format!("{base_sha}..{head_sha}");
    let patch = git
        .output(&[
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            &base_sha,
            &head_sha,
            "--",
        ])
        .await?;
    let patch_hash = format!("sha256:{}", hex::encode(Sha256::digest(&patch)));
    let numstat = git
        .output(&["diff", "--numstat", "-z", &base_sha, &head_sha, "--"])
        .await?;
    let (changed_file_count, additions, deletions) = parse_numstat(&numstat)?;
    let log_format = format!(
        "--format=%H{FIELD_SEPARATOR}%an{FIELD_SEPARATOR}%ae{FIELD_SEPARATOR}%aI{FIELD_SEPARATOR}%s{RECORD_SEPARATOR}"
    );
    let log = git
        .output(&["log", "--reverse", &log_format, &range, "--"])
        .await?;
    let commit_metadata = parse_commit_metadata(&log)?;

    Ok(CollectedRepository {
        repository_id: repository.repository_id.to_string(),
        state: CollectedRepositoryState::Changed,
        base_sha,
        recorded_head_sha,
        head_sha: Some(head_sha),
        commit_metadata,
        changed_file_count,
        additions,
        deletions,
        patch_hash: Some(patch_hash),
        patch,
        failure_code: None,
        failure_message: None,
    })
}

#[cfg(test)]
struct ReadOnlyGit {
    repository_path: PathBuf,
}

#[cfg(test)]
impl ReadOnlyGit {
    fn new(repository_path: PathBuf) -> Self {
        Self { repository_path }
    }

    async fn output(&self, args: &[&str]) -> Result<Vec<u8>, ChangeSetCollectionError> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.repository_path)
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("core.untrackedCache=false")
            .arg("-c")
            .arg("diff.external=")
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true);
        let output = timeout(GIT_TIMEOUT, command.output())
            .await
            .map_err(|_| ChangeSetCollectionError::Git("git command timed out".to_owned()))?
            .map_err(|_| ChangeSetCollectionError::Git("git command could not start".to_owned()))?;
        if output.stdout.len() > MAX_GIT_OUTPUT_BYTES || output.stderr.len() > 64 * 1024 {
            return Err(ChangeSetCollectionError::Git(
                "git command output exceeded the collection limit".to_owned(),
            ));
        }
        if !output.status.success() {
            return Err(ChangeSetCollectionError::Git(format!(
                "git command exited with {}",
                output.status
            )));
        }
        Ok(output.stdout)
    }

    async fn object_exists(&self, sha: &str) -> Result<bool, ChangeSetCollectionError> {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.repository_path)
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(["-c", "core.fsmonitor=false"])
            .args(["-c", "core.untrackedCache=false"])
            .arg("cat-file")
            .arg("-e")
            .arg(format!("{sha}^{{commit}}"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .status();
        let status = timeout(GIT_TIMEOUT, status)
            .await
            .map_err(|_| ChangeSetCollectionError::Git("git command timed out".to_owned()))?
            .map_err(|_| ChangeSetCollectionError::Git("git command could not start".to_owned()))?;
        Ok(status.success())
    }

    async fn is_ancestor(
        &self,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, ChangeSetCollectionError> {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.repository_path)
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(["-c", "core.fsmonitor=false"])
            .args(["-c", "core.untrackedCache=false"])
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .status();
        let status = timeout(GIT_TIMEOUT, status)
            .await
            .map_err(|_| ChangeSetCollectionError::Git("git command timed out".to_owned()))?
            .map_err(|_| ChangeSetCollectionError::Git("git command could not start".to_owned()))?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(ChangeSetCollectionError::Git(
                "git ancestry check failed".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
fn validate_sha(value: &str) -> Result<(), ChangeSetCollectionError> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ChangeSetCollectionError::Invalid(
            "recorded Git SHA is invalid".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn parse_numstat(bytes: &[u8]) -> Result<(u32, u32, u32), ChangeSetCollectionError> {
    let mut files = 0_u32;
    let mut additions = 0_u32;
    let mut deletions = 0_u32;
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let text = std::str::from_utf8(record)
            .map_err(|_| ChangeSetCollectionError::Git("invalid diff statistics".to_owned()))?;
        let mut parts = text.splitn(3, '\t');
        let added = parts.next().unwrap_or_default();
        let deleted = parts.next().unwrap_or_default();
        if parts.next().is_none() {
            return Err(ChangeSetCollectionError::Git(
                "invalid diff statistics".to_owned(),
            ));
        }
        files = files.saturating_add(1);
        additions = additions.saturating_add(added.parse::<u32>().unwrap_or(0));
        deletions = deletions.saturating_add(deleted.parse::<u32>().unwrap_or(0));
    }
    Ok((files, additions, deletions))
}

#[cfg(test)]
fn parse_commit_metadata(bytes: &[u8]) -> Result<Vec<Value>, ChangeSetCollectionError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ChangeSetCollectionError::Git("invalid commit metadata".to_owned()))?;
    text.split(RECORD_SEPARATOR)
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .map(|record| {
            let fields = record.splitn(5, FIELD_SEPARATOR).collect::<Vec<_>>();
            if fields.len() != 5 {
                return Err(ChangeSetCollectionError::Git(
                    "invalid commit metadata".to_owned(),
                ));
            }
            Ok(serde_json::json!({
                "sha": fields[0],
                "author_name": fields[1],
                "author_email": fields[2],
                "authored_at": fields[3],
                "subject": fields[4],
            }))
        })
        .collect()
}

#[cfg(test)]
fn failed_repository(
    repository: &WorkspaceRepositorySnapshot,
    base_sha: String,
    recorded_head_sha: String,
    code: &str,
    message: &str,
) -> CollectedRepository {
    failed_repository_with_optional_head(
        repository,
        base_sha,
        recorded_head_sha,
        None,
        code,
        message,
    )
}

#[cfg(test)]
fn failed_repository_with_head(
    repository: &WorkspaceRepositorySnapshot,
    base_sha: String,
    recorded_head_sha: String,
    head_sha: String,
    code: &str,
    message: &str,
) -> CollectedRepository {
    failed_repository_with_optional_head(
        repository,
        base_sha,
        recorded_head_sha,
        Some(head_sha),
        code,
        message,
    )
}

#[cfg(test)]
fn needs_completion(
    repository: &WorkspaceRepositorySnapshot,
    base_sha: String,
    recorded_head_sha: String,
    head_sha: String,
    code: &str,
    message: &str,
) -> CollectedRepository {
    CollectedRepository {
        state: CollectedRepositoryState::NeedsAgentCompletion,
        ..failed_repository_with_optional_head(
            repository,
            base_sha,
            recorded_head_sha,
            Some(head_sha),
            code,
            message,
        )
    }
}

#[cfg(test)]
fn failed_repository_with_optional_head(
    repository: &WorkspaceRepositorySnapshot,
    base_sha: String,
    recorded_head_sha: String,
    head_sha: Option<String>,
    code: &str,
    message: &str,
) -> CollectedRepository {
    CollectedRepository {
        repository_id: repository.repository_id.to_string(),
        state: CollectedRepositoryState::Failed,
        base_sha,
        recorded_head_sha,
        head_sha,
        commit_metadata: Vec::new(),
        changed_file_count: 0,
        additions: 0,
        deletions: 0,
        patch_hash: None,
        patch: Vec::new(),
        failure_code: Some(code.to_owned()),
        failure_message: Some(message.to_owned()),
    }
}

#[cfg(test)]
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        hex::decode(encoded).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use centaur_session_core::development::{RepositoryId, WorkspaceRepositorySnapshot};

    use super::{CollectedRepositoryState, collect_repository};

    struct TempRepository {
        root: std::path::PathBuf,
    }

    impl TempRepository {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "centaur-changeset-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(&root).unwrap();
            git(&root, &["init", "--initial-branch=main"]);
            git(&root, &["config", "user.name", "Centaur Test"]);
            git(&root, &["config", "user.email", "centaur@example.test"]);
            fs::write(root.join("README.md"), "base\n").unwrap();
            git(&root, &["add", "README.md"]);
            git(&root, &["commit", "-m", "base"]);
            Self { root }
        }

        fn head(&self) -> String {
            git_output(&self.root, &["rev-parse", "HEAD"])
        }

        fn snapshot(&self, base_sha: String) -> WorkspaceRepositorySnapshot {
            WorkspaceRepositorySnapshot {
                repository_id: RepositoryId::parse("gitlab:42").unwrap(),
                display_name: "project".to_owned(),
                path_with_namespace: "group/project".to_owned(),
                default_branch: "main".to_owned(),
                clone_url: "http://git.example.test:82/group/project.git".to_owned(),
                relative_path: ".".to_owned(),
                state: centaur_session_core::development::RepositoryState::Ready,
                base_sha: Some(base_sha.clone()),
                local_branch: Some("main".to_owned()),
                head_sha: Some(base_sha),
            }
        }
    }

    impl Drop for TempRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn changeset_collects_exact_committed_head_and_stable_patch_hash() {
        let repository = TempRepository::new();
        let base = repository.head();
        fs::write(repository.root.join("README.md"), "base\nchanged\n").unwrap();
        fs::write(repository.root.join("new.txt"), "new\n").unwrap();
        git(&repository.root, &["add", "README.md", "new.txt"]);
        git(&repository.root, &["commit", "-m", "implement change"]);
        let expected_head = repository.head();

        let first = collect_repository(&repository.root, &repository.snapshot(base.clone()))
            .await
            .unwrap();
        let second = collect_repository(&repository.root, &repository.snapshot(base))
            .await
            .unwrap();

        assert_eq!(first.state, CollectedRepositoryState::Changed);
        assert_eq!(first.head_sha, Some(expected_head));
        assert_eq!(first.changed_file_count, 2);
        assert_eq!(first.additions, 2);
        assert_eq!(first.deletions, 0);
        assert_eq!(first.commit_metadata.len(), 1);
        assert_eq!(first.patch_hash, second.patch_hash);
        assert_eq!(first.patch, second.patch);
    }

    #[tokio::test]
    async fn changeset_returns_unchanged_for_clean_base_head() {
        let repository = TempRepository::new();
        let base = repository.head();

        let collected = collect_repository(&repository.root, &repository.snapshot(base.clone()))
            .await
            .unwrap();

        assert_eq!(collected.state, CollectedRepositoryState::Unchanged);
        assert_eq!(collected.head_sha.as_deref(), Some(base.as_str()));
        assert!(collected.patch.is_empty());
    }

    #[tokio::test]
    async fn changeset_rejects_dirty_or_untracked_worktrees() {
        for filename in ["README.md", "untracked.txt"] {
            let repository = TempRepository::new();
            let base = repository.head();
            fs::write(repository.root.join(filename), "dirty\n").unwrap();

            let collected = collect_repository(&repository.root, &repository.snapshot(base))
                .await
                .unwrap();

            assert_eq!(
                collected.state,
                CollectedRepositoryState::NeedsAgentCompletion
            );
            assert_eq!(
                collected.failure_code.as_deref(),
                Some("working_tree_dirty")
            );
        }
    }

    #[tokio::test]
    async fn changeset_rejects_missing_or_non_descendant_recorded_history() {
        let missing = TempRepository::new();
        let collected = collect_repository(&missing.root, &missing.snapshot("0".repeat(40)))
            .await
            .unwrap();
        assert_eq!(collected.state, CollectedRepositoryState::Failed);
        assert_eq!(
            collected.failure_code.as_deref(),
            Some("base_object_missing")
        );

        let rewritten = TempRepository::new();
        let base = rewritten.head();
        git(&rewritten.root, &["checkout", "--orphan", "rewritten"]);
        git(&rewritten.root, &["rm", "-rf", "."]);
        fs::write(rewritten.root.join("replacement.txt"), "replacement\n").unwrap();
        git(&rewritten.root, &["add", "replacement.txt"]);
        git(&rewritten.root, &["commit", "-m", "rewrite"]);

        let collected = collect_repository(&rewritten.root, &rewritten.snapshot(base))
            .await
            .unwrap();
        assert_eq!(collected.state, CollectedRepositoryState::Failed);
        assert_eq!(
            collected.failure_code.as_deref(),
            Some("head_not_descendant")
        );
    }

    #[tokio::test]
    async fn changeset_uses_recorded_head_to_detect_incremental_work_and_rewrites() {
        let repository = TempRepository::new();
        let base = repository.head();
        fs::write(repository.root.join("README.md"), "first\n").unwrap();
        git(&repository.root, &["add", "README.md"]);
        git(&repository.root, &["commit", "-m", "first change"]);
        let recorded_head = repository.head();
        let mut snapshot = repository.snapshot(base.clone());
        snapshot.head_sha = Some(recorded_head.clone());

        let unchanged = collect_repository(&repository.root, &snapshot)
            .await
            .unwrap();
        assert_eq!(unchanged.state, CollectedRepositoryState::Unchanged);

        git(&repository.root, &["reset", "--hard", &base]);
        fs::write(repository.root.join("README.md"), "replacement\n").unwrap();
        git(&repository.root, &["add", "README.md"]);
        git(&repository.root, &["commit", "-m", "replacement change"]);
        let rewritten = collect_repository(&repository.root, &snapshot)
            .await
            .unwrap();
        assert_eq!(rewritten.state, CollectedRepositoryState::Failed);
        assert_eq!(
            rewritten.failure_code.as_deref(),
            Some("recorded_history_rewritten")
        );
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_output(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}

#[cfg(test)]
mod integration_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use centaur_iron_control::{IronControlError, Principal};
    use centaur_sandbox_core::{
        CollectedWorkspaceRepository, ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle,
        SandboxId, SandboxIo, SandboxResult, SandboxSpec, SandboxStatus, WorkspaceCollection,
        WorkspaceCollectionRequest, WorkspaceCollectionState, WorkspaceManager,
        WorkspacePreparation, WorkspacePreparationRequest,
    };
    use centaur_session_core::{
        HarnessType, MessageRole, SessionMessageInput,
        development::{
            AcceptDevelopmentTask, ChangeSetState, CompleteWorkspacePreparation,
            ConfirmRepositorySelection, DevelopmentChannel, DevelopmentInitiator,
            PreparedRepositorySnapshot, ResolvedRepository,
        },
    };
    use centaur_session_sqlx::PgSessionStore;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::collect_changeset;
    use crate::{SandboxRuntime, SessionPrincipalRegistrar, SessionRuntime};

    struct CollectingManager;

    #[async_trait]
    impl WorkspaceManager for CollectingManager {
        async fn prepare(
            &self,
            _request: WorkspacePreparationRequest,
        ) -> Result<WorkspacePreparation, centaur_sandbox_core::WorkspaceError> {
            unreachable!("workspace is prepared directly in this test")
        }

        async fn collect(
            &self,
            request: WorkspaceCollectionRequest,
        ) -> Result<WorkspaceCollection, centaur_sandbox_core::WorkspaceError> {
            let repository = request.repositories.first().unwrap();
            let patch = b"diff --git a/README.md b/README.md\n";
            Ok(WorkspaceCollection {
                workspace_id: request.workspace_id,
                execution_id: request.execution_id,
                repositories: vec![CollectedWorkspaceRepository {
                    repository_id: repository.repository_id.clone(),
                    state: WorkspaceCollectionState::Changed,
                    base_sha: repository.base_sha.clone(),
                    recorded_head_sha: repository.recorded_head_sha.clone(),
                    head_sha: Some("b".repeat(40)),
                    commit_metadata: vec![json!({"sha": "b".repeat(40)})],
                    changed_file_count: 1,
                    additions: 1,
                    deletions: 0,
                    patch_hash: Some(format!("sha256:{}", hex::encode(Sha256::digest(patch)))),
                    patch_base64: Some(STANDARD.encode(patch)),
                    failure_code: None,
                    failure_message: None,
                }],
            })
        }
    }

    struct UnusedBackend;

    #[async_trait]
    impl SandboxBackend for UnusedBackend {
        fn name(&self) -> &'static str {
            "changeset-test"
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
            id: "prn_changeset_test".to_owned(),
            foreign_id: None,
            name: "ChangeSet Test".to_owned(),
            labels: Default::default(),
            sandbox_observability_enabled: true,
            sandbox_api_server_enabled: true,
        }
    }

    #[tokio::test]
    async fn changeset_worker_persists_artifact_and_ready_event() {
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
                    parts: vec![json!({"type": "text", "text": "Fix it"})],
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
        let workspace_claim = store
            .claim_workspace_preparation(
                &accepted.workspace_id,
                "workspace-owner",
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap();
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: accepted.workspace_id,
                attempt: workspace_claim.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: "workspace-test".to_owned(),
                prepared: vec![PreparedRepositorySnapshot {
                    repository_id: "gitlab:42".parse().unwrap(),
                    base_sha: "a".repeat(40),
                    local_branch: "centaur/test".to_owned(),
                    head_sha: "a".repeat(40),
                }],
                failed: Vec::new(),
            })
            .await
            .unwrap();
        store
            .complete_execution(&accepted.execution_id)
            .await
            .unwrap();
        let runtime = SessionRuntime::new(
            store.clone(),
            SandboxRuntime::backend(Arc::new(UnusedBackend), SandboxSpec::new("unused")),
            Registrar,
        )
        .with_workspace_manager(Arc::new(CollectingManager), "unused-secret");
        let collector_owner = runtime
            .context()
            .workspace
            .as_ref()
            .unwrap()
            .lease_owner
            .clone();
        let changeset = store
            .begin_changeset_collection(&accepted.execution_id, &collector_owner)
            .await
            .unwrap()
            .unwrap();

        collect_changeset(&runtime.context(), &changeset.changeset_id)
            .await
            .unwrap();

        let review = store
            .get_changeset(&changeset.changeset_id, "principal-1", false)
            .await
            .unwrap();
        assert_eq!(review.state, ChangeSetState::Ready);
        assert_eq!(review.repositories.len(), 1);
        let events = store
            .list_events_after(&accepted.thread_key, 0, Some(&accepted.execution_id), 100)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "development.changeset_ready")
        );
    }
}
