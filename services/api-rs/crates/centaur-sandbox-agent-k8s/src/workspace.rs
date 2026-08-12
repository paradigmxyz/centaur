use std::{collections::BTreeMap, time::Duration};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use centaur_sandbox_core::{
    PreparedWorkspaceRepository, WorkspaceError, WorkspaceManager, WorkspacePreparation,
    WorkspacePreparationRequest,
};
use k8s_openapi::api::{batch::v1::Job, core::v1::PersistentVolumeClaim};
use kube::{
    Api, Client, Error,
    api::{ListParams, LogParams, Patch, PatchParams, PostParams},
};
use serde::Serialize;
use serde_json::json;
use tokio::time::{Instant, sleep};

const MANAGED_BY_LABEL: &str = "centaur.ai/managed-by";
const WORKSPACE_ID_LABEL: &str = "centaur.ai/workspace-id";
const WORKSPACE_ATTEMPT_LABEL: &str = "centaur.ai/workspace-attempt";
const MANAGED_BY_VALUE: &str = "api-rs-workspace";
const WORKSPACE_VOLUME: &str = "workspace";
const TOKEN_VOLUME: &str = "gitlab-token";
const TOKEN_MOUNT_PATH: &str = "/var/run/secrets/centaur-gitlab";
const TOKEN_FILE_PATH: &str = "/var/run/secrets/centaur-gitlab/token";
const RESULT_PREFIX: &str = "CENTAUR_WORKSPACE_RESULT=";

#[derive(Clone, Debug)]
pub struct KubeWorkspaceConfig {
    pub namespace: String,
    pub field_manager: String,
    pub provisioner_image: String,
    pub service_account_name: Option<String>,
    pub token_secret_key: String,
    pub storage_size: String,
    pub storage_class_name: Option<String>,
    pub image_pull_policy: Option<String>,
    pub ready_timeout: Duration,
}

impl KubeWorkspaceConfig {
    pub fn new(namespace: impl Into<String>, provisioner_image: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            field_manager: "centaur-api-rs-workspace".to_owned(),
            provisioner_image: provisioner_image.into(),
            service_account_name: None,
            token_secret_key: "token".to_owned(),
            storage_size: "20Gi".to_owned(),
            storage_class_name: None,
            image_pull_policy: None,
            ready_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone)]
pub struct KubeWorkspaceManager {
    client: Client,
    config: KubeWorkspaceConfig,
}

impl KubeWorkspaceManager {
    pub fn new(client: Client, config: KubeWorkspaceConfig) -> Self {
        Self { client, config }
    }

    fn pvcs(&self) -> Api<PersistentVolumeClaim> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    fn jobs(&self) -> Api<Job> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    async fn ensure_pvc(
        &self,
        request: &WorkspacePreparationRequest,
    ) -> Result<String, WorkspaceError> {
        let name = workspace_pvc_name(&request.workspace_id);
        let pvc = build_workspace_pvc(request, &self.config)?;
        let params = PatchParams::apply(&self.config.field_manager).force();
        self.pvcs()
            .patch(&name, &params, &Patch::Apply(&pvc))
            .await
            .map_err(|_| WorkspaceError::Backend("workspace storage is unavailable".to_owned()))?;
        Ok(name)
    }

    async fn ensure_job(
        &self,
        request: &WorkspacePreparationRequest,
        storage_ref: &str,
    ) -> Result<String, WorkspaceError> {
        let name = workspace_job_name(&request.workspace_id, request.attempt);
        match self.jobs().get(&name).await {
            Ok(_) => return Ok(name),
            Err(error) if is_not_found(&error) => {}
            Err(_) => {
                return Err(WorkspaceError::Backend(
                    "workspace provisioner lookup failed".to_owned(),
                ));
            }
        }
        let job = build_workspace_job(request, storage_ref, &self.config)?;
        match self.jobs().create(&PostParams::default(), &job).await {
            Ok(_) => Ok(name),
            Err(Error::Api(error)) if error.code == 409 => Ok(name),
            Err(_) => Err(WorkspaceError::Backend(
                "workspace provisioner could not start".to_owned(),
            )),
        }
    }

