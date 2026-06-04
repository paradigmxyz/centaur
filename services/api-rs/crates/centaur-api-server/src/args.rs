use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use centaur_api_server::SandboxRuntime;
use centaur_iron_control::{
    IdentityInput, IronControlClient, RoleSpec, SessionRegistrar, register_role,
};
use centaur_iron_proxy::{
    ProxyFragment, SourceKind, SourcePolicy, harness_auth_fragment, infra_fragment,
};
use centaur_sandbox_agent_k8s::{
    AgentSandboxBackend, AgentSandboxConfig, IronControlSettings, IronProxyConfig,
};
use centaur_sandbox_core::{Mount, MountKind};
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_sandbox_manager::WarmPoolConfig;
use centaur_session_core::HarnessType;
use centaur_session_runtime::SandboxWorkloadMode;
use clap::{Args as ClapArgs, Parser, ValueEnum};
use tracing::info;

use crate::{
    ServerError,
    tool_discovery::{
        DiscoveredToolProxyFragment, ToolDiscoveryConfig, discover_tool_proxy_fragment,
    },
};

const SANDBOX_REPOS_MOUNT_PATH: &str = "/home/agent/github";

#[derive(Debug, Parser)]
#[command(about = "Run the Centaur API Rust session control plane")]
pub(crate) struct Args {
    #[command(flatten)]
    pub(crate) server: ServerArgs,
    #[command(flatten)]
    sandbox: SandboxArgs,
}

impl Args {
    pub(crate) async fn sandbox_runtime(&self) -> Result<SandboxRuntime, ServerError> {
        self.sandbox.runtime().await
    }

    pub(crate) async fn iron_control_runtime(
        &self,
    ) -> Result<Option<IronControlRuntime>, ServerError> {
        self.sandbox.iron_control_runtime().await
    }

    pub(crate) fn warm_pool_config(&self) -> Option<WarmPoolConfig> {
        self.sandbox.warm_pool_config()
    }
}

pub(crate) struct IronControlRuntime {
    pub(crate) registrar: SessionRegistrar,
    pub(crate) warm_pool_bootstrap_principal: String,
}

#[derive(Debug, ClapArgs)]
struct IronControlArgs {
    #[arg(long = "iron-control-url", env = "IRON_CONTROL_URL")]
    url: Option<String>,
    #[arg(long = "iron-control-api-key", env = "IRON_CONTROL_API_KEY")]
    api_key: Option<String>,
    #[arg(
        long = "iron-control-namespace",
        env = "IRON_CONTROL_NAMESPACE",
        default_value = "default"
    )]
    namespace: String,
}

impl IronControlArgs {
    /// An [`IronControlClient`] when both URL and API key are configured.
    fn client(&self) -> Option<IronControlClient> {
        let url = non_empty(self.url.as_deref())?;
        let api_key = non_empty(self.api_key.as_deref())?;
        Some(IronControlClient::new(url, api_key))
    }

