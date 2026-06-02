use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use centaur_api_server::{SandboxRuntime, build_router_with_runtime};
use centaur_iron_proxy::{
    ProxyFragment, SourceKind, SourcePolicy, discover_fragment_files, harness_fragment,
    load_fragment_files,
};
use centaur_sandbox_agent_k8s::{AgentSandboxBackend, AgentSandboxConfig, IronProxyConfig};
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_session_runtime::SandboxWorkloadMode;
use centaur_session_sqlx::PgSessionStore;
use clap::{Args as ClapArgs, Parser, ValueEnum};
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt as tracing_fmt};

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    init_crypto_provider();
    init_tracing();

    let args = Args::parse();

    let store = PgSessionStore::connect(&args.server.database_url).await?;
    if args.server.run_migrations {
        store.run_migrations().await?;
    }
    let sandbox_runtime = sandbox_runtime_from_args(&args).await?;

    let listener = TcpListener::bind(args.server.bind_addr).await?;
    info!(bind_addr = %args.server.bind_addr, "starting centaur api-rs server");

    axum::serve(listener, build_router_with_runtime(store, sandbox_runtime))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_fmt().with_env_filter(filter).json().init();
}

fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn sandbox_runtime_from_args(args: &Args) -> Result<SandboxRuntime, ServerError> {
    match args.sandbox.backend {
        SandboxBackendKind::Local => Ok(SandboxRuntime::backend_with_workload(
            Arc::new(LocalSandboxBackend::new()),
            args.sandbox.local_workload_mode()?,
        )),
        SandboxBackendKind::AgentK8s => {
            let mut config = AgentSandboxConfig::new(args.sandbox.k8s_namespace.clone());
            config.image_pull_policy = args.sandbox.agent_image_pull_policy.clone();
            config.ready_timeout = Duration::from_secs(args.sandbox.ready_timeout_secs);
            config.iron_proxy = args.sandbox.iron_proxy.to_config()?;

            let client = if let Some(context) = args.sandbox.k8s_context.as_deref() {
                let kube_config = kube::Config::from_kubeconfig(&kube::config::KubeConfigOptions {
                    context: Some(context.to_owned()),
                    ..kube::config::KubeConfigOptions::default()
                })
                .await?;
                kube::Client::try_from(kube_config)?
            } else {
                kube::Client::try_default().await?
            };
            let backend = AgentSandboxBackend::new(client, config);

            Ok(SandboxRuntime::backend_with_workload(
                Arc::new(backend),
                args.sandbox.container_workload_mode(),
            ))
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Run the Centaur API Rust session control plane")]
struct Args {
    #[command(flatten)]
    server: ServerArgs,
    #[command(flatten)]
    sandbox: SandboxArgs,
}

#[derive(Debug, ClapArgs)]
struct ServerArgs {
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,
    #[arg(long, env = "BIND_ADDR", default_value = "127.0.0.1:8080")]
    pub bind_addr: SocketAddr,
    #[arg(long, env = "RUN_MIGRATIONS", default_value_t = false)]
    pub run_migrations: bool,
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
    #[arg(
        long = "session-sandbox-centaur-api-key",
        env = "SESSION_SANDBOX_CENTAUR_API_KEY"
    )]
    centaur_api_key_override: Option<String>,
    #[arg(long, env = "CENTAUR_API_KEY")]
    centaur_api_key: Option<String>,
    #[arg(
        long = "session-sandbox-passthrough-env",
        env = "SESSION_SANDBOX_PASSTHROUGH_ENV",
        value_delimiter = ','
    )]
    passthrough_env: Vec<String>,
    #[command(flatten)]
    iron_proxy: IronProxyArgs,
}

impl SandboxArgs {
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