    async fn wait_for_result(
        &self,
        request: &WorkspacePreparationRequest,
        job_name: &str,
    ) -> Result<WorkspacePreparation, WorkspaceError> {
        let deadline = Instant::now() + self.config.ready_timeout;
        loop {
            let job = self.jobs().get(job_name).await.map_err(|_| {
                WorkspaceError::Backend("workspace provisioner disappeared".to_owned())
            })?;
            let status = job.status.unwrap_or_default();
            if status.succeeded.unwrap_or_default() > 0 {
                return self.read_job_result(request, job_name).await;
            }
            if status.failed.unwrap_or_default() > 0 {
                let failure = failed_preparation(request);
                if failure.failed.is_empty() {
                    return Err(WorkspaceError::Backend(
                        "workspace provisioner failed".to_owned(),
                    ));
                }
                return Ok(failure);
            }
            if Instant::now() >= deadline {
                return Err(WorkspaceError::Backend(
                    "workspace provisioner timed out".to_owned(),
                ));
            }
            sleep(Duration::from_millis(500)).await;
        }
    }

    async fn read_job_result(
        &self,
        request: &WorkspacePreparationRequest,
        job_name: &str,
    ) -> Result<WorkspacePreparation, WorkspaceError> {
        let pods: Api<k8s_openapi::api::core::v1::Pod> =
            Api::namespaced(self.client.clone(), &self.config.namespace);
        let listed = pods
            .list(&ListParams::default().labels(&format!("job-name={job_name}")))
            .await
            .map_err(|_| WorkspaceError::Backend("workspace result is unavailable".to_owned()))?;
        let pod_name = listed
            .items
            .into_iter()
            .find_map(|pod| pod.metadata.name)
            .ok_or_else(|| WorkspaceError::Backend("workspace result pod is missing".to_owned()))?;
        let logs = pods
            .logs(
                &pod_name,
                &LogParams {
                    container: Some("provisioner".to_owned()),
                    tail_lines: Some(10),
                    ..LogParams::default()
                },
            )
            .await
            .map_err(|_| WorkspaceError::Backend("workspace result is unavailable".to_owned()))?;
        let encoded = logs
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(RESULT_PREFIX))
            .ok_or_else(|| WorkspaceError::Backend("workspace result is invalid".to_owned()))?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| WorkspaceError::Backend("workspace result is invalid".to_owned()))?;
        let result = serde_json::from_slice::<WorkspacePreparation>(&bytes)
            .map_err(|_| WorkspaceError::Backend("workspace result is invalid".to_owned()))?;
        if result.workspace_id != request.workspace_id {
            return Err(WorkspaceError::Backend(
                "workspace result does not match the request".to_owned(),
            ));
        }
        Ok(result)
    }
}

#[async_trait]
impl WorkspaceManager for KubeWorkspaceManager {
    async fn prepare(
        &self,
        request: WorkspacePreparationRequest,
    ) -> Result<WorkspacePreparation, WorkspaceError> {
        request.validate()?;
        let storage_ref = self.ensure_pvc(&request).await?;
        let job_name = self.ensure_job(&request, &storage_ref).await?;
        self.wait_for_result(&request, &job_name).await
    }
}

#[derive(Serialize)]
struct ProvisionerInput<'a> {
    workspace_id: &'a str,
    thread_key: &'a str,
    storage_ref: &'a str,
    manifest_json: String,
    repositories: Vec<ProvisionerRepository<'a>>,
}

#[derive(Serialize)]
struct ProvisionerRepository<'a> {
    repository_id: &'a str,
    clone_url: &'a str,
    default_branch: &'a str,
    relative_path: &'a str,
    existing: Option<&'a PreparedWorkspaceRepository>,
}

fn build_workspace_pvc(
    request: &WorkspacePreparationRequest,
    config: &KubeWorkspaceConfig,
) -> Result<PersistentVolumeClaim, WorkspaceError> {
    let name = workspace_pvc_name(&request.workspace_id);
    let mut spec = json!({
        "accessModes": ["ReadWriteOnce"],
        "resources": {"requests": {"storage": config.storage_size}},
    });
    if let Some(storage_class_name) = &config.storage_class_name {
        spec["storageClassName"] = json!(storage_class_name);
    }
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": name,
            "namespace": config.namespace,
            "labels": workspace_labels(request),
        },
        "spec": spec,
    }))
    .map_err(|error| WorkspaceError::Invalid(format!("workspace PVC: {error}")))
}

