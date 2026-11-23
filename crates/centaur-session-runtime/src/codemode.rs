//! Per-principal MCP Code Mode sandboxes.
//!
//! One sandbox runs `centaur-codemode serve` for exactly one principal. Cells are
//! stateless, so replicas share nothing: each keeps its own endpoint cache and
//! adopts or creates whatever serves the principal and its *generation*.
//!
//! - Readiness is Kubernetes': the pod's readiness probe is `GET /healthz` on
//!   [`CODEMODE_PORT`], so a `Running` observation means the process is serving
//!   and the control plane never polls the sandbox.
//! - The generation (the `centaur.ai/codemode-fingerprint` label) hashes the
//!   Code Mode image and the principal's sandbox capabilities, so a new image or
//!   a repo-cache revocation routes to a sandbox built for the new inputs and
//!   never to one still holding revoked mounts. Sandboxes of other generations
//!   are stopped after [`SUPERSEDE_GRACE`] and the reaper removes them.
//! - A replica adopts the newest `Running` sandbox of the principal and
//!   generation and otherwise creates one; racing replicas or a rolling deploy
//!   may briefly leave a duplicate, which is harmless.
//!
//! Code Mode sandboxes have no session, execution, or warm-pool row, so their
//! lifecycle keys off the `centaur.ai/workload: codemode` labels and the cache
//! here. The sandbox HTTP API is unauthenticated: the NetworkPolicy admitting
//! only api-rs is the boundary.

use std::{sync::Arc, time::Duration};

use centaur_sandbox_core::{
    CODEMODE_FINGERPRINT_LABEL, CODEMODE_PORT, CODEMODE_PRINCIPAL_LABEL, CODEMODE_WORKLOAD,
    ObservedSandbox, SandboxError, SandboxSpec, SandboxStatus, WORKLOAD_LABEL,
    is_codemode_workload,
};
use centaur_sandbox_manager::SandboxManager;
use centaur_session_core::{SandboxCapabilities as SessionSandboxCapabilities, ThreadKey};
use dashmap::{DashMap, DashSet};
use sha2::{Digest, Sha256};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep},
};
use tracing::{debug, info, warn};

use crate::{
    SessionRuntime, SessionRuntimeError, ToolHostCallPolicy, apply_sandbox_capabilities,
    remove_spec_env, upsert_spec_env,
};

const CODEMODE_COMPONENT: &str = "codemode";
/// Sandbox thread key prefix. Never registered as a session.
const CODEMODE_THREAD_PREFIX: &str = "mcp-codemode:";
/// Longer than the longest cell, so superseding never cuts a running cell off.
const SUPERSEDE_GRACE: Duration = Duration::from_secs(12 * 60);
/// How long a resolved endpoint stays cached per principal and generation.
const ENDPOINT_CACHE_TTL: Duration = Duration::from_secs(30);

/// Where api-rs can reach a principal's Code Mode sandbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodemodeEndpoint {
    /// Sandbox serving the endpoint.
    pub sandbox_id: String,
    /// Base URL (`http://<host>:<port>`) of the Code Mode HTTP API.
    pub base_url: String,
}

/// Hashed principal and its generation: the Code Mode image plus the principal's
/// sandbox capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CodemodeIdentity {
    principal_hash: String,
    generation: String,
}

impl CodemodeIdentity {
    fn derive(principal_id: &str, image: &str, capabilities: &SessionSandboxCapabilities) -> Self {
        let generation = truncated_sha256_hex(
            &format!(
                "image={image}\nrepo_cache={}\nobservability={}",
                capabilities.repo_cache.as_str(),
                capabilities.observability_enabled
            ),
            10,
        );
        Self {
            principal_hash: truncated_sha256_hex(principal_id.trim(), 16),
            generation,
        }
    }

    /// Endpoint-cache and single-flight key, so a capability change misses the
    /// cache and resolves a sandbox built for the new inputs.
    fn cache_key(&self, principal_id: &str) -> String {
        format!("{}\n{}", principal_id.trim(), self.generation)
    }

    /// Whether an observed sandbox is this principal's, whatever its generation.
    fn belongs_to(&self, sandbox: &ObservedSandbox) -> bool {
        is_codemode_workload(&sandbox.labels)
            && sandbox.labels.get(CODEMODE_PRINCIPAL_LABEL) == Some(&self.principal_hash)
    }

