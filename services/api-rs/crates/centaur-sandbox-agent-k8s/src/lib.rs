//! Agent Sandbox Kubernetes backend.
//!
//! The Agent Sandbox CRD types are generated from the upstream CRD with
//! `just codegen-agent-sandbox-crd`.

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use centaur_iron_control::IronControlClient;
use centaur_sandbox_core::{
    MountKind, ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
    SandboxResult, SandboxSpec, SandboxStatus,
};
use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod};
use kube::api::{
    AttachParams, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams,
};
use kube::{Api, Client, Error};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};

pub use generated::agents_x_k8s_io as crd;
pub use iron_proxy::IronProxyConfig;
pub use tools::{GitHubTokenRef, ToolSource, ToolsConfig};

pub mod generated;
mod iron_proxy;
mod tools;

const BACKEND_NAME: &str = "agent-sandbox-k8s";
const DEFAULT_CONTAINER_NAME: &str = "agent";
const MANAGED_BY_LABEL: &str = "centaur.ai/managed-by";
const SANDBOX_ID_LABEL: &str = "centaur.ai/sandbox-id";
const OBSERVABILITY_ENABLED_LABEL: &str = "centaur.ai/observability-enabled";
const API_SERVER_ENABLED_LABEL: &str = "centaur.ai/api-server-enabled";
const MANAGED_BY_VALUE: &str = "api-rs";
// iron-control principal OID the sandbox's proxy binds to, stamped at create
// so resume (which has only the sandbox id) can rebind without the spec or any
// in-memory state. Survives pause and api-rs restarts.
const IRON_CONTROL_PRINCIPAL_ANNOTATION: &str = "centaur.ai/iron-control-principal";
// RFC 3339 instant stamped when the sandbox is paused for idleness and cleared
// on resume. This keeps suspended status observable across api-rs restarts.
const PAUSED_AT_ANNOTATION: &str = "centaur.ai/paused-at";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct AgentSandboxConfig {
    pub namespace: String,
    pub field_manager: String,
    pub container_name: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub image_pull_policy: Option<String>,
    pub image_pull_secrets: Vec<String>,
    pub state_volume: Option<StateVolumeConfig>,
    pub iron_proxy: Option<IronProxyConfig>,
    pub iron_control: Option<IronControlSettings>,
    /// When set, every sandbox gets a `tools-bootstrap` init container that
    /// git-clones the tools repo into the agent's `/app/tools`, and `TOOL_DIRS`
    /// is set so the agent's shim installer finds them.
    pub tools: Option<ToolsConfig>,
    /// In-cluster OTLP collector (e.g. Laminar) used for observability-capable
    /// sandboxes. Sandbox pod egress is granted by chart-level label policy;
    /// the per-sandbox proxy uses this target for its own explicit egress.
    pub otlp_egress: Option<OtlpEgressTarget>,
    pub ready_timeout: Duration,
}

/// Destination of the sandbox's direct OTLP export, expressed as the target
/// namespace (matched by `kubernetes.io/metadata.name`) and port of the
/// collector service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OtlpEgressTarget {
    pub namespace: String,
    pub port: u16,
}

/// iron-control coordinates for sync-mode egress proxies. When set, a sandbox
/// whose spec carries an `iron_control_principal` gets a per-sandbox proxy
/// registered in iron-control (synced over `IRON_CONTROL_URL` with its
/// `iprx_` token) instead of a rendered static proxy config.
#[derive(Clone, Debug)]
pub struct IronControlSettings {
    /// Admin client used to register/deregister the per-sandbox proxy.
    pub client: IronControlClient,
    /// Base URL injected into the proxy pod as `IRON_CONTROL_URL`.
    pub control_url: String,
    /// iron-control namespace, used to resolve principals by `foreign_id`.
    pub namespace: String,
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
            image_pull_secrets: Vec::new(),
            state_volume: None,
            iron_proxy: None,
            iron_control: None,
            tools: None,
            otlp_egress: None,
            ready_timeout: Duration::from_secs(60),
        }
    }

    pub fn state_volume(mut self, state_volume: StateVolumeConfig) -> Self {
        self.state_volume = Some(state_volume);
        self
    }

    pub fn iron_proxy(mut self, iron_proxy: IronProxyConfig) -> Self {
        self.iron_proxy = Some(iron_proxy);
        self
    }

    pub fn iron_control(mut self, iron_control: IronControlSettings) -> Self {
        self.iron_control = Some(iron_control);
        self
    }

    pub fn tools(mut self, tools: ToolsConfig) -> Self {
        self.tools = Some(tools);
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
    // sandbox id -> iron-control proxy OID, so the proxy can be deregistered on
    // stop. Only populated for sync-mode sandboxes.
    proxy_ids: Arc<Mutex<HashMap<String, String>>>,
}

