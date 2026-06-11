use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use centaur_iron_proxy::{ProxyFragment, SourceKind, SourcePolicy, pg_sandbox_dsns};
use centaur_sandbox_core::{SandboxError, SandboxId, SandboxResult, SandboxSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, ContainerPort, EmptyDirVolumeSource, EnvFromSource,
    EnvVar as K8sEnvVar, HTTPGetAction, Pod, PodSpec, Probe, SecretEnvSource, SecretVolumeSource,
    SecurityContext, Service, ServicePort, ServiceSpec, Volume, VolumeMount,
};
use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPeer,
    NetworkPolicyPort, NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::Api;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use crate::{
    AgentSandboxBackend, MANAGED_BY_LABEL, MANAGED_BY_VALUE, OtlpEgressTarget, SANDBOX_ID_LABEL,
    is_not_found, map_kube_error,
};

const IRON_PROXY_LABEL: &str = "centaur.ai/iron-proxy";
const IRON_CONTROL_PROXY_ID_ANNOTATION: &str = "centaur.ai/iron-control-proxy-id";
const FIREWALL_CA_MOUNT_PATH: &str = "/firewall-certs";
pub(crate) const FIREWALL_CA_CERT_PATH: &str = "/firewall-certs/ca-cert.pem";
const PROXY_MANAGEMENT_PORT: u16 = 9092;
const PROXY_HEALTH_PORT: u16 = 9090;
// Managed-mode proxies carry no rendered config; these local listen/TLS
// settings (everything the control plane does not own) are passed as IRON_*
// env vars instead. The CA paths match where the entrypoint copies the
// mounted CA secret.
const PROXY_TUNNEL_PORT: u16 = 8080;
const PROXY_DNS_LISTEN: &str = ":53";
const PROXY_DNS_PROXY_IP: &str = "127.0.0.1";
const PROXY_TLS_MODE: &str = "mitm";
const PROXY_TLS_CA_CERT_PATH: &str = "/etc/iron-proxy/ca.crt";
const PROXY_TLS_CA_KEY_PATH: &str = "/etc/iron-proxy/ca.key";
const PROXY_LOG_LEVEL: &str = "info";
// iron-control multiplexes every Postgres upstream through a single listener,
// routing by database name; the control plane owns each upstream DSN/role/
// database. api-rs binds one local port (matching the chart's pgPort) and one
// shared client credential (random per sandbox) the sandbox presents on every
// DSN. These are the deploy-level env vars iron-proxy reads for that listener.
const PG_LISTENER_PORT: u16 = 5432;
const PG_LISTEN_ENV: &str = "IRON_PROXY_PG_LISTEN";
const PG_CLIENT_USER_ENV: &str = "IRON_PROXY_PG_CLIENT_USER";
const PG_CLIENT_PASSWORD_ENV: &str = "IRON_PROXY_PG_CLIENT_PASSWORD";
// Managed iron-proxy instances pick up principal/config changes on their next
// /proxy/sync poll. Claiming a warm sandbox must not return before the proxy
// has had a chance to stop serving the bootstrap principal's empty config.
const PROXY_REASSIGN_SYNC_DELAY: Duration = Duration::from_secs(6);

#[derive(Clone, Debug)]
pub struct IronProxyConfig {
    pub image: String,
    pub image_pull_policy: Option<String>,
    pub fragments: Vec<ProxyFragment>,
    pub source_policy: SourcePolicy,
    pub ca_cert_secret_name: String,
    pub ca_key_secret_name: String,
    pub env_from_secret_names: Vec<String>,
    pub extra_env: BTreeMap<String, String>,
    pub op_connect_app_name: String,
    pub op_connect_port: u16,
    pub api_pod_labels: BTreeMap<String, String>,
}