fn build_workspace_job(
    request: &WorkspacePreparationRequest,
    storage_ref: &str,
    config: &KubeWorkspaceConfig,
) -> Result<Job, WorkspaceError> {
    let name = workspace_job_name(&request.workspace_id, request.attempt);
    let input = ProvisionerInput {
        workspace_id: &request.workspace_id,
        thread_key: &request.thread_key,
        storage_ref,
        manifest_json: request.manifest_json()?,
        repositories: request
            .repositories
            .iter()
            .map(|repository| ProvisionerRepository {
                repository_id: &repository.repository_id,
                clone_url: &repository.clone_url,
                default_branch: &repository.default_branch,
                relative_path: &repository.relative_path,
                existing: repository.existing.as_ref(),
            })
            .collect(),
    };
    let encoded = STANDARD.encode(
        serde_json::to_vec(&input)
            .map_err(|error| WorkspaceError::Invalid(format!("workspace job input: {error}")))?,
    );
    let mut pod_spec = json!({
        "restartPolicy": "Never",
        "securityContext": {"fsGroup": 1000},
        "containers": [{
            "name": "provisioner",
            "image": config.provisioner_image,
            "command": ["python3", "-c", PROVISIONER_SCRIPT],
            "env": [
                {"name": "CENTAUR_WORKSPACE_REQUEST_B64", "value": encoded},
                {"name": "GIT_TERMINAL_PROMPT", "value": "0"}
            ],
            "volumeMounts": [
                {"name": WORKSPACE_VOLUME, "mountPath": "/workspace"}
            ],
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "runAsNonRoot": true,
                "runAsUser": 1000,
                "capabilities": {"drop": ["ALL"]}
            }
        }],
        "volumes": [
            {"name": WORKSPACE_VOLUME, "persistentVolumeClaim": {"claimName": storage_ref}}
        ]
    });
    if !request.repositories.is_empty() {
        pod_spec["containers"][0]["env"]
            .as_array_mut()
            .expect("workspace provisioner env is an array")
            .extend([
                json!({"name": "GIT_ASKPASS", "value": "/tmp/centaur-git-askpass"}),
                json!({"name": "CENTAUR_GIT_TOKEN_FILE", "value": TOKEN_FILE_PATH}),
            ]);
        pod_spec["containers"][0]["volumeMounts"]
            .as_array_mut()
            .expect("workspace provisioner mounts are an array")
            .push(json!({
                "name": TOKEN_VOLUME,
                "mountPath": TOKEN_MOUNT_PATH,
                "readOnly": true
            }));
        pod_spec["volumes"]
            .as_array_mut()
            .expect("workspace provisioner volumes are an array")
            .push(json!({
                "name": TOKEN_VOLUME,
                "secret": {
                    "secretName": request.credential_ref,
                    "items": [{"key": config.token_secret_key, "path": "token"}]
                }
            }));
    }
    if let Some(service_account_name) = &config.service_account_name {
        pod_spec["serviceAccountName"] = json!(service_account_name);
    }
    if let Some(image_pull_policy) = &config.image_pull_policy {
        pod_spec["containers"][0]["imagePullPolicy"] = json!(image_pull_policy);
    }
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "namespace": config.namespace,
            "labels": workspace_labels(request),
        },
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": config.ready_timeout.as_secs().max(1),
            "ttlSecondsAfterFinished": 3600,
            "template": {
                "metadata": {"labels": workspace_labels(request)},
                "spec": pod_spec,
            }
        }
    }))
    .map_err(|error| WorkspaceError::Invalid(format!("workspace Job: {error}")))
}

fn workspace_labels(request: &WorkspacePreparationRequest) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
        (
            WORKSPACE_ID_LABEL.to_owned(),
            label_value(&request.workspace_id),
        ),
        (
            WORKSPACE_ATTEMPT_LABEL.to_owned(),
            request.attempt.to_string(),
        ),
    ])
}

fn workspace_pvc_name(workspace_id: &str) -> String {
    resource_name("workspace", workspace_id, None)
}

fn workspace_job_name(workspace_id: &str, attempt: u32) -> String {
    resource_name("workspace", workspace_id, Some(attempt))
}

fn resource_name(prefix: &str, workspace_id: &str, attempt: Option<u32>) -> String {
    let suffix = attempt.map_or_else(String::new, |attempt| format!("-a{attempt}"));
    let max_id_len = 63usize.saturating_sub(prefix.len() + suffix.len() + 1);
    let id = dns_component(workspace_id, max_id_len);
    format!("{prefix}-{id}{suffix}")
}

fn dns_component(value: &str, max_len: usize) -> String {
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
        if output.len() >= max_len {
            break;
        }
    }
    let output = output.trim_matches('-');
    if output.is_empty() {
        "unknown".to_owned()
    } else {
        output.to_owned()
    }
}

fn label_value(value: &str) -> String {
    dns_component(value, 63)
}

fn is_not_found(error: &Error) -> bool {
    matches!(error, Error::Api(response) if response.code == 404)
}