    /// Whether an observed sandbox is this principal's current generation.
    fn owns(&self, sandbox: &ObservedSandbox) -> bool {
        self.belongs_to(sandbox)
            && sandbox.labels.get(CODEMODE_FINGERPRINT_LABEL) == Some(&self.generation)
    }
}

fn truncated_sha256_hex(value: &str, length: usize) -> String {
    let digest = hex::encode(Sha256::digest(value.as_bytes()));
    digest[..length].to_owned()
}

/// Thread key for the Code Mode spec factory: never a registered session.
fn codemode_thread_key(principal_id: &str) -> Result<ThreadKey, SessionRuntimeError> {
    ThreadKey::parse(format!("{CODEMODE_THREAD_PREFIX}{}", principal_id.trim()))
        .map_err(|error| SessionRuntimeError::BadRequest(error.to_string()))
}

/// Build the Code Mode workload spec from the agent spec for the principal.
///
/// The base spec supplies the mounts and repo-cache wiring a session sandbox
/// would get; this swaps in the dedicated Code Mode image and workload, scopes
/// capabilities to the principal, and labels the sandbox so adoption can
/// recognise it. The labels are authoritative: the process reads nothing about
/// its own generation.
fn codemode_sandbox_spec(
    mut spec: SandboxSpec,
    policy: &ToolHostCallPolicy,
    identity: &CodemodeIdentity,
    image: &str,
) -> SandboxSpec {
    spec.image = image.to_owned();
    spec.labels.extend(
        [
            ("centaur.ai/component", CODEMODE_COMPONENT),
            (WORKLOAD_LABEL, CODEMODE_WORKLOAD),
            (CODEMODE_PRINCIPAL_LABEL, identity.principal_hash.as_str()),
            (CODEMODE_FINGERPRINT_LABEL, identity.generation.as_str()),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned())),
    );

    // Code Mode is stateless: no context file and no artifact directory. It
    // runs no harness and serves no session either.
    spec.files.clear();
    spec.labels.remove("centaur.ai/harness");
    for name in ["CENTAUR_CODEMODE_FILES_DIR", "CENTAUR_THREAD_KEY"] {
        remove_spec_env(&mut spec, name);
    }

    apply_sandbox_capabilities(&mut spec, &policy.sandbox_capabilities);
    let principal_id = policy.principal_id.trim().to_owned();
    spec.iron_control_principal = Some(principal_id.clone());
    upsert_spec_env(
        &mut spec,
        "CENTAUR_CODEMODE_LISTEN",
        format!("0.0.0.0:{CODEMODE_PORT}"),
    );
    upsert_spec_env(&mut spec, "CENTAUR_MCP_PRINCIPAL_ID", principal_id);
    // `CENTAUR_CODEMODE_CONNECTORS` and `CENTAUR_CODEMODE_CA_FILE` come from the
    // deployment (chart values and the backend's firewall mount); do not
    // overwrite them here.

    // The Code Mode image's entrypoint is `centaur-codemode`; run `serve`
    // directly so the sandbox needs no `/entrypoint.sh`.
    spec.command = Some(vec!["centaur-codemode".to_owned(), "serve".to_owned()]);
    spec.args = Vec::new();
    spec
}

/// Per-replica Code Mode state: the endpoint cache, one resolution mutex per
/// cache key, and the sandboxes already scheduled for a supersede stop.
#[derive(Clone)]
pub(crate) struct CodemodeState {
    /// Full image reference for Code Mode sandboxes.
    image: String,
    endpoints: Arc<DashMap<String, (Instant, CodemodeEndpoint)>>,
    /// Serializes resolutions for one cache key, so concurrent callers share one
    /// create.
    resolving: Arc<DashMap<String, Arc<Mutex<()>>>>,
    /// Sandboxes this replica already scheduled to stop.
    superseded: Arc<DashSet<String>>,
    /// Grace before a superseded sandbox is stopped. Overridable in tests.
    supersede_grace: Duration,
}

impl CodemodeState {
    fn new(image: String) -> Self {
        Self {
            image,
            endpoints: Arc::new(DashMap::new()),
            resolving: Arc::new(DashMap::new()),
            superseded: Arc::new(DashSet::new()),
            supersede_grace: SUPERSEDE_GRACE,
        }
    }

