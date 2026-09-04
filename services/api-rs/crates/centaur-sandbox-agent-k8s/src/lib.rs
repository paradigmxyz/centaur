//! Agent Sandbox Kubernetes backend.
//!
//! The Agent Sandbox CRD types are generated from the upstream CRD with
//! `just codegen-agent-sandbox-crd`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
use k8s_openapi::api::core::v1::{ConfigMap, PersistentVolumeClaim, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{
    AttachParams, DeleteParams, ListParams, LogParams, Patch, PatchParams, PostParams,
};
use kube::{Api, Client, Error, Resource};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};

pub use generated::agents_x_k8s_io as crd;
pub use iron_proxy::IronProxyConfig;
pub use k8s_openapi::api::core::v1::Toleration;
pub use tools::{GitHubTokenRef, ToolSource, ToolsConfig};

pub mod generated;
mod iron_proxy;
mod tools;

const BACKEND_NAME: &str = "agent-sandbox-k8s";
const DEFAULT_CONTAINER_NAME: &str = "agent";
const MANAGED_BY_LABEL: &str = "centaur.ai/managed-by";
const SANDBOX_ID_LABEL: &str = "centaur.ai/sandbox-id";
const OBSERVABILITY_ENABLED_LABEL: &str = "centaur.ai/observability-enabled";
const MANAGED_BY_VALUE: &str = "api-rs";
const SANDBOX_FILES_VOLUME: &str = "sandbox-files";
// iron-control principal OID the sandbox's proxy binds to, stamped at create
// so resume (which has only the sandbox id) can rebind without the spec or any
// in-memory state. Survives pause and api-rs restarts.
const IRON_CONTROL_PRINCIPAL_ANNOTATION: &str = "centaur.ai/iron-control-principal";
// Requesting user's principal OID bound to the proxy for the current turn.
// Absent when the turn has no requester, so an annotation-vs-binding
// comparison treats "absent" and "no requester" as equal.
const IRON_CONTROL_REQUESTER_ANNOTATION: &str = "centaur.ai/iron-control-requester-principal";
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
    /// Node steering for every sandbox pod **and** its paired iron-proxy pod.
    /// `Sandbox.spec.podTemplate.spec` already accepts `nodeSelector`,
    /// `tolerations`, and `runtimeClassName`; without wiring these through
    /// api-rs, chart values such as `sandbox.runtimeClassName` are inert
    /// because sandbox pods are created at runtime rather than by Helm.
    pub node_selector: BTreeMap<String, String>,
    /// Tolerations applied with `node_selector` so sandboxes can land on a
    /// tainted agents pool. Empty leaves default scheduling untouched.
    pub tolerations: Vec<Toleration>,
    /// RuntimeClass for sandbox and iron-proxy pods (e.g. `gvisor`).
    pub runtime_class_name: Option<String>,
    /// ServiceAccount for sandbox pods (session, warm, and workflow-host),
    /// e.g. for cloud workload identity (EKS IRSA). Not applied to iron-proxy
    /// pods; `automountServiceAccountToken` stays `false`.
    pub service_account_name: Option<String>,
    /// PriorityClass for sandbox and iron-proxy pods. A dedicated (low)
    /// class lets the cluster scope a ResourceQuota to sandbox workloads and
    /// makes the kubelet/scheduler sacrifice them before the control plane
    /// under node pressure. Empty leaves the cluster default untouched.
    pub priority_class_name: Option<String>,
    pub state_volume: Option<StateVolumeConfig>,
    pub iron_proxy: Option<IronProxyConfig>,
    pub iron_control: IronControlSettings,
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

/// iron-control coordinates for sync-mode egress proxies. A sandbox
/// whose spec carries an `iron_control_principal` gets a per-sandbox proxy
/// registered in iron-control (synced over `IRON_CONTROL_URL` with its
/// `iprx_` token) instead of a rendered static proxy config.
#[derive(Clone, Debug)]
pub struct IronControlSettings {
    /// Admin client used to register/deregister the per-sandbox proxy.
    pub client: IronControlClient,
    /// Base URL injected into the proxy pod as `IRON_CONTROL_URL`.
    pub control_url: String,
}

