//! Agent Sandbox Kubernetes backend.
//!
//! The Agent Sandbox CRD types are generated from the upstream CRD with
//! `just codegen-agent-sandbox-crd`.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use centaur_sandbox_core::{
    ExecCommand, ExecResult, MountKind, ObservedSandbox, ReadOptions, ReadResult, SandboxBackend,
    SandboxError, SandboxHandle, SandboxId, SandboxResult, SandboxSpec, SandboxStatus, WriteAck,
};
use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{
    AttachParams, AttachedProcess, DeleteParams, ListParams, Patch, PatchParams, PostParams,
};
use kube::{Api, Client, Error};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep, timeout};

pub use generated::agents_x_k8s_io as crd;

pub mod generated;

const BACKEND_NAME: &str = "agent-sandbox-k8s";
const DEFAULT_CONTAINER_NAME: &str = "agent";
const MANAGED_BY_LABEL: &str = "centaur.ai/managed-by";
const SANDBOX_ID_LABEL: &str = "centaur.ai/sandbox-id";
const MANAGED_BY_VALUE: &str = "api-rs";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct AgentSandboxConfig {
    pub namespace: String,
    pub field_manager: String,
    pub container_name: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub image_pull_policy: Option<String>,
    pub state_volume: Option<StateVolumeConfig>,
    pub ready_timeout: Duration,
}