    fn cached(&self, key: &str) -> Option<CodemodeEndpoint> {
        let entry = self.endpoints.get(key)?;
        (entry.0.elapsed() < ENDPOINT_CACHE_TTL).then(|| entry.1.clone())
    }

    fn cache(&self, key: &str, endpoint: &CodemodeEndpoint) {
        self.endpoints
            .insert(key.to_owned(), (Instant::now(), endpoint.clone()));
    }

    /// The resolution mutex for one cache key. A caller that finds the entry
    /// already gone starts a fresh lock and then re-checks the cache.
    fn resolving(&self, key: &str) -> Arc<Mutex<()>> {
        self.resolving
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Drop every cached endpoint of a principal, whatever its generation.
    fn forget(&self, principal_id: &str) {
        let prefix = format!("{}\n", principal_id.trim());
        self.endpoints.retain(|key, _| !key.starts_with(&prefix));
    }
}

impl SessionRuntime {
    /// Enable per-principal MCP Code Mode sandboxes on this runtime.
    pub fn with_codemode(mut self, image: impl Into<String>) -> Self {
        self.codemode = Some(CodemodeState::new(image.into()));
        self
    }

    /// Whether this runtime serves MCP Code Mode.
    pub fn codemode_enabled(&self) -> bool {
        self.codemode.is_some()
    }

    /// Adopt or create the principal's current-generation sandbox and return its
    /// endpoint.
    ///
    /// Resolution runs inline: concurrent callers for one principal and
    /// generation on this replica share a single create, and a later caller hits
    /// the endpoint cache. Readiness already happened in the backend, where
    /// `create_running` returns only once the pod is ready; a timeout surfaces as
    /// a retryable [`SandboxError::NotReady`].
    pub async fn codemode_endpoint(
        &self,
        policy: &ToolHostCallPolicy,
    ) -> Result<CodemodeEndpoint, SessionRuntimeError> {
        let Some(state) = self.codemode.clone() else {
            return Err(SessionRuntimeError::BadRequest(
                "MCP Code Mode is disabled on this control plane".to_owned(),
            ));
        };
        let principal_id = policy.principal_id.trim();
        if principal_id.is_empty() {
            return Err(SessionRuntimeError::BadRequest(
                "MCP Code Mode requires a principal".to_owned(),
            ));
        }
        let thread_key = codemode_thread_key(principal_id)?;
        let harness = self.default_harness();
        let base =
            (self.sandbox_runtime.spec_factory)(&thread_key, CODEMODE_COMPONENT, &harness, None);
        let identity =
            CodemodeIdentity::derive(principal_id, &state.image, &policy.sandbox_capabilities);
        let key = identity.cache_key(principal_id);

        let resolving = state.resolving(&key);
        let _resolution = resolving.lock().await;
        let endpoint = match state.cached(&key) {
            Some(endpoint) => endpoint,
            None => {
                let endpoint = resolve_codemode(
                    &state,
                    &self.sandbox_runtime.manager,
                    policy,
                    principal_id,
                    &identity,
                    &base,
                )
                .await?;
                state.cache(&key, &endpoint);
                endpoint
            }
        };
        state.resolving.remove(&key);
        Ok(endpoint)
    }

    /// Fire-and-forget [`SessionRuntime::codemode_endpoint`]. Errors are logged:
    /// the point is to start the sandbox before the first real call arrives.
    pub fn prewarm_codemode(&self, policy: ToolHostCallPolicy) {
        if !self.codemode_enabled() || policy.principal_id.trim().is_empty() {
            return;
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            let principal_id = policy.principal_id.trim().to_owned();
            match runtime.codemode_endpoint(&policy).await {
                Ok(endpoint) => info!(
                    component = CODEMODE_COMPONENT,
                    principal_id,
                    sandbox_id = %endpoint.sandbox_id,
                    "Code Mode sandbox prewarmed"
                ),
                Err(error) => debug!(
                    component = CODEMODE_COMPONENT,
                    principal_id,
                    %error,
                    "Code Mode prewarm did not finish yet"
                ),
            }
        });
    }

