use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use centaur_api_server::SandboxRuntime;
use centaur_iron_proxy::{
    ProxyFragment, SourceKind, SourcePolicy, default_harness_fragment_dirs,
    discover_fragment_files, harness_broker_fragments_from_dirs, harness_fragment_from_dirs,
    load_fragment_files,
};
use centaur_sandbox_agent_k8s::{AgentSandboxBackend, AgentSandboxConfig, IronProxyConfig};
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_session_core::HarnessType;
use centaur_session_runtime::SandboxWorkloadMode;
use clap::{Args as ClapArgs, Parser, ValueEnum};

use crate::ServerError;

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
        long = "kubernetes-sandbox-backend",
        env = "KUBERNETES_SANDBOX_BACKEND",
        value_enum,
        default_value = "local"
    )]
    backend: SandboxBackendKind,
    #[arg(
        long = "kubernetes-sandbox-workload",
        env = "KUBERNETES_SANDBOX_WORKLOAD",
        value_enum,
        default_value = "mock"
    )]
    workload: SandboxWorkloadKind,
    #[arg(
        long = "kubernetes-namespace",
        env = "KUBERNETES_NAMESPACE",
        default_value = "centaur-sandbox-e2e"
    )]
    namespace: String,
    #[arg(long = "kubernetes-agent-image", env = "KUBERNETES_AGENT_IMAGE")]
    agent_image: Option<String>,
    #[arg(
        long = "kubernetes-agent-image-pull-policy",
        env = "KUBERNETES_AGENT_IMAGE_PULL_POLICY"
    )]
    agent_image_pull_policy: Option<String>,
    #[arg(
        long = "kubernetes-sandbox-ready-timeout-s",
        env = "KUBERNETES_SANDBOX_READY_TIMEOUT_S",
        default_value_t = 90
    )]
    ready_timeout_s: u64,
    #[arg(long = "kubernetes-context", env = "KUBERNETES_CONTEXT")]
    context: Option<String>,
    #[arg(
        long = "kubernetes-sandbox-centaur-api-url",
        env = "KUBERNETES_SANDBOX_CENTAUR_API_URL"
    )]
    centaur_api_url: Option<String>,
    #[arg(
        long = "kubernetes-sandbox-passthrough-env",
        env = "KUBERNETES_SANDBOX_PASSTHROUGH_ENV",
        value_delimiter = ','
    )]
    passthrough_env: Vec<String>,
    #[command(flatten)]
    iron_proxy: IronProxyArgs,
}

impl SandboxArgs {
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
                    self.container_workload_mode(),
                ))
            }
        }
    }

    async fn kube_client(&self) -> Result<kube::Client, ServerError> {
        if let Some(context) = self.context.as_deref() {
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
                "codex-app-server workload requires --kubernetes-sandbox-backend agent-k8s"
                    .to_owned(),
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
        let mut envs = vec![(
            "CENTAUR_API_URL".to_owned(),
            self.centaur_api_url
                .clone()
                .unwrap_or_else(|| "http://api:8000".to_owned()),
        )];

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

        envs
    }
}

impl TryFrom<&SandboxArgs> for AgentSandboxConfig {
    type Error = ServerError;

    fn try_from(args: &SandboxArgs) -> Result<Self, Self::Error> {
        let mut config = AgentSandboxConfig::new(args.namespace.clone());
        config.image_pull_policy = args.agent_image_pull_policy.clone();
        config.ready_timeout = Duration::from_secs(args.ready_timeout_s);
        config.iron_proxy = args.iron_proxy.to_config()?;
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
        long = "kubernetes-token-broker-url",
        env = "KUBERNETES_TOKEN_BROKER_URL"
    )]
    token_broker_url: Option<String>,
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
        config.token_broker_fragments = self.harness.broker_fragments()?;
        config.env_from_secret_names = self.env_from_secret_names();
        config.token_broker_name = self.token_broker_name.clone();
        config.token_broker_url = self
            .token_broker_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
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
        if let Some(secret_name) = self
            .secret_env_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            names.insert(secret_name.to_owned());
        }
        if self.source.uses_bootstrap_secret()
            && let Some(secret_name) = self
                .bootstrap_secret_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
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
        long = "kubernetes-firewall-manager-token-broker-ttl",
        env = "FIREWALL_MANAGER_TOKEN_BROKER_TTL",
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
    engine: HarnessType,
    #[arg(
        long = "kubernetes-iron-proxy-harness-auth-mode",
        env = "KUBERNETES_IRON_PROXY_HARNESS_AUTH_MODE"
    )]
    auth_mode: Option<String>,
    #[arg(
        long = "kubernetes-iron-proxy-harness-fragment-dirs",
        env = "KUBERNETES_IRON_PROXY_HARNESS_FRAGMENT_DIRS",
        value_delimiter = ','
    )]
    fragment_dirs: Vec<PathBuf>,
}

impl IronProxyHarnessArgs {
    fn fragments(&self) -> Result<Vec<ProxyFragment>, ServerError> {
        let auth_mode = self
            .auth_mode
            .clone()
            .or_else(|| harness_auth_mode_env(&self.engine))
            .unwrap_or_else(|| "api_key".to_owned());
        Ok(harness_fragment_from_dirs(
            harness_fragment_engine_name(&self.engine),
            auth_mode.as_str(),
            &self.fragment_dirs(),
        )?
        .into_iter()
        .collect())
    }

    fn broker_fragments(&self) -> Result<Vec<ProxyFragment>, ServerError> {
        Ok(harness_broker_fragments_from_dirs(&self.fragment_dirs())?)
    }

    fn fragment_dirs(&self) -> Vec<PathBuf> {
        if self.fragment_dirs.is_empty() {
            default_harness_fragment_dirs()
        } else {
            self.fragment_dirs.clone()
        }
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
    fn parses_kubernetes_sandbox_flags() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-workload",
            "codex-app-server",
            "--kubernetes-namespace",
            "centaur-test",
            "--kubernetes-agent-image",
            "centaur-agent:test",
            "--kubernetes-sandbox-ready-timeout-s",
            "17",
            "--kubernetes-context",
            "kind-test",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        assert_eq!(args.sandbox.backend, SandboxBackendKind::AgentK8s);
        assert_eq!(args.sandbox.workload, SandboxWorkloadKind::CodexAppServer);
        assert_eq!(args.sandbox.namespace, "centaur-test");
        assert_eq!(args.sandbox.ready_timeout_s, 17);
        assert_eq!(args.sandbox.context.as_deref(), Some("kind-test"));
    }

    #[test]
    fn rejects_removed_session_sandbox_flags() {
        assert!(
            Args::try_parse_from([
                "centaur-api-server",
                "--database-url",
                "postgres://postgres:postgres@localhost/centaur",
                "--session-sandbox-backend",
                "agent-k8s",
            ])
            .is_err()
        );
    }

    #[test]
    fn agent_k8s_config_converts_from_sandbox_args() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-sandbox-backend",
            "agent-k8s",
            "--kubernetes-namespace",
            "centaur-test",
            "--kubernetes-agent-image-pull-policy",
            "IfNotPresent",
            "--kubernetes-sandbox-ready-timeout-s",
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
    fn codex_app_server_env_template_omits_api_key_by_default() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--kubernetes-sandbox-workload",
            "codex-app-server",
            "--kubernetes-sandbox-centaur-api-url",
            "http://host.docker.internal:8080",
        ])
        .unwrap();

        assert_eq!(
            args.sandbox.codex_app_server_env_template(),
            vec![(
                "CENTAUR_API_URL".to_owned(),
                "http://host.docker.internal:8080".to_owned()
            )]
        );
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
