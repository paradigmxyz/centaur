use std::{collections::BTreeMap, time::Duration};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use centaur_sandbox_core::{
    PreparedWorkspaceRepository, WorkspaceCollection, WorkspaceCollectionRequest, WorkspaceError,
    WorkspaceManager, WorkspacePreparation, WorkspacePreparationRequest,
};
use k8s_openapi::api::{batch::v1::Job, core::v1::PersistentVolumeClaim};
use kube::{
    Api, Client, Error,
    api::{AttachParams, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams},
};
use serde::Serialize;
use serde_json::json;
use tokio::{
    io::AsyncReadExt,
    time::{Instant, sleep},
};

const MANAGED_BY_LABEL: &str = "centaur.ai/managed-by";
const WORKSPACE_ID_LABEL: &str = "centaur.ai/workspace-id";
const WORKSPACE_ATTEMPT_LABEL: &str = "centaur.ai/workspace-attempt";
const MANAGED_BY_VALUE: &str = "api-rs-workspace";
const WORKSPACE_VOLUME: &str = "workspace";
const TOKEN_VOLUME: &str = "gitlab-token";
const TOKEN_MOUNT_PATH: &str = "/var/run/secrets/centaur-gitlab";
const TOKEN_FILE_PATH: &str = "/var/run/secrets/centaur-gitlab/token";
const RESULT_PREFIX: &str = "CENTAUR_WORKSPACE_RESULT=";
const COLLECTION_READY_MARKER: &str = "CENTAUR_CHANGESET_READY";
const MAX_COLLECTION_RESULT_BYTES: usize = 3 * 1024 * 1024;

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

    async fn collect(
        &self,
        request: WorkspaceCollectionRequest,
    ) -> Result<WorkspaceCollection, WorkspaceError> {
        request.validate()?;
        let job_name = collection_job_name(&request.execution_id);
        match self.jobs().get(&job_name).await {
            Ok(_) => {}
            Err(error) if is_not_found(&error) => {
                let job = build_collection_job(&request, &self.config)?;
                match self.jobs().create(&PostParams::default(), &job).await {
                    Ok(_) => {}
                    Err(Error::Api(error)) if error.code == 409 => {}
                    Err(_) => {
                        return Err(WorkspaceError::Backend(
                            "workspace collector could not start".to_owned(),
                        ));
                    }
                }
            }
            Err(_) => {
                return Err(WorkspaceError::Backend(
                    "workspace collector lookup failed".to_owned(),
                ));
            }
        }
        let deadline = Instant::now() + self.config.ready_timeout;
        loop {
            let status = self
                .jobs()
                .get(&job_name)
                .await
                .map_err(|_| WorkspaceError::Backend("workspace collector disappeared".to_owned()))?
                .status
                .unwrap_or_default();
            if status.failed.unwrap_or_default() > 0 {
                let _ = self
                    .jobs()
                    .delete(&job_name, &DeleteParams::default())
                    .await;
                return Err(WorkspaceError::Backend(
                    "workspace collector failed".to_owned(),
                ));
            }
            if let Some(result) = self.collection_result_if_ready(&request, &job_name).await? {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Err(WorkspaceError::Backend(
                    "workspace collector timed out".to_owned(),
                ));
            }
            sleep(Duration::from_millis(500)).await;
        }
    }
}