    /// Drop every cached endpoint of a principal so the next call re-resolves
    /// (and adopts the running sandbox) instead of reusing a dead address.
    pub fn forget_codemode_endpoint(&self, principal_id: &str) {
        if let Some(state) = &self.codemode {
            state.forget(principal_id);
        }
    }
}

/// Adopt the newest running sandbox of this principal and generation, or create
/// one, then schedule the stop of every superseded generation.
async fn resolve_codemode(
    state: &CodemodeState,
    manager: &Arc<SandboxManager>,
    policy: &ToolHostCallPolicy,
    principal_id: &str,
    identity: &CodemodeIdentity,
    base: &SandboxSpec,
) -> Result<CodemodeEndpoint, SessionRuntimeError> {
    let observed = manager.list_observed().await?;
    let mut candidates = observed
        .iter()
        .filter(|sandbox| identity.owns(sandbox) && sandbox.status == SandboxStatus::Running)
        .collect::<Vec<_>>();
    // Newest first: a rolling deploy adopts its replacement, not a sandbox about
    // to be stopped.
    candidates.sort_by_key(|sandbox| std::cmp::Reverse(sandbox.created_at));

    let mut adopted = None;
    for sandbox in candidates {
        match manager.workload_address(&sandbox.id).await {
            Ok(host) => {
                adopted = Some(CodemodeEndpoint {
                    sandbox_id: sandbox.id.as_str().to_owned(),
                    base_url: codemode_base_url(&host),
                });
                break;
            }
            Err(SandboxError::Unsupported { .. }) => return Err(needs_workload_address()),
            Err(error) => debug!(
                component = CODEMODE_COMPONENT,
                sandbox_id = %sandbox.id.as_str(),
                %error,
                "running Code Mode sandbox has no address; creating another"
            ),
        }
    }

    let endpoint = match adopted {
        Some(endpoint) => {
            info!(
                component = CODEMODE_COMPONENT,
                principal_id,
                sandbox_id = %endpoint.sandbox_id,
                "adopted a running Code Mode sandbox"
            );
            endpoint
        }
        None => {
            // `create_running` returns once the pod is ready, so a backend
            // timeout arrives here as a retryable `NotReady`.
            let spec = codemode_sandbox_spec(base.clone(), policy, identity, &state.image);
            let handle = manager.create_running(spec).await?;
            let base_url = match manager.workload_address(&handle.id).await {
                Ok(host) => codemode_base_url(&host),
                Err(error) => {
                    // Never leave a sandbox the control plane cannot reach.
                    let _ = manager.stop(&handle.id).await;
                    return Err(match error {
                        SandboxError::Unsupported { .. } => needs_workload_address(),
                        other => SessionRuntimeError::Sandbox(other),
                    });
                }
            };
            info!(
                component = CODEMODE_COMPONENT,
                principal_id,
                sandbox_id = %handle.id.as_str(),
                "created Code Mode sandbox"
            );
            CodemodeEndpoint {
                sandbox_id: handle.id.as_str().to_owned(),
                base_url,
            }
        }
    };

    schedule_supersede_stops(state, manager, &observed, identity);
    Ok(endpoint)
}

fn codemode_base_url(host: &str) -> String {
    format!("http://{host}:{CODEMODE_PORT}")
}

fn needs_workload_address() -> SessionRuntimeError {
    SessionRuntimeError::Sandbox(SandboxError::backend(
        "the sandbox backend cannot expose a workload address, so MCP Code Mode is unavailable",
    ))
}