impl AgentSandboxConfig {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            field_manager: "centaur-api-rs".to_owned(),
            container_name: DEFAULT_CONTAINER_NAME.to_owned(),
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
            image_pull_policy: None,
            state_volume: None,
            ready_timeout: Duration::from_secs(60),
        }
    }

    pub fn state_volume(mut self, state_volume: StateVolumeConfig) -> Self {
        self.state_volume = Some(state_volume);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateVolumeConfig {
    pub mount_path: String,
    pub size: String,
    pub storage_class_name: Option<String>,
}

impl StateVolumeConfig {
    pub fn new(mount_path: impl Into<String>, size: impl Into<String>) -> Self {
        Self {
            mount_path: mount_path.into(),
            size: size.into(),
            storage_class_name: None,
        }
    }

    pub fn storage_class_name(mut self, storage_class_name: impl Into<String>) -> Self {
        self.storage_class_name = Some(storage_class_name.into());
        self
    }
}

#[derive(Clone)]
pub struct AgentSandboxBackend {
    client: Client,
    config: AgentSandboxConfig,
    streams: Arc<Mutex<HashMap<SandboxId, K8sStreams>>>,
}

struct K8sStreams {
    attached: AttachedProcess,
    stdin: Option<Pin<Box<dyn AsyncWrite + Send>>>,
    stdout: Option<Pin<Box<dyn AsyncRead + Send>>>,
    stderr: Option<Pin<Box<dyn AsyncRead + Send>>>,
}

impl AgentSandboxBackend {
    pub fn new(client: Client, config: AgentSandboxConfig) -> Self {
        Self {
            client,
            config,
            streams: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn try_default(namespace: impl Into<String>) -> SandboxResult<Self> {
        let client = Client::try_default()
            .await
            .map_err(|err| SandboxError::Backend(format!("create kube client: {err}")))?;
        Ok(Self::new(client, AgentSandboxConfig::new(namespace)))
    }

    fn sandboxes(&self) -> Api<crd::Sandbox> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    fn persistent_volume_claims(&self) -> Api<PersistentVolumeClaim> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    async fn get_sandbox(&self, id: &SandboxId) -> SandboxResult<Option<crd::Sandbox>> {
        match self.sandboxes().get(id.as_str()).await {
            Ok(sandbox) => Ok(Some(sandbox)),
            Err(err) if is_not_found(&err) => Ok(None),
            Err(err) => Err(map_kube_error("get sandbox", err)),
        }
    }

    async fn get_pod(&self, id: &SandboxId) -> SandboxResult<Option<Pod>> {
        match self.pods().get(id.as_str()).await {
            Ok(pod) => Ok(Some(pod)),
            Err(err) if is_not_found(&err) => Ok(None),
            Err(err) => Err(map_kube_error("get sandbox pod", err)),
        }
    }

    async fn patch_replicas(&self, id: &SandboxId, replicas: i32) -> SandboxResult<()> {
        let params = PatchParams::apply(&self.config.field_manager);
        let patch = Patch::Merge(json!({ "spec": { "replicas": replicas } }));
        self.sandboxes()
            .patch(id.as_str(), &params, &patch)
            .await
            .map(|_| ())
            .map_err(|err| map_kube_error("patch sandbox replicas", err))
    }

    async fn delete_state_pvc(&self, id: &SandboxId) -> SandboxResult<()> {
        if self.config.state_volume.is_none() {
            return Ok(());
        }
        match self
            .persistent_volume_claims()
            .delete(&state_pvc_name(id), &DeleteParams::default())
            .await
        {
            Ok(_) => Ok(()),
            Err(err) if is_not_found(&err) => Ok(()),
            Err(err) => Err(map_kube_error("delete sandbox state pvc", err)),
        }
    }

    async fn wait_until_running(&self, id: &SandboxId) -> SandboxResult<()> {
        let deadline = Instant::now() + self.config.ready_timeout;
        loop {
            match self.status(id).await? {
                SandboxStatus::Running => return Ok(()),
                SandboxStatus::Gone | SandboxStatus::Stopped => {
                    return Err(SandboxError::NotReady(format!(
                        "sandbox {} reached terminal state before running",
                        id.as_str()
                    )));
                }
                status if Instant::now() >= deadline => {
                    return Err(SandboxError::NotReady(format!(
                        "sandbox {} did not become running before timeout; latest status: {status:?}",
                        id.as_str()
                    )));
                }
                _ => sleep(Duration::from_millis(500)).await,
            }
        }
    }

    async fn ensure_attached(&self, id: &SandboxId) -> SandboxResult<()> {
        if self.status(id).await? != SandboxStatus::Running {
            self.drop_streams(id).await;
            return Err(SandboxError::NotReady(format!(
                "agent sandbox {} is not running",
                id.as_str()
            )));
        }
        if self.streams.lock().await.contains_key(id) {
            return Ok(());
        }
        let params = AttachParams::default()
            .container(self.config.container_name.clone())
            .stdin(true)
            .stdout(true)
            .stderr(true)
            .tty(false)
            .max_stdout_buf_size(1024 * 1024)
            .max_stderr_buf_size(1024 * 1024);
        let mut attached = self
            .pods()
            .attach(id.as_str(), &params)
            .await
            .map_err(|err| map_kube_error("attach sandbox pod", err))?;
        let stdin = attached
            .stdin()
            .map(|stream| Box::pin(stream) as Pin<Box<dyn AsyncWrite + Send>>);
        let stdout = attached
            .stdout()
            .map(|stream| Box::pin(stream) as Pin<Box<dyn AsyncRead + Send>>);
        let stderr = attached
            .stderr()
            .map(|stream| Box::pin(stream) as Pin<Box<dyn AsyncRead + Send>>);
        self.streams.lock().await.insert(
            id.clone(),
            K8sStreams {
                attached,
                stdin,
                stdout,
                stderr,
            },
        );
        Ok(())
    }

    async fn drop_streams(&self, id: &SandboxId) {
        if let Some(streams) = self.streams.lock().await.remove(id) {
            streams.attached.abort();
        }
    }
}

#[async_trait]
impl SandboxBackend for AgentSandboxBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        let id = SandboxId::new(next_sandbox_name());
        let sandbox = build_agent_sandbox(&id, &spec, &self.config)?;
        let create_result = self
            .sandboxes()
            .create(&PostParams::default(), &sandbox)
            .await
            .map_err(|err| map_kube_error("create sandbox", err));
        create_result?;
        if let Err(err) = self.wait_until_running(&id).await {
            let _ = self.stop(&id).await;
            return Err(err);
        }
        Ok(SandboxHandle::new(id, BACKEND_NAME))
    }

    async fn read_bytes(&self, id: &SandboxId, options: ReadOptions) -> SandboxResult<ReadResult> {
        self.ensure_attached(id).await?;
        let mut streams = self.streams.lock().await;
        let streams = streams
            .get_mut(id)
            .ok_or_else(|| SandboxError::Io("attach stream was not initialized".to_owned()))?;
        let reader = match options.stream {
            centaur_sandbox_core::OutputStream::Stdout => streams
                .stdout
                .as_mut()
                .ok_or_else(|| SandboxError::Io("stdout is closed".to_owned()))?,
            centaur_sandbox_core::OutputStream::Stderr => streams
                .stderr
                .as_mut()
                .ok_or_else(|| SandboxError::Io("stderr is closed".to_owned()))?,
        };

        let mut buf = vec![0; options.max_bytes];
        let read = read_with_timeout(reader.as_mut(), &mut buf, options.timeout_ms).await?;
        match read {
            Some(0) => Ok(ReadResult::Eof),
            Some(n) => {
                buf.truncate(n);
                Ok(ReadResult::Bytes {
                    bytes: Bytes::from(buf),
                    stream: options.stream,
                    start_offset: None,
                    next_offset: None,
                })
            }
            None => Ok(ReadResult::TimedOut),
        }
    }

    async fn write_bytes(&self, id: &SandboxId, bytes: Bytes) -> SandboxResult<WriteAck> {
        self.ensure_attached(id).await?;
        let mut streams = self.streams.lock().await;
        let streams = streams
            .get_mut(id)
            .ok_or_else(|| SandboxError::Io("attach stream was not initialized".to_owned()))?;
        let stdin = streams
            .stdin
            .as_mut()
            .ok_or_else(|| SandboxError::Io("stdin is closed".to_owned()))?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|err| SandboxError::Io(format!("failed to write stdin: {err}")))?;
        stdin
            .flush()
            .await
            .map_err(|err| SandboxError::Io(format!("failed to flush stdin: {err}")))?;
        Ok(WriteAck::new(bytes.len()))
    }

    async fn close_stdin(&self, id: &SandboxId) -> SandboxResult<()> {
        self.ensure_attached(id).await?;
        let mut streams = self.streams.lock().await;
        let streams = streams
            .get_mut(id)
            .ok_or_else(|| SandboxError::Io("attach stream was not initialized".to_owned()))?;
        streams.stdin.take();
        Ok(())
    }

    async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        let Some(sandbox) = self.get_sandbox(id).await? else {
            return Ok(SandboxStatus::Gone);
        };
        let replicas = sandbox.spec.replicas.unwrap_or(1);
        let pod = self.get_pod(id).await?;
        Ok(sandbox_status_from_pod(replicas, pod.as_ref()))
    }

    async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        let status = self.status(id).await?;
        let generation = self
            .get_sandbox(id)
            .await?
            .and_then(|sandbox| sandbox.metadata.resource_version);
        let mut observed = ObservedSandbox::new(id.clone(), BACKEND_NAME, status);
        observed.generation = generation;
        Ok(observed)
    }

    async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
        let params =
            ListParams::default().labels(&format!("{MANAGED_BY_LABEL}={MANAGED_BY_VALUE}"));
        let sandboxes = self
            .sandboxes()
            .list(&params)
            .await
            .map_err(|err| map_kube_error("list sandboxes", err))?;
        let mut observed = Vec::with_capacity(sandboxes.items.len());
        for sandbox in sandboxes.items {
            let Some(name) = sandbox.metadata.name else {
                continue;
            };
            let id = SandboxId::new(name);
            observed.push(self.observe(&id).await?);
        }
        Ok(observed)
    }

    async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        self.drop_streams(id).await;
        match self
            .sandboxes()
            .delete(id.as_str(), &DeleteParams::default())
            .await
        {
            Ok(_) => self.delete_state_pvc(id).await,
            Err(err) if is_not_found(&err) => self.delete_state_pvc(id).await,
            Err(err) => Err(map_kube_error("delete sandbox", err)),
        }
    }

    async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        self.drop_streams(id).await;
        self.patch_replicas(id, 0).await
    }

    async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
        self.patch_replicas(id, 1).await?;
        self.wait_until_running(id).await
    }

    async fn exec(&self, id: &SandboxId, command: ExecCommand) -> SandboxResult<ExecResult> {
        validate_kubernetes_exec_command(&command)?;
        let params = AttachParams::default()
            .container(self.config.container_name.clone())
            .stdin(false)
            .stdout(true)
            .stderr(true)
            .tty(false);
        let mut attached = self
            .pods()
            .exec(id.as_str(), command.argv, &params)
            .await
            .map_err(|err| map_kube_error("exec sandbox pod", err))?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut reader) = attached.stdout() {
            reader
                .read_to_end(&mut stdout)
                .await
                .map_err(|err| SandboxError::Io(format!("failed to read exec stdout: {err}")))?;
        }
        if let Some(mut reader) = attached.stderr() {
            reader
                .read_to_end(&mut stderr)
                .await
                .map_err(|err| SandboxError::Io(format!("failed to read exec stderr: {err}")))?;
        }
        let exit_code = match attached.take_status() {
            Some(status) => status
                .await
                .map_or(-1, |status| exec_status_exit_code(&status)),
            None => -1,
        };
        Ok(ExecResult::new(exit_code, stdout, stderr))
    }

    async fn interrupt(&self, _id: &SandboxId) -> SandboxResult<()> {
        Err(unsupported("interrupt"))
    }
}