impl AgentSandboxBackend {
    pub fn new(client: Client, config: AgentSandboxConfig) -> Self {
        Self {
            client,
            config,
            proxy_ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn try_default(namespace: impl Into<String>) -> SandboxResult<Self> {
        let client = Client::try_default()
            .await
            .map_err(|err| SandboxError::backend_source("create kube client", err))?;
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

    async fn observed_from_sandbox(
        &self,
        id: &SandboxId,
        sandbox: &crd::Sandbox,
    ) -> SandboxResult<ObservedSandbox> {
        let replicas = sandbox.spec.replicas.unwrap_or(1);
        let pod = self.get_pod(id).await?;
        let status = sandbox_status_from_pod(replicas, pod.as_ref());
        Ok(ObservedSandbox::new(id.clone(), BACKEND_NAME, status)
            .with_created_at(sandbox_creation_time(sandbox))
            .with_suspended_since(sandbox_paused_at(sandbox)))
    }

    async fn patch_sandbox_merge(&self, id: &SandboxId, patch: Value) -> SandboxResult<()> {
        let params = PatchParams::apply(&self.config.field_manager);
        self.sandboxes()
            .patch(id.as_str(), &params, &Patch::Merge(patch))
            .await
            .map(|_| ())
            .map_err(|err| map_kube_error("patch sandbox", err))
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

    async fn attach_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        if self.status(id).await? != SandboxStatus::Running {
            return Err(SandboxError::NotReady(format!(
                "agent sandbox {} is not running",
                id.as_str()
            )));
        }
        let params = AttachParams::default()
            .container(self.config.container_name.clone())
            .stdin(true)
            .stdout(true)
            .stderr(true)
            .tty(false);
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
        let stdin = stdin.ok_or_else(|| SandboxError::io("stdin was not attached"))?;
        let stdout = stdout.ok_or_else(|| SandboxError::io("stdout was not attached"))?;
        let stderr = stderr.ok_or_else(|| SandboxError::io("stderr was not attached"))?;
        // Keep kube's attach process alive as long as the returned streams are in use.
        Ok(SandboxIo::with_guard(stdin, stdout, stderr, attached))
    }
}

#[async_trait]
impl SandboxBackend for AgentSandboxBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        let id = SandboxId::new(next_sandbox_name());
        let mut spec = spec;
        let resolved_iron_proxy = self.resolve_iron_proxy(&id, &spec).await?;
        if let Some(resolved) = &resolved_iron_proxy {
            iron_proxy::apply_proxy_env(&mut spec, resolved);
        }
        if let Err(err) = self
            .create_iron_proxy_resources(&id, resolved_iron_proxy.as_ref())
            .await
        {
            let _ = self.delete_iron_proxy_resources(&id).await;
            return Err(err);
        }
        let sandbox = build_agent_sandbox(&id, &spec, &self.config)?;
        let created = match self
            .sandboxes()
            .create(&PostParams::default(), &sandbox)
            .await
        {
            Ok(created) => created,
            Err(err) => {
                let _ = self.delete_iron_proxy_resources(&id).await;
                return Err(map_kube_error("create sandbox", err));
            }
        };
        // The proxy resources are created before the Sandbox CR (the egress
        // policies must exist before the pod starts), so bind them to it here
        // for cascade deletion. Failure leaves them cleanable by stop() only.
        if let Err(error) = self.adopt_iron_proxy_resources(&id, &created).await {
            tracing::warn!(
                sandbox_id = id.as_str(),
                %error,
                "failed to set ownerReferences on iron-proxy resources"
            );
        }
        if let Err(err) = self.wait_until_running(&id).await {
            let _ = self.stop(&id).await;
            return Err(err);
        }
        Ok(SandboxHandle::new(id, BACKEND_NAME))
    }

    async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        self.attach_io(id).await
    }