/// Stop this principal's non-terminal sandboxes of other generations once each,
/// after [`SUPERSEDE_GRACE`]; a replica that restarts first relies on the
/// sandbox's own idle exit.
fn schedule_supersede_stops(
    state: &CodemodeState,
    manager: &Arc<SandboxManager>,
    observed: &[ObservedSandbox],
    identity: &CodemodeIdentity,
) {
    let superseded = observed
        .iter()
        .filter(|sandbox| !sandbox.status.is_terminal() && identity.belongs_to(sandbox))
        .filter(|sandbox| !identity.owns(sandbox))
        .map(|sandbox| sandbox.id.clone());
    for id in superseded {
        if !state.superseded.insert(id.as_str().to_owned()) {
            continue;
        }
        let manager = Arc::clone(manager);
        let grace = state.supersede_grace;
        tokio::spawn(async move {
            sleep(grace).await;
            match manager.stop(&id).await {
                Ok(()) | Err(SandboxError::NotFound(_)) => info!(
                    component = CODEMODE_COMPONENT,
                    sandbox_id = %id.as_str(),
                    "stopped superseded Code Mode sandbox"
                ),
                Err(error) => warn!(
                    component = CODEMODE_COMPONENT,
                    sandbox_id = %id.as_str(),
                    %error,
                    "failed to stop superseded Code Mode sandbox"
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex, time::SystemTime};

    use async_trait::async_trait;
    use centaur_iron_control::{IronControlClient, SessionRegistrar};
    use centaur_sandbox_core::{
        EnvVar, ObservedSandbox, SandboxBackend, SandboxFile, SandboxHandle, SandboxId, SandboxIo,
        SandboxResult, SandboxSpec, SandboxStatus,
    };
    use centaur_session_core::SandboxRepoCacheAccess;
    use centaur_session_sqlx::PgSessionStore;

    use super::*;

    const IMAGE: &str = "registry.local/centaur-sandbox:test";
    const CODEMODE_IMAGE: &str = "registry.local/centaur-codemode:test";
    const ADDRESS: &str = "10.0.0.7";
    type R<T> = SandboxResult<T>;

    #[derive(Default)]
    struct FakeState {
        listed: Vec<ObservedSandbox>,
        created: Vec<(String, SandboxSpec)>,
        stopped: Vec<String>,
    }

    /// `create` returns a ready sandbox, as `create_running` promises.
    struct FakeBackend {
        state: Mutex<FakeState>,
        address: Option<String>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                state: Mutex::default(),
                address: Some(ADDRESS.to_owned()),
            }
        }
    }

    impl FakeBackend {
        fn inserted(&self, sandbox: ObservedSandbox) {
            self.state.lock().unwrap().listed.push(sandbox);
        }

        fn created(&self) -> Vec<(String, SandboxSpec)> {
            self.state.lock().unwrap().created.clone()
        }

        fn stopped(&self) -> Vec<String> {
            self.state.lock().unwrap().stopped.clone()
        }
    }

    fn unused<T>() -> R<T> {
        Err(SandboxError::backend("unused by Code Mode resolution"))
    }

    #[async_trait]
    impl SandboxBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn create(&self, spec: SandboxSpec) -> R<SandboxHandle> {
            let id = format!("cm-{}", uuid::Uuid::new_v4().simple());
            let mut state = self.state.lock().unwrap();
            state.listed.push(
                ObservedSandbox::new(id.clone(), "fake", SandboxStatus::Running)
                    .with_labels(spec.labels.clone())
                    .with_created_at(Some(SystemTime::now())),
            );
            state.created.push((id.clone(), spec));
            Ok(SandboxHandle::new(id, "fake"))
        }

        async fn open_io(&self, _: &SandboxId) -> R<SandboxIo> {
            unused()
        }

        async fn workload_address(&self, _: &SandboxId) -> R<String> {
            self.address.clone().ok_or(SandboxError::Unsupported {
                backend: self.name(),
                operation: "workload_address",
            })
        }

        async fn status(&self, _: &SandboxId) -> R<SandboxStatus> {
            unused()
        }

        async fn observe(&self, _: &SandboxId) -> R<ObservedSandbox> {
            unused()
        }

        async fn list_observed(&self) -> R<Vec<ObservedSandbox>> {
            Ok(self.state.lock().unwrap().listed.clone())
        }

        async fn stop(&self, id: &SandboxId) -> R<()> {
            self.state
                .lock()
                .unwrap()
                .stopped
                .push(id.as_str().to_owned());
            Ok(())
        }

        async fn pause(&self, _: &SandboxId) -> R<()> {
            unused()
        }

        async fn resume(&self, _: &SandboxId) -> R<()> {
            unused()
        }
    }

    fn base_spec() -> SandboxSpec {
        let mut spec = SandboxSpec::new(IMAGE);
        spec.command = Some(vec!["/entrypoint.sh".to_owned()]);
        spec.labels
            .insert("centaur.ai/component".to_owned(), "sandbox".to_owned());
        spec.env = vec![
            EnvVar::new("CENTAUR_CODEMODE_CONNECTORS", "/opt/centaur/connectors"),
            EnvVar::new("CENTAUR_CODEMODE_FILES_DIR", "/home/agent/files"),
        ];
        spec.files = vec![SandboxFile::new("context.md", "hi")];
        spec
    }

    fn policy(principal_id: &str) -> ToolHostCallPolicy {
        ToolHostCallPolicy {
            principal_id: principal_id.to_owned(),
            tool_filter: crate::ToolHostToolFilter::default(),
            sandbox_capabilities: Default::default(),
        }
    }

    fn identity(principal_id: &str, capabilities: &SessionSandboxCapabilities) -> CodemodeIdentity {
        CodemodeIdentity::derive(principal_id, CODEMODE_IMAGE, capabilities)
    }

    fn observation(
        id: &str,
        identity: &CodemodeIdentity,
        status: SandboxStatus,
    ) -> ObservedSandbox {
        let labels = BTreeMap::from([
            (WORKLOAD_LABEL.to_owned(), CODEMODE_WORKLOAD.to_owned()),
            (
                CODEMODE_PRINCIPAL_LABEL.to_owned(),
                identity.principal_hash.clone(),
            ),
            (
                CODEMODE_FINGERPRINT_LABEL.to_owned(),
                identity.generation.clone(),
            ),
        ]);
        ObservedSandbox::new(id, "fake", status)
            .with_labels(labels)
            .with_created_at(Some(SystemTime::now()))
    }

    fn codemode_runtime(backend: Arc<FakeBackend>, grace: Duration) -> SessionRuntime {
        let pool = sqlx::PgPool::connect_lazy("postgres://user:pass@127.0.0.1:1/unused")
            .expect("lazy pool");
        let mut runtime = SessionRuntime::new(
            PgSessionStore::new(pool.clone()),
            crate::SandboxRuntime::backend(backend, base_spec()),
            SessionRegistrar::new(IronControlClient::new(pool)),
        )
        .with_codemode(CODEMODE_IMAGE);
        runtime
            .codemode
            .as_mut()
            .expect("codemode state")
            .supersede_grace = grace;
        runtime
    }

    fn env(spec: &SandboxSpec, name: &str) -> Option<String> {
        spec.env
            .iter()
            .find(|env| env.name == name)
            .map(|env| env.value.clone())
    }

    fn label<'a>(spec: &'a SandboxSpec, key: &str) -> Option<&'a str> {
        spec.labels.get(key).map(String::as_str)
    }

    #[tokio::test]
    async fn creates_once_with_the_contract_spec_and_adopts_after_restart() {
        let backend = Arc::new(FakeBackend::default());
        let own = identity("prn_test", &SessionSandboxCapabilities::default());
        let runtime = codemode_runtime(backend.clone(), SUPERSEDE_GRACE);

        let endpoint = runtime
            .codemode_endpoint(&policy("prn_test"))
            .await
            .expect("endpoint");
        assert_eq!(
            backend.created()[0].0.as_str(),
            endpoint.sandbox_id.as_str()
        );
        assert_eq!(
            endpoint.base_url,
            format!("http://{ADDRESS}:{CODEMODE_PORT}")
        );
        let (_, spec) = backend.created().pop().expect("created spec");
        assert!(is_codemode_workload(&spec.labels));
        assert_eq!(
            label(&spec, CODEMODE_PRINCIPAL_LABEL),
            Some(own.principal_hash.as_str())
        );
        assert_eq!(
            label(&spec, CODEMODE_FINGERPRINT_LABEL),
            Some(own.generation.as_str())
        );
        assert_eq!(spec.iron_control_principal.as_deref(), Some("prn_test"));
        assert_eq!(
            spec.image, CODEMODE_IMAGE,
            "Code Mode uses its dedicated image, not the session sandbox image"
        );
        assert!(spec.files.is_empty(), "Code Mode has no files capability");
        assert_eq!(
            spec.command,
            Some(vec!["centaur-codemode".to_owned(), "serve".to_owned()])
        );
        assert!(spec.args.is_empty());
        assert_eq!(
            env(&spec, "CENTAUR_CODEMODE_LISTEN"),
            Some(format!("0.0.0.0:{CODEMODE_PORT}"))
        );
        assert_eq!(
            env(&spec, "CENTAUR_MCP_PRINCIPAL_ID").as_deref(),
            Some("prn_test")
        );
        assert_eq!(env(&spec, "CENTAUR_CODEMODE_FILES_DIR"), None);

        // A second call hits the cache; after a forget, the running sandbox is
        // adopted; a restarted replica starts with an empty cache and adopts it
        // too. None of these creates another sandbox.
        assert_eq!(
            runtime
                .codemode_endpoint(&policy("prn_test"))
                .await
                .expect("cached"),
            endpoint
        );
        runtime.forget_codemode_endpoint(" prn_test ");
        assert_eq!(
            runtime
                .codemode_endpoint(&policy("prn_test"))
                .await
                .expect("after forget"),
            endpoint
        );
        let restarted = codemode_runtime(backend.clone(), SUPERSEDE_GRACE);
        assert_eq!(
            restarted
                .codemode_endpoint(&policy("prn_test"))
                .await
                .expect("after restart"),
            endpoint
        );
        assert_eq!(backend.created().len(), 1);
        assert!(backend.stopped().is_empty());
    }

    #[tokio::test]
    async fn a_capability_change_creates_a_sandbox_and_stops_the_old_one_after_the_grace() {
        let enabled = SessionSandboxCapabilities::default();
        let revoked = SessionSandboxCapabilities {
            repo_cache: SandboxRepoCacheAccess::Public,
            observability_enabled: false,
        };
        let backend = Arc::new(FakeBackend::default());
        backend.inserted(observation(
            "cm-old-generation",
            &identity("prn_test", &enabled),
            SandboxStatus::Running,
        ));
        let runtime = codemode_runtime(backend.clone(), Duration::from_millis(50));
        let changed = ToolHostCallPolicy {
            sandbox_capabilities: revoked.clone(),
            ..policy("prn_test")
        };

        let endpoint = runtime
            .codemode_endpoint(&changed)
            .await
            .expect("a freshly built sandbox");
        assert_eq!(
            backend.created()[0].0.as_str(),
            endpoint.sandbox_id.as_str()
        );
        let (_, spec) = backend.created().pop().expect("created spec");
        assert_eq!(
            label(&spec, CODEMODE_FINGERPRINT_LABEL),
            Some(identity("prn_test", &revoked).generation.as_str())
        );
        assert!(backend.stopped().is_empty(), "the grace has not elapsed");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(backend.stopped(), vec!["cm-old-generation".to_owned()]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_create_once() {
        let backend = Arc::new(FakeBackend::default());
        let runtime = codemode_runtime(backend.clone(), SUPERSEDE_GRACE);
        let tasks = (0..16)
            .map(|_| {
                let runtime = runtime.clone();
                tokio::spawn(async move { runtime.codemode_endpoint(&policy("prn_test")).await })
            })
            .collect::<Vec<_>>();
        let mut endpoints = Vec::new();
        for task in tasks {
            endpoints.push(task.await.unwrap().expect("endpoint"));
        }
        assert!(endpoints.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(backend.created().len(), 1);
    }

    #[tokio::test]
    async fn unavailable_codemode_backends_error_clearly() {
        let backend = Arc::new(FakeBackend::default());
        let mut disabled = codemode_runtime(backend.clone(), SUPERSEDE_GRACE);
        disabled.codemode = None;
        assert!(!disabled.codemode_enabled());
        let error = disabled
            .codemode_endpoint(&policy("prn_test"))
            .await
            .expect_err("disabled runtime must reject Code Mode");
        assert!(
            matches!(&error, SessionRuntimeError::BadRequest(message) if message.contains("disabled")),
            "expected a clear disabled error, got {error}"
        );
        assert!(backend.created().is_empty());

        let backend = Arc::new(FakeBackend {
            address: None,
            ..Default::default()
        });
        let runtime = codemode_runtime(backend.clone(), SUPERSEDE_GRACE);
        let error = runtime
            .codemode_endpoint(&policy("prn_test"))
            .await
            .expect_err("a backend without workload networking cannot serve Code Mode");
        assert!(
            matches!(
                &error,
                SessionRuntimeError::Sandbox(SandboxError::Backend { context, .. })
                    if context.contains("workload address")
            ),
            "expected a backend error naming the missing capability, got {error}"
        );
        assert_eq!(
            backend.stopped(),
            backend
                .created()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        );
    }
}
