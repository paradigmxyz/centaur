use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use centaur_iron_control::IronControlClient;
use centaur_sandbox_core::ResourceRequirements;
use k8s_openapi::{
    ByteString,
    api::core::v1::{
        Capabilities, Container, ContainerPort, EmptyDirVolumeSource, EnvFromSource, EnvVar,
        EnvVarSource, HTTPGetAction, KeyToPath, LocalObjectReference, Pod, PodSecurityContext,
        PodSpec, Probe, ProjectedVolumeSource, SeccompProfile, Secret, SecretEnvSource,
        SecretKeySelector, SecretProjection, SecurityContext, Toleration, Volume, VolumeMount,
        VolumeProjection,
    },
    apimachinery::pkg::{
        api::resource::Quantity, apis::meta::v1::ObjectMeta, util::intstr::IntOrString,
    },
};
use kube::{
    Api, Client,
    api::{DeleteParams, ListParams, Patch, PatchParams, PostParams},
};
use serde_json::json;
use tokio::time::{Instant, sleep};
use tracing::{info, warn};

use crate::{
    model::{HandBox, HandSize, pod_name_for_hand, secret_name_for_hand},
    store::{HandsError, HandsResult},
};

const HAND_LABEL: &str = "centaur.ai/workload";
const HAND_LABEL_VALUE: &str = "hand";
const HAND_ID_LABEL: &str = "centaur.ai/hand-id";
const HAND_SIZE_LABEL: &str = "centaur.ai/hand-size";
const MANAGED_BY_LABEL: &str = "centaur.ai/managed-by";
const MANAGED_BY_VALUE: &str = "api-rs";
const PROXY_ID_ANNOTATION: &str = "centaur.ai/iron-control-proxy-id";

const HAND_PORT: u16 = 7070;
const HAND_HEALTH_PORT: u16 = 7071;
const PROXY_TUNNEL_PORT: u16 = 8080;
const PROXY_MANAGEMENT_PORT: u16 = 9092;
const PROXY_HEALTH_PORT: u16 = 9090;
const PROXY_TUNNEL_LISTEN: &str = ":8080";
const PROXY_DNS_LISTEN: &str = ":53";
const PROXY_DNS_PROXY_IP: &str = "127.0.0.1";
const PROXY_TLS_MODE: &str = "mitm";
const PROXY_TLS_CA_CERT_PATH: &str = "/etc/iron-proxy/ca.crt";
const PROXY_TLS_CA_KEY_PATH: &str = "/etc/iron-proxy/ca.key";
const PROXY_UPSTREAM_RESPONSE_HEADER_TIMEOUT: &str = "120s";
const PROXY_LOG_LEVEL: &str = "info";
const PROXY_UPSTREAM_DENY_CIDRS_ENV: &str = "IRON_PROXY_UPSTREAM_DENY_CIDRS";
const HAND_TOKEN_FILE: &str = "/var/run/centaur-hand/token";

const PROXY_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const PROXY_ACK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PROXY_ACK_PROBE_WINDOW: Duration = Duration::from_secs(2);

/// The result of creating a hand's Kubernetes objects and iron-control proxy.
#[derive(Clone, Debug)]
pub struct HandResources {
    pub pod_name: String,
    pub pod_ip: Option<String>,
    pub iron_proxy_id: String,
}

/// What the pool can see of a hand's pod right now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandObservation {
    Pending,
    Ready { pod_ip: String },
    Gone,
}