    /// Backend sync settings (admin client + control-plane URL) when iron-control
    /// is configured.
    fn settings(&self) -> Option<IronControlSettings> {
        let url = non_empty(self.url.as_deref())?;
        Some(IronControlSettings {
            client: self.client()?,
            control_url: url.to_owned(),
            namespace: self.namespace.clone(),
        })
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[derive(Debug, ClapArgs)]
pub(crate) struct ServerArgs {
    #[arg(long, env = "DATABASE_URL")]
    pub(crate) database_url: String,
    #[arg(long, env = "BIND_ADDR", default_value = "127.0.0.1:8080")]
    pub(crate) bind_addr: SocketAddr,
    #[arg(long, env = "RUN_MIGRATIONS", default_value_t = false)]
    pub(crate) run_migrations: bool,
}

#[derive(Debug, ClapArgs)]
struct SandboxArgs {
    #[arg(
        long = "session-sandbox-backend",
        alias = "kubernetes-sandbox-backend",
        env = "SESSION_SANDBOX_BACKEND",
        value_enum,
        default_value = "local"
    )]
    backend: SandboxBackendKind,
    #[arg(
        long = "session-sandbox-workload",
        alias = "kubernetes-sandbox-workload",
        env = "SESSION_SANDBOX_WORKLOAD",
        value_enum,
        default_value = "mock"
    )]
    workload: SandboxWorkloadKind,
    #[arg(
        long = "session-sandbox-k8s-namespace",
        alias = "kubernetes-namespace",
        env = "SESSION_SANDBOX_K8S_NAMESPACE",
        default_value = "centaur-sandbox-e2e"
    )]
    k8s_namespace: String,
    #[arg(
        long = "session-sandbox-image",
        alias = "kubernetes-agent-image",
        env = "SESSION_SANDBOX_IMAGE"
    )]
    agent_image: Option<String>,
    #[arg(
        long = "session-sandbox-image-pull-policy",
        alias = "kubernetes-agent-image-pull-policy",
        env = "SESSION_SANDBOX_IMAGE_PULL_POLICY"
    )]
    agent_image_pull_policy: Option<String>,
    #[arg(
        long = "session-sandbox-ready-timeout-secs",
        alias = "kubernetes-sandbox-ready-timeout-s",
        env = "SESSION_SANDBOX_READY_TIMEOUT_SECS",
        default_value_t = 90
    )]
    ready_timeout_secs: u64,
    #[arg(
        long = "session-sandbox-warm-pool-size",
        env = "SESSION_SANDBOX_WARM_POOL_SIZE",
        default_value_t = 0
    )]
    warm_pool_size: usize,
    #[arg(
        long = "session-sandbox-warm-pool-replenish-interval-secs",
        env = "SESSION_SANDBOX_WARM_POOL_REPLENISH_INTERVAL_SECS",
        default_value_t = 5,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    warm_pool_replenish_interval_secs: u64,
    #[arg(
        long = "session-sandbox-k8s-context",
        alias = "kubernetes-context",
        env = "SESSION_SANDBOX_K8S_CONTEXT"
    )]
    k8s_context: Option<String>,
    #[arg(
        long = "session-sandbox-centaur-api-url",
        env = "SESSION_SANDBOX_CENTAUR_API_URL"
    )]
    centaur_api_url_override: Option<String>,
    #[arg(long, env = "CENTAUR_API_URL")]
    centaur_api_url: Option<String>,
    #[arg(long = "repos-path", env = "REPOS_PATH")]
    repos_path: Option<String>,
    #[arg(
        long = "session-sandbox-passthrough-env",
        env = "SESSION_SANDBOX_PASSTHROUGH_ENV",
        value_delimiter = ','
    )]
    passthrough_env: Vec<String>,
    #[command(flatten)]
    tools: ToolDiscoveryArgs,
    #[command(flatten)]
    iron_proxy: IronProxyArgs,
    #[command(flatten)]
    iron_control: IronControlArgs,
}

impl SandboxArgs {
    /// Build the iron-control registrar. The warm-pool bootstrap principal
    /// stays roleless until claim-time reassignment binds the session principal.
    async fn iron_control_runtime(&self) -> Result<Option<IronControlRuntime>, ServerError> {
        let Some(client) = self.iron_control.client() else {
            return Ok(None);
        };
        let namespace = self.iron_control.namespace.clone();
        let policy = self.iron_proxy.source_policy();
        let tool_fragment = self.discover_tool_proxy_fragment()?;
        let roles = self.iron_proxy.roles_to_register(tool_fragment.as_ref())?;
        let mut role_ids = Vec::with_capacity(roles.len());
        for (spec, fragment) in &roles {
            role_ids.push(register_role(&client, &namespace, spec, fragment, &policy).await?);
        }
        let bootstrap = client
            .upsert_principal(&IdentityInput {
                namespace: namespace.clone(),
                foreign_id: "warm-pool-bootstrap".to_owned(),
                name: "Warm pool bootstrap".to_owned(),
                labels: BTreeMap::from([
                    ("managed-by".to_owned(), "centaur".to_owned()),
                    ("purpose".to_owned(), "warm-pool-bootstrap".to_owned()),
                ]),
            })
            .await?;
        Ok(Some(IronControlRuntime {
            registrar: SessionRegistrar::new(client, namespace, role_ids),
            warm_pool_bootstrap_principal: bootstrap.id,
        }))
    }

    async fn runtime(&self) -> Result<SandboxRuntime, ServerError> {
        match self.backend {
            SandboxBackendKind::Local => Ok(SandboxRuntime::backend_with_workload(
                Arc::new(LocalSandboxBackend::new()),
                self.local_workload_mode()?,
            )),
            SandboxBackendKind::AgentK8s => {
                let backend = AgentSandboxBackend::new(
                    self.kube_client().await?,
                    AgentSandboxConfig::try_from(self)?,
                );
                Ok(SandboxRuntime::backend_with_workload(
                    Arc::new(backend),
                    self.container_workload_mode()?,
                ))
            }
        }
    }