#[cfg(test)]
fn test_iron_control_settings() -> IronControlSettings {
    IronControlSettings {
        client: IronControlClient::new("http://127.0.0.1:1", "test-key"),
        control_url: "http://iron-control".to_owned(),
    }
}

impl AgentSandboxConfig {
    pub fn new(namespace: impl Into<String>, iron_control: IronControlSettings) -> Self {
        Self {
            namespace: namespace.into(),
            field_manager: "centaur-api-rs".to_owned(),
            container_name: DEFAULT_CONTAINER_NAME.to_owned(),
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
            image_pull_policy: None,
            image_pull_secrets: Vec::new(),
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            runtime_class_name: None,
            service_account_name: None,
            priority_class_name: None,
            state_volume: None,
            iron_proxy: None,
            iron_control,
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

    pub async fn try_default(
        namespace: impl Into<String>,
        iron_control: IronControlSettings,
    ) -> SandboxResult<Self> {
        let client = Client::try_default()
            .await
            .map_err(|err| SandboxError::backend_source("create kube client", err))?;
        Ok(Self::new(
            client,
            AgentSandboxConfig::new(namespace, iron_control),
        ))
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

    fn config_maps(&self) -> Api<ConfigMap> {
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

    /// Stop every retained sandbox whose immutable pod service account does
    /// not match the current backend configuration. This runs before api-rs
    /// enables session reuse or warm-pool claims, so a rollout cannot keep an
    /// old workload identity alive or assign it to a new session.
    pub async fn drain_service_account_mismatches(&self) -> SandboxResult<Vec<SandboxId>> {
        let params =
            ListParams::default().labels(&format!("{MANAGED_BY_LABEL}={MANAGED_BY_VALUE}"));
        let sandboxes = self.sandboxes().list(&params).await.map_err(|err| {
            map_kube_error("list sandboxes for service account reconciliation", err)
        })?;
        let mut stopped = Vec::new();

        for sandbox in sandboxes.items {
            if sandbox_service_account_matches(
                &sandbox,
                self.config.service_account_name.as_deref(),
            ) {
                continue;
            }
            let Some(name) = sandbox.metadata.name.as_deref() else {
                continue;
            };
            let id = SandboxId::new(name);
            tracing::warn!(
                sandbox_id = id.as_str(),
                configured_service_account = ?normalized_name(self.config.service_account_name.as_deref()),
                existing_service_account = ?normalized_name(
                    sandbox.spec.pod_template.spec.service_account_name.as_deref()
                ),
                "stopping sandbox whose service account does not match configuration"
            );
            SandboxBackend::stop(self, &id).await?;
            stopped.push(id);
        }

        Ok(stopped)
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
            .with_labels(sandbox.metadata.labels.clone().unwrap_or_default())
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

    async fn create_sandbox_files_config_map(
        &self,
        id: &SandboxId,
        spec: &SandboxSpec,
    ) -> SandboxResult<()> {
        let Some(config_map) = build_sandbox_files_config_map(id, spec)? else {
            return Ok(());
        };
        self.config_maps()
            .create(&PostParams::default(), &config_map)
            .await
            .map(|_| ())
            .map_err(|error| map_kube_error("create sandbox files config map", error))
    }

    async fn adopt_sandbox_files_config_map(
        &self,
        id: &SandboxId,
        sandbox: &crd::Sandbox,
    ) -> SandboxResult<()> {
        let Some(owner_reference) = sandbox_owner_reference(sandbox) else {
            return Ok(());
        };
        let patch = Patch::Merge(json!({
            "metadata": { "ownerReferences": [owner_reference] },
        }));
        match self
            .config_maps()
            .patch(
                &sandbox_files_config_map_name(id),
                &PatchParams::default(),
                &patch,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(map_kube_error("adopt sandbox files config map", error)),
        }
    }

    async fn delete_sandbox_files_config_map(&self, id: &SandboxId) -> SandboxResult<()> {
        match self
            .config_maps()
            .delete(&sandbox_files_config_map_name(id), &DeleteParams::default())
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(map_kube_error("delete sandbox files config map", error)),
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
        if let Err(error) = self.create_sandbox_files_config_map(&id, &spec).await {
            let _ = self.delete_iron_proxy_resources(&id).await;
            return Err(error);
        }
        let sandbox = match build_agent_sandbox(&id, &spec, &self.config) {
            Ok(sandbox) => sandbox,
            Err(error) => {
                let _ = self.delete_sandbox_files_config_map(&id).await;
                let _ = self.delete_iron_proxy_resources(&id).await;
                return Err(error);
            }
        };
        let created = match self
            .sandboxes()
            .create(&PostParams::default(), &sandbox)
            .await
        {
            Ok(created) => created,
            Err(err) => {
                let _ = self.delete_sandbox_files_config_map(&id).await;
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
        if let Err(error) = self.adopt_sandbox_files_config_map(&id, &created).await {
            tracing::warn!(
                sandbox_id = id.as_str(),
                %error,
                "failed to set ownerReference on sandbox files config map"
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
        let files_result = self.delete_sandbox_files_config_map(id).await;
        match self
            .sandboxes()
            .delete(id.as_str(), &DeleteParams::default())
            .await
        {
            Ok(_) => {
                proxy_result?;
                files_result?;
                self.delete_state_pvc(id).await
            }
            Err(err) if is_not_found(&err) => {
                proxy_result?;
                files_result?;
                self.delete_state_pvc(id).await
            }
            Err(err) => Err(map_kube_error("delete sandbox", err)),
        }
    }

    async fn assign_iron_control_proxy_principal(
        &self,
        id: &SandboxId,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.assign_proxy_principal(id, principal_id, requester_principal_id, labels)
            .await
    }

    async fn ensure_iron_control_proxy_resources(
        &self,
        id: &SandboxId,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.ensure_proxy_resources_for_principal(id, principal_id, requester_principal_id, labels)
            .await
    }

    async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        self.patch_sandbox_merge(id, sandbox_pause_patch(jiff::Timestamp::now()))
            .await?;
        // A paused sandbox has no agent pod, so its egress proxy is idle;
        // keeping it running holds a node pod slot per suspended sandbox for
        // the whole retention window (at ~500 pods per kubelet that starves
        // the cluster of schedulable slots). Delete the proxy resources here:
        // `resume()` unconditionally recreates them from the principal
        // recorded at create, and already handles proxies deleted out from
        // under a suspended sandbox.
        self.delete_iron_proxy_resources(id).await
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
        let sandbox = self.get_sandbox(id).await?;
        if let Some(sandbox) = &sandbox
            && let Err(error) = self.adopt_iron_proxy_resources(id, sandbox).await
        {
            tracing::warn!(
                sandbox_id = id.as_str(),
                %error,
                "failed to set ownerReferences on resumed iron-proxy resources"
            );
        }
        // A pod that was deleted out from under a `Suspended`/`Created`
        // sandbox (janitor, node pressure, manual reap) comes back through
        // this same resume path. Re-derive the capability labels from the
        // sandbox's own recorded env (the durable source of truth `resolve_
        // iron_proxy_for_resume` already reads for the same purpose) and
        // reassert them on both the Sandbox and its pod template, so the
        // recreated agent pod keeps the observability label the create path
        // applied.
        let capability_labels = sandbox
            .as_ref()
            .map(|sandbox| {
                sandbox_capability_labels(sandbox, &self.config.container_name, id.as_str())
            })
            .unwrap_or_default();
        self.patch_sandbox_merge(id, sandbox_resume_patch(&capability_labels))
            .await?;
        self.wait_until_running(id).await
    }
}

fn sandbox_pause_patch(paused_at: jiff::Timestamp) -> Value {
    json!({
        "spec": { "replicas": 0 },
        "metadata": { "annotations": { PAUSED_AT_ANNOTATION: paused_at.to_string() } },
    })
}

fn sandbox_resume_patch(capability_labels: &BTreeMap<&'static str, bool>) -> Value {
    // A JSON merge patch null removes a key, so a disabled capability clears
    // its label rather than writing "false" — matching `build_agent_sandbox`,
    // which only ever inserts these labels, never sets them false.
    let labels: Map<String, Value> = capability_labels
        .iter()
        .map(|(&label, &enabled)| {
            (
                label.to_owned(),
                if enabled { json!("true") } else { Value::Null },
            )
        })
        .collect();
    json!({
        "spec": {
            "replicas": 1,
            "podTemplate": { "metadata": { "labels": labels } },
        },
        "metadata": {
            "annotations": { PAUSED_AT_ANNOTATION: null },
            "labels": labels,
        },
    })
}

/// Re-derive the capability labels `build_agent_sandbox` would apply for this
/// sandbox's recorded capabilities, reading them back from the durable env
/// vars `apply_sandbox_capabilities` stamped on the container at create time.
/// Missing or invalid env values use the same fail-closed CR-label fallback as
/// `resolve_iron_proxy_for_resume`. Used to reassert the labels on resume,
/// since a pod recreated after external deletion (janitor, node pressure,
/// manual reap) only inherits whatever the Sandbox's `podTemplate` currently
/// carries.
fn sandbox_capability_labels(
    sandbox: &crd::Sandbox,
    container_name: &str,
    sandbox_id: &str,
) -> BTreeMap<&'static str, bool> {
    let mut labels = BTreeMap::new();
    labels.insert(
        OBSERVABILITY_ENABLED_LABEL,
        iron_proxy::resolve_resume_capability(
            iron_proxy::sandbox_observability_enabled(sandbox, container_name),
            sandbox.metadata.labels.as_ref(),
            OBSERVABILITY_ENABLED_LABEL,
            "observability",
            sandbox_id,
        ),
    );
    labels
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
    if !spec.files.is_empty() {
        volumes.push(json!({
            "name": SANDBOX_FILES_VOLUME,
            "configMap": {
                "name": sandbox_files_config_map_name(id),
                "defaultMode": 0o444,
            },
        }));
        for (index, file) in spec.files.iter().enumerate() {
            validate_sandbox_file_target_path(&file.target_path)?;
            volume_mounts.push(json!({
                "name": SANDBOX_FILES_VOLUME,
                "mountPath": file.target_path,
                "subPath": sandbox_file_key(index),
                "readOnly": true,
            }));
        }
    }
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
    // Node steering — passed through to the CRD podTemplate fields. Chart
    // values alone cannot reach these pods; api-rs creates them at runtime.
    insert_optional(
        &mut pod_spec,
        "nodeSelector",
        (!config.node_selector.is_empty()).then(|| config.node_selector.clone()),
    );
    insert_optional(
        &mut pod_spec,
        "tolerations",
        (!config.tolerations.is_empty()).then(|| config.tolerations.clone()),
    );
    insert_optional(
        &mut pod_spec,
        "runtimeClassName",
        config
            .runtime_class_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
    );
    insert_optional(
        &mut pod_spec,
        "serviceAccountName",
        config
            .service_account_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
    );
    insert_optional(
        &mut pod_spec,
        "priorityClassName",
        config
            .priority_class_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
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
    if let Some(requester) = &spec.iron_control_requester_principal {
        annotations.insert(
            IRON_CONTROL_REQUESTER_ANNOTATION.to_owned(),
            requester.clone(),
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

fn sandbox_files_config_map_name(id: &SandboxId) -> String {
    format!("{}-files", id.as_str())
}

fn sandbox_file_key(index: usize) -> String {
    format!("file-{index}")
}

fn validate_sandbox_file_target_path(path: &str) -> SandboxResult<()> {
    let path = std::path::Path::new(path);
    if path.as_os_str().is_empty()
        || !path.is_absolute()
        || path.file_name().is_none()
        || path.components().any(|component| {
            !matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return Err(SandboxError::InvalidSpec(format!(
            "invalid sandbox file target path {path:?}"
        )));
    }
    Ok(())
}

fn build_sandbox_files_config_map(
    id: &SandboxId,
    spec: &SandboxSpec,
) -> SandboxResult<Option<ConfigMap>> {
    if spec.files.is_empty() {
        return Ok(None);
    }
    let mut paths = BTreeSet::new();
    let mut data = BTreeMap::new();
    for (index, file) in spec.files.iter().enumerate() {
        validate_sandbox_file_target_path(&file.target_path)?;
        if !paths.insert(file.target_path.as_str()) {
            return Err(SandboxError::InvalidSpec(format!(
                "duplicate sandbox file target path {:?}",
                file.target_path
            )));
        }
        data.insert(sandbox_file_key(index), file.contents.clone());
    }
    Ok(Some(ConfigMap {
        metadata: ObjectMeta {
            name: Some(sandbox_files_config_map_name(id)),
            labels: Some(BTreeMap::from([
                (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
                (SANDBOX_ID_LABEL.to_owned(), id.as_str().to_owned()),
            ])),
            ..ObjectMeta::default()
        },
        data: Some(data),
        immutable: Some(true),
        ..ConfigMap::default()
    }))
}

fn sandbox_owner_reference(sandbox: &crd::Sandbox) -> Option<Value> {
    let name = sandbox.metadata.name.as_ref()?;
    let uid = sandbox.metadata.uid.as_ref()?;
    Some(json!({
        "apiVersion": crd::Sandbox::api_version(&()),
        "kind": crd::Sandbox::kind(&()),
        "name": name,
        "uid": uid,
    }))
}

fn resources_json(spec: &SandboxSpec) -> Option<Value> {
    let resources = spec.resources.as_ref()?;
    (!resources.is_empty()).then(|| json!(resources))
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

fn normalized_name(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn sandbox_service_account_matches(
    sandbox: &crd::Sandbox,
    configured_service_account: Option<&str>,
) -> bool {
    normalized_name(
        sandbox
            .spec
            .pod_template
            .spec
            .service_account_name
            .as_deref(),
    ) == normalized_name(configured_service_account)
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
    use centaur_sandbox_core::{
        RepoCacheAccess, ResourceRequirements, SandboxCapabilities, SandboxSpec,
    };
    use k8s_openapi::api::core::v1::{PodCondition, PodStatus};
    use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

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
                ResourceRequirements::new()
                    .request("cpu", "250m")
                    .request("memory", "256Mi")
                    .request("ephemeral-storage", "1Gi")
                    .limit("cpu", "500m")
                    .limit("memory", "512Mi")
                    .limit("example.com/gpu", "1"),
            );
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings())
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
        let resources = container.resources.as_ref().unwrap();
        let quantity = |value: &str| IntOrString::String(value.to_owned());
        assert_eq!(
            resources.requests.as_ref().unwrap().get("cpu"),
            Some(&quantity("250m"))
        );
        assert_eq!(
            resources.requests.as_ref().unwrap().get("memory"),
            Some(&quantity("256Mi"))
        );
        assert_eq!(
            resources
                .requests
                .as_ref()
                .unwrap()
                .get("ephemeral-storage"),
            Some(&quantity("1Gi"))
        );
        assert_eq!(
            resources.limits.as_ref().unwrap().get("cpu"),
            Some(&quantity("500m"))
        );
        assert_eq!(
            resources.limits.as_ref().unwrap().get("memory"),
            Some(&quantity("512Mi"))
        );
        assert_eq!(
            resources.limits.as_ref().unwrap().get("example.com/gpu"),
            Some(&quantity("1"))
        );
    }

    #[test]
    fn mounts_large_sandbox_files_without_putting_contents_in_env() {
        let prompt = "p".repeat(256 * 1024);
        let spec = SandboxSpec::new("centaur-agent:latest")
            .file("/home/agent/AGENTS_PERSONA.md", prompt.clone())
            .file("/tmp/runtime-config", "runtime config");
        let id = SandboxId::new("asbx-test");
        let config_map = build_sandbox_files_config_map(&id, &spec)
            .unwrap()
            .expect("sandbox files config map");

        assert_eq!(
            config_map.data.as_ref().and_then(|data| data.get("file-0")),
            Some(&prompt)
        );
        assert_eq!(
            config_map
                .data
                .as_ref()
                .and_then(|data| data.get("file-1"))
                .map(String::as_str),
            Some("runtime config")
        );

        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        let sandbox = build_agent_sandbox(&id, &spec, &config).unwrap();
        let sandbox = serde_json::to_value(sandbox).unwrap();
        let container = &sandbox["spec"]["podTemplate"]["spec"]["containers"][0];
        assert!(
            container["env"]
                .as_array()
                .is_none_or(|env| { env.iter().all(|entry| entry["value"] != prompt) })
        );
        assert!(
            container["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mount| {
                    mount["name"] == SANDBOX_FILES_VOLUME
                        && mount["mountPath"] == "/home/agent/AGENTS_PERSONA.md"
                        && mount["subPath"] == "file-0"
                        && mount["readOnly"] == true
                })
        );
        assert!(
            container["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|mount| {
                    mount["name"] == SANDBOX_FILES_VOLUME
                        && mount["mountPath"] == "/tmp/runtime-config"
                        && mount["subPath"] == "file-1"
                        && mount["readOnly"] == true
                })
        );
    }

    #[test]
    fn renders_partial_sandbox_resources() {
        let spec = SandboxSpec::new("centaur-agent:latest").resources(
            ResourceRequirements::new()
                .request("memory", "4Gi")
                .limit("memory", "4Gi"),
        );
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        let resources = sandbox.spec.pod_template.spec.containers[0]
            .resources
            .as_ref()
            .unwrap();
        let memory = IntOrString::String("4Gi".to_owned());
        assert_eq!(
            resources.requests.as_ref().unwrap().get("memory"),
            Some(&memory)
        );
        assert_eq!(
            resources.limits.as_ref().unwrap().get("memory"),
            Some(&memory)
        );
        assert!(!resources.requests.as_ref().unwrap().contains_key("cpu"));
        assert!(!resources.limits.as_ref().unwrap().contains_key("cpu"));
    }

    #[test]
    fn omits_resources_when_unset() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert!(
            sandbox.spec.pod_template.spec.containers[0]
                .resources
                .is_none()
        );
    }

    #[test]
    fn node_steering_reaches_the_sandbox_pod_template() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let mut config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        config.node_selector =
            BTreeMap::from([("workload".to_owned(), "centaur-sandbox".to_owned())]);
        config.tolerations = vec![Toleration {
            key: Some("example.com/sandbox".to_owned()),
            operator: Some("Exists".to_owned()),
            effect: Some("NoSchedule".to_owned()),
            ..Default::default()
        }];
        config.runtime_class_name = Some("gvisor".to_owned());
        config.priority_class_name = Some("centaur-sandbox".to_owned());

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let pod_spec = &sandbox.spec.pod_template.spec;

        assert_eq!(
            pod_spec
                .node_selector
                .as_ref()
                .and_then(|selector| selector.get("workload"))
                .map(String::as_str),
            Some("centaur-sandbox")
        );
        let tolerations = pod_spec
            .tolerations
            .as_ref()
            .expect("tolerations should be set");
        assert_eq!(tolerations.len(), 1);
        assert_eq!(tolerations[0].key.as_deref(), Some("example.com/sandbox"));
        assert_eq!(tolerations[0].effect.as_deref(), Some("NoSchedule"));
        assert_eq!(pod_spec.runtime_class_name.as_deref(), Some("gvisor"));
        assert_eq!(
            pod_spec.priority_class_name.as_deref(),
            Some("centaur-sandbox")
        );
    }

    #[test]
    fn node_steering_is_omitted_when_unset() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let pod_spec = &sandbox.spec.pod_template.spec;

        assert!(pod_spec.node_selector.is_none());
        assert!(pod_spec.tolerations.is_none());
        assert!(pod_spec.runtime_class_name.is_none());
        assert!(pod_spec.service_account_name.is_none());
        assert!(pod_spec.priority_class_name.is_none());
    }

    #[test]
    fn service_account_name_reaches_the_sandbox_pod_template() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let mut config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        config.service_account_name = Some("centaur-sandbox".to_owned());

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let pod_spec = &sandbox.spec.pod_template.spec;

        assert_eq!(
            pod_spec.service_account_name.as_deref(),
            Some("centaur-sandbox")
        );
        // The Kubernetes API token stays unmounted even with an account set.
        assert_eq!(pod_spec.automount_service_account_token, Some(false));
    }

    #[test]
    fn blank_service_account_name_is_omitted() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let mut config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        config.service_account_name = Some("  ".to_owned());

        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert!(
            sandbox
                .spec
                .pod_template
                .spec
                .service_account_name
                .is_none()
        );
    }

    #[test]
    fn service_account_reconciliation_detects_identity_changes() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let mut config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        config.service_account_name = Some("old-sandbox".to_owned());
        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert!(sandbox_service_account_matches(
            &sandbox,
            Some("old-sandbox")
        ));
        assert!(!sandbox_service_account_matches(
            &sandbox,
            Some("new-sandbox")
        ));
        assert!(!sandbox_service_account_matches(&sandbox, None));
    }

    #[test]
    fn service_account_reconciliation_treats_blank_as_unset() {
        let spec = SandboxSpec::new("centaur-agent:latest");
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        assert!(sandbox_service_account_matches(&sandbox, None));
        assert!(sandbox_service_account_matches(&sandbox, Some("  ")));
    }

    #[test]
    fn stamps_requester_annotation_only_when_spec_carries_one() {
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());

        let mut spec = SandboxSpec::new("centaur-agent:latest").iron_control_principal("prn_conv");
        spec.iron_control_requester_principal = Some("prn_req".to_owned());
        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        let annotations = sandbox.metadata.annotations.as_ref().unwrap();
        assert_eq!(
            annotations
                .get(IRON_CONTROL_PRINCIPAL_ANNOTATION)
                .map(String::as_str),
            Some("prn_conv")
        );
        assert_eq!(
            annotations
                .get(IRON_CONTROL_REQUESTER_ANNOTATION)
                .map(String::as_str),
            Some("prn_req")
        );

        let spec = SandboxSpec::new("centaur-agent:latest").iron_control_principal("prn_conv");
        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();
        assert!(
            sandbox.metadata.annotations.as_ref().is_none_or(
                |annotations| !annotations.contains_key(IRON_CONTROL_REQUESTER_ANNOTATION)
            )
        );
    }

    #[test]
    fn labels_observability_enabled_sandboxes_for_chart_policy() {
        let spec = SandboxSpec::new("centaur-agent:latest").capabilities(SandboxCapabilities {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: true,
        });
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());

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
    }

    #[test]
    fn omits_observability_label_for_restricted_sandboxes() {
        let spec = SandboxSpec::new("centaur-agent:latest").capabilities(SandboxCapabilities {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: false,
        });
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());

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
    }

    /// A pod deleted out from under a sandbox (janitor, node pressure, manual
    /// reap) comes back through `resume`, which only has the sandbox id, not
    /// the original `SandboxSpec`. Regression test for the recreated agent
    /// pod losing `centaur.ai/observability-enabled`: the resume patch must
    /// restore the label (derived from the sandbox's own recorded capability
    /// env, the same durable source `resolve_iron_proxy_for_resume` already trusts)
    /// on the Sandbox and its pod template, matching what `build_agent_sandbox`
    /// would have applied for these capabilities.
    #[test]
    fn resume_reasserts_capability_labels_from_recorded_env() {
        // Mirrors what `apply_sandbox_capabilities` (centaur-session-runtime)
        // stamps onto the spec env alongside `.capabilities(..)`, since that's
        // the durable record `sandbox_capability_labels` reads back on resume.
        let spec = SandboxSpec::new("centaur-agent:latest")
            .capabilities(SandboxCapabilities {
                repo_cache: RepoCacheAccess::All,
                observability_enabled: true,
            })
            .env("CENTAUR_SANDBOX_OBSERVABILITY_ENABLED", "true");
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        let mut sandbox =
            build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        // Simulate the observed production bug: the recreated pod's template
        // lost the capability labels even though the container's capability
        // env (the create path's durable record) is untouched.
        if let Some(labels) = sandbox
            .spec
            .pod_template
            .metadata
            .as_mut()
            .and_then(|metadata| metadata.labels.as_mut())
        {
            labels.remove(OBSERVABILITY_ENABLED_LABEL);
        }
        if let Some(labels) = sandbox.metadata.labels.as_mut() {
            labels.remove(OBSERVABILITY_ENABLED_LABEL);
        }

        let labels = sandbox_capability_labels(&sandbox, DEFAULT_CONTAINER_NAME, "asbx-test");
        assert_eq!(labels.get(OBSERVABILITY_ENABLED_LABEL), Some(&true));

        let patch = sandbox_resume_patch(&labels);
        assert_eq!(
            patch["metadata"]["labels"][OBSERVABILITY_ENABLED_LABEL],
            json!("true")
        );
        assert_eq!(
            patch["spec"]["podTemplate"]["metadata"]["labels"][OBSERVABILITY_ENABLED_LABEL],
            json!("true")
        );
    }

    #[test]
    fn resume_patch_clears_labels_for_restricted_capabilities() {
        // Exercise the fail-closed fallback for callers that set the backend
        // capabilities without duplicating them into the container env.
        let spec = SandboxSpec::new("centaur-agent:latest").capabilities(SandboxCapabilities {
            repo_cache: RepoCacheAccess::All,
            observability_enabled: false,
        });
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings());
        let sandbox = build_agent_sandbox(&SandboxId::new("asbx-test"), &spec, &config).unwrap();

        let labels = sandbox_capability_labels(&sandbox, DEFAULT_CONTAINER_NAME, "asbx-test");
        assert_eq!(labels.get(OBSERVABILITY_ENABLED_LABEL), Some(&false));

        // A JSON merge patch null removes the key rather than writing
        // "false", matching how `build_agent_sandbox` omits (not falsifies)
        // the label for a disabled capability.
        let patch = sandbox_resume_patch(&labels);
        assert!(patch["metadata"]["labels"][OBSERVABILITY_ENABLED_LABEL].is_null());
        assert!(
            patch["spec"]["podTemplate"]["metadata"]["labels"][OBSERVABILITY_ENABLED_LABEL]
                .is_null()
        );
    }

    #[test]
    fn tools_clone_rides_iron_proxy_when_enabled() {
        // apply_proxy_env runs before build_agent_sandbox in create(), so the
        // resolved per-sandbox proxy URL arrives on the spec env.
        let spec = SandboxSpec::new("centaur-agent:latest")
            .env("HTTPS_PROXY", "http://asbx-test-iron-proxy:8080");
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings())
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
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings())
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
        });
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "api:test");
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings()).tools(tools);

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
        let config = AgentSandboxConfig::new("centaur", test_iron_control_settings())
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