/// The Kubernetes and iron-control operations the pool needs. Fakes in tests
/// implement this instead of talking to a cluster or the Console database.
#[async_trait]
pub trait HandInfra: Send + Sync {
    async fn create_hand(&self, id: &str, size: HandSize) -> HandsResult<HandResources>;
    async fn observe_hand(&self, id: &str, pod_name: &str) -> HandsResult<HandObservation>;
    /// Assign the lease principal to the hand's proxy and wait until the proxy
    /// reports that principal's configuration applied.
    async fn bind_principal(
        &self,
        hand: &HandBox,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<()>;
    /// The replace-secret placeholders the principal's tools send; never a
    /// real secret.
    async fn placeholders(&self, principal: &str) -> HandsResult<BTreeMap<String, String>>;
    async fn read_token(&self, id: &str) -> HandsResult<String>;
    async fn delete_hand(&self, id: &str, proxy_id: Option<&str>) -> HandsResult<()>;
    /// Delete hand pods and Secrets that no row owns, once they are older than
    /// `grace`. Returns how many pods were deleted.
    async fn sweep_orphans(&self, live: &BTreeSet<String>, grace: Duration) -> HandsResult<u32>;
}

/// Configuration for the Kubernetes hand driver.
#[derive(Clone, Debug)]
pub struct HandK8sConfig {
    pub namespace: String,
    pub image: String,
    pub image_pull_policy: Option<String>,
    pub image_pull_secrets: Vec<String>,
    pub iron_proxy_image: String,
    pub iron_proxy_image_pull_policy: Option<String>,
    pub iron_proxy_resources: Option<ResourceRequirements>,
    pub iron_proxy_env_from_secret_names: Vec<String>,
    pub iron_proxy_extra_env: BTreeMap<String, String>,
    pub iron_proxy_upstream_deny_cidrs: Vec<String>,
    pub ca_cert_secret_name: String,
    pub ca_cert_secret_key: String,
    pub ca_key_secret_name: String,
    pub ca_key_secret_key: String,
    /// proxy-sync base URL the sidecar pulls its configuration from.
    pub control_url: String,
    /// Principal new, unleased hands register their proxy under.
    pub bootstrap_principal: String,
    pub resources_default: Option<ResourceRequirements>,
    pub resources_large: Option<ResourceRequirements>,
    pub node_selector: BTreeMap<String, String>,
    pub tolerations: Vec<Toleration>,
    pub runtime_class_name: Option<String>,
    pub priority_class_name: Option<String>,
    pub pod_annotations: BTreeMap<String, String>,
}

/// Kubernetes-backed hand lifecycle and iron-control proxy binding.
pub struct KubernetesHandInfra {
    client: Client,
    iron_control: IronControlClient,
    config: HandK8sConfig,
}

impl KubernetesHandInfra {
    #[must_use]
    pub fn new(client: Client, iron_control: IronControlClient, config: HandK8sConfig) -> Self {
        Self {
            client,
            iron_control,
            config,
        }
    }

    fn pods(&self) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    fn secrets(&self) -> Api<Secret> {
        Api::namespaced(self.client.clone(), &self.config.namespace)
    }

    fn labels(id: &str, size: HandSize) -> BTreeMap<String, String> {
        BTreeMap::from([
            (MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned()),
            (HAND_LABEL.to_owned(), HAND_LABEL_VALUE.to_owned()),
            (HAND_ID_LABEL.to_owned(), id.to_owned()),
            (HAND_SIZE_LABEL.to_owned(), size.as_str().to_owned()),
        ])
    }
}

#[async_trait]
impl HandInfra for KubernetesHandInfra {
    async fn create_hand(&self, id: &str, size: HandSize) -> HandsResult<HandResources> {
        let pod_name = pod_name_for_hand(id);
        let secret_name = secret_name_for_hand(id);
        let labels = Self::labels(id, size);

        let proxy = self
            .iron_control
            .create_proxy(id, &self.config.bootstrap_principal, None, labels.clone())
            .await
            .map_err(|error| HandsError::Backend(format!("register hand proxy: {error}")))?;
        let proxy_token = proxy.token.clone().ok_or_else(|| {
            HandsError::Backend("iron-control create proxy returned no token".to_owned())
        })?;
        let management_api_key = format!("mgmt-{}", uuid::Uuid::new_v4().simple());
        let hand_token = format!("hnd_{}", uuid::Uuid::new_v4().simple());

        let mut data = BTreeMap::from([
            (
                "token".to_owned(),
                ByteString(hand_token.as_bytes().to_vec()),
            ),
            (
                "proxy-token".to_owned(),
                ByteString(proxy_token.as_bytes().to_vec()),
            ),
            (
                "management-api-key".to_owned(),
                ByteString(management_api_key.as_bytes().to_vec()),
            ),
        ]);
        // Keep the Secret opaque in logs even when a field is added later.
        data.retain(|_, value| !value.0.is_empty());

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(secret_name.clone()),
                labels: Some(labels.clone()),
                ..Default::default()
            },
            data: Some(data),
            type_: Some("Opaque".to_owned()),
            ..Default::default()
        };
        if let Err(error) = self.secrets().create(&PostParams::default(), &secret).await {
            let _ = self.iron_control.delete_proxy(&proxy.id).await;
            return Err(backend("create hand Secret", error));
        }

        let pod = self.build_pod(id, size, &pod_name, &secret_name, &labels, &proxy.id);
        let created = match self.pods().create(&PostParams::default(), &pod).await {
            Ok(pod) => pod,
            Err(error) => {
                let _ = self
                    .secrets()
                    .delete(&secret_name, &DeleteParams::default())
                    .await;
                let _ = self.iron_control.delete_proxy(&proxy.id).await;
                return Err(backend("create hand pod", error));
            }
        };
        if let Some(uid) = created.metadata.uid.as_deref() {
            self.adopt_secret(&secret_name, &pod_name, uid).await;
        }