    async fn kube_client(&self) -> Result<kube::Client, ServerError> {
        if let Some(context) = self.k8s_context.as_deref() {
            let kube_config = kube::Config::from_kubeconfig(&kube::config::KubeConfigOptions {
                context: Some(context.to_owned()),
                ..kube::config::KubeConfigOptions::default()
            })
            .await?;
            Ok(kube::Client::try_from(kube_config)?)
        } else {
            Ok(kube::Client::try_default().await?)
        }
    }

    fn local_workload_mode(&self) -> Result<SandboxWorkloadMode, ServerError> {
        match self.workload {
            SandboxWorkloadKind::Mock => Ok(SandboxWorkloadMode::mock_app_server(
                self.agent_image
                    .clone()
                    .unwrap_or_else(|| "local-mock-app-server".to_owned()),
            )),
            SandboxWorkloadKind::CodexAppServer => Err(ServerError::UnsupportedConfig(
                "codex-app-server workload requires --session-sandbox-backend agent-k8s".to_owned(),
            )),
        }
    }

    fn container_workload_mode(&self) -> Result<SandboxWorkloadMode, ServerError> {
        let image = self
            .agent_image
            .clone()
            .unwrap_or_else(|| default_sandbox_image(self.workload).to_owned());
        match self.workload {
            SandboxWorkloadKind::Mock => Ok(SandboxWorkloadMode::mock_app_server(image)),
            SandboxWorkloadKind::CodexAppServer => {
                let mut workload = SandboxWorkloadMode::codex_app_server(
                    image,
                    self.codex_app_server_env_template()?,
                );
                if let Some(repos_path) = clean_optional_value(self.repos_path.as_deref()) {
                    workload = workload.mount(
                        Mount::new(
                            MountKind::Bind {
                                source_path: repos_path,
                            },
                            SANDBOX_REPOS_MOUNT_PATH,
                        )
                        .read_only(),
                    );
                }
                Ok(workload)
            }
        }
    }

    fn codex_app_server_env_template(&self) -> Result<Vec<(String, String)>, ServerError> {
        let tool_fragment = self.discover_tool_proxy_fragment()?;
        let mut envs = vec![(
            "CENTAUR_API_URL".to_owned(),
            self.centaur_api_url_override
                .as_deref()
                .or(self.centaur_api_url.as_deref())
                .unwrap_or("http://api:8000")
                .to_owned(),
        )];

        // Single source of truth: propagate this control plane's harness auth
        // modes into the sandbox so the agent's auth.json matches the
        // credential the egress proxy injects — api-rs reads the same
        // CODEX_AUTH_MODE to register the iron-control fragment. Codex defaults
        // to api_key so the agent never silently falls back to the ChatGPT
        // auth.json; CLAUDE_CODE_AUTH_MODE rides along when set.
        let codex_auth_mode = clean_optional_value(env::var("CODEX_AUTH_MODE").ok().as_deref())
            .unwrap_or_else(|| "api_key".to_owned());
        envs.push(("CODEX_AUTH_MODE".to_owned(), codex_auth_mode.clone()));
        if let Some(mode) = clean_optional_value(env::var("CLAUDE_CODE_AUTH_MODE").ok().as_deref())
        {
            envs.push(("CLAUDE_CODE_AUTH_MODE".to_owned(), mode));
        }

        // Inject the proxy fragments' placeholder credentials so env-based
        // consumers send the proxy_value iron-proxy replaces with the real
        // secret: codex's OPENAI_API_KEY (api_key mode → codex logs in and
        // hits api.openai.com instead of falling back to the ChatGPT
        // auth.json), git's GITHUB_TOKEN, and the rest of the infra/tool set.
        for (name, value) in self
            .iron_proxy
            .sandbox_placeholder_env(tool_fragment.as_ref())?
        {
            if !envs.iter().any(|(existing, _)| existing == &name) {
                envs.push((name, value));
            }
        }
        if codex_auth_mode == "api_key"
            && !envs
                .iter()
                .any(|(existing, _)| existing == "OPENAI_API_KEY")
        {
            envs.push(("OPENAI_API_KEY".to_owned(), "OPENAI_API_KEY".to_owned()));
        }

        for name in &self.passthrough_env {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            if let Ok(value) = env::var(name) {
                if let Some((_, existing_value)) = envs
                    .iter_mut()
                    .find(|(existing_name, _)| existing_name == name)
                {
                    *existing_value = value;
                } else {
                    envs.push((name.to_owned(), value));
                }
            }
        }

        Ok(envs)
    }