impl IronProxyConfig {
    pub fn new(
        image: impl Into<String>,
        ca_cert_secret_name: impl Into<String>,
        ca_key_secret_name: impl Into<String>,
    ) -> Self {
        Self {
            image: image.into(),
            image_pull_policy: None,
            fragments: Vec::new(),
            source_policy: SourcePolicy::default(),
            ca_cert_secret_name: ca_cert_secret_name.into(),
            ca_key_secret_name: ca_key_secret_name.into(),
            env_from_secret_names: Vec::new(),
            extra_env: BTreeMap::new(),
            op_connect_app_name: "onepassword-connect".to_owned(),
            op_connect_port: 8080,
            api_pod_labels: BTreeMap::from([(
                "app.kubernetes.io/component".to_owned(),
                "api".to_owned(),
            )]),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedIronProxy {
    proxy_host: String,
    proxy_pod_name: String,
    proxy_port: u16,
    // iron-control principal OID this sandbox's proxy binds to.
    principal_id: String,
    // The single Postgres listener the proxy multiplexes all upstreams through,
    // derived from the principal's effective config. `None` when the principal
    // resolves to no Postgres upstreams. The upstream DSN/role/database are
    // control-plane-owned; api-rs assigns the local listen/client knobs
    // (IRON_PROXY_PG_* + the per-upstream sandbox DSN env vars).
    pg: Option<ResolvedPg>,
    // Replace-secret placeholders the operator granted the principal
    // (`proxy_value` -> same), set as sandbox env so tools send the value the
    // proxy swaps. Infra placeholders are set separately from the known set.
    replace_placeholders: BTreeMap<String, String>,
}

/// The single Postgres listener the proxy multiplexes every upstream through.
/// iron-control owns each upstream DSN/role/database and routes by database
/// name; api-rs only assigns the local listen port and the shared client
/// credential the sandbox presents (random per sandbox).
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedPg {
    /// Local listen address the proxy binds (e.g. ``0.0.0.0:5432``).
    listen: String,
    /// Listen port, exposed on the proxy Service and allowed sandbox→proxy.
    port: u16,
    /// Shared client user the sandbox connects as (random per sandbox).
    user: String,
    /// Shared client password (random per sandbox); set on both the proxy
    /// (CLIENT_PASSWORD) and every sandbox DSN so the two agree.
    password: String,
    /// One DSN per upstream, all pointing at this listener and differing only
    /// by database.
    dsns: Vec<ResolvedPgDsn>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedPgDsn {
    /// Sandbox env var that receives the proxied DSN.
    sandbox_env_name: String,
    /// Database the sandbox connects to; iron-proxy routes the listener on it.
    database: String,
}

/// Env injected into a managed proxy pod so iron-proxy pulls its config from
/// iron-control instead of any local file.
struct ProxySyncEnv {
    proxy_id: String,
    control_url: String,
    token: String,
}

impl AgentSandboxBackend {
    pub(crate) async fn resolve_iron_proxy(
        &self,
        id: &SandboxId,
        spec: &SandboxSpec,
    ) -> SandboxResult<Option<ResolvedIronProxy>> {
        if self.config.iron_proxy.is_none() {
            return Ok(None);
        }
        // iron-control is the only mode: the proxy pulls its entire effective
        // config from iron-control over `/proxy/sync`, so no config is rendered
        // locally — the remaining local settings are passed as IRON_* env vars
        // on the pod. The sandbox must carry the principal its proxy binds to.
        if self.config.iron_control.is_none() {
            return Err(SandboxError::InvalidSpec(
                "iron-proxy requires iron-control to be configured".to_owned(),
            ));
        }
        let principal_id = spec.iron_control_principal.clone().ok_or_else(|| {
            SandboxError::InvalidSpec(
                "iron-proxy sandbox spec is missing its iron-control principal".to_owned(),
            )
        })?;
        let pg = self.resolved_pg_from_fragments();
        let replace_placeholders = self.effective_replace_placeholders(&principal_id).await?;

        Ok(Some(ResolvedIronProxy {
            proxy_host: iron_proxy_service_name(id),
            proxy_pod_name: new_iron_proxy_pod_name(id),
            proxy_port: PROXY_TUNNEL_PORT,
            principal_id,
            pg,
            replace_placeholders,
        }))
    }

    /// Read the principal's effective config from iron-control for the
    /// replace-secret placeholders set as sandbox env (so tools send the value
    /// the proxy swaps for the real secret). The Postgres DSN catalog is
    /// derived statically from fragments instead — see
    /// [`Self::resolved_pg_from_fragments`].
    async fn effective_replace_placeholders(
        &self,
        principal: &str,
    ) -> SandboxResult<BTreeMap<String, String>> {
        let Some(iron_control) = self.config.iron_control.as_ref() else {
            return Ok(BTreeMap::new());
        };
        let effective = iron_control
            .client
            .effective_config(&iron_control.namespace, principal)
            .await
            .map_err(|err| SandboxError::backend_source("iron-control effective_config", err))?;

        Ok(effective
            .secrets
            .iter()
            .filter_map(|secret| secret.replace.as_ref())
            .map(|replace| replace.proxy_value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.contains('='))
            .map(|value| (value.clone(), value))
            .collect())
    }

    /// Build the single Postgres listener from the static fragment catalog
    /// (every tool's declared `pg_dsn` env var name + database). Unlike the
    /// per-principal `effective_config` path this needs no principal, so warm
    /// sandboxes — created under the roleless bootstrap principal — are still
    /// born with the full DSN set and a live listener; the reassignable proxy
    /// enforces per-principal access at runtime by routing only the claimed
    /// principal's granted databases. iron-proxy multiplexes every upstream
    /// through one listener, so the DSNs differ only by database; api-rs binds a
    /// single local port and a shared client credential (random per sandbox,
    /// set on both the proxy CLIENT_PASSWORD and every sandbox DSN). Returns
    /// `None` when no fragment declares a `pg_dsn`.
    fn resolved_pg_from_fragments(&self) -> Option<ResolvedPg> {
        let iron_proxy = self.config.iron_proxy.as_ref()?;
        let dsns: Vec<ResolvedPgDsn> = pg_sandbox_dsns(&iron_proxy.fragments)
            .into_iter()
            .map(|(sandbox_env_name, database)| ResolvedPgDsn {
                sandbox_env_name,
                database,
            })
            .collect();
        (!dsns.is_empty()).then(|| ResolvedPg {
            listen: format!("0.0.0.0:{PG_LISTENER_PORT}"),
            port: PG_LISTENER_PORT,
            user: format!("pg-user-{}", uuid::Uuid::new_v4().simple()),
            password: format!("pg-{}", uuid::Uuid::new_v4().simple()),
            dsns,
        })
    }

    /// Resolve the proxy for a resume, where only the sandbox id is known.
    /// Rebinds to the principal stamped on the sandbox at create (read back off
    /// its annotation, so it survives pause and api-rs restarts). Returns `None`
    /// when the sandbox has no proxy or carries no principal annotation.
    pub(crate) async fn resolve_iron_proxy_for_resume(
        &self,
        id: &SandboxId,
    ) -> SandboxResult<Option<ResolvedIronProxy>> {
        if self.config.iron_proxy.is_none() {
            return Ok(None);
        }
        let sandbox = self
            .sandboxes()
            .get(id.as_str())
            .await
            .map_err(|err| map_kube_error("get sandbox for resume", err))?;
        let principal_id = sandbox
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(crate::IRON_CONTROL_PRINCIPAL_ANNOTATION))
            .cloned();
        let Some(principal_id) = principal_id else {
            return Ok(None);
        };
        let pg = self.resolved_pg_from_fragments();
        let replace_placeholders = self.effective_replace_placeholders(&principal_id).await?;
        Ok(Some(ResolvedIronProxy {
            proxy_host: iron_proxy_service_name(id),
            proxy_pod_name: new_iron_proxy_pod_name(id),
            proxy_port: PROXY_TUNNEL_PORT,
            principal_id,
            pg,
            replace_placeholders,
        }))
    }

    pub(crate) async fn create_iron_proxy_resources(
        &self,
        id: &SandboxId,
        resolved: Option<&ResolvedIronProxy>,
    ) -> SandboxResult<()> {
        let (Some(resolved), Some(iron_proxy)) = (resolved, self.config.iron_proxy.as_ref()) else {
            return Ok(());
        };
        self.delete_iron_proxy_resources(id).await?;
        let sync = self.register_sync_proxy(id, resolved).await?;
        self.services()
            .create(
                &PostParams::default(),
                &build_iron_proxy_service(id, resolved),
            )
            .await
            .map_err(|err| map_kube_error("create iron-proxy service", err))?;
        let control_port = url_port(&sync.control_url).unwrap_or(443);
        for policy in build_iron_proxy_network_policies(
            id,
            resolved,
            iron_proxy,
            control_port,
            self.config.otlp_egress.as_ref(),
        ) {
            self.network_policies()
                .create(&PostParams::default(), &policy)
                .await
                .map_err(|err| map_kube_error("create iron-proxy network policy", err))?;
        }
        self.pods()
            .create(
                &PostParams::default(),
                &build_iron_proxy_pod(id, iron_proxy, resolved, &sync),
            )
            .await
            .map_err(|err| map_kube_error("create iron-proxy pod", err))?;
        self.wait_until_proxy_running(resolved).await
    }

    /// Register a per-sandbox proxy in iron-control and return the env (URL +
    /// `iprx_` token) to inject. The proxy OID is recorded so it can be
    /// deregistered on stop.
    async fn register_sync_proxy(
        &self,
        id: &SandboxId,
        resolved: &ResolvedIronProxy,
    ) -> SandboxResult<ProxySyncEnv> {
        let iron_control = self.config.iron_control.as_ref().ok_or_else(|| {
            SandboxError::backend("iron-proxy requires iron-control to be configured")
        })?;
        let proxy = iron_control
            .client
            .create_proxy(id.as_str(), &resolved.principal_id)
            .await
            .map_err(|err| SandboxError::backend_source("iron-control create proxy", err))?;
        let token = proxy
            .token
            .ok_or_else(|| SandboxError::backend("iron-control create proxy returned no token"))?;
        self.proxy_ids
            .lock()
            .await
            .insert(id.as_str().to_owned(), proxy.id.clone());
        Ok(ProxySyncEnv {
            proxy_id: proxy.id,
            control_url: iron_control.control_url.clone(),
            token,
        })
    }

    pub(crate) async fn delete_iron_proxy_resources(&self, id: &SandboxId) -> SandboxResult<()> {
        if self.config.iron_proxy.is_none() {
            return Ok(());
        }
        // Deregister the iron-control proxy first (best-effort): once the pod is
        // gone the token is useless, and a stale proxy row just fails to sync.
        if let Some(iron_control) = self.config.iron_control.as_ref()
            && let Some(proxy_id) = self.proxy_ids.lock().await.remove(id.as_str())
        {
            let _ = iron_control.client.delete_proxy(&proxy_id).await;
        }
        let _ = self.delete_iron_proxy_pods_for_sandbox(id).await;
        let _ = self
            .services()
            .delete(&iron_proxy_service_name(id), &DeleteParams::default())
            .await;
        for name in [
            iron_proxy_sandbox_egress_policy_name(id),
            iron_proxy_policy_name(id),
        ] {
            let _ = self
                .network_policies()
                .delete(&name, &DeleteParams::default())
                .await;
        }
        Ok(())
    }

    pub(crate) async fn assign_proxy_principal(
        &self,
        id: &SandboxId,
        principal_id: &str,
    ) -> SandboxResult<()> {
        let iron_control = self
            .config
            .iron_control
            .as_ref()
            .ok_or(SandboxError::Unsupported {
                backend: crate::BACKEND_NAME,
                operation: "assign_iron_control_proxy_principal",
            })?;
        let proxy_id = self.proxy_id_for_sandbox(id).await?.ok_or_else(|| {
            SandboxError::backend(format!(
                "iron-control proxy id for sandbox {} was not found",
                id.as_str()
            ))
        })?;
        let proxy = iron_control
            .client
            .assign_proxy_principal(&proxy_id, principal_id)
            .await
            .map_err(|err| SandboxError::backend_source("iron-control assign proxy", err))?;
        self.proxy_ids
            .lock()
            .await
            .insert(id.as_str().to_owned(), proxy.id);
        self.patch_iron_control_principal_annotation(id, principal_id)
            .await?;
        sleep(PROXY_REASSIGN_SYNC_DELAY).await;
        Ok(())
    }

    async fn proxy_id_for_sandbox(&self, id: &SandboxId) -> SandboxResult<Option<String>> {
        if let Some(proxy_id) = self.proxy_ids.lock().await.get(id.as_str()).cloned() {
            return Ok(Some(proxy_id));
        }
        let params = ListParams::default().labels(&format!(
            "{IRON_PROXY_LABEL}=true,{SANDBOX_ID_LABEL}={}",
            id.as_str()
        ));
        let pods = self
            .pods()
            .list(&params)
            .await
            .map_err(|err| map_kube_error("list iron-proxy pods", err))?;
        for pod in pods.items {
            if let Some(proxy_id) = pod
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(IRON_CONTROL_PROXY_ID_ANNOTATION))
                .filter(|value| !value.trim().is_empty())
            {
                let proxy_id = proxy_id.to_owned();
                self.proxy_ids
                    .lock()
                    .await
                    .insert(id.as_str().to_owned(), proxy_id.clone());
                return Ok(Some(proxy_id));
            }
        }
        Ok(None)
    }

    async fn patch_iron_control_principal_annotation(
        &self,
        id: &SandboxId,
        principal_id: &str,
    ) -> SandboxResult<()> {
        let patch = Patch::Merge(json!({
            "metadata": {
                "annotations": {
                    crate::IRON_CONTROL_PRINCIPAL_ANNOTATION: principal_id,
                },
            },
        }));
        self.sandboxes()
            .patch(id.as_str(), &PatchParams::default(), &patch)
            .await
            .map(|_| ())
            .map_err(|err| map_kube_error("patch sandbox iron-control principal", err))
    }

    fn services(&self) -> Api<Service> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    fn network_policies(&self) -> Api<NetworkPolicy> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    async fn delete_iron_proxy_pods_for_sandbox(&self, id: &SandboxId) -> SandboxResult<()> {
        let params = ListParams::default().labels(&format!(
            "{IRON_PROXY_LABEL}=true,{SANDBOX_ID_LABEL}={}",
            id.as_str()
        ));
        let pods = self
            .pods()
            .list(&params)
            .await
            .map_err(|err| map_kube_error("list iron-proxy pods", err))?;
        for pod in pods.items {
            if let Some(name) = pod.metadata.name {
                let _ = self.pods().delete(&name, &DeleteParams::default()).await;
            }
        }
        Ok(())
    }

    async fn wait_until_proxy_running(&self, resolved: &ResolvedIronProxy) -> SandboxResult<()> {
        let deadline = Instant::now() + self.config.ready_timeout;
        loop {
            match self.pods().get(&resolved.proxy_pod_name).await {
                Ok(pod) if pod_running(&pod) => return Ok(()),
                Ok(pod) if pod_stopped(&pod) => {
                    return Err(SandboxError::NotReady(format!(
                        "iron-proxy pod {} reached terminal state before running",
                        resolved.proxy_pod_name
                    )));
                }
                Ok(pod) if Instant::now() >= deadline => {
                    return Err(SandboxError::NotReady(format!(
                        "iron-proxy pod {} did not become running before timeout; latest phase: {:?}",
                        resolved.proxy_pod_name,
                        pod.status.and_then(|status| status.phase)
                    )));
                }
                Ok(_) => sleep(Duration::from_millis(500)).await,
                Err(err) if is_not_found(&err) && Instant::now() < deadline => {
                    sleep(Duration::from_millis(500)).await;
                }
                Err(err) if is_not_found(&err) => {
                    return Err(SandboxError::NotReady(format!(
                        "iron-proxy pod {} was not created before timeout",
                        resolved.proxy_pod_name
                    )));
                }
                Err(err) => return Err(map_kube_error("wait iron-proxy pod", err)),
            }
        }
    }
}

pub(crate) fn apply_proxy_env(spec: &mut SandboxSpec, resolved: &ResolvedIronProxy) {
    let mut no_proxy_extra = current_env_values(spec, ["NO_PROXY", "no_proxy"]);
    // The harness exports OTLP traces (usage/cost spans) straight to the
    // collector; routing them through iron-proxy fails (plain-HTTP forwards
    // are rejected), so the endpoint host always bypasses the proxy.
    no_proxy_extra.extend(otlp_endpoint_hosts(spec));
    let api_host = env_value(spec, "CENTAUR_API_URL").and_then(host_from_url);
    for (name, value) in proxy_env(
        &resolved.proxy_host,
        resolved.proxy_port,
        api_host.as_deref(),
        &no_proxy_extra,
    ) {
        set_env(spec, &name, &value);
    }
    // Operator-granted replace placeholders: the sandbox sends the proxy_value
    // and iron-proxy swaps in the real secret. set_missing so infra placeholders
    // (already on the spec from the known set) win.
    for (name, value) in &resolved.replace_placeholders {
        set_missing_env(spec, name, value);
    }
    // Each Postgres upstream reaches the sandbox as a DSN env var. They all
    // point at the proxy's single listener with the same shared credential and
    // differ only by database; iron-proxy routes on the database and fronts the
    // real upstream.
    if let Some(pg) = &resolved.pg {
        for dsn in &pg.dsns {
            let value = format!(
                "postgresql://{}:{}@{}:{}/{}",
                pg.user, pg.password, resolved.proxy_host, pg.port, dsn.database,
            );
            set_missing_env(spec, &dsn.sandbox_env_name, &value);
        }
    }
}

pub(crate) fn sandbox_ca_volume_mount_json() -> Value {
    json!({
        "name": "firewall-ca",
        "mountPath": FIREWALL_CA_MOUNT_PATH,
        "readOnly": true,
    })
}

pub(crate) fn sandbox_ca_volume_json(iron_proxy: &IronProxyConfig) -> Value {
    json!({
        "name": "firewall-ca",
        "secret": {"secretName": iron_proxy.ca_cert_secret_name},
    })
}

fn build_iron_proxy_pod(
    id: &SandboxId,
    iron_proxy: &IronProxyConfig,
    resolved: &ResolvedIronProxy,
    sync: &ProxySyncEnv,
) -> Pod {
    let annotations = BTreeMap::from([
        (
            IRON_CONTROL_PROXY_ID_ANNOTATION.to_owned(),
            sync.proxy_id.clone(),
        ),
        (
            crate::IRON_CONTROL_PRINCIPAL_ANNOTATION.to_owned(),
            resolved.principal_id.clone(),
        ),
    ]);
    Pod {
        metadata: object_meta_with_annotations(
            resolved.proxy_pod_name.clone(),
            iron_proxy_labels(id),
            annotations,
        ),
        spec: Some(PodSpec {
            automount_service_account_token: Some(false),
            restart_policy: Some("Never".to_owned()),
            containers: vec![iron_proxy_container(iron_proxy, resolved, sync)],
            volumes: Some(iron_proxy_volumes(iron_proxy)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn iron_proxy_container(
    iron_proxy: &IronProxyConfig,
    resolved: &ResolvedIronProxy,
    sync: &ProxySyncEnv,
) -> Container {
    Container {
        name: "iron-proxy".to_owned(),
        image: Some(iron_proxy.image.clone()),
        image_pull_policy: iron_proxy.image_pull_policy.clone(),
        env: Some(iron_proxy_env_vars(iron_proxy, resolved, sync)),
        env_from: iron_proxy_env_from(iron_proxy),
        ports: Some(container_ports(resolved)),
        readiness_probe: Some(health_probe(Some(5), Some(30))),
        liveness_probe: Some(health_probe(None, None)),
        security_context: Some(SecurityContext {
            allow_privilege_escalation: Some(false),
            capabilities: Some(Capabilities {
                drop: Some(vec!["ALL".to_owned()]),
                ..Default::default()
            }),
            seccomp_profile: Some(k8s_openapi::api::core::v1::SeccompProfile {
                type_: "RuntimeDefault".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        volume_mounts: Some(vec![
            // Writable config dir for the entrypoint's CA copy; no proxy.yaml
            // is rendered in managed mode.
            volume_mount("iron-proxy-config", "/etc/iron-proxy", false),
            volume_mount("iron-proxy-certs", "/certs", false),
            volume_mount("iron-proxy-ca", "/etc/iron-proxy-ca", true),
        ]),
        // Use the image entrypoint directly: it loads the CA and, with
        // IRON_CONTROL_PLANE_URL set, runs iron-proxy with no local config.
        ..Default::default()
    }
}

fn iron_proxy_env_vars(
    iron_proxy: &IronProxyConfig,
    resolved: &ResolvedIronProxy,
    sync: &ProxySyncEnv,
) -> Vec<K8sEnvVar> {
    let mut env = BTreeMap::new();
    env.insert(
        "IRON_MANAGEMENT_API_KEY".to_owned(),
        env_var("IRON_MANAGEMENT_API_KEY", "unused-local-sidecar-key"),
    );
    // iron-proxy pulls its effective config (allowlist, secrets, management)
    // from iron-control using this token; no local config file is rendered.
    // The binary reads the control-plane base URL from IRON_CONTROL_PLANE_URL
    // (distinct from api-rs's own IRON_CONTROL_URL admin-client var); a wrong
    // name makes it fall back to its built-in default endpoint.
    env.insert(
        "IRON_CONTROL_PLANE_URL".to_owned(),
        env_var("IRON_CONTROL_PLANE_URL", &sync.control_url),
    );
    env.insert(
        "IRON_PROXY_TOKEN".to_owned(),
        env_var("IRON_PROXY_TOKEN", &sync.token),
    );
    // The local listen/TLS settings the control plane does not own, passed as
    // env instead of a config file. CA paths match the entrypoint's CA copy.
    for (name, value) in [
        ("IRON_PROXY_TUNNEL_LISTEN", format!(":{PROXY_TUNNEL_PORT}")),
        ("IRON_DNS_LISTEN", PROXY_DNS_LISTEN.to_owned()),
        ("IRON_DNS_PROXY_IP", PROXY_DNS_PROXY_IP.to_owned()),
        ("IRON_TLS_MODE", PROXY_TLS_MODE.to_owned()),
        ("IRON_TLS_CA_CERT", PROXY_TLS_CA_CERT_PATH.to_owned()),
        ("IRON_TLS_CA_KEY", PROXY_TLS_CA_KEY_PATH.to_owned()),
        ("IRON_LOG_LEVEL", PROXY_LOG_LEVEL.to_owned()),
    ] {
        env.insert(name.to_owned(), env_var(name, &value));
    }
    for (name, value) in &iron_proxy.extra_env {
        env.insert(name.clone(), env_var(name, value));
    }
    // Single-listener Postgres local config. The control plane owns every
    // upstream DSN + role (the pg_dsn secrets) and multiplexes them through this
    // one listener; api-rs only supplies the bind address and the shared client
    // credential the sandbox presents.
    if let Some(pg) = &resolved.pg {
        for (name, value) in [
            (PG_LISTEN_ENV, pg.listen.as_str()),
            (PG_CLIENT_USER_ENV, pg.user.as_str()),
            (PG_CLIENT_PASSWORD_ENV, pg.password.as_str()),
        ] {
            env.insert(name.to_owned(), env_var(name, value));
        }
    }
    env.into_values().collect()
}

fn iron_proxy_env_from(iron_proxy: &IronProxyConfig) -> Option<Vec<EnvFromSource>> {
    (!iron_proxy.env_from_secret_names.is_empty()).then(|| {
        iron_proxy
            .env_from_secret_names
            .iter()
            .map(|name| EnvFromSource {
                secret_ref: Some(SecretEnvSource {
                    name: name.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect()
    })
}

fn iron_proxy_volumes(iron_proxy: &IronProxyConfig) -> Vec<Volume> {
    vec![
        empty_dir_volume("iron-proxy-config"),
        empty_dir_volume("iron-proxy-certs"),
        Volume {
            name: "iron-proxy-ca".to_owned(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(iron_proxy.ca_key_secret_name.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    ]
}

fn build_iron_proxy_service(id: &SandboxId, resolved: &ResolvedIronProxy) -> Service {
    let mut ports = vec![service_port("proxy", resolved.proxy_port)];
    if let Some(pg) = &resolved.pg {
        ports.push(service_port("pg", pg.port));
    }
    Service {
        metadata: object_meta(iron_proxy_service_name(id), iron_proxy_labels(id)),
        spec: Some(ServiceSpec {
            selector: Some(iron_proxy_labels(id)),
            ports: Some(ports),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_iron_proxy_network_policies(
    id: &SandboxId,
    resolved: &ResolvedIronProxy,
    iron_proxy: &IronProxyConfig,
    control_port: u16,
    otlp_egress: Option<&OtlpEgressTarget>,
) -> Vec<NetworkPolicy> {
    let sandbox_to_proxy_ports = sandbox_to_proxy_ports(resolved);
    let mut sandbox_egress = vec![
        egress_to(
            vec![pod_peer(iron_proxy_labels(id))],
            sandbox_to_proxy_ports.clone(),
        ),
        egress_to(
            vec![pod_peer(iron_proxy.api_pod_labels.clone())],
            vec![network_port(8000), network_port(8080)],
        ),
        dns_egress_rule(),
    ];
    if let Some(target) = otlp_egress {
        // Direct harness OTLP export (codex usage/cost spans). The collector
        // lives outside this namespace, so the sandbox bypasses iron-proxy for
        // this one destination (the endpoint host also rides NO_PROXY).
        sandbox_egress.push(egress_to(
            vec![namespace_peer(&target.namespace)],
            vec![network_port(target.port)],
        ));
    }
    vec![
        NetworkPolicy {
            metadata: object_meta(
                iron_proxy_sandbox_egress_policy_name(id),
                sandbox_labels(id),
            ),
            spec: Some(NetworkPolicySpec {
                pod_selector: Some(label_selector(sandbox_labels(id))),
                policy_types: Some(vec!["Egress".to_owned()]),
                egress: Some(sandbox_egress),
                ..Default::default()
            }),
        },
        NetworkPolicy {
            metadata: object_meta(iron_proxy_policy_name(id), iron_proxy_labels(id)),
            spec: Some(NetworkPolicySpec {
                pod_selector: Some(label_selector(iron_proxy_labels(id))),
                policy_types: Some(vec!["Ingress".to_owned(), "Egress".to_owned()]),
                ingress: Some(vec![NetworkPolicyIngressRule {
                    from: Some(vec![pod_peer(sandbox_labels(id))]),
                    ports: Some(sandbox_to_proxy_ports),
                }]),
                egress: Some(proxy_egress_rules(iron_proxy, control_port)),
            }),
        },
    ]
}

fn sandbox_to_proxy_ports(resolved: &ResolvedIronProxy) -> Vec<NetworkPolicyPort> {
    std::iter::once(network_port(resolved.proxy_port))
        .chain(resolved.pg.as_ref().map(|pg| network_port(pg.port)))
        .collect()
}

fn proxy_egress_rules(
    iron_proxy: &IronProxyConfig,
    control_port: u16,
) -> Vec<NetworkPolicyEgressRule> {
    // Upstream egress: 443/5432 for normal traffic, plus the iron-control port
    // (deduped) so a sync-mode proxy can reach the control plane.
    let mut upstream_ports = vec![network_port(443), network_port(5432)];
    if control_port != 443 && control_port != 5432 {
        upstream_ports.push(network_port(control_port));
    }
    let mut rules = vec![
        dns_egress_rule(),
        egress_to(
            vec![pod_peer(iron_proxy.api_pod_labels.clone())],
            vec![network_port(8000), network_port(8080)],
        ),
        NetworkPolicyEgressRule {
            ports: Some(upstream_ports),
            ..Default::default()
        },
    ];
    if matches!(
        iron_proxy.source_policy.kind,
        SourceKind::OnePasswordConnect
    ) {
        rules.push(egress_to(
            vec![pod_peer(BTreeMap::from([(
                "app".to_owned(),
                iron_proxy.op_connect_app_name.clone(),
            )]))],
            vec![network_port(iron_proxy.op_connect_port)],
        ));
    }
    rules
}

fn dns_egress_rule() -> NetworkPolicyEgressRule {
    egress_to(
        vec![namespace_peer("kube-system")],
        vec![udp_port(53), network_port(53)],
    )
}

fn namespace_peer(namespace: &str) -> NetworkPolicyPeer {
    NetworkPolicyPeer {
        namespace_selector: Some(label_selector(BTreeMap::from([(
            "kubernetes.io/metadata.name".to_owned(),
            namespace.to_owned(),
        )]))),
        ..Default::default()
    }
}

fn proxy_env(
    proxy_host: &str,
    proxy_port: u16,
    api_host: Option<&str>,
    no_proxy_extra: &[String],
) -> BTreeMap<String, String> {
    let proxy_url = format!("http://{proxy_host}:{proxy_port}");
    let no_proxy = no_proxy_value(proxy_host, api_host, no_proxy_extra);
    BTreeMap::from([
        ("FIREWALL_HOST".to_owned(), proxy_host.to_owned()),
        ("FIREWALL_PROXY_PORT".to_owned(), proxy_port.to_string()),
        ("HTTP_PROXY".to_owned(), proxy_url.clone()),
        ("HTTPS_PROXY".to_owned(), proxy_url.clone()),
        ("http_proxy".to_owned(), proxy_url.clone()),
        ("https_proxy".to_owned(), proxy_url),
        ("NO_PROXY".to_owned(), no_proxy.clone()),
        ("no_proxy".to_owned(), no_proxy),
        (
            "NODE_EXTRA_CA_CERTS".to_owned(),
            FIREWALL_CA_CERT_PATH.to_owned(),
        ),
        (
            "REQUESTS_CA_BUNDLE".to_owned(),
            FIREWALL_CA_CERT_PATH.to_owned(),
        ),
        (
            "CURL_CA_BUNDLE".to_owned(),
            FIREWALL_CA_CERT_PATH.to_owned(),
        ),
        ("SSL_CERT_FILE".to_owned(), FIREWALL_CA_CERT_PATH.to_owned()),
        (
            "GIT_SSL_CAINFO".to_owned(),
            FIREWALL_CA_CERT_PATH.to_owned(),
        ),
    ])
}

fn no_proxy_value(proxy_host: &str, api_host: Option<&str>, extra_values: &[String]) -> String {
    let mut hosts = BTreeSet::<String>::from([
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
        proxy_host.to_owned(),
        "api".to_owned(),
        "victoriametrics".to_owned(),
        "victorialogs".to_owned(),
    ]);
    if let Some(api_host) = api_host.filter(|value| !value.is_empty()) {
        hosts.insert(api_host.to_owned());
    }
    for value in extra_values {
        hosts.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    hosts.into_iter().collect::<Vec<_>>().join(",")
}

fn set_missing_env(spec: &mut SandboxSpec, name: &str, value: &str) {
    if env_value(spec, name).is_none() {
        set_env(spec, name, value);
    }
}

fn set_env(spec: &mut SandboxSpec, name: &str, value: &str) {
    if let Some(env) = spec.env.iter_mut().find(|env| env.name == name) {
        env.value = value.to_owned();
    } else {
        spec.env
            .push(centaur_sandbox_core::EnvVar::new(name, value));
    }
}

fn env_value(spec: &SandboxSpec, name: &str) -> Option<String> {
    spec.env
        .iter()
        .find(|env| env.name == name)
        .map(|env| env.value.clone())
}

fn current_env_values<const N: usize>(spec: &SandboxSpec, names: [&str; N]) -> Vec<String> {
    names
        .into_iter()
        .filter_map(|name| env_value(spec, name))
        .collect()
}

/// Hosts of the spec's OTLP exporter endpoints, mirrored into NO_PROXY (same
/// contract as the Python control plane's `_sandbox_otel_endpoint_hosts`).
fn otlp_endpoint_hosts(spec: &SandboxSpec) -> Vec<String> {
    [
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ]
    .into_iter()
    .filter_map(|name| env_value(spec, name))
    .filter_map(host_from_url)
    .collect()
}

/// The authority (`[user@]host[:port]`) of a URL or bare `host:port`, with any
/// scheme and path stripped and surrounding whitespace trimmed.
fn authority(value: &str) -> Option<&str> {
    let without_scheme = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value);
    let authority = without_scheme.split('/').next()?.trim();
    (!authority.is_empty()).then_some(authority)
}

fn host_from_url(value: String) -> Option<String> {
    let authority = authority(&value)?;
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host_port)| host_port)
        .unwrap_or(authority);
    let host = host_port
        .split_once(':')
        .map_or(host_port, |(host, _)| host);
    (!host.is_empty()).then(|| host.to_owned())
}

fn url_port(value: &str) -> Option<u16> {
    authority(value)?.rsplit_once(':')?.1.parse().ok()
}

fn pod_running(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.phase.as_deref())
        .is_some_and(|phase| phase.eq_ignore_ascii_case("running"))
        && pod
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|condition| condition.type_ == "Ready" && condition.status == "True")
            })
}

fn pod_stopped(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.phase.as_deref())
        .is_some_and(|phase| {
            phase.eq_ignore_ascii_case("succeeded") || phase.eq_ignore_ascii_case("failed")
        })
}

fn object_meta(name: impl Into<String>, labels: BTreeMap<String, String>) -> ObjectMeta {
    object_meta_with_annotations(name, labels, BTreeMap::new())
}

fn object_meta_with_annotations(
    name: impl Into<String>,
    labels: BTreeMap<String, String>,
    annotations: BTreeMap<String, String>,
) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.into()),
        labels: Some(labels),
        annotations: (!annotations.is_empty()).then_some(annotations),
        ..Default::default()
    }
}

fn env_var(name: &str, value: &str) -> K8sEnvVar {
    K8sEnvVar {
        name: name.to_owned(),
        value: Some(value.to_owned()),
        ..Default::default()
    }
}

fn container_port(name: impl Into<String>, port: u16) -> ContainerPort {
    ContainerPort {
        name: Some(name.into()),
        container_port: i32::from(port),
        ..Default::default()
    }
}

fn service_port(name: impl Into<String>, port: u16) -> ServicePort {
    let port = i32::from(port);
    ServicePort {
        name: Some(name.into()),
        port,
        target_port: Some(IntOrString::Int(port)),
        protocol: Some("TCP".to_owned()),
        ..Default::default()
    }
}

fn network_port(port: u16) -> NetworkPolicyPort {
    policy_port("TCP", port)
}

fn udp_port(port: u16) -> NetworkPolicyPort {
    policy_port("UDP", port)
}

fn policy_port(protocol: &str, port: u16) -> NetworkPolicyPort {
    NetworkPolicyPort {
        port: Some(IntOrString::Int(i32::from(port))),
        protocol: Some(protocol.to_owned()),
        ..Default::default()
    }
}

fn label_selector(match_labels: BTreeMap<String, String>) -> LabelSelector {
    LabelSelector {
        match_labels: Some(match_labels),
        ..Default::default()
    }
}

fn pod_peer(match_labels: BTreeMap<String, String>) -> NetworkPolicyPeer {
    NetworkPolicyPeer {
        pod_selector: Some(label_selector(match_labels)),
        ..Default::default()
    }
}

fn egress_to(to: Vec<NetworkPolicyPeer>, ports: Vec<NetworkPolicyPort>) -> NetworkPolicyEgressRule {
    NetworkPolicyEgressRule {
        to: Some(to),
        ports: Some(ports),
    }
}

fn health_probe(period_seconds: Option<i32>, failure_threshold: Option<i32>) -> Probe {
    Probe {
        http_get: Some(HTTPGetAction {
            path: Some("/healthz".to_owned()),
            port: IntOrString::Int(i32::from(PROXY_HEALTH_PORT)),
            ..Default::default()
        }),
        period_seconds,
        failure_threshold,
        ..Default::default()
    }
}

fn volume_mount(name: &str, mount_path: &str, read_only: bool) -> VolumeMount {
    VolumeMount {
        name: name.to_owned(),
        mount_path: mount_path.to_owned(),
        read_only: read_only.then_some(true),
        ..Default::default()
    }
}

fn empty_dir_volume(name: &str) -> Volume {
    Volume {
        name: name.to_owned(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    }
}

fn container_ports(resolved: &ResolvedIronProxy) -> Vec<ContainerPort> {
    let mut ports = vec![
        container_port("proxy", resolved.proxy_port),
        container_port("management", PROXY_MANAGEMENT_PORT),
        container_port("health", PROXY_HEALTH_PORT),
    ];
    if let Some(pg) = &resolved.pg {
        ports.push(container_port("pg", pg.port));
    }
    ports
}

fn iron_proxy_service_name(id: &SandboxId) -> String {
    format!("{}-proxy", id.as_str())
}

fn new_iron_proxy_pod_name(id: &SandboxId) -> String {
    format!("{}-proxy-{}", id.as_str(), unique_suffix())
}

fn iron_proxy_sandbox_egress_policy_name(id: &SandboxId) -> String {
    format!("{}-sandbox-egress", id.as_str())
}

fn iron_proxy_policy_name(id: &SandboxId) -> String {
    format!("{}-proxy-net", id.as_str())
}

fn sandbox_labels(id: &SandboxId) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
        (SANDBOX_ID_LABEL.to_owned(), id.as_str().to_owned()),
    ])
}

fn iron_proxy_labels(id: &SandboxId) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
        (SANDBOX_ID_LABEL.to_owned(), id.as_str().to_owned()),
        (IRON_PROXY_LABEL.to_owned(), "true".to_owned()),
    ])
}

fn unique_suffix() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{millis}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved() -> ResolvedIronProxy {
        ResolvedIronProxy {
            proxy_host: "asbx-test-iron-proxy".to_owned(),
            proxy_pod_name: "asbx-test-iron-proxy-1".to_owned(),
            proxy_port: 8080,
            principal_id: "principal".to_owned(),
            pg: None,
            replace_placeholders: BTreeMap::new(),
        }
    }

    fn rule_allows_namespace_port(
        rule: &NetworkPolicyEgressRule,
        namespace: &str,
        port: u16,
    ) -> bool {
        rule.to.as_ref().is_some_and(|peers| {
            peers.iter().any(|peer| {
                peer.namespace_selector.as_ref().is_some_and(|selector| {
                    selector.match_labels.as_ref().is_some_and(|labels| {
                        labels
                            .get("kubernetes.io/metadata.name")
                            .map(String::as_str)
                            == Some(namespace)
                    })
                })
            })
        }) && rule.ports.as_ref().is_some_and(|ports| {
            ports
                .iter()
                .any(|policy_port| policy_port.port == Some(IntOrString::Int(i32::from(port))))
        })
    }

    #[test]
    fn sandbox_egress_policy_allows_otlp_collector_when_configured() {
        let id = SandboxId::new("asbx-test");
        let iron_proxy = IronProxyConfig::new("proxy:test", "ca-cert", "ca-key");
        let target = OtlpEgressTarget {
            namespace: "laminar".to_owned(),
            port: 8000,
        };

        let policies =
            build_iron_proxy_network_policies(&id, &resolved(), &iron_proxy, 3000, Some(&target));
        let sandbox_egress = policies[0]
            .spec
            .as_ref()
            .unwrap()
            .egress
            .as_ref()
            .unwrap()
            .clone();
        assert!(
            sandbox_egress
                .iter()
                .any(|rule| rule_allows_namespace_port(rule, "laminar", 8000))
        );

        let policies = build_iron_proxy_network_policies(&id, &resolved(), &iron_proxy, 3000, None);
        let sandbox_egress = policies[0]
            .spec
            .as_ref()
            .unwrap()
            .egress
            .as_ref()
            .unwrap()
            .clone();
        assert!(
            !sandbox_egress
                .iter()
                .any(|rule| rule_allows_namespace_port(rule, "laminar", 8000))
        );
    }

    #[test]
    fn apply_proxy_env_adds_otlp_endpoint_host_to_no_proxy() {
        let mut spec = SandboxSpec::new("centaur-agent:latest").env(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces",
        );

        apply_proxy_env(&mut spec, &resolved());

        for name in ["NO_PROXY", "no_proxy"] {
            let value = spec
                .env
                .iter()
                .find(|env| env.name == name)
                .map(|env| env.value.clone())
                .unwrap();
            assert!(
                value
                    .split(',')
                    .any(|host| host == "laminar-app-server.laminar.svc.cluster.local"),
                "{name} should contain the OTLP endpoint host: {value}"
            );
        }
    }
}