fn failed_preparation(request: &WorkspacePreparationRequest) -> WorkspacePreparation {
    WorkspacePreparation {
        workspace_id: request.workspace_id.clone(),
        storage_ref: workspace_pvc_name(&request.workspace_id),
        prepared: request
            .repositories
            .iter()
            .filter_map(|repository| repository.existing.clone())
            .collect(),
        failed: request
            .repositories
            .iter()
            .filter(|repository| repository.existing.is_none())
            .map(
                |repository| centaur_sandbox_core::FailedWorkspaceRepository {
                    repository_id: repository.repository_id.clone(),
                    failure_code: "workspace_provisioner_failed".to_owned(),
                    failure_message: "workspace provisioner failed".to_owned(),
                },
            )
            .collect(),
    }
}

const PROVISIONER_SCRIPT: &str = r##"
import base64, json, os, pathlib, shutil, subprocess

request = json.loads(base64.b64decode(os.environ["CENTAUR_WORKSPACE_REQUEST_B64"]))
workspace = pathlib.Path("/workspace")
repositories_root = workspace / "repos"
if repositories_root.is_symlink():
    raise RuntimeError("workspace repositories root is a symlink")
repositories_root.mkdir(parents=True, exist_ok=True)
askpass = pathlib.Path("/tmp/centaur-git-askpass")
askpass.write_text("#!/bin/sh\ncase \"$1\" in *Username*) printf '%s\\n' oauth2 ;; *) cat \"$CENTAUR_GIT_TOKEN_FILE\" ;; esac\n")
askpass.chmod(0o700)
prepared, failed = [], []

def git(repo, *args):
    env = dict(os.environ)
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    return subprocess.run(
        ["git", "-c", "credential.helper=", "-c", "core.hooksPath=/dev/null", "-c", "http.followRedirects=false", *args],
        cwd=repo,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=180,
        check=True,
    ).stdout.strip()