    fn discover_tool_proxy_fragment(
        &self,
    ) -> Result<Option<DiscoveredToolProxyFragment>, ServerError> {
        let tool_dirs = self.tools.resolve_tool_dirs()?;
        let discovered = discover_tool_proxy_fragment(&tool_dirs)?;
        if discovered.secret_count == 0 {
            return Ok(None);
        }
        info!(
            tool_count = discovered.tool_count,
            secret_count = discovered.secret_count,
            "api-rs tool proxy fragment enabled"
        );
        Ok(Some(discovered))
    }

    fn warm_pool_config(&self) -> Option<WarmPoolConfig> {
        (self.warm_pool_size > 0).then(|| WarmPoolConfig {
            target_size: self.warm_pool_size,
            replenish_interval: Duration::from_secs(self.warm_pool_replenish_interval_secs),
            bootstrap_iron_control_principal: None,
        })
    }
}

#[derive(Debug, ClapArgs)]
struct ToolDiscoveryArgs {
    #[arg(long = "tool-dirs", env = "TOOL_DIRS")]
    tool_dirs: Option<String>,
    #[arg(long = "tools-path", env = "TOOLS_PATH")]
    tools_path: Option<PathBuf>,
    #[arg(long = "tools-overlay-path", env = "TOOLS_OVERLAY_PATH")]
    tools_overlay_path: Option<PathBuf>,
    #[arg(long = "plugins-dir", env = "PLUGINS_DIR")]
    plugins_dir: Option<PathBuf>,
    #[arg(long = "tools-config", env = "TOOLS_CONFIG")]
    tools_config: Option<PathBuf>,
}

impl ToolDiscoveryArgs {
    fn resolve_tool_dirs(&self) -> Result<Vec<PathBuf>, ServerError> {
        Ok(ToolDiscoveryConfig {
            tool_dirs: self.tool_dirs.clone(),
            tools_path: self.tools_path.clone(),
            tools_overlay_path: self.tools_overlay_path.clone(),
            plugins_dir: self.plugins_dir.clone(),
            tools_config: self.tools_config.clone(),
        }
        .resolve_tool_dirs()?)
    }
}

impl TryFrom<&SandboxArgs> for AgentSandboxConfig {
    type Error = ServerError;

    fn try_from(args: &SandboxArgs) -> Result<Self, Self::Error> {
        let mut config = AgentSandboxConfig::new(args.k8s_namespace.clone());
        config.image_pull_policy = args.agent_image_pull_policy.clone();
        config.ready_timeout = Duration::from_secs(args.ready_timeout_secs);
        config.iron_proxy = args.iron_proxy.to_config()?;
        if let Some(proxy) = config.iron_proxy.as_mut() {
            // The k8s backend derives the static sandbox PG DSN catalog from
            // these fragments (see `pg_sandbox_dsns`); `to_config` only ships
            // the harness fragment, so add the infra fragment and the
            // discovered tool fragment (where `pg_dsn` secrets are declared).
            let mut fragments = vec![args.iron_proxy.infra_fragment()?];
            if let Some(tool_fragment) = args.discover_tool_proxy_fragment()? {
                fragments.push(tool_fragment.fragment);
            }
            fragments.append(&mut proxy.fragments);
            proxy.fragments = fragments;
        }
        config.iron_control = args.iron_control.settings();
        // iron-control is the only proxy mode: a per-sandbox proxy syncs its
        // secrets from the control plane, so configuring iron-proxy without
        // iron-control would produce a non-functional proxy. Fail fast.
        if config.iron_proxy.is_some() && config.iron_control.is_none() {
            return Err(ServerError::UnsupportedConfig(
                "iron-proxy requires iron-control: set IRON_CONTROL_URL and IRON_CONTROL_API_KEY"
                    .to_owned(),
            ));
        }
        Ok(config)
    }
}