async fn read_with_timeout(
    mut reader: Pin<&mut (dyn AsyncRead + Send)>,
    buf: &mut [u8],
    timeout_ms: Option<u64>,
) -> SandboxResult<Option<usize>> {
    if let Some(timeout_ms) = timeout_ms {
        match timeout(Duration::from_millis(timeout_ms), reader.read(buf)).await {
            Ok(result) => result
                .map(Some)
                .map_err(|err| SandboxError::Io(format!("failed to read output: {err}"))),
            Err(_) => Ok(None),
        }
    } else {
        reader
            .read(buf)
            .await
            .map(Some)
            .map_err(|err| SandboxError::Io(format!("failed to read output: {err}")))
    }
}

fn sandbox_status_from_pod(replicas: i32, pod: Option<&Pod>) -> SandboxStatus {
    if replicas == 0 {
        return SandboxStatus::Suspended;
    }
    let Some(pod) = pod else {
        return SandboxStatus::Created;
    };
    if pod.metadata.deletion_timestamp.is_some() {
        return SandboxStatus::Created;
    }

    let phase = pod
        .status
        .as_ref()
        .and_then(|status| status.phase.as_deref())
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    match phase.as_str() {
        "running" if pod_ready(pod) => SandboxStatus::Running,
        "running" | "pending" => SandboxStatus::Created,
        "succeeded" | "failed" => SandboxStatus::Stopped,
        "unknown" => SandboxStatus::Unknown("unknown".to_owned()),
        other => SandboxStatus::Unknown(other.to_owned()),
    }
}