impl KubeWorkspaceManager {
    async fn collection_result_if_ready(
        &self,
        request: &WorkspaceCollectionRequest,
        job_name: &str,
    ) -> Result<Option<WorkspaceCollection>, WorkspaceError> {
        let pods: Api<k8s_openapi::api::core::v1::Pod> =
            Api::namespaced(self.client.clone(), &self.config.namespace);
        let listed = pods
            .list(&ListParams::default().labels(&format!("job-name={job_name}")))
            .await
            .map_err(|_| WorkspaceError::Backend("collection result is unavailable".to_owned()))?;
        let Some(pod_name) = listed.items.into_iter().find_map(|pod| pod.metadata.name) else {
            return Ok(None);
        };
        let logs = pods
            .logs(
                &pod_name,
                &LogParams {
                    container: Some("collector".to_owned()),
                    tail_lines: Some(5),
                    ..LogParams::default()
                },
            )
            .await
            .map_err(|_| WorkspaceError::Backend("collection result is unavailable".to_owned()))?;
        if !logs.lines().any(|line| line == COLLECTION_READY_MARKER) {
            return Ok(None);
        }
        let params = AttachParams::default()
            .container("collector".to_owned())
            .stdout(true)
            .stderr(false)
            .stdin(false)
            .tty(false);
        let mut attached = pods
            .exec(
                &pod_name,
                [
                    "python3",
                    "-c",
                    "import sys;sys.stdout.buffer.write(open('/result/result.json','rb').read())",
                ],
                &params,
            )
            .await
            .map_err(|_| WorkspaceError::Backend("collection result is unavailable".to_owned()))?;
        let mut stdout = attached
            .stdout()
            .ok_or_else(|| WorkspaceError::Backend("collection result is unavailable".to_owned()))?
            .take((MAX_COLLECTION_RESULT_BYTES + 1) as u64);
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| WorkspaceError::Backend("collection result is unavailable".to_owned()))?;
        if bytes.len() > MAX_COLLECTION_RESULT_BYTES {
            return Err(WorkspaceError::Backend(
                "collection result exceeds the size limit".to_owned(),
            ));
        }
        let result = serde_json::from_slice::<WorkspaceCollection>(&bytes)
            .map_err(|_| WorkspaceError::Backend("collection result is invalid".to_owned()))?;
        if result.workspace_id != request.workspace_id
            || result.execution_id != request.execution_id
        {
            return Err(WorkspaceError::Backend(
                "collection result does not match the request".to_owned(),
            ));
        }
        Ok(Some(result))
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

fn build_collection_job(
    request: &WorkspaceCollectionRequest,
    config: &KubeWorkspaceConfig,
) -> Result<Job, WorkspaceError> {
    let name = collection_job_name(&request.execution_id);
    let encoded = STANDARD.encode(
        serde_json::to_vec(request)
            .map_err(|error| WorkspaceError::Invalid(format!("collection input: {error}")))?,
    );
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "namespace": config.namespace,
            "labels": collection_labels(request),
        },
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": 3660,
            "ttlSecondsAfterFinished": 3600,
            "template": {
                "metadata": {"labels": collection_labels(request)},
                "spec": {
                    "restartPolicy": "Never",
                    "serviceAccountName": config.service_account_name,
                    "automountServiceAccountToken": false,
                    "securityContext": {"fsGroup": 1000},
                    "containers": [{
                        "name": "collector",
                        "image": config.provisioner_image,
                        "imagePullPolicy": config.image_pull_policy,
                        "command": ["python3", "-c", COLLECTION_SCRIPT],
                        "env": [
                            {"name": "CENTAUR_CHANGESET_REQUEST_B64", "value": encoded},
                            {"name": "GIT_TERMINAL_PROMPT", "value": "0"},
                            {"name": "GIT_OPTIONAL_LOCKS", "value": "0"},
                            {"name": "GIT_CONFIG_NOSYSTEM", "value": "1"},
                            {"name": "GIT_CONFIG_GLOBAL", "value": "/dev/null"}
                        ],
                    "volumeMounts": [{
                        "name": WORKSPACE_VOLUME,
                        "mountPath": "/workspace",
                        "readOnly": true
                    }, {
                        "name": "result",
                        "mountPath": "/result"
                    }],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "readOnlyRootFilesystem": true,
                            "runAsNonRoot": true,
                            "runAsUser": 1000,
                            "capabilities": {"drop": ["ALL"]}
                        },
                        "resources": {
                            "requests": {"cpu": "25m", "memory": "64Mi"},
                            "limits": {"cpu": "1", "memory": "256Mi"}
                        }
                    }],
                    "volumes": [{
                        "name": WORKSPACE_VOLUME,
                        "persistentVolumeClaim": {"claimName": request.storage_ref}
                    }, {
                        "name": "result",
                        "emptyDir": {"sizeLimit": "4Mi"}
                    }]
                }
            }
        }
    }))
    .map_err(|error| WorkspaceError::Invalid(format!("collection Job: {error}")))
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