#[derive(Debug, ClapArgs)]
struct IronProxyArgs {
    #[arg(
        long = "kubernetes-sandbox-iron-proxy-mode",
        env = "KUBERNETES_SANDBOX_IRON_PROXY_MODE",
        value_enum,
        default_value = "auto"
    )]
    mode: IronProxyMode,
    #[arg(
        long = "kubernetes-iron-proxy-image",
        env = "KUBERNETES_IRON_PROXY_IMAGE",
        default_value = "centaur-iron-proxy:latest"
    )]
    image: String,
    #[arg(
        long = "kubernetes-iron-proxy-image-pull-policy",
        env = "KUBERNETES_IRON_PROXY_IMAGE_PULL_POLICY"
    )]
    image_pull_policy: Option<String>,
    #[command(flatten)]
    ca: IronProxyCaArgs,
    #[command(flatten)]
    source: IronProxySourceArgs,
    #[command(flatten)]
    harness: IronProxyHarnessArgs,
    #[arg(
        long = "kubernetes-secret-env-name",
        env = "KUBERNETES_SECRET_ENV_NAME"
    )]
    secret_env_name: Option<String>,
    #[arg(
        long = "kubernetes-bootstrap-secret-name",
        env = "KUBERNETES_BOOTSTRAP_SECRET_NAME"
    )]
    bootstrap_secret_name: Option<String>,
    #[arg(long = "kubernetes-api-pod-label-selector", env = "KUBERNETES_API_POD_LABEL_SELECTOR", value_parser = parse_label_selector_arg)]
    api_pod_label_selector: Option<BTreeMap<String, String>>,
}

impl IronProxyArgs {
    fn to_config(&self) -> Result<Option<IronProxyConfig>, ServerError> {
        let mode = self.mode;
        let ca = self.ca.secrets(mode)?;
        // The harness auth fragment (infra) is always present, so iron-proxy is
        // enabled whenever a CA is available (or mode forces it).
        if !mode.enabled(true, ca.is_some()) {
            return Ok(None);
        }
        let (ca_cert_secret_name, ca_key_secret_name) =
            ca.ok_or(ServerError::MissingIronProxyCaSecret)?;

        let harness_fragment = self.harness.fragment()?;
        let mut config =
            IronProxyConfig::new(self.image.clone(), ca_cert_secret_name, ca_key_secret_name);
        config.image_pull_policy = self.image_pull_policy.clone();
        self.source.apply_to_config(&mut config);
        config.fragments = vec![harness_fragment];
        config.env_from_secret_names = self.env_from_secret_names();
        if let Some(labels) = self
            .api_pod_label_selector
            .as_ref()
            .filter(|labels| !labels.is_empty())
        {
            config.api_pod_labels = labels.clone();
        }
        Ok(Some(config))
    }

    fn source_policy(&self) -> SourcePolicy {
        self.source.policy()
    }

    /// The role to register in iron-control. The shared `infra` role contains
    /// infra, harness, and discovered tool secrets, and every session principal
    /// is granted that single role (see [`SessionRegistrar`]).
    fn roles_to_register(
        &self,
        tool_fragment: Option<&DiscoveredToolProxyFragment>,
    ) -> Result<Vec<(RoleSpec, ProxyFragment)>, ServerError> {
        let mut infra = self.infra_fragment()?;
        if let Some(tool_fragment) = tool_fragment {
            merge_fragment(&mut infra, tool_fragment.fragment.clone());
        }
        Ok(vec![(RoleSpec::infra(), infra)])
    }

    /// The full infra fragment: the shared infra secrets plus the harness auth
    /// (also infra), selected by auth mode. Discovered tool secrets are folded
    /// into the same infra role at registration time.
    fn infra_fragment(&self) -> Result<ProxyFragment, ServerError> {
        let mut infra = infra_fragment()?;
        merge_fragment(&mut infra, self.harness.fragment()?);
        Ok(infra)
    }

    /// Placeholder env (`PLACEHOLDER=PLACEHOLDER`) for every secret the proxy
    /// fragments declare — infra, harness, and tools — so env-based consumers
    /// in the sandbox send the proxy_value iron-proxy replaces with the real
    /// credential (codex's `OPENAI_API_KEY`, git's `GITHUB_TOKEN`, …). Mirrors
    /// the full fragment set registered as iron-control roles.
    fn sandbox_placeholder_env(
        &self,
        tool_fragment: Option<&DiscoveredToolProxyFragment>,
    ) -> Result<BTreeMap<String, String>, ServerError> {
        let mut fragments = vec![self.infra_fragment()?];
        if let Some(tool_fragment) = tool_fragment {
            fragments.push(tool_fragment.fragment.clone());
        }
        Ok(centaur_iron_proxy::placeholder_env(&fragments))
    }