fn pod_ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}

fn exec_status_exit_code(status: &Status) -> i32 {
    if status.status.as_deref() == Some("Success") {
        return 0;
    }
    status
        .details
        .as_ref()
        .and_then(|details| details.causes.as_ref())
        .and_then(|causes| {
            causes
                .iter()
                .find(|cause| cause.reason.as_deref() == Some("ExitCode"))
                .and_then(|cause| cause.message.as_deref())
                .and_then(|message| message.parse::<i32>().ok())
        })
        .unwrap_or(1)
}

fn validate_kubernetes_exec_command(command: &ExecCommand) -> SandboxResult<()> {
    if command.argv.is_empty() {
        return Err(SandboxError::InvalidSpec("exec argv is empty".to_owned()));
    }
    if !command.env.is_empty() {
        return Err(SandboxError::InvalidSpec(
            "kubernetes exec does not support per-command env".to_owned(),
        ));
    }
    if command.working_dir.is_some() {
        return Err(SandboxError::InvalidSpec(
            "kubernetes exec does not support per-command working_dir".to_owned(),
        ));
    }
    Ok(())
}

fn build_agent_sandbox(
    id: &SandboxId,
    spec: &SandboxSpec,
    config: &AgentSandboxConfig,
) -> SandboxResult<crd::Sandbox> {
    let mut labels = config.labels.clone();
    labels.insert(MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned());
    labels.insert(SANDBOX_ID_LABEL.to_owned(), id.as_str().to_owned());

    let mut pod_labels = labels.clone();
    pod_labels.insert(
        "app.kubernetes.io/name".to_owned(),
        "centaur-sandbox".to_owned(),
    );

    let mut container = json!({
        "name": config.container_name,
        "image": spec.image,
        "stdin": true,
        "stdinOnce": false,
        "tty": false,
    });
    insert_optional(
        &mut container,
        "imagePullPolicy",
        config.image_pull_policy.clone(),
    );
    insert_optional(&mut container, "command", spec.command.clone());
    insert_optional(
        &mut container,
        "args",
        (!spec.args.is_empty()).then(|| spec.args.clone()),
    );
    insert_optional(
        &mut container,
        "env",
        (!spec.env.is_empty()).then(|| {
            spec.env
                .iter()
                .map(|env| json!({ "name": env.name, "value": env.value }))
                .collect::<Vec<_>>()
        }),
    );
    insert_optional(&mut container, "workingDir", spec.working_dir.clone());
    insert_optional(&mut container, "resources", resources_json(spec));

    let (mut volumes, mut volume_mounts) = mount_json(spec);
    if let Some(state_volume) = &config.state_volume {
        volume_mounts.push(json!({
            "name": "state",
            "mountPath": state_volume.mount_path,
        }));
    }
    insert_optional(
        &mut container,
        "volumeMounts",
        (!volume_mounts.is_empty()).then_some(volume_mounts),
    );

    let mut pod_spec = json!({
        "containers": [container],
        "restartPolicy": "Never",
        "automountServiceAccountToken": false,
    });
    insert_optional(
        &mut pod_spec,
        "volumes",
        (!volumes.is_empty()).then(|| std::mem::take(&mut volumes)),
    );

    let mut agent_spec = json!({
        "replicas": 1,
        "service": false,
        "shutdownPolicy": "Retain",
        "podTemplate": {
            "metadata": {
                "labels": pod_labels,
                "annotations": config.annotations,
            },
            "spec": pod_spec,
        },
    });
    insert_optional(
        &mut agent_spec,
        "volumeClaimTemplates",
        config.state_volume.as_ref().map(state_volume_claim_json),
    );

    let spec = serde_json::from_value(agent_spec)
        .map_err(|err| SandboxError::InvalidSpec(format!("invalid Agent Sandbox spec: {err}")))?;
    let mut sandbox = crd::Sandbox::new(id.as_str(), spec);
    sandbox.metadata.labels = Some(labels);
    sandbox.metadata.annotations = Some(config.annotations.clone());
    Ok(sandbox)
}