    /// Replays the workload container's stdout from the kubelet's log files.
    /// Unlike an attach stream, this includes output emitted while no reader
    /// was attached, which is what makes orphaned-execution adoption possible.
    async fn read_output_since(
        &self,
        id: &SandboxId,
        since: Option<std::time::SystemTime>,
    ) -> SandboxResult<Vec<String>> {
        let mut params = LogParams {
            container: Some(self.config.container_name.clone()),
            ..LogParams::default()
        };
        if let Some(since) = since {
            params.since_time = Some(
                jiff::Timestamp::try_from(since)
                    .map_err(|error| SandboxError::io_source("invalid log since time", error))?,
            );
        }
        let text = self
            .pods()
            .logs(id.as_str(), &params)
            .await
            .map_err(|err| map_kube_error("read sandbox pod logs", err))?;
        Ok(text.lines().map(str::to_owned).collect())
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
        let Some(sandbox) = self.get_sandbox(id).await? else {
            return Ok(ObservedSandbox::new(
                id.clone(),
                BACKEND_NAME,
                SandboxStatus::Gone,
            ));
        };
        self.observed_from_sandbox(id, &sandbox).await
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
            let Some(name) = sandbox.metadata.name.clone() else {
                continue;
            };
            let id = SandboxId::new(name);
            observed.push(self.observed_from_sandbox(&id, &sandbox).await?);
        }
        Ok(observed)
    }

    async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        let proxy_result = self.delete_iron_proxy_resources(id).await;
        match self
            .sandboxes()
            .delete(id.as_str(), &DeleteParams::default())
            .await
        {
            Ok(_) => {
                proxy_result?;
                self.delete_state_pvc(id).await
            }
            Err(err) if is_not_found(&err) => {
                proxy_result?;
                self.delete_state_pvc(id).await
            }
            Err(err) => Err(map_kube_error("delete sandbox", err)),
        }
    }

    async fn assign_iron_control_proxy_principal(
        &self,
        id: &SandboxId,
        principal_id: &str,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.assign_proxy_principal(id, principal_id, labels).await
    }

    async fn ensure_iron_control_proxy_resources(
        &self,
        id: &SandboxId,
        principal_id: &str,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.ensure_proxy_resources_for_principal(id, principal_id, labels)
            .await
    }

    async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        self.patch_sandbox_merge(id, sandbox_pause_patch(jiff::Timestamp::now()))
            .await
    }

    async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
        // Resume only has the sandbox id, not the spec, so rebind the proxy to
        // the principal recorded at create rather than re-resolving from spec.
        let resolved_iron_proxy = self.resolve_iron_proxy_for_resume(id).await?;
        if let Err(err) = self
            .create_iron_proxy_resources(id, resolved_iron_proxy.as_ref())
            .await
        {
            let _ = self.delete_iron_proxy_resources(id).await;
            return Err(err);
        }
        // The proxy resources were recreated, so re-bind them to the sandbox
        // for cascade deletion.
        if let Some(sandbox) = self.get_sandbox(id).await?
            && let Err(error) = self.adopt_iron_proxy_resources(id, &sandbox).await
        {
            tracing::warn!(
                sandbox_id = id.as_str(),
                %error,
                "failed to set ownerReferences on resumed iron-proxy resources"
            );
        }
        self.patch_sandbox_merge(id, sandbox_resume_patch()).await?;
        self.wait_until_running(id).await
    }
}

fn sandbox_pause_patch(paused_at: jiff::Timestamp) -> Value {
    json!({
        "spec": { "replicas": 0 },
        "metadata": { "annotations": { PAUSED_AT_ANNOTATION: paused_at.to_string() } },
    })
}

fn sandbox_resume_patch() -> Value {
    // A JSON merge patch null removes the annotation.
    json!({
        "spec": { "replicas": 1 },
        "metadata": { "annotations": { PAUSED_AT_ANNOTATION: null } },
    })
}

fn sandbox_creation_time(sandbox: &crd::Sandbox) -> Option<SystemTime> {
    sandbox
        .metadata
        .creation_timestamp
        .as_ref()
        .map(|time| SystemTime::from(time.0))
}

fn sandbox_paused_at(sandbox: &crd::Sandbox) -> Option<SystemTime> {
    let raw = sandbox
        .metadata
        .annotations
        .as_ref()?
        .get(PAUSED_AT_ANNOTATION)?;
    let timestamp = raw.parse::<jiff::Timestamp>().ok()?;
    Some(SystemTime::from(timestamp))
}