    fn env_from_secret_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        if let Some(secret_name) = non_empty(self.secret_env_name.as_deref()) {
            names.insert(secret_name.to_owned());
        }
        if self.source.uses_bootstrap_secret()
            && let Some(secret_name) = non_empty(self.bootstrap_secret_name.as_deref())
        {
            names.insert(secret_name.to_owned());
        }
        names.into_iter().collect()
    }
}

#[derive(Debug, ClapArgs)]
struct IronProxyCaArgs {
    #[arg(
        long = "kubernetes-firewall-ca-secret-name",
        env = "KUBERNETES_FIREWALL_CA_SECRET_NAME"
    )]
    cert_secret_name: Option<String>,
    #[arg(
        long = "kubernetes-firewall-ca-key-secret-name",
        env = "KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME"
    )]
    key_secret_name: Option<String>,
}

impl IronProxyCaArgs {
    fn secrets(&self, mode: IronProxyMode) -> Result<Option<(String, String)>, ServerError> {
        match (&self.cert_secret_name, &self.key_secret_name) {
            (Some(cert), Some(key)) => Ok(Some((cert.clone(), key.clone()))),
            (None, None) if mode == IronProxyMode::Enabled => Ok(Some((
                "centaur-firewall-ca".to_owned(),
                "centaur-firewall-ca-key".to_owned(),
            ))),
            (None, None) => Ok(None),
            _ => Err(ServerError::MissingIronProxyCaSecret),
        }
    }
}

#[derive(Debug, ClapArgs)]
struct IronProxySourceArgs {
    #[arg(
        long = "kubernetes-firewall-manager-secret-source",
        env = "FIREWALL_MANAGER_SECRET_SOURCE",
        default_value = "env"
    )]
    source: SourceKind,
    #[arg(long = "op-vault", env = "OP_VAULT", default_value = "ai-agents")]
    op_vault: String,
    #[arg(
        long = "kubernetes-firewall-manager-secret-ttl",
        env = "FIREWALL_MANAGER_SECRET_TTL",
        default_value = "10m"
    )]
    secret_ttl: String,
    #[arg(
        long = "kubernetes-op-connect-host",
        env = "KUBERNETES_OP_CONNECT_HOST"
    )]
    op_connect_host: Option<String>,
    #[arg(
        long = "kubernetes-op-connect-app-name",
        env = "KUBERNETES_OP_CONNECT_APP_NAME"
    )]
    op_connect_app_name: Option<String>,
    #[arg(
        long = "kubernetes-op-connect-port",
        env = "KUBERNETES_OP_CONNECT_PORT"
    )]
    op_connect_port: Option<u16>,
}

impl IronProxySourceArgs {
    fn policy(&self) -> SourcePolicy {
        SourcePolicy {
            kind: self.source,
            op_vault: self.op_vault.clone(),
            ttl: self.secret_ttl.clone(),
        }
    }

    fn apply_to_config(&self, config: &mut IronProxyConfig) {
        config.source_policy = self.policy();
        if let Some(app_name) = &self.op_connect_app_name {
            config.op_connect_app_name = app_name.clone();
        }
        if let Some(port) = self
            .op_connect_port
            .or_else(|| self.op_connect_host.as_deref().and_then(parse_host_port))
        {
            config.op_connect_port = port;
        }
        if let Some(host) = &self.op_connect_host {
            config
                .extra_env
                .insert("OP_CONNECT_HOST".to_owned(), host.clone());
        }
    }

    fn uses_bootstrap_secret(&self) -> bool {
        matches!(self.source, SourceKind::Env | SourceKind::OnePassword)
    }
}

#[derive(Debug, ClapArgs)]
struct IronProxyHarnessArgs {
    #[arg(
        long = "kubernetes-iron-proxy-harness-engine",
        env = "KUBERNETES_IRON_PROXY_HARNESS_ENGINE",
        default_value = "codex"
    )]
    engine: HarnessType,
    #[arg(
        long = "kubernetes-iron-proxy-harness-auth-mode",
        env = "KUBERNETES_IRON_PROXY_HARNESS_AUTH_MODE"
    )]
    auth_mode: Option<String>,
}

impl IronProxyHarnessArgs {
    fn resolved_auth_mode(&self) -> String {
        self.auth_mode
            .clone()
            .or_else(|| harness_auth_mode_env(&self.engine))
            .unwrap_or_else(|| "api_key".to_owned())
    }