        Ok(HandResources {
            pod_name,
            pod_ip: created.status.and_then(|status| status.pod_ip),
            iron_proxy_id: proxy.id,
        })
    }

    async fn observe_hand(&self, _id: &str, pod_name: &str) -> HandsResult<HandObservation> {
        let pod = match self.pods().get(pod_name).await {
            Ok(pod) => pod,
            Err(error) if is_not_found(&error) => return Ok(HandObservation::Gone),
            Err(error) => return Err(backend("get hand pod", error)),
        };
        if pod.metadata.deletion_timestamp.is_some() {
            return Ok(HandObservation::Gone);
        }
        let Some(status) = pod.status else {
            return Ok(HandObservation::Pending);
        };
        if status.phase.as_deref().is_some_and(|phase| {
            phase.eq_ignore_ascii_case("failed") || phase.eq_ignore_ascii_case("succeeded")
        }) {
            return Ok(HandObservation::Gone);
        }
        let ready = status.conditions.as_ref().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        });
        let pod_ip = status.pod_ip.filter(|ip| !ip.trim().is_empty());
        match (ready, pod_ip) {
            (true, Some(pod_ip)) => Ok(HandObservation::Ready { pod_ip }),
            _ => Ok(HandObservation::Pending),
        }
    }

    async fn bind_principal(
        &self,
        hand: &HandBox,
        principal: &str,
        requester: Option<&str>,
    ) -> HandsResult<()> {
        let proxy_id = hand
            .iron_proxy_id
            .as_deref()
            .ok_or_else(|| HandsError::Backend("hand has no iron-proxy".to_owned()))?;
        let labels = Self::labels(&hand.id, hand.size());
        self.iron_control
            .assign_proxy_principal(proxy_id, principal, requester, &labels)
            .await
            .map_err(|error| {
                HandsError::Backend(format!("assign hand proxy principal: {error}"))
            })?;
        self.wait_for_proxy_principal_applied(&hand.id, &hand.pod_name, principal)
            .await
    }

    async fn placeholders(&self, principal: &str) -> HandsResult<BTreeMap<String, String>> {
        let effective = self
            .iron_control
            .effective_config(principal)
            .await
            .map_err(|error| HandsError::Backend(format!("read principal config: {error}")))?;
        Ok(effective
            .secrets
            .iter()
            .filter_map(|secret| secret.replace.as_ref())
            .map(|replace| replace.proxy_value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.contains('='))
            .map(|value| (value.clone(), value))
            .collect())
    }

    async fn read_token(&self, id: &str) -> HandsResult<String> {
        let secret = self
            .secrets()
            .get(&secret_name_for_hand(id))
            .await
            .map_err(|error| backend("read hand Secret", error))?;
        if let Some(token) = secret
            .data
            .as_ref()
            .and_then(|data| data.get("token"))
            .map(|token| String::from_utf8_lossy(&token.0).into_owned())
            .filter(|token| !token.is_empty())
        {
            return Ok(token);
        }
        secret
            .string_data
            .and_then(|data| data.get("token").cloned())
            .filter(|token| !token.is_empty())
            .ok_or_else(|| HandsError::Backend("hand Secret has no token".to_owned()))
    }

    async fn delete_hand(&self, id: &str, proxy_id: Option<&str>) -> HandsResult<()> {
        if let Some(proxy_id) = proxy_id
            && let Err(error) = self.iron_control.delete_proxy(proxy_id).await
        {
            warn!(hand_id = id, %error, "failed to deregister hand proxy");
        }
        let pod_name = pod_name_for_hand(id);
        let mut failures = Vec::new();
        match self
            .pods()
            .delete(&pod_name, &DeleteParams::default())
            .await
        {
            Ok(_) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => failures.push(format!(
                "pod {pod_name}: {}",
                backend("delete hand pod", error)
            )),
        }
        let secret_name = secret_name_for_hand(id);
        match self
            .secrets()
            .delete(&secret_name, &DeleteParams::default())
            .await
        {
            Ok(_) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => failures.push(format!(
                "secret {secret_name}: {}",
                backend("delete hand Secret", error)
            )),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HandsError::Backend(failures.join("; ")))
        }
    }

    async fn sweep_orphans(&self, live: &BTreeSet<String>, grace: Duration) -> HandsResult<u32> {
        let now = SystemTime::now();
        let selector = format!("{HAND_LABEL}={HAND_LABEL_VALUE}");
        let pods = self
            .pods()
            .list(&ListParams::default().labels(&selector))
            .await
            .map_err(|error| backend("list hand pods", error))?;
        let mut deleted = 0u32;
        for pod in pods.items {
            let Some(id) = pod
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(HAND_ID_LABEL))
                .cloned()
            else {
                continue;
            };
            if live.contains(&id) || !old_enough(&pod.metadata, now, grace) {
                continue;
            }
            let Some(pod_name) = pod.metadata.name.clone() else {
                continue;
            };
            let proxy_id = pod
                .metadata
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(PROXY_ID_ANNOTATION))
                .cloned();
            if let Some(proxy_id) = proxy_id
                && let Err(error) = self.iron_control.delete_proxy(&proxy_id).await
            {
                warn!(%proxy_id, %error, "failed to delete orphaned hand proxy");
            }
            match self
                .pods()
                .delete(&pod_name, &DeleteParams::default())
                .await
            {
                Ok(_) => {
                    deleted += 1;
                    info!(%id, %pod_name, "deleted orphaned hand pod");
                }
                Err(error) if is_not_found(&error) => {}
                Err(error) => warn!(%pod_name, error = %backend("delete orphaned hand pod", error)),
            }
            let secret_name = secret_name_for_hand(&id);
            match self
                .secrets()
                .delete(&secret_name, &DeleteParams::default())
                .await
            {
                Ok(_) => {}
                Err(error) if is_not_found(&error) => {}
                Err(error) => {
                    warn!(%secret_name, error = %backend("delete orphaned hand Secret", error))
                }
            }
        }

        let secrets = self
            .secrets()
            .list(&ListParams::default().labels(&selector))
            .await
            .map_err(|error| backend("list hand Secrets", error))?;
        for secret in secrets.items {
            let Some(id) = secret
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(HAND_ID_LABEL))
                .cloned()
            else {
                continue;
            };
            if live.contains(&id) || !old_enough(&secret.metadata, now, grace) {
                continue;
            }
            let Some(name) = secret.metadata.name.clone() else {
                continue;
            };
            match self.secrets().delete(&name, &DeleteParams::default()).await {
                Ok(_) => info!(%id, %name, "deleted orphaned hand Secret"),
                Err(error) if is_not_found(&error) => {}
                Err(error) => warn!(%name, error = %backend("delete orphaned hand Secret", error)),
            }
        }

        // A hand proxy is registered before its row is inserted, so a crash in
        // between leaves a proxy no row owns. Sweep hand proxies by their
        // labels, not by rows.
        let proxy_labels = BTreeMap::from([(HAND_LABEL.to_owned(), HAND_LABEL_VALUE.to_owned())]);
        match self
            .iron_control
            .list_proxies_by_labels(&proxy_labels)
            .await
        {
            Ok(proxies) => {
                for proxy in proxies {
                    let Some(id) = proxy.labels.get(HAND_ID_LABEL) else {
                        continue;
                    };
                    if live.contains(id) {
                        continue;
                    }
                    match self.iron_control.delete_proxy(&proxy.id).await {
                        Ok(()) => info!(%id, proxy_id = %proxy.id, "deleted orphaned hand proxy"),
                        Err(error) => {
                            warn!(%id, proxy_id = %proxy.id, %error, "failed to delete orphaned hand proxy")
                        }
                    }
                }
            }
            Err(error) => warn!(%error, "failed to list hand proxies for orphan sweep"),
        }
        Ok(deleted)
    }
}