    fn container_workload_mode(&self) -> SandboxWorkloadMode {
        let image = self
            .agent_image
            .clone()
            .unwrap_or_else(|| default_sandbox_image(self.workload).to_owned());
        match self.workload {
            SandboxWorkloadKind::Mock => SandboxWorkloadMode::mock_app_server(image),
            SandboxWorkloadKind::CodexAppServer => {
                SandboxWorkloadMode::codex_app_server(image, self.codex_app_server_env_template())
            }
        }
    }

    fn codex_app_server_env_template(&self) -> Vec<(String, String)> {
        let mut envs = Vec::new();
        push_env(
            &mut envs,
            "CENTAUR_API_URL",
            self.centaur_api_url_override
                .as_deref()
                .or(self.centaur_api_url.as_deref())
                .unwrap_or("http://api:8000")
                .to_owned(),
        );
        if let Some(api_key) = self
            .centaur_api_key_override
            .as_deref()
            .or(self.centaur_api_key.as_deref())
        {
            push_env(&mut envs, "CENTAUR_API_KEY", api_key.to_owned());
        }

        for name in &self.passthrough_env {
            if let Ok(value) = env::var(name) {
                push_env(&mut envs, name, value);
            }
        }

        envs
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
    fragments: IronProxyFragmentsArgs,
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
    #[arg(
        long = "kubernetes-token-broker-name",
        env = "KUBERNETES_TOKEN_BROKER_NAME"
    )]
    token_broker_name: Option<String>,
    #[arg(
        long = "kubernetes-token-broker-configmap-name",
        env = "KUBERNETES_TOKEN_BROKER_CONFIGMAP_NAME"
    )]
    token_broker_configmap_name: Option<String>,
}

impl IronProxyArgs {
    fn to_config(&self) -> Result<Option<IronProxyConfig>, ServerError> {
        let mode = self.mode;
        let fragment_paths = self.fragments.paths()?;
        let ca = self.ca.secrets(mode)?;
        if !mode.enabled(!fragment_paths.is_empty(), ca.is_some()) {
            return Ok(None);
        }
        let (ca_cert_secret_name, ca_key_secret_name) =
            ca.ok_or(ServerError::MissingIronProxyCaSecret)?;

        let mut config =
            IronProxyConfig::new(self.image.clone(), ca_cert_secret_name, ca_key_secret_name);
        config.image_pull_policy = self.image_pull_policy.clone();
        self.source.apply_to_config(&mut config);
        config.fragments = self.harness.fragments()?;
        config
            .fragments
            .extend(load_fragment_files(&fragment_paths)?);
        config.env_from_secret_names = self.env_from_secret_names();
        config.token_broker_name = self.token_broker_name.clone();
        config.token_broker_configmap_name = self.token_broker_configmap_name.clone();
        if let Some(labels) = self
            .api_pod_label_selector
            .as_ref()
            .filter(|labels| !labels.is_empty())
        {
            config.api_pod_labels = labels.clone();
        }
        Ok(Some(config))
    }

    fn env_from_secret_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        if let Some(secret_name) = &self.secret_env_name {
            insert_non_empty(&mut names, secret_name);
        }
        if self.source.uses_bootstrap_secret()
            && let Some(secret_name) = &self.bootstrap_secret_name
        {
            insert_non_empty(&mut names, secret_name);
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
        env = "KUBERNETES_FIREWALL_MANAGER_SECRET_SOURCE",
        default_value = "env"
    )]
    source: SourceKind,
    #[arg(long = "op-vault", env = "OP_VAULT", default_value = "ai-agents")]
    op_vault: String,
    #[arg(
        long = "kubernetes-firewall-manager-secret-ttl",
        env = "KUBERNETES_FIREWALL_MANAGER_SECRET_TTL",
        default_value = "10m"
    )]
    secret_ttl: String,
    #[arg(
        long = "kubernetes-firewall-manager-token-broker-ttl",
        env = "KUBERNETES_FIREWALL_MANAGER_TOKEN_BROKER_TTL",
        default_value = "1m"
    )]
    token_broker_ttl: String,
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
    fn apply_to_config(&self, config: &mut IronProxyConfig) {
        config.source_policy = SourcePolicy {
            kind: self.source,
            op_vault: self.op_vault.clone(),
            ttl: self.secret_ttl.clone(),
            token_broker_ttl: self.token_broker_ttl.clone(),
        };
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
        matches!(self.source, SourceKind::OnePassword)
    }
}