for repository in request["repositories"]:
    repository_id = repository["repository_id"]
    try:
        target = workspace / repository["relative_path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        existing = repository.get("existing")
        if existing:
            if not (target / ".git").is_dir():
                raise RuntimeError("existing repository is missing")
            prepared.append(existing)
            continue
        if target.is_symlink():
            target.unlink()
        elif target.exists():
            shutil.rmtree(target)
        subprocess.run(
            ["git", "-c", "credential.helper=", "-c", "core.hooksPath=/dev/null", "-c", "http.followRedirects=false", "clone", "--single-branch", "--branch", repository["default_branch"], repository["clone_url"], str(target)],
            env=dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null"),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=300,
            check=True,
        )
        branch = "centaur/" + request["workspace_id"].replace("_", "-")[:32]
        branch_exists = subprocess.run(["git", "show-ref", "--verify", "--quiet", "refs/heads/" + branch], cwd=target).returncode == 0
        if branch_exists:
            git(target, "checkout", branch)
            base_sha = git(target, "rev-parse", "HEAD")
        else:
            git(target, "fetch", "--prune", "origin", repository["default_branch"])
            base_sha = git(target, "rev-parse", "FETCH_HEAD")
            git(target, "checkout", "-b", branch, base_sha)
        subprocess.run(["git", "config", "--unset-all", "credential.helper"], cwd=target, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        prepared.append({"repository_id": repository_id, "base_sha": base_sha, "local_branch": branch, "head_sha": base_sha})
    except Exception:
        failed.append({"repository_id": repository_id, "failure_code": "repository_preparation_failed", "failure_message": "repository preparation failed"})

manifest = json.loads(request["manifest_json"])
prepared_by_id = {item["repository_id"]: item for item in prepared}
for repository in manifest["repositories"]:
    state = prepared_by_id.get(repository["repository_id"])
    if state:
        repository.update({key: state[key] for key in ("base_sha", "local_branch", "head_sha")})
manifest_path = workspace / "workspace.json"
if manifest_path.is_symlink():
    manifest_path.unlink()
manifest_fd = os.open(manifest_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
with os.fdopen(manifest_fd, "w") as manifest_file:
    manifest_file.write(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
result = {"workspace_id": request["workspace_id"], "storage_ref": request["storage_ref"], "prepared": prepared, "failed": failed}
encoded = base64.b64encode(json.dumps(result, separators=(",", ":")).encode()).decode()
print("CENTAUR_WORKSPACE_RESULT=" + encoded)
"##;

#[cfg(test)]
mod tests {
    use centaur_sandbox_core::{PreparedWorkspaceRepository, WorkspaceRepository};

    use super::*;

    fn request() -> WorkspacePreparationRequest {
        WorkspacePreparationRequest::new(
            "wsp_abc_123",
            "development:123",
            2,
            "gitlab-token-secret",
            vec![
                WorkspaceRepository {
                    repository_id: "gitlab:42".to_owned(),
                    display_name: "Project".to_owned(),
                    path_with_namespace: "platform/project".to_owned(),
                    default_branch: "main".to_owned(),
                    clone_url: "http://git.example.test:82/platform/project.git".to_owned(),
                    relative_path: "repos/42-project".to_owned(),
                    existing: None,
                },
                WorkspaceRepository {
                    repository_id: "gitlab:84".to_owned(),
                    display_name: "Existing".to_owned(),
                    path_with_namespace: "platform/existing".to_owned(),
                    default_branch: "main".to_owned(),
                    clone_url: "http://git.example.test:82/platform/existing.git".to_owned(),
                    relative_path: "repos/84-existing".to_owned(),
                    existing: Some(PreparedWorkspaceRepository {
                        repository_id: "gitlab:84".to_owned(),
                        base_sha: "a".repeat(40),
                        local_branch: "centaur/existing".to_owned(),
                        head_sha: "b".repeat(40),
                    }),
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn workspace_objects_isolate_credentials_to_provisioner() {
        let config = KubeWorkspaceConfig::new("centaur", "centaur-api:latest");
        let request = request();
        let pvc = build_workspace_pvc(&request, &config).unwrap();
        assert_eq!(pvc.metadata.name.as_deref(), Some("workspace-wsp-abc-123"));
        assert_eq!(
            pvc.metadata.labels.as_ref().unwrap()[WORKSPACE_ID_LABEL],
            "wsp-abc-123"
        );

        let job = build_workspace_job(&request, "workspace-wsp-abc-123", &config).unwrap();
        let value = serde_json::to_value(job).unwrap();
        assert_eq!(value["metadata"]["name"], "workspace-wsp-abc-123-a2");
        let pod = &value["spec"]["template"]["spec"];
        assert_eq!(pod["restartPolicy"], "Never");
        assert_eq!(
            pod["volumes"][0]["persistentVolumeClaim"]["claimName"],
            "workspace-wsp-abc-123"
        );
        assert_eq!(
            pod["volumes"][1]["secret"]["secretName"],
            "gitlab-token-secret"
        );
        assert_eq!(pod["containers"][0]["env"][1]["value"], "0");
        let rendered = serde_json::to_string(&value).unwrap();
        assert!(rendered.contains("core.hooksPath=/dev/null"));
        assert!(rendered.contains("credential.helper="));
        assert!(rendered.contains("GIT_CONFIG_GLOBAL"));
        assert!(rendered.contains("shutil.rmtree"));
        assert!(rendered.contains("O_NOFOLLOW"));
        assert!(rendered.contains("GIT_ASKPASS"));
        assert!(!rendered.contains("oauth2:"));
        assert!(!rendered.contains("not-a-real-token"));
    }

    #[test]
    fn workspace_resource_names_are_deterministic_and_bounded() {
        let long_id = format!("wsp_{}", "a".repeat(200));
        assert_eq!(workspace_pvc_name("wsp_abc_123"), "workspace-wsp-abc-123");
        assert_eq!(
            workspace_job_name("wsp_abc_123", 2),
            "workspace-wsp-abc-123-a2"
        );
        assert!(workspace_job_name(&long_id, u32::MAX).len() <= 63);
    }

    #[test]
    fn workspace_without_repositories_does_not_mount_gitlab_credentials() {
        let config = KubeWorkspaceConfig::new("centaur", "centaur-api:latest");
        let request = WorkspacePreparationRequest::new(
            "wsp_empty_123",
            "development:empty",
            1,
            "gitlab-token-secret",
            Vec::new(),
        )
        .unwrap();

        let job = build_workspace_job(&request, "workspace-wsp-empty-123", &config).unwrap();
        let value = serde_json::to_value(job).unwrap();
        let pod = &value["spec"]["template"]["spec"];
        let env = pod["containers"][0]["env"].as_array().unwrap();
        let volumes = pod["volumes"].as_array().unwrap();

        assert_eq!(env.len(), 2);
        assert_eq!(volumes.len(), 1);
        assert!(
            env.iter()
                .all(|item| item["name"] != "CENTAUR_GIT_TOKEN_FILE")
        );
        assert!(volumes.iter().all(|item| item["name"] != TOKEN_VOLUME));
    }
}