impl KubernetesHandInfra {
    fn build_pod(
        &self,
        id: &str,
        size: HandSize,
        pod_name: &str,
        secret_name: &str,
        labels: &BTreeMap<String, String>,
        proxy_id: &str,
    ) -> Pod {
        let mut annotations = self.config.pod_annotations.clone();
        annotations.insert(PROXY_ID_ANNOTATION.to_owned(), proxy_id.to_owned());
        let hand_labels = labels.clone();
        Pod {
            metadata: ObjectMeta {
                name: Some(pod_name.to_owned()),
                labels: Some(hand_labels),
                annotations: (!annotations.is_empty()).then_some(std::mem::take(&mut annotations)),
                ..Default::default()
            },
            spec: Some(PodSpec {
                automount_service_account_token: Some(false),
                share_process_namespace: Some(false),
                restart_policy: Some("Never".to_owned()),
                image_pull_secrets: (!self.config.image_pull_secrets.is_empty()).then(|| {
                    self.config
                        .image_pull_secrets
                        .iter()
                        .map(|name| LocalObjectReference { name: name.clone() })
                        .collect()
                }),
                node_selector: (!self.config.node_selector.is_empty())
                    .then(|| self.config.node_selector.clone()),
                tolerations: (!self.config.tolerations.is_empty())
                    .then(|| self.config.tolerations.clone()),
                runtime_class_name: clean(self.config.runtime_class_name.as_deref()),
                priority_class_name: clean(self.config.priority_class_name.as_deref()),
                security_context: Some(PodSecurityContext {
                    fs_group: Some(1001),
                    fs_group_change_policy: Some("OnRootMismatch".to_owned()),
                    ..Default::default()
                }),
                containers: vec![
                    self.hand_container(size, id, pod_name),
                    self.proxy_container(secret_name),
                ],
                volumes: Some(vec![
                    secret_volume(
                        "hand-token",
                        secret_name,
                        &[KeyToPath {
                            key: "token".to_owned(),
                            path: "token".to_owned(),
                            ..Default::default()
                        }],
                    ),
                    // The CA comes straight from the deployment's CA Secrets:
                    // the hand container sees only the certificate, and the
                    // key never leaves the proxy container or gets copied.
                    secret_volume(
                        "hand-ca",
                        &self.config.ca_cert_secret_name,
                        &[KeyToPath {
                            key: self.config.ca_cert_secret_key.clone(),
                            path: "ca.crt".to_owned(),
                            ..Default::default()
                        }],
                    ),
                    empty_dir_volume("iron-proxy-config"),
                    empty_dir_volume("iron-proxy-certs"),
                    Volume {
                        name: "iron-proxy-ca".to_owned(),
                        projected: Some(ProjectedVolumeSource {
                            sources: Some(vec![
                                secret_projection(
                                    &self.config.ca_cert_secret_name,
                                    &self.config.ca_cert_secret_key,
                                    "ca-cert.pem",
                                ),
                                secret_projection(
                                    &self.config.ca_key_secret_name,
                                    &self.config.ca_key_secret_key,
                                    "ca-key.pem",
                                ),
                            ]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn hand_container(&self, size: HandSize, id: &str, pod_name: &str) -> Container {
        let resources = match size {
            HandSize::Default => self.config.resources_default.as_ref(),
            HandSize::Large => self.config.resources_large.as_ref(),
        };
        let proxy_url = format!("http://127.0.0.1:{PROXY_TUNNEL_PORT}");
        Container {
            name: "hand".to_owned(),
            image: Some(self.config.image.clone()),
            image_pull_policy: self.config.image_pull_policy.clone(),
            env: Some(vec![
                env("CENTAUR_HAND_LISTEN", &format!("0.0.0.0:{HAND_PORT}")),
                env(
                    "CENTAUR_HAND_HEALTH_LISTEN",
                    &format!("0.0.0.0:{HAND_HEALTH_PORT}"),
                ),
                env("CENTAUR_HAND_TOKEN_FILE", HAND_TOKEN_FILE),
                env("CENTAUR_HAND_WORKSPACE", "/home/agent/workspace"),
                env("CENTAUR_HAND_NAME", pod_name),
                env("HTTP_PROXY", &proxy_url),
                env("HTTPS_PROXY", &proxy_url),
                env("http_proxy", &proxy_url),
                env("https_proxy", &proxy_url),
                env("NO_PROXY", "localhost,127.0.0.1"),
                env("no_proxy", "localhost,127.0.0.1"),
                env("CENTAUR_HAND_ID", id),
            ]),
            ports: Some(vec![
                container_port("hand", HAND_PORT),
                container_port("health", HAND_HEALTH_PORT),
            ]),
            readiness_probe: Some(Probe {
                http_get: Some(HTTPGetAction {
                    path: Some("/healthz".to_owned()),
                    port: IntOrString::Int(i32::from(HAND_HEALTH_PORT)),
                    ..Default::default()
                }),
                period_seconds: Some(2),
                failure_threshold: Some(30),
                ..Default::default()
            }),
            security_context: Some(SecurityContext {
                allow_privilege_escalation: Some(false),
                capabilities: Some(Capabilities {
                    drop: Some(vec!["ALL".to_owned()]),
                    ..Default::default()
                }),
                run_as_group: Some(1001),
                run_as_non_root: Some(true),
                run_as_user: Some(1001),
                seccomp_profile: Some(SeccompProfile {
                    type_: "RuntimeDefault".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            volume_mounts: Some(vec![
                volume_mount("hand-token", "/var/run/centaur-hand", true),
                volume_mount("hand-ca", "/etc/centaur-hand", true),
            ]),
            resources: resources.and_then(container_resources),
            ..Default::default()
        }
    }

    fn proxy_container(&self, secret_name: &str) -> Container {
        let mut env_vars = BTreeMap::new();
        env_vars.insert(
            "IRON_MANAGEMENT_API_KEY".to_owned(),
            secret_env(secret_name, "management-api-key", "IRON_MANAGEMENT_API_KEY"),
        );
        env_vars.insert(
            "IRON_MANAGEMENT_LISTEN".to_owned(),
            env(
                "IRON_MANAGEMENT_LISTEN",
                &format!(":{PROXY_MANAGEMENT_PORT}"),
            ),
        );
        env_vars.insert(
            "IRON_CONTROL_PLANE_URL".to_owned(),
            env("IRON_CONTROL_PLANE_URL", &self.config.control_url),
        );
        env_vars.insert(
            "IRON_PROXY_TOKEN".to_owned(),
            secret_env(secret_name, "proxy-token", "IRON_PROXY_TOKEN"),
        );
        for (name, value) in [
            ("IRON_PROXY_TUNNEL_LISTEN", PROXY_TUNNEL_LISTEN.to_owned()),
            (
                "IRON_PROXY_UPSTREAM_RESPONSE_HEADER_TIMEOUT",
                PROXY_UPSTREAM_RESPONSE_HEADER_TIMEOUT.to_owned(),
            ),
            ("IRON_DNS_LISTEN", PROXY_DNS_LISTEN.to_owned()),
            ("IRON_DNS_PROXY_IP", PROXY_DNS_PROXY_IP.to_owned()),
            ("IRON_TLS_MODE", PROXY_TLS_MODE.to_owned()),
            ("IRON_TLS_CA_CERT", PROXY_TLS_CA_CERT_PATH.to_owned()),
            ("IRON_TLS_CA_KEY", PROXY_TLS_CA_KEY_PATH.to_owned()),
            ("IRON_LOG_LEVEL", PROXY_LOG_LEVEL.to_owned()),
        ] {
            env_vars.insert(name.to_owned(), env(name, &value));
        }
        if !self.config.iron_proxy_upstream_deny_cidrs.is_empty() {
            env_vars.insert(
                PROXY_UPSTREAM_DENY_CIDRS_ENV.to_owned(),
                env(
                    PROXY_UPSTREAM_DENY_CIDRS_ENV,
                    &self.config.iron_proxy_upstream_deny_cidrs.join(","),
                ),
            );
        }
        for (name, value) in &self.config.iron_proxy_extra_env {
            env_vars.insert(name.clone(), env(name, value));
        }
        Container {
            name: "iron-proxy".to_owned(),
            image: Some(self.config.iron_proxy_image.clone()),
            image_pull_policy: self.config.iron_proxy_image_pull_policy.clone(),
            env: Some(env_vars.into_values().collect()),
            env_from: (!self.config.iron_proxy_env_from_secret_names.is_empty()).then(|| {
                self.config
                    .iron_proxy_env_from_secret_names
                    .iter()
                    .map(|name| EnvFromSource {
                        secret_ref: Some(SecretEnvSource {
                            name: name.clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect()
            }),
            resources: self
                .config
                .iron_proxy_resources
                .as_ref()
                .and_then(container_resources),
            ports: Some(vec![
                container_port("proxy", PROXY_TUNNEL_PORT),
                container_port("management", PROXY_MANAGEMENT_PORT),
                container_port("health", PROXY_HEALTH_PORT),
            ]),
            readiness_probe: Some(proxy_health_probe(Some(1), Some(150))),
            liveness_probe: Some(proxy_health_probe(None, None)),
            security_context: Some(SecurityContext {
                allow_privilege_escalation: Some(false),
                capabilities: Some(Capabilities {
                    drop: Some(vec!["ALL".to_owned()]),
                    ..Default::default()
                }),
                seccomp_profile: Some(SeccompProfile {
                    type_: "RuntimeDefault".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            volume_mounts: Some(vec![
                volume_mount("iron-proxy-config", "/etc/iron-proxy", false),
                volume_mount("iron-proxy-certs", "/certs", false),
                volume_mount("iron-proxy-ca", "/etc/iron-proxy-ca", true),
            ]),
            ..Default::default()
        }
    }

    async fn adopt_secret(&self, secret_name: &str, pod_name: &str, pod_uid: &str) {
        let patch = Patch::Merge(json!({
            "metadata": {
                "ownerReferences": [{
                    "apiVersion": "v1",
                    "kind": "Pod",
                    "name": pod_name,
                    "uid": pod_uid,
                }],
            },
        }));
        if let Err(error) = self
            .secrets()
            .patch(secret_name, &PatchParams::default(), &patch)
            .await
        {
            warn!(
                %secret_name,
                %pod_name,
                error = %error,
                "failed to adopt hand Secret; delete_hand still removes it explicitly"
            );
        }
    }

    async fn wait_for_proxy_principal_applied(
        &self,
        id: &str,
        pod_name: &str,
        principal: &str,
    ) -> HandsResult<()> {
        let started = Instant::now();
        let Some(endpoint) = self.proxy_management_endpoint(id, pod_name).await else {
            return Err(HandsError::Backend(format!(
                "hand {id} iron-proxy has no management endpoint; the lease is unusable"
            )));
        };
        match wait_for_proxy_ack(
            &endpoint,
            principal,
            2,
            PROXY_ACK_TIMEOUT,
            PROXY_ACK_PROBE_WINDOW,
            PROXY_ACK_POLL_INTERVAL,
        )
        .await
        {
            ProxyAck::Applied => {
                info!(
                    %pod_name,
                    principal,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "hand iron-proxy acknowledged the lease principal"
                );
                Ok(())
            }
            ProxyAck::ManagementUnavailable => Err(HandsError::Backend(format!(
                "hand {id} iron-proxy management API is unavailable before the lease deadline"
            ))),
            ProxyAck::TimedOut => Err(HandsError::Backend(format!(
                "hand {id} iron-proxy did not acknowledge the lease principal before the deadline"
            ))),
        }
    }

    async fn proxy_management_endpoint(
        &self,
        id: &str,
        pod_name: &str,
    ) -> Option<ProxyManagementEndpoint> {
        let pod = match self.pods().get(pod_name).await {
            Ok(pod) => pod,
            Err(error) => {
                warn!(%pod_name, %error, "failed to read hand pod for proxy management endpoint");
                return None;
            }
        };
        // The management key lives in the per-hand Secret and reaches the
        // proxy container as a secretKeyRef, so the pod spec has no literal
        // value to read back.
        let api_key = match self.secrets().get(&secret_name_for_hand(id)).await {
            Ok(secret) => secret
                .data
                .as_ref()
                .and_then(|data| data.get("management-api-key"))
                .map(|value| String::from_utf8_lossy(&value.0).into_owned()),
            Err(error) => {
                warn!(%id, %error, "failed to read hand Secret for proxy management key");
                None
            }
        };
        proxy_management_endpoint_from_pod(&pod, api_key.as_deref())
    }
}

struct ProxyManagementEndpoint {
    base_url: String,
    api_key: String,
}

#[derive(serde::Deserialize)]
struct ProxyManagedStatus {
    #[serde(default)]
    principal_id: String,
    #[serde(default)]
    synced_once: bool,
    #[serde(default)]
    last_sync_at: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
enum ProxyAck {
    Applied,
    ManagementUnavailable,
    TimedOut,
}

async fn wait_for_proxy_ack(
    endpoint: &ProxyManagementEndpoint,
    principal_id: &str,
    fresh_syncs: usize,
    ack_timeout: Duration,
    probe_window: Duration,
    poll_interval: Duration,
) -> ProxyAck {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return ProxyAck::ManagementUnavailable;
    };
    let started = Instant::now();
    let mut management_confirmed = false;
    let mut last_sync: Option<Option<String>> = None;
    let mut syncs_seen = 0;
    let mut poke_pending = true;
    loop {
        let status = client
            .get(format!("{}/v1/status", endpoint.base_url))
            .bearer_auth(&endpoint.api_key)
            .send()
            .await;
        if let Ok(response) = status
            && response.status().is_success()
        {
            management_confirmed = true;
            if let Ok(status) = response.json::<ProxyManagedStatus>().await {
                match &last_sync {
                    None => last_sync = Some(status.last_sync_at.clone()),
                    Some(previous) if *previous != status.last_sync_at => {
                        syncs_seen += 1;
                        last_sync = Some(status.last_sync_at.clone());
                        poke_pending = true;
                    }
                    Some(_) => {}
                }
                if syncs_seen >= fresh_syncs
                    && status.synced_once
                    && status.principal_id == principal_id
                {
                    return ProxyAck::Applied;
                }
            }
        }
        if poke_pending
            && last_sync.is_some()
            && !matches!(
                client
                    .post(format!("{}/v1/sync", endpoint.base_url))
                    .bearer_auth(&endpoint.api_key)
                    .send()
                    .await,
                Ok(response) if response.status().is_success()
            )
        {
            poke_pending = true;
        } else if last_sync.is_some() {
            poke_pending = false;
        }
        let elapsed = started.elapsed();
        if !management_confirmed && elapsed >= probe_window {
            return ProxyAck::ManagementUnavailable;
        }
        if elapsed >= ack_timeout {
            return ProxyAck::TimedOut;
        }
        sleep(poll_interval).await;
    }
}

fn proxy_management_endpoint_from_pod(
    pod: &Pod,
    fallback_api_key: Option<&str>,
) -> Option<ProxyManagementEndpoint> {
    let status = pod.status.as_ref()?;
    let phase = status.phase.as_deref()?;
    if !phase.eq_ignore_ascii_case("running") {
        return None;
    }
    let pod_ip = status
        .pod_ip
        .as_deref()
        .filter(|ip| !ip.trim().is_empty())?;
    let env = pod
        .spec
        .as_ref()?
        .containers
        .iter()
        .find(|container| container.name == "iron-proxy")?
        .env
        .as_ref()?;
    let env_value = |name: &str| {
        env.iter()
            .find(|env| env.name == name)
            .and_then(|env| env.value.as_deref())
    };
    let api_key = env_value("IRON_MANAGEMENT_API_KEY")
        .or(fallback_api_key)
        .filter(|key| !key.is_empty())?
        .to_owned();
    let port = env_value("IRON_MANAGEMENT_LISTEN")
        .and_then(listen_port)
        .unwrap_or(PROXY_MANAGEMENT_PORT);
    let host = if pod_ip.contains(':') {
        format!("[{pod_ip}]")
    } else {
        pod_ip.to_owned()
    };
    Some(ProxyManagementEndpoint {
        base_url: format!("http://{host}:{port}"),
        api_key,
    })
}

fn listen_port(listen: &str) -> Option<u16> {
    listen.rsplit_once(':')?.1.parse().ok()
}

fn old_enough(metadata: &ObjectMeta, now: SystemTime, grace: Duration) -> bool {
    let Some(created) = &metadata.creation_timestamp else {
        return false;
    };
    now.duration_since(SystemTime::from(created.0))
        .is_ok_and(|age| age >= grace)
}

fn is_not_found(error: &kube::Error) -> bool {
    matches!(error, kube::Error::Api(api_error) if api_error.code == 404)
}

fn backend(operation: &str, error: impl std::fmt::Display) -> HandsError {
    HandsError::Backend(format!("{operation}: {error}"))
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn env(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_owned(),
        value: Some(value.to_owned()),
        ..Default::default()
    }
}

fn secret_env(secret_name: &str, key: &str, name: &str) -> EnvVar {
    EnvVar {
        name: name.to_owned(),
        value_from: Some(EnvVarSource {
            secret_key_ref: Some(SecretKeySelector {
                name: secret_name.to_owned(),
                key: key.to_owned(),
                optional: Some(false),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn container_port(name: &str, port: u16) -> ContainerPort {
    ContainerPort {
        name: Some(name.to_owned()),
        container_port: i32::from(port),
        ..Default::default()
    }
}

fn proxy_health_probe(period_seconds: Option<i32>, failure_threshold: Option<i32>) -> Probe {
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

fn container_resources(
    resources: &ResourceRequirements,
) -> Option<k8s_openapi::api::core::v1::ResourceRequirements> {
    (!resources.is_empty()).then(|| k8s_openapi::api::core::v1::ResourceRequirements {
        claims: None,
        limits: (!resources.limits.is_empty()).then(|| {
            resources
                .limits
                .iter()
                .map(|(name, quantity)| (name.clone(), Quantity(quantity.clone())))
                .collect()
        }),
        requests: (!resources.requests.is_empty()).then(|| {
            resources
                .requests
                .iter()
                .map(|(name, quantity)| (name.clone(), Quantity(quantity.clone())))
                .collect()
        }),
    })
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

fn secret_volume(name: &str, secret_name: &str, items: &[KeyToPath]) -> Volume {
    Volume {
        name: name.to_owned(),
        secret: Some(k8s_openapi::api::core::v1::SecretVolumeSource {
            secret_name: Some(secret_name.to_owned()),
            items: Some(items.to_vec()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn secret_projection(secret_name: &str, key: &str, path: &str) -> VolumeProjection {
    VolumeProjection {
        secret: Some(SecretProjection {
            name: secret_name.to_owned(),
            items: Some(vec![KeyToPath {
                key: key.to_owned(),
                path: path.to_owned(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::new_hand_id;

    #[test]
    fn hand_ids_become_dns_label_safe_pod_names() {
        let id = new_hand_id();
        let pod = pod_name_for_hand(&id);
        assert!(id.starts_with("hnd_"));
        assert!(pod.starts_with("hand-hnd-"));
        assert!(!pod.contains('_'));
        assert!(pod.len() <= 63);
        assert_eq!(secret_name_for_hand(&id), format!("{pod}-secret"));
    }

    #[test]
    fn pod_name_keeps_spaces_out_of_label_values() {
        let id = new_hand_id();
        let labels = KubernetesHandInfra::labels(&id, HandSize::Large);
        assert_eq!(labels.get(HAND_ID_LABEL), Some(&id));
        assert_eq!(labels.get(HAND_SIZE_LABEL), Some(&"large".to_owned()));
    }

    #[test]
    fn parses_management_listen_ports() {
        assert_eq!(listen_port(":9092"), Some(9092));
        assert_eq!(listen_port("0.0.0.0:9092"), Some(9092));
        assert_eq!(listen_port("nonsense"), None);
    }
}