fn mount_json(spec: &SandboxSpec) -> (Vec<Value>, Vec<Value>) {
    let mut volumes = Vec::with_capacity(spec.mounts.len());
    let mut mounts = Vec::with_capacity(spec.mounts.len());
    for (index, mount) in spec.mounts.iter().enumerate() {
        let name = format!("mount-{index}");
        mounts.push(json!({
            "name": name,
            "mountPath": mount.target_path,
            "readOnly": mount.read_only,
        }));
        volumes.push(match &mount.kind {
            MountKind::EmptyDir => json!({
                "name": name,
                "emptyDir": {},
            }),
            MountKind::NamedVolume(claim_name) => json!({
                "name": name,
                "persistentVolumeClaim": {
                    "claimName": claim_name,
                    "readOnly": mount.read_only,
                },
            }),
            MountKind::Bind { source_path } => json!({
                "name": name,
                "hostPath": {
                    "path": source_path,
                },
            }),
        });
    }
    (volumes, mounts)
}

fn resources_json(spec: &SandboxSpec) -> Option<Value> {
    let resources = spec.resources.as_ref()?;
    let mut limits = serde_json::Map::new();
    if let Some(cpu_millis) = resources.cpu_millis {
        limits.insert("cpu".to_owned(), json!(format!("{cpu_millis}m")));
    }
    if let Some(memory_bytes) = resources.memory_bytes {
        limits.insert("memory".to_owned(), json!(format!("{memory_bytes}")));
    }
    (!limits.is_empty()).then(|| json!({ "limits": limits }))
}

fn state_volume_claim_json(state_volume: &StateVolumeConfig) -> Vec<Value> {
    let mut pvc_spec = json!({
        "accessModes": ["ReadWriteOnce"],
        "resources": {
            "requests": {
                "storage": state_volume.size,
            },
        },
    });
    insert_optional(
        &mut pvc_spec,
        "storageClassName",
        state_volume.storage_class_name.clone(),
    );
    vec![json!({
        "metadata": {
            "name": "state",
        },
        "spec": pvc_spec,
    })]
}

fn state_pvc_name(id: &SandboxId) -> String {
    format!("state-{}", id.as_str())
}

fn insert_optional<T>(target: &mut Value, key: &str, value: Option<T>)
where
    T: serde::Serialize,
{
    if let Some(value) = value {
        target[key] = json!(value);
    }
}

fn next_sandbox_name() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("asbx-{millis}-{sequence}")
}

fn unsupported(operation: &'static str) -> SandboxError {
    SandboxError::Unsupported {
        backend: BACKEND_NAME,
        operation,
    }
}

fn is_not_found(err: &Error) -> bool {
    matches!(err, Error::Api(api_error) if api_error.code == 404)
}