    /// The harness auth fragment — infra, baked in and selected by auth mode.
    /// Carries the harness credential secret(s) and, for access_token, the
    /// token-broker credential.
    fn fragment(&self) -> Result<ProxyFragment, ServerError> {
        let engine = harness_fragment_engine_name(&self.engine);
        let auth_mode = self.resolved_auth_mode();
        harness_auth_fragment(engine, &auth_mode)?.ok_or_else(|| {
            ServerError::UnsupportedConfig(format!(
                "no harness auth fragment for engine {engine} auth-mode {auth_mode}"
            ))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum IronProxyMode {
    Auto,
    Enabled,
    Disabled,
}

impl IronProxyMode {
    fn enabled(self, has_fragments: bool, has_ca_config: bool) -> bool {
        match self {
            IronProxyMode::Auto => has_fragments || has_ca_config,
            IronProxyMode::Enabled => true,
            IronProxyMode::Disabled => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SandboxBackendKind {
    Local,
    #[value(name = "agent-k8s")]
    AgentK8s,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SandboxWorkloadKind {
    Mock,
    #[value(name = "codex-app-server")]
    CodexAppServer,
}

fn default_sandbox_image(workload: SandboxWorkloadKind) -> &'static str {
    match workload {
        SandboxWorkloadKind::Mock => "busybox:1.36",
        SandboxWorkloadKind::CodexAppServer => "centaur-agent:latest",
    }
}

fn harness_fragment_engine_name(engine: &HarnessType) -> &'static str {
    match engine {
        HarnessType::Codex => "codex",
        HarnessType::Amp => "amp",
        HarnessType::ClaudeCode => "claude-code",
    }
}

/// Fold ``source`` into ``target`` so several fragments register under one
/// role: concatenate transforms and postgres listeners, and merge top-level
/// keys (later fragments win on conflict).
fn merge_fragment(target: &mut ProxyFragment, source: ProxyFragment) {
    target.transforms.extend(source.transforms);
    target.postgres.extend(source.postgres);
    target.top_level.extend(source.top_level);
}

fn harness_auth_mode_env(engine: &HarnessType) -> Option<String> {
    match engine {
        HarnessType::Codex => env::var("CODEX_AUTH_MODE").ok(),
        HarnessType::ClaudeCode => env::var("CLAUDE_CODE_AUTH_MODE").ok(),
        HarnessType::Amp => None,
    }
}

fn parse_host_port(value: &str) -> Option<u16> {
    value.rsplit_once(':')?.1.parse().ok()
}

fn clean_optional_value(value: Option<&str>) -> Option<String> {
    non_empty(value).map(ToOwned::to_owned)
}

fn parse_label_selector_arg(value: &str) -> Result<BTreeMap<String, String>, String> {
    let mut labels = BTreeMap::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let Some((key, value)) = item.split_once('=') else {
            return Err(format!("label selector item {item:?} must be key=value"));
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err(format!("label selector item {item:?} must be key=value"));
        }
        labels.insert(key.to_owned(), value.to_owned());
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_sandbox_flags() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--session-sandbox-workload",
            "codex-app-server",
            "--session-sandbox-k8s-namespace",
            "centaur-test",
            "--session-sandbox-image",
            "centaur-agent:test",
            "--session-sandbox-ready-timeout-secs",
            "17",
            "--session-sandbox-k8s-context",
            "kind-test",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        assert_eq!(args.sandbox.backend, SandboxBackendKind::AgentK8s);
        assert_eq!(args.sandbox.workload, SandboxWorkloadKind::CodexAppServer);
        assert_eq!(args.sandbox.k8s_namespace, "centaur-test");
        assert_eq!(args.sandbox.ready_timeout_secs, 17);
        assert_eq!(args.sandbox.k8s_context.as_deref(), Some("kind-test"));
    }

    #[test]
    fn accepts_kubernetes_aliases_for_sandbox_flags() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-sandbox-backend",
            "agent-k8s",
            "--kubernetes-namespace",
            "centaur-test",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        assert_eq!(args.sandbox.backend, SandboxBackendKind::AgentK8s);
        assert_eq!(args.sandbox.k8s_namespace, "centaur-test");
    }

    #[test]
    fn agent_k8s_config_converts_from_sandbox_args() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--session-sandbox-k8s-namespace",
            "centaur-test",
            "--session-sandbox-image-pull-policy",
            "IfNotPresent",
            "--session-sandbox-ready-timeout-secs",
            "42",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        let config = AgentSandboxConfig::try_from(&args.sandbox).unwrap();
        assert_eq!(config.namespace, "centaur-test");
        assert_eq!(config.image_pull_policy.as_deref(), Some("IfNotPresent"));
        assert_eq!(config.ready_timeout, Duration::from_secs(42));
        assert!(config.iron_proxy.is_none());
    }

    #[test]
    fn codex_app_server_env_template_injects_auth_mode_and_placeholder() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-workload",
            "codex-app-server",
            "--session-sandbox-centaur-api-url",
            "http://host.docker.internal:8080",
        ])
        .unwrap();

        let env = args.sandbox.codex_app_server_env_template().unwrap();
        // CENTAUR_API_URL is always first.
        assert_eq!(
            env[0],
            (
                "CENTAUR_API_URL".to_owned(),
                "http://host.docker.internal:8080".to_owned()
            )
        );
        // The codex auth mode is propagated so the sandbox agent matches the
        // proxy's registered credential.
        assert!(env.iter().any(|(name, _)| name == "CODEX_AUTH_MODE"));
        // api_key mode (the default) injects the placeholder the egress proxy
        // replaces, so codex logs in and hits api.openai.com instead of
        // falling back to the ChatGPT auth.json.
        assert!(
            env.iter()
                .any(|(name, value)| name == "OPENAI_API_KEY" && value == "OPENAI_API_KEY")
        );
    }