#[derive(Debug, ClapArgs)]
struct IronProxyFragmentsArgs {
    #[arg(
        long = "kubernetes-iron-proxy-fragment-paths",
        env = "KUBERNETES_IRON_PROXY_FRAGMENT_PATHS",
        value_delimiter = ','
    )]
    paths: Vec<PathBuf>,
    #[arg(
        long = "kubernetes-iron-proxy-fragment-dirs",
        env = "KUBERNETES_IRON_PROXY_FRAGMENT_DIRS",
        value_delimiter = ','
    )]
    dirs: Vec<PathBuf>,
    #[arg(long = "tool-dirs", env = "TOOL_DIRS", value_delimiter = ':')]
    tool_dirs: Vec<PathBuf>,
}

impl IronProxyFragmentsArgs {
    fn paths(&self) -> Result<Vec<PathBuf>, ServerError> {
        let mut paths = self.paths.clone();
        let mut dirs = self.dirs.clone();
        if dirs.is_empty() {
            dirs.extend(self.tool_dirs.clone());
        }
        paths.extend(discover_fragment_files(&dirs)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[derive(Debug, ClapArgs)]
struct IronProxyHarnessArgs {
    #[arg(
        long = "kubernetes-iron-proxy-harness-engine",
        env = "KUBERNETES_IRON_PROXY_HARNESS_ENGINE",
        default_value = "codex"
    )]
    engine: String,
    #[arg(
        long = "kubernetes-iron-proxy-harness-auth-mode",
        env = "KUBERNETES_IRON_PROXY_HARNESS_AUTH_MODE"
    )]
    auth_mode: Option<String>,
}

impl IronProxyHarnessArgs {
    fn fragments(&self) -> Result<Vec<ProxyFragment>, ServerError> {
        let auth_mode = self
            .auth_mode
            .clone()
            .or_else(|| harness_auth_mode_env(&self.engine))
            .unwrap_or_else(|| "api_key".to_owned());
        Ok(harness_fragment(self.engine.as_str(), auth_mode.as_str())?
            .into_iter()
            .collect())
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

fn harness_auth_mode_env(engine: &str) -> Option<String> {
    match engine {
        "codex" => env::var("CODEX_AUTH_MODE").ok(),
        "claude-code" | "claudecode" => env::var("CLAUDE_CODE_AUTH_MODE").ok(),
        _ => None,
    }
}

fn parse_host_port(value: &str) -> Option<u16> {
    value.rsplit_once(':')?.1.parse().ok()
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

fn insert_non_empty(values: &mut BTreeSet<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        values.insert(value.to_owned());
    }
}

fn push_env(envs: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some((_, existing_value)) = envs
        .iter_mut()
        .find(|(existing_name, _)| existing_name == name)
    {
        *existing_value = value;
    } else {
        envs.push((name.to_owned(), value));
    }
}

#[derive(Debug, Error)]
enum ServerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] centaur_session_sqlx::SessionStoreError),
    #[error(transparent)]
    KubeConfig(#[from] kube::config::KubeconfigError),
    #[error(transparent)]
    KubeInferConfig(#[from] kube::config::InferConfigError),
    #[error(transparent)]
    Kube(#[from] kube::Error),
    #[error(transparent)]
    IronProxy(#[from] centaur_iron_proxy::IronProxyConfigError),
    #[error("iron-proxy requires both firewall CA cert and key Secret names")]
    MissingIronProxyCaSecret,
    #[error("{0}")]
    UnsupportedConfig(String),
}