fn map_kube_error(operation: &str, err: Error) -> SandboxError {
    if is_not_found(&err) {
        SandboxError::NotFound(operation.to_owned())
    } else {
        SandboxError::Backend(format!("{operation}: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use centaur_sandbox_core::{ExecCommand, ResourceLimits, SandboxError, SandboxSpec};
    use k8s_openapi::api::core::v1::{PodCondition, PodStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{StatusCause, StatusDetails};

    use super::*;

    #[test]
    fn builds_agent_sandbox_spec_with_state_volume_and_limits() {
        let spec = SandboxSpec::new("centaur-agent:latest")
            .command(["/bin/sh", "-lc"])
            .args(["cat"])
            .env("CENTAUR_API_URL", "http://api:8000")
            .mount(centaur_sandbox_core::Mount::new(
                MountKind::EmptyDir,
                "/workspace",
            ))
            .resources(
                ResourceLimits::new()
                    .cpu_millis(500)
                    .memory_bytes(512 * 1024 * 1024),
            );
        let config = AgentSandboxConfig::new("centaur")
            .state_volume(StateVolumeConfig::new("/home/agent/state", "10Gi"));

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert_eq!(sandbox.metadata.name.as_deref(), Some("asbx-test"));
        assert_eq!(sandbox.spec.replicas, Some(1));
        assert_eq!(
            sandbox.spec.shutdown_policy,
            Some(crd::SandboxShutdownPolicy::Retain)
        );
        assert_eq!(
            sandbox.spec.volume_claim_templates.as_ref().unwrap().len(),
            1
        );
        let container = &sandbox.spec.pod_template.spec.containers[0];
        assert_eq!(container.image.as_deref(), Some("centaur-agent:latest"));
        assert_eq!(container.stdin, Some(true));
        assert_eq!(container.volume_mounts.as_ref().unwrap().len(), 2);
        assert!(container.resources.as_ref().unwrap().limits.is_some());
    }

    #[test]
    fn maps_agent_sandbox_replicas_and_pod_readiness_to_status() {
        let ready_pod = pod_with_phase_and_ready("Running", true);
        assert_eq!(
            sandbox_status_from_pod(0, Some(&ready_pod)),
            SandboxStatus::Suspended
        );
        assert_eq!(
            sandbox_status_from_pod(1, Some(&ready_pod)),
            SandboxStatus::Running
        );

        let unready_pod = pod_with_phase_and_ready("Running", false);
        assert_eq!(
            sandbox_status_from_pod(1, Some(&unready_pod)),
            SandboxStatus::Created
        );
        assert_eq!(sandbox_status_from_pod(1, None), SandboxStatus::Created);

        let failed_pod = pod_with_phase_and_ready("Failed", false);
        assert_eq!(
            sandbox_status_from_pod(1, Some(&failed_pod)),
            SandboxStatus::Stopped
        );
    }

    #[test]
    fn state_pvc_name_matches_agent_sandbox_template() {
        assert_eq!(
            state_pvc_name(&SandboxId::new("asbx-test")),
            "state-asbx-test"
        );
    }

    #[test]
    fn preserves_kubernetes_exec_exit_code() {
        let status = Status {
            status: Some("Failure".to_owned()),
            details: Some(StatusDetails {
                causes: Some(vec![StatusCause {
                    reason: Some("ExitCode".to_owned()),
                    message: Some("42".to_owned()),
                    ..StatusCause::default()
                }]),
                ..StatusDetails::default()
            }),
            ..Status::default()
        };
        assert_eq!(exec_status_exit_code(&status), 42);

        let status = Status {
            status: Some("Success".to_owned()),
            ..Status::default()
        };
        assert_eq!(exec_status_exit_code(&status), 0);
    }

    #[test]
    fn rejects_unsupported_kubernetes_exec_options() {
        let err = validate_kubernetes_exec_command(&ExecCommand::new(["/bin/true"]).env("A", "B"))
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidSpec(message) if message.contains("env")));

        let err = validate_kubernetes_exec_command(
            &ExecCommand::new(["/bin/true"]).working_dir("/workspace"),
        )
        .unwrap_err();
        assert!(
            matches!(err, SandboxError::InvalidSpec(message) if message.contains("working_dir"))
        );
    }

    fn pod_with_phase_and_ready(phase: &str, ready: bool) -> Pod {
        Pod {
            status: Some(PodStatus {
                phase: Some(phase.to_owned()),
                conditions: Some(vec![PodCondition {
                    type_: "Ready".to_owned(),
                    status: if ready { "True" } else { "False" }.to_owned(),
                    ..PodCondition::default()
                }]),
                ..PodStatus::default()
            }),
            ..Pod::default()
        }
    }
}