fn sandbox_status_from_pod(replicas: i32, pod: Option<&Pod>) -> SandboxStatus {
    if replicas == 0 {
        return SandboxStatus::Suspended;
    }
    // The backing Pod Ready condition is the attach boundary; phase alone can be Running while
    // the sandbox is still not ready for I/O.
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

fn build_agent_sandbox(
    id: &SandboxId,
    spec: &SandboxSpec,
    config: &AgentSandboxConfig,
) -> SandboxResult<crd::Sandbox> {
    let mut labels = config.labels.clone();
    labels.extend(spec.labels.clone());
    labels.insert(MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned());
    labels.insert(SANDBOX_ID_LABEL.to_owned(), id.as_str().to_owned());
    if spec.capabilities.observability_enabled {
        labels.insert(OBSERVABILITY_ENABLED_LABEL.to_owned(), "true".to_owned());
    }
    if spec.capabilities.api_server_enabled {
        labels.insert(API_SERVER_ENABLED_LABEL.to_owned(), "true".to_owned());
    }

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
    // Agent container env: spec env + tools wiring (deduped). `TOOL_DIRS`
    // is set deterministically here (not via passthrough) so it always matches
    // the path the bootstrap init container actually populates in this pod.
    let mut agent_env: Vec<(String, String)> = spec
        .env
        .iter()
        .map(|env| (env.name.clone(), env.value.clone()))
        .collect();
    let repo_cache_enabled = spec.capabilities.repo_cache.enabled();
    let scoped_tools = config
        .tools
        .as_ref()
        .filter(|_| repo_cache_enabled)
        .map(|tools| tools.scoped_for_repo_cache_access(&spec.capabilities.repo_cache));
    let repo_cache_tools = scoped_tools.as_ref().filter(|tools| tools.has_sources());
    let baked_base_tools = config.tools.is_some() && repo_cache_tools.is_none();

    if repo_cache_tools.is_some() {
        for (name, value) in tools::agent_env(repo_cache_tools) {
            upsert_env(&mut agent_env, &name, value);
        }
    } else if baked_base_tools {
        for (name, value) in tools::baked_base_agent_env() {
            upsert_env(&mut agent_env, &name, value);
        }
    }
    insert_optional(
        &mut container,
        "env",
        (!agent_env.is_empty()).then(|| {
            agent_env
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect::<Vec<_>>()
        }),
    );
    insert_optional(&mut container, "workingDir", spec.working_dir.clone());
    insert_optional(&mut container, "resources", resources_json(spec));

    let (mut volumes, mut volume_mounts) = mount_json(spec);
    let mut init_containers = Vec::new();
    if let Some(state_volume) = &config.state_volume {
        volume_mounts.push(json!({
            "name": "state",
            "mountPath": state_volume.mount_path,
        }));
    }
    if let Some(iron_proxy) = &config.iron_proxy {
        volume_mounts.push(iron_proxy::sandbox_ca_volume_mount_json());
        volumes.push(iron_proxy::sandbox_ca_volume_json(iron_proxy));
    }
    // Tool sources are bootstrapped into an emptyDir by an init container and
    // mounted into the agent at the same path `TOOL_DIRS` points at. The mount is
    // writable so `centaur-tools refresh` can fetch and republish the tree.
    if repo_cache_tools.is_some() {
        volume_mounts.extend(tools::agent_volume_mounts_json(repo_cache_tools));
        volumes.extend(tools::volumes_json(repo_cache_tools));
    }
    insert_optional(
        &mut container,
        "volumeMounts",
        (!volume_mounts.is_empty()).then_some(volume_mounts),
    );

    // tools-bootstrap publishes the tools repo into /app/tools.
    if let Some(tools) = repo_cache_tools {
        // The sandbox NetworkPolicy only allows egress to the per-sandbox proxy
        // (plus api-rs and DNS), so when iron-proxy is on the clone must ride it.
        // `apply_proxy_env` ran before this builder, so the resolved proxy URL is
        // on the spec env; absent (proxy disabled/unresolved) the clone goes direct.
        let clone_proxy = config.iron_proxy.as_ref().and_then(|_| {
            spec.env
                .iter()
                .find(|env| env.name == "HTTPS_PROXY")
                .map(|env| tools::CloneProxy {
                    https_proxy: env.value.clone(),
                    ca_cert_path: iron_proxy::FIREWALL_CA_CERT_PATH.to_owned(),
                    ca_volume_mount: iron_proxy::sandbox_ca_volume_mount_json(),
                })
        });
        init_containers.push(tools::tools_init_container_json(
            tools,
            clone_proxy.as_ref(),
        ));
    }

    let mut pod_spec = json!({
        "containers": [container],
        "restartPolicy": "Never",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
    });
    if repo_cache_tools.is_some() {
        pod_spec["securityContext"] = tools::pod_security_context_json();
    }
    insert_optional(
        &mut pod_spec,
        "initContainers",
        (!init_containers.is_empty()).then_some(init_containers),
    );
    insert_optional(
        &mut pod_spec,
        "volumes",
        (!volumes.is_empty()).then(|| std::mem::take(&mut volumes)),
    );
    insert_optional(
        &mut pod_spec,
        "imagePullSecrets",
        (!config.image_pull_secrets.is_empty()).then(|| {
            config
                .image_pull_secrets
                .iter()
                .map(|name| json!({ "name": name }))
                .collect::<Vec<_>>()
        }),
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

    let mut annotations = config.annotations.clone();
    if let Some(principal) = &spec.iron_control_principal {
        annotations.insert(
            IRON_CONTROL_PRINCIPAL_ANNOTATION.to_owned(),
            principal.clone(),
        );
    }

    let crd_spec = serde_json::from_value(agent_spec)
        .map_err(|err| SandboxError::InvalidSpec(format!("invalid Agent Sandbox spec: {err}")))?;
    let mut sandbox = crd::Sandbox::new(id.as_str(), crd_spec);
    sandbox.metadata.labels = Some(labels);
    sandbox.metadata.annotations = Some(annotations);
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
        if let Some(sub_path) = &mount.sub_path
            && let Some(mount_obj) = mounts.last_mut().and_then(Value::as_object_mut)
        {
            mount_obj.insert("subPath".to_owned(), json!(sub_path));
        }
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

/// Override-or-append an env entry, so the agent container never emits a
/// duplicate env name when we layer tools/overlay wiring over `spec.env`.
fn upsert_env(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(entry) = env.iter_mut().find(|(existing, _)| existing == name) {
        entry.1 = value;
    } else {
        env.push((name.to_owned(), value));
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

fn is_not_found(err: &Error) -> bool {
    matches!(err, Error::Api(api_error) if api_error.code == 404)
}

fn map_kube_error(operation: &str, err: Error) -> SandboxError {
    if is_not_found(&err) {
        SandboxError::NotFound(operation.to_owned())
    } else {
        SandboxError::backend_source(operation, err)
    }
}

#[cfg(test)]
mod tests {
    use centaur_sandbox_core::{RepoCacheAccess, ResourceLimits, SandboxCapabilities, SandboxSpec};
    use k8s_openapi::api::core::v1::{PodCondition, PodStatus};

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
        assert_eq!(
            sandbox.spec.pod_template.spec.enable_service_links,
            Some(false)
        );
        assert_eq!(container.image.as_deref(), Some("centaur-agent:latest"));
        assert_eq!(container.stdin, Some(true));
        assert_eq!(container.volume_mounts.as_ref().unwrap().len(), 2);
        assert!(container.resources.as_ref().unwrap().limits.is_some());
    }

    #[test]
    fn labels_observability_enabled_sandboxes_for_chart_policy() {
        let spec = SandboxSpec::new("centaur-agent:latest").capabilities(SandboxCapabilities {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: true,
            api_server_enabled: true,
        });
        let config = AgentSandboxConfig::new("centaur");

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert_eq!(
            sandbox
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(OBSERVABILITY_ENABLED_LABEL))
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            sandbox
                .spec
                .pod_template
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.as_ref())
                .and_then(|labels| labels.get(OBSERVABILITY_ENABLED_LABEL))
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            sandbox
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(API_SERVER_ENABLED_LABEL))
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            sandbox
                .spec
                .pod_template
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.as_ref())
                .and_then(|labels| labels.get(API_SERVER_ENABLED_LABEL))
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn omits_api_server_label_for_restricted_sandboxes() {
        let spec = SandboxSpec::new("centaur-agent:latest").capabilities(SandboxCapabilities {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: false,
            api_server_enabled: false,
        });
        let config = AgentSandboxConfig::new("centaur");

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert!(
            sandbox
                .metadata
                .labels
                .as_ref()
                .is_none_or(|labels| !labels.contains_key(OBSERVABILITY_ENABLED_LABEL))
        );
        assert!(
            sandbox
                .spec
                .pod_template
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.as_ref())
                .is_none_or(|labels| !labels.contains_key(OBSERVABILITY_ENABLED_LABEL))
        );
        assert!(
            sandbox
                .metadata
                .labels
                .as_ref()
                .is_none_or(|labels| !labels.contains_key(API_SERVER_ENABLED_LABEL))
        );
        assert!(
            sandbox
                .spec
                .pod_template
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.as_ref())
                .is_none_or(|labels| !labels.contains_key(API_SERVER_ENABLED_LABEL))
        );
    }

    #[test]
    fn tools_clone_rides_iron_proxy_when_enabled() {
        // apply_proxy_env runs before build_agent_sandbox in create(), so the
        // resolved per-sandbox proxy URL arrives on the spec env.
        let spec = SandboxSpec::new("centaur-agent:latest")
            .env("HTTPS_PROXY", "http://asbx-test-iron-proxy:8080");
        let config = AgentSandboxConfig::new("centaur")
            .tools(ToolsConfig::new("paradigmxyz/centaur", "api:test"))
            .iron_proxy(IronProxyConfig::new("proxy:test", "ca-cert", "ca-key"));

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let pod_spec = &sandbox.spec.pod_template.spec;
        let bootstrap = &pod_spec.init_containers.as_ref().unwrap()[0];
        assert_eq!(bootstrap.name, "tools-bootstrap");
        let script = &bootstrap.command.as_ref().unwrap()[2];
        assert!(script.contains("export HTTPS_PROXY=\"http://asbx-test-iron-proxy:8080\""));
        assert!(script.contains("export GIT_SSL_CAINFO=\"/firewall-certs/ca-cert.pem\""));
        assert!(
            bootstrap
                .volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .any(|mount| mount.name == "firewall-ca")
        );

        // Without iron-proxy the clone goes direct: no proxy exports, no CA mount.
        let spec = SandboxSpec::new("centaur-agent:latest");
        let config = AgentSandboxConfig::new("centaur")
            .tools(ToolsConfig::new("paradigmxyz/centaur", "api:test"));
        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let bootstrap = &sandbox
            .spec
            .pod_template
            .spec
            .init_containers
            .as_ref()
            .unwrap()[0];
        let script = &bootstrap.command.as_ref().unwrap()[2];
        assert!(!script.contains("HTTPS_PROXY"));
        assert!(
            !bootstrap
                .volume_mounts
                .as_ref()
                .unwrap()
                .iter()
                .any(|mount| mount.name == "firewall-ca")
        );
    }

    #[test]
    fn disabled_repo_cache_uses_baked_base_tools_without_bootstrap() {
        let spec = SandboxSpec::new("centaur-agent:latest").capabilities(SandboxCapabilities {
            repo_cache: RepoCacheAccess::None,
            observability_enabled: true,
            api_server_enabled: true,
        });
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "api:test");
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());
        let config = AgentSandboxConfig::new("centaur").tools(tools);

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let pod_spec = &sandbox.spec.pod_template.spec;
        assert!(pod_spec.init_containers.as_ref().is_none_or(Vec::is_empty));
        let tool_dirs = pod_spec.containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|env| env.name == "TOOL_DIRS")
            .and_then(|env| env.value.as_deref());
        assert_eq!(tool_dirs, Some("/opt/centaur/tools"));
        assert!(
            pod_spec.containers[0]
                .volume_mounts
                .as_ref()
                .is_none_or(|mounts| {
                    !mounts.iter().any(|mount| {
                        mount.name == "tools-root"
                            || mount.name == "tools-repo-cache"
                            || mount.mount_path == "/app/tools"
                            || mount.mount_path == "/var/lib/centaur/repos"
                    })
                })
        );
        assert!(pod_spec.volumes.as_ref().is_none_or(|volumes| {
            !volumes
                .iter()
                .any(|volume| volume.name == "tools-root" || volume.name == "tools-repo-cache")
        }));
    }

    #[test]
    fn bootstrap_empty_dirs_are_writable_by_agent_uid() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let config = AgentSandboxConfig::new("centaur")
            .tools(ToolsConfig::new("paradigmxyz/centaur", "api:test"));

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let pod_spec = &sandbox.spec.pod_template.spec;

        let security_context = pod_spec.security_context.as_ref().unwrap();
        assert_eq!(security_context.fs_group, Some(1001));
        assert_eq!(
            security_context.fs_group_change_policy.as_deref(),
            Some("OnRootMismatch")
        );
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