    #[test]
    fn env_secret_source_mounts_bootstrap_secret_into_iron_proxy() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-sandbox-iron-proxy-mode",
            "enabled",
            "--kubernetes-firewall-ca-secret-name",
            "centaur-firewall-ca",
            "--kubernetes-firewall-ca-key-secret-name",
            "centaur-firewall-ca-key",
            "--kubernetes-firewall-manager-secret-source",
            "env",
            "--kubernetes-bootstrap-secret-name",
            "centaur-infra-env",
            "--kubernetes-secret-env-name",
            "centaur-secret-env",
        ])
        .unwrap();

        assert_eq!(
            args.sandbox.iron_proxy.env_from_secret_names(),
            vec![
                "centaur-infra-env".to_owned(),
                "centaur-secret-env".to_owned()
            ]
        );
    }

    #[test]
    fn iron_control_registers_discovered_tool_secrets_on_infra_role() {
        use centaur_iron_proxy::{Secret, SecretReplace, Transform, TransformConfig};

        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-iron-proxy-harness-auth-mode",
            "api_key",
        ])
        .unwrap();
        let tool_fragment = DiscoveredToolProxyFragment {
            fragment: ProxyFragment {
                transforms: vec![Transform {
                    name: "secrets".to_owned(),
                    config: TransformConfig {
                        secrets: vec![Secret {
                            id: Some("TOOL_API_KEY".to_owned()),
                            replace: Some(SecretReplace {
                                proxy_value: Some("TOOL_API_KEY".to_owned()),
                                ..Default::default()
                            }),
                            rules: vec![serde_yaml::from_str("{host: api.tool.test}").unwrap()],
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            },
            tool_count: 1,
            secret_count: 1,
        };

        let roles = args
            .sandbox
            .iron_proxy
            .roles_to_register(Some(&tool_fragment))
            .unwrap();

        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].0.foreign_id, "infra");
        assert!(roles[0].1.transforms.iter().any(|transform| {
            transform.config.secrets.iter().any(|secret| {
                secret.id.as_deref() == Some("TOOL_API_KEY")
                    && secret
                        .replace
                        .as_ref()
                        .and_then(|replace| replace.proxy_value.as_deref())
                        == Some("TOOL_API_KEY")
            })
        }));
    }

    #[test]
    fn codex_workload_mounts_repos_path_read_only() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-workload",
            "codex-app-server",
            "--repos-path",
            "/var/lib/centaur/repos",
        ])
        .unwrap();

        let workload = args.sandbox.container_workload_mode().unwrap();
        let SandboxWorkloadMode::CodexAppServer { mounts, .. } = workload else {
            panic!("expected codex app server workload");
        };

        assert!(mounts.iter().any(|mount| {
            mount.target_path == SANDBOX_REPOS_MOUNT_PATH
                && mount.read_only
                && mount.kind
                    == (MountKind::Bind {
                        source_path: "/var/lib/centaur/repos".to_owned(),
                    })
        }));
    }

    #[test]
    fn parses_harness_type_enum_for_iron_proxy() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-iron-proxy-harness-engine",
            "claudecode",
        ])
        .unwrap();

        assert_eq!(
            args.sandbox.iron_proxy.harness.engine,
            HarnessType::ClaudeCode
        );
    }
}