fn collection_labels(request: &WorkspaceCollectionRequest) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
        (
            WORKSPACE_ID_LABEL.to_owned(),
            label_value(&request.workspace_id),
        ),
        (
            "centaur.ai/execution-id".to_owned(),
            label_value(&request.execution_id),
        ),
    ])
}

fn workspace_pvc_name(workspace_id: &str) -> String {
    resource_name("workspace", workspace_id, None)
}

fn workspace_job_name(workspace_id: &str, attempt: u32) -> String {
    resource_name("workspace", workspace_id, Some(attempt))
}

fn collection_job_name(execution_id: &str) -> String {
    resource_name("changeset", execution_id, None)
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

const COLLECTION_SCRIPT: &str = r##"
import base64, hashlib, json, os, pathlib, subprocess, time

request = json.loads(base64.b64decode(os.environ["CENTAUR_CHANGESET_REQUEST_B64"]))
workspace = pathlib.Path("/workspace").resolve()
results = []
total_patch_bytes = 0

def git(repo, *args, check=True):
    completed = subprocess.run(
        ["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false", "-c", "diff.external=", *args],
        cwd=repo,
        env=dict(os.environ),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=30,
        check=False,
    )
    if check and completed.returncode != 0:
        raise RuntimeError("git command failed")
    if len(completed.stdout) > 2 * 1024 * 1024:
        raise RuntimeError("git output too large")
    return completed

def failed(repository, state, code, message, head=None):
    return {
        "repository_id": repository["repository_id"],
        "state": state,
        "base_sha": repository["base_sha"],
        "recorded_head_sha": repository["recorded_head_sha"],
        "head_sha": head,
        "commit_metadata": [],
        "changed_file_count": 0,
        "additions": 0,
        "deletions": 0,
        "patch_hash": None,
        "patch_base64": None,
        "failure_code": code,
        "failure_message": message,
    }

for repository in request["repositories"]:
    try:
        repo = (workspace / repository["relative_path"]).resolve()
        if workspace not in repo.parents or not (repo / ".git").is_dir():
            results.append(failed(repository, "failed", "repository_missing", "repository is unavailable"))
            continue
        base = repository["base_sha"]
        recorded = repository["recorded_head_sha"]
        if git(repo, "cat-file", "-e", base + "^{commit}", check=False).returncode != 0:
            results.append(failed(repository, "failed", "base_object_missing", "recorded base commit is unavailable"))
            continue
        if git(repo, "cat-file", "-e", recorded + "^{commit}", check=False).returncode != 0:
            results.append(failed(repository, "failed", "recorded_head_object_missing", "recorded repository head is unavailable"))
            continue
        head = git(repo, "rev-parse", "--verify", "HEAD^{commit}").stdout.decode().strip()
        branch = git(repo, "symbolic-ref", "--quiet", "--short", "HEAD", check=False)
        if branch.returncode != 0 or branch.stdout.decode().strip() != repository["local_branch"]:
            results.append(failed(repository, "needs_agent_completion", "branch_mismatch", "repository is not on its recorded branch", head))
            continue
        if git(repo, "merge-base", "--is-ancestor", base, head, check=False).returncode != 0:
            results.append(failed(repository, "failed", "head_not_descendant", "repository head does not descend from the recorded base", head))
            continue
        if git(repo, "merge-base", "--is-ancestor", recorded, head, check=False).returncode != 0:
            results.append(failed(repository, "failed", "recorded_history_rewritten", "repository head rewrites recorded history", head))
            continue
        if git(repo, "status", "--porcelain=v1", "-z").stdout:
            results.append(failed(repository, "needs_agent_completion", "working_tree_dirty", "repository has uncommitted changes", head))
            continue
        if head == recorded:
            result = failed(repository, "unchanged", None, None, head)
            result["failure_code"] = None
            result["failure_message"] = None
            results.append(result)
            continue
        patch = git(repo, "diff", "--binary", "--no-ext-diff", "--no-textconv", base, head, "--").stdout
        total_patch_bytes += len(patch)
        if not patch or total_patch_bytes > 2 * 1024 * 1024:
            results.append(failed(repository, "failed", "patch_too_large", "review patch exceeds the size limit", head))
            continue
        names = [item for item in git(repo, "diff", "--name-only", "-z", base, head, "--").stdout.split(b"\0") if item]
        additions = deletions = 0
        for line in git(repo, "diff", "--numstat", base, head, "--").stdout.decode(errors="replace").splitlines():
            fields = line.split("\t", 2)
            if len(fields) >= 2:
                additions += int(fields[0]) if fields[0].isdigit() else 0
                deletions += int(fields[1]) if fields[1].isdigit() else 0
        log = git(repo, "log", "--reverse", "--format=%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e", base + ".." + head, "--").stdout.decode(errors="replace")
        commits = []
        for record in log.split("\x1e"):
            fields = record.strip().split("\x1f", 4)
            if len(fields) == 5:
                commits.append({"sha": fields[0], "author_name": fields[1], "author_email": fields[2], "authored_at": fields[3], "subject": fields[4]})
        results.append({
            "repository_id": repository["repository_id"],
            "state": "changed",
            "base_sha": base,
            "recorded_head_sha": recorded,
            "head_sha": head,
            "commit_metadata": commits,
            "changed_file_count": len(names),
            "additions": additions,
            "deletions": deletions,
            "patch_hash": "sha256:" + hashlib.sha256(patch).hexdigest(),
            "patch_base64": base64.b64encode(patch).decode(),
            "failure_code": None,
            "failure_message": None,
        })
    except Exception:
        results.append(failed(repository, "failed", "collection_failed", "repository collection failed"))

result = {"workspace_id": request["workspace_id"], "execution_id": request["execution_id"], "repositories": results}
result_path = pathlib.Path("/result/result.json")
result_path.write_text(json.dumps(result, separators=(",", ":")))
print("CENTAUR_CHANGESET_READY", flush=True)
time.sleep(3600)
"##;

#[cfg(test)]
mod tests {
    use centaur_sandbox_core::{
        PreparedWorkspaceRepository, WorkspaceCollectionRepository, WorkspaceCollectionRequest,
        WorkspaceRepository,
    };

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

    #[test]
    fn changeset_collector_is_read_only_and_has_no_credentials() {
        let config = KubeWorkspaceConfig::new("centaur", "centaur-api:latest");
        let request = WorkspaceCollectionRequest {
            workspace_id: "wsp_abc_123".to_owned(),
            execution_id: "exe_abc_123".to_owned(),
            storage_ref: "workspace-wsp-abc-123".to_owned(),
            repositories: vec![WorkspaceCollectionRepository {
                repository_id: "gitlab:42".to_owned(),
                path_with_namespace: "platform/project".to_owned(),
                relative_path: "repos/42-project".to_owned(),
                base_sha: "a".repeat(40),
                recorded_head_sha: "b".repeat(40),
                local_branch: "centaur/test".to_owned(),
            }],
        };
        let job = build_collection_job(&request, &config).unwrap();
        let value = serde_json::to_value(job).unwrap();
        assert_eq!(value["metadata"]["name"], "changeset-exe-abc-123");
        let pod = &value["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["containers"][0]["volumeMounts"][0]["readOnly"], true);
        assert_eq!(pod["volumes"].as_array().unwrap().len(), 2);
        assert_eq!(pod["volumes"][1]["emptyDir"]["sizeLimit"], "4Mi");
        let rendered = serde_json::to_string(&value).unwrap();
        assert!(!rendered.contains("secretName"));
        assert!(!rendered.contains("GIT_ASKPASS"));
        for command in [
            "\"add\"",
            "\"commit\"",
            "\"reset\"",
            "\"checkout\"",
            "\"push\"",
        ] {
            assert!(
                !COLLECTION_SCRIPT.contains(command),
                "found mutating git command {command}"
            );
        }
        assert!(rendered.contains("GIT_OPTIONAL_LOCKS"));
        assert!(COLLECTION_SCRIPT.contains("core.fsmonitor=false"));
        assert!(COLLECTION_SCRIPT.contains("core.untrackedCache=false"));
        assert!(COLLECTION_SCRIPT.contains("--no-ext-diff"));
    }
}
