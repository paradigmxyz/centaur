use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use centaur_api_server::SandboxRuntime;
use centaur_iron_control::{
    IdentityInput, IronControlClient, IronControlError, RegisterError, RoleSpec, SessionRegistrar,
    register_role,
};
use centaur_iron_proxy::{
    ProxyFragment, SourceKind, SourcePolicy, bedrock_enabled, harness_auth_fragment, infra_fragment,
};
use centaur_sandbox_agent_k8s::{
    AgentSandboxBackend, AgentSandboxConfig, GitHubTokenRef, IronControlSettings, IronProxyConfig,
    OtlpEgressTarget, ToolSource, ToolsConfig,
};
use centaur_sandbox_core::{Mount, MountKind, SandboxSpec};
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_sandbox_manager::{SandboxReaperConfig, WarmPoolConfig};
use centaur_session_core::HarnessType;
use centaur_session_runtime::{
    PersonaRegistry, SandboxCapacityConfig, SandboxWorkloadMode, SessionSandboxCleanupConfig,
};
use centaur_workflows::WorkflowHostSandboxRuntime;
use clap::{Args as ClapArgs, Parser, ValueEnum};
use tracing::{info, warn};

use crate::{
    ServerError,
    activity_summary::ActivitySummaryConfig,
    tool_discovery::{
        DiscoveredToolProxyFragment, ToolDiscoveryConfig, discover_persona_registry,
        discover_tool_proxy_fragment,
    },
};

const SANDBOX_REPOS_MOUNT_PATH: &str = "/home/agent/github";

/// OTLP env always forwarded from the api-rs process into codex sandboxes,
/// mirroring the Python control plane's `_SANDBOX_PASSTHROUGH_ENV_KEYS`. The
/// wrapper inside the sandbox reads these to configure codex's trace export
/// (endpoint, Laminar ingest auth header, resource attributes).
const SANDBOX_OTLP_PASSTHROUGH_ENV_KEYS: [&str; 4] = [
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_RESOURCE_ATTRIBUTES",
];

#[derive(Debug, Parser)]
#[command(about = "Run the Centaur API Rust session control plane")]
pub(crate) struct Args {
    #[command(flatten)]
    pub(crate) server: ServerArgs,
    #[command(flatten)]
    sandbox: SandboxArgs,
    #[command(flatten)]
    activity_summary: ActivitySummaryArgs,
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

    pub(crate) fn persona_registry(&self) -> Result<PersonaRegistry, ServerError> {
        self.sandbox.persona_registry()
    }

    pub(crate) fn warm_pool_config(&self) -> Option<WarmPoolConfig> {
        self.sandbox.warm_pool_config()
    }

    pub(crate) fn sandbox_capacity_config(&self) -> Option<SandboxCapacityConfig> {
        self.sandbox.sandbox_capacity_config()
    }

    pub(crate) fn sandbox_reaper_config(&self) -> SandboxReaperConfig {
        self.sandbox.sandbox_reaper_config()
    }

    pub(crate) fn sandbox_cleanup_config(&self) -> SessionSandboxCleanupConfig {
        self.sandbox.sandbox_cleanup_config()
    }

    pub(crate) async fn workflow_host_sandbox_runtime(
        &self,
        bootstrap_iron_control_principal: Option<&str>,
    ) -> Result<Option<WorkflowHostSandboxRuntime>, ServerError> {
        self.sandbox
            .workflow_host_sandbox_runtime(bootstrap_iron_control_principal)
            .await
    }

    pub(crate) fn activity_summary_config(&self) -> Option<ActivitySummaryConfig> {
        self.activity_summary.config()
    }
}

pub(crate) struct IronControlRuntime {
    pub(crate) registrar: SessionRegistrar,
    pub(crate) warm_pool_bootstrap_principal: String,
    pub(crate) workflow_host_principal: String,
}

#[derive(Debug, ClapArgs)]
struct ActivitySummaryArgs {
    /// Enable API-side model summaries of durable Codex App Server activity.
    #[arg(
        long = "session-activity-summary-enabled",
        env = "SESSION_ACTIVITY_SUMMARY_ENABLED",
        default_value_t = false,
        action = clap::ArgAction::Set
    )]
    enabled: bool,
    #[arg(
        long = "session-activity-summary-model",
        env = "SESSION_ACTIVITY_SUMMARY_MODEL",
        default_value = "gpt-5.4-nano"
    )]
    model: String,
    #[arg(
        long = "session-activity-summary-openai-base-url",
        env = "SESSION_ACTIVITY_SUMMARY_OPENAI_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    openai_base_url: String,
    #[arg(
        long = "session-activity-summary-min-interval-secs",
        env = "SESSION_ACTIVITY_SUMMARY_MIN_INTERVAL_SECS",
        default_value_t = 8,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    min_interval_secs: u64,
    #[arg(
        long = "session-activity-summary-timeout-secs",
        env = "SESSION_ACTIVITY_SUMMARY_TIMEOUT_SECS",
        default_value_t = 5,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    timeout_secs: u64,
    #[arg(
        long = "session-activity-summary-max-facts",
        env = "SESSION_ACTIVITY_SUMMARY_MAX_FACTS",
        default_value_t = 12,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    max_facts: u64,
    #[arg(
        long = "session-activity-summary-max-output-tokens",
        env = "SESSION_ACTIVITY_SUMMARY_MAX_OUTPUT_TOKENS",
        default_value_t = 128,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    max_output_tokens: u64,
}

impl ActivitySummaryArgs {
    fn config(&self) -> Option<ActivitySummaryConfig> {
        if !self.enabled {
            return None;
        }
        let Some(api_key) = clean_optional_value(env::var("OPENAI_API_KEY").ok().as_deref()) else {
            warn!(
                "session activity summaries are enabled but no OpenAI credential is configured; \
                 set OPENAI_API_KEY in the api-rs environment"
            );
            return None;
        };
        Some(ActivitySummaryConfig {
            base_url: self.openai_base_url.clone(),
            api_key,
            max_facts: usize::try_from(self.max_facts).unwrap_or(usize::MAX),
            max_output_tokens: u16::try_from(self.max_output_tokens).unwrap_or(u16::MAX),
            min_interval: Duration::from_secs(self.min_interval_secs),
            model: self.model.clone(),
            timeout: Duration::from_secs(self.timeout_secs),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolGitSource {
    repo: String,
    git_ref: Option<String>,
    source_subdir: String,
    cache_dir: PathBuf,
    repo_cache_path: Option<String>,
}

impl ToolGitSource {
    fn from_config(tools: &ToolsConfig) -> Vec<Self> {
        let mut sources = vec![Self::from_source(
            &ToolSource {
                repo: tools.repo.clone(),
                git_ref: tools.git_ref.clone(),
                source_subdir: tools.source_subdir.clone(),
            },
            tools.repo_cache_path.clone(),
        )];
        sources.extend(
            tools
                .extra_sources
                .iter()
                .map(|source| Self::from_source(source, tools.repo_cache_path.clone())),
        );
        sources
    }

    fn from_source(source: &ToolSource, repo_cache_path: Option<String>) -> Self {
        Self {
            repo: source.repo.clone(),
            git_ref: source.git_ref.clone(),
            source_subdir: source.source_subdir.clone(),
            cache_dir: env::temp_dir()
                .join("centaur-api-rs-tools")
                .join(slug_path_component(&source.repo)),
            repo_cache_path,
        }
    }

    fn tools_dir(&self) -> PathBuf {
        if let Some(repo_cache_path) = &self.repo_cache_path {
            return PathBuf::from(repo_cache_path)
                .join(&self.repo)
                .join(&self.source_subdir);
        }
        self.cache_dir.join(&self.source_subdir)
    }

    fn sync(&self) -> Result<(), ServerError> {
        if self.repo_cache_path.is_some() {
            return Ok(());
        }
        static TOOL_PROXY_GIT_SYNC: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = TOOL_PROXY_GIT_SYNC
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.sync_locked()
    }

    fn sync_locked(&self) -> Result<(), ServerError> {
        let repo_url = format!("https://github.com/{}.git", self.repo);
        if !self.cache_dir.join(".git").is_dir() {
            if self.cache_dir.exists() {
                fs::remove_dir_all(&self.cache_dir)?;
            }
            if let Some(parent) = self.cache_dir.parent() {
                fs::create_dir_all(parent)?;
            }
            run_git(
                Command::new("git")
                    .arg("clone")
                    .arg("--quiet")
                    .arg("--filter=blob:none")
                    .arg("--no-checkout")
                    .arg(&repo_url)
                    .arg(&self.cache_dir),
                "clone api-rs tools repo",
            )?;
            run_git(
                Command::new("git")
                    .arg("-C")
                    .arg(&self.cache_dir)
                    .arg("sparse-checkout")
                    .arg("set")
                    .arg(&self.source_subdir),
                "configure api-rs tools sparse checkout",
            )?;
        }

        match &self.git_ref {
            Some(git_ref) => {
                run_git(
                    Command::new("git")
                        .arg("-C")
                        .arg(&self.cache_dir)
                        .arg("-c")
                        .arg("gc.auto=0")
                        .arg("fetch")
                        .arg("--quiet")
                        .arg("origin")
                        .arg(git_ref),
                    "fetch api-rs tools ref",
                )?;
                run_git(
                    Command::new("git")
                        .arg("-C")
                        .arg(&self.cache_dir)
                        .arg("checkout")
                        .arg("--quiet")
                        .arg("--detach")
                        .arg("FETCH_HEAD"),
                    "checkout api-rs tools ref",
                )?;
            }
            None => {
                run_git(
                    Command::new("git")
                        .arg("-C")
                        .arg(&self.cache_dir)
                        .arg("checkout")
                        .arg("--quiet"),
                    "checkout api-rs tools default branch",
                )?;
                run_git(
                    Command::new("git")
                        .arg("-C")
                        .arg(&self.cache_dir)
                        .arg("pull")
                        .arg("--ff-only")
                        .arg("--quiet"),
                    "pull api-rs tools default branch",
                )?;
            }
        }

        // A synced source without the tools subdir is skipped by callers, not
        // an error: with chart-defaulted subdirs, workflows- or skills-only
        // overlay repos legitimately carry no tools tree.
        if !self.tools_dir().is_dir() {
            warn!(
                repo = %self.repo,
                tools_dir = %self.tools_dir().display(),
                "tools subdir missing after sync; skipping tools source"
            );
        }
        Ok(())
    }
}

fn run_git(command: &mut Command, operation: &str) -> Result<(), ServerError> {
    command.env("GIT_TERMINAL_PROMPT", "0");
    let askpass = configure_git_askpass(command)?;
    let output = command.output()?;
    if let Some(path) = askpass {
        let _ = fs::remove_file(path);
    }
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ServerError::ToolSource(format!(
        "{operation} failed with status {}: {}",
        output.status,
        stderr.trim()
    )))
}

fn configure_git_askpass(command: &mut Command) -> Result<Option<PathBuf>, ServerError> {
    let token = env::var("GITHUB_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let token_file = env::var("CENTAUR_TOOLS_GITHUB_TOKEN_FILE")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let Some(password_command) = token
        .map(|token| format!("echo {}", shell_quote(&token)))
        .or_else(|| token_file.map(|path| format!("cat {}", shell_quote(&path))))
    else {
        return Ok(None);
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "centaur-api-rs-git-askpass-{}-{nonce}.sh",
        std::process::id()
    ));
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  *Username*) echo x-access-token;;\n  *Password*) {password_command};;\n  *) echo;;\nesac\n"
        ),
    )?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    command.env("GIT_ASKPASS", &path);
    Ok(Some(path))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
}

fn slug_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
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
    /// The default harness for warm sandboxes. Per-session sandboxes always
    /// run their session's harness (pinned via container args); this only
    /// decides what the warm pool boots ahead of time. Defaults to codex
    /// to match the sandbox image's CMD.
    #[arg(
        long = "session-sandbox-harness",
        env = "SESSION_SANDBOX_HARNESS",
        default_value = "codex"
    )]
    default_harness: HarnessType,
    #[arg(long = "centaur-default-persona", env = "CENTAUR_DEFAULT_PERSONA")]
    default_persona: Option<String>,
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
        long = "session-sandbox-image-pull-secrets",
        env = "SESSION_SANDBOX_IMAGE_PULL_SECRETS",
        value_delimiter = ','
    )]
    image_pull_secrets: Vec<String>,
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
    /// Hard cap on observed running-like sandboxes. 0 disables capacity
    /// admission.
    #[arg(
        long = "session-sandbox-running-limit",
        env = "SESSION_SANDBOX_RUNNING_LIMIT",
        default_value_t = 0
    )]
    sandbox_running_limit: usize,
    /// Do not evict assigned idle sandboxes that were active within this
    /// window. Warm sandboxes can still be discarded first.
    #[arg(
        long = "session-sandbox-hot-idle-grace-secs",
        env = "SESSION_SANDBOX_HOT_IDLE_GRACE_SECS",
        default_value_t = 300
    )]
    sandbox_hot_idle_grace_secs: u64,
    /// Stop any sandbox older than this regardless of status; sessions replace
    /// reaped sandboxes on their next message. 0 disables the max-lifetime
    /// sweep.
    #[arg(
        long = "session-sandbox-max-lifetime-secs",
        env = "SESSION_SANDBOX_MAX_LIFETIME_SECS",
        default_value_t = 259_200
    )]
    sandbox_max_lifetime_secs: u64,
    #[arg(
        long = "session-sandbox-reap-interval-secs",
        env = "SESSION_SANDBOX_REAP_INTERVAL_SECS",
        default_value_t = 300,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    sandbox_reap_interval_secs: u64,
    #[arg(
        long = "session-sandbox-cleanup-interval-secs",
        env = "SESSION_SANDBOX_CLEANUP_INTERVAL_SECS",
        default_value_t = 300
    )]
    sandbox_cleanup_interval_secs: u64,
    #[arg(
        long = "session-sandbox-idle-cleanup-backstop-secs",
        env = "SESSION_SANDBOX_IDLE_CLEANUP_BACKSTOP_SECS",
        default_value_t = 21_600
    )]
    sandbox_idle_cleanup_backstop_secs: u64,
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
    #[arg(long = "repos-pvc", env = "REPOS_PVC")]
    repos_pvc: Option<String>,
    #[arg(
        long = "session-sandbox-passthrough-env",
        env = "SESSION_SANDBOX_PASSTHROUGH_ENV",
        value_delimiter = ','
    )]
    passthrough_env: Vec<String>,
    /// Operator-supplied sandbox env as a JSON list of `{"name","value"}`
    /// objects — the chart renders `sandbox.extraEnv` into this (the same
    /// contract as the Python control plane's `KUBERNETES_SANDBOX_EXTRA_ENV`).
    /// Carries the harness OTLP wiring (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`,
    /// `OTEL_SERVICE_NAME`, NO_PROXY extras) into every codex sandbox.
    #[arg(long = "session-sandbox-extra-env", env = "SESSION_SANDBOX_EXTRA_ENV")]
    extra_env_json: Option<String>,
    #[command(flatten)]
    tools: ToolDiscoveryArgs,
    #[command(flatten)]
    iron_proxy: IronProxyArgs,
    #[command(flatten)]
    iron_control: IronControlArgs,
    #[arg(
        long = "iron-control-sync-infra-secrets",
        env = "IRON_CONTROL_SYNC_INFRA_SECRETS",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    iron_control_sync_infra_secrets: bool,
    #[arg(
        long = "workflow-host-sandbox",
        env = "WORKFLOW_HOST_SANDBOX",
        default_value_t = true
    )]
    workflow_host_sandbox: bool,
    #[arg(long = "workflow-host-image", env = "WORKFLOW_HOST_IMAGE")]
    workflow_host_image: Option<String>,
    #[arg(long = "workflow-host-command", env = "WORKFLOW_HOST_COMMAND")]
    workflow_host_command: Option<String>,
    #[arg(long = "kubernetes-workflow-dirs", env = "KUBERNETES_WORKFLOW_DIRS")]
    kubernetes_workflow_dirs: Option<String>,
    #[command(flatten)]
    tools_source: ToolsArgs,
}

impl SandboxArgs {
    /// Build the iron-control registrar. The warm-pool bootstrap principal
    /// stays roleless until claim-time reassignment binds the session principal.
    async fn iron_control_runtime(&self) -> Result<Option<IronControlRuntime>, ServerError> {
        let Some(client) = self.iron_control.client() else {
            return Ok(None);
        };
        let namespace = self.iron_control.namespace.clone();
        let role_ids = if self.iron_control_sync_infra_secrets {
            let policy = self.iron_proxy.source_policy();
            let roles = self.iron_proxy.roles_to_register()?;
            let mut role_ids = Vec::with_capacity(roles.len());
            for (spec, fragment) in &roles {
                role_ids.push(
                    register_role_with_retry(&client, &namespace, spec, fragment, &policy).await?,
                );
            }
            role_ids
        } else {
            let spec = RoleSpec::infra();
            vec![
                client
                    .upsert_role(&IdentityInput {
                        namespace: namespace.clone(),
                        foreign_id: spec.foreign_id,
                        name: spec.name,
                        labels: BTreeMap::from([("managed-by".to_owned(), "centaur".to_owned())]),
                    })
                    .await?
                    .id,
            ]
        };
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
        let workflow_host = client
            .upsert_principal(&IdentityInput {
                namespace: namespace.clone(),
                foreign_id: "workflow-host".to_owned(),
                name: "Workflow host".to_owned(),
                labels: BTreeMap::from([
                    ("managed-by".to_owned(), "centaur".to_owned()),
                    ("purpose".to_owned(), "workflow-host".to_owned()),
                ]),
            })
            .await?;
        for role_id in &role_ids {
            client.assign_role(&workflow_host.id, role_id).await?;
        }
        Ok(Some(IronControlRuntime {
            registrar: SessionRegistrar::new(client, namespace, role_ids),
            warm_pool_bootstrap_principal: bootstrap.id,
            workflow_host_principal: workflow_host.id,
        }))
    }

    fn persona_registry(&self) -> Result<PersonaRegistry, ServerError> {
        let default_persona_id = clean_optional_value(self.default_persona.as_deref());
        Ok(discover_persona_registry(
            &self.tools.resolve_tool_dirs()?,
            default_persona_id,
        )?)
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

    async fn workflow_host_sandbox_runtime(
        &self,
        bootstrap_iron_control_principal: Option<&str>,
    ) -> Result<Option<WorkflowHostSandboxRuntime>, ServerError> {
        if !self.workflow_host_sandbox {
            return Ok(None);
        }
        let spec = self.workflow_host_spec(bootstrap_iron_control_principal)?;
        let runtime = match self.backend {
            SandboxBackendKind::Local => {
                SandboxRuntime::backend(Arc::new(LocalSandboxBackend::new()), spec.clone())
            }
            SandboxBackendKind::AgentK8s => SandboxRuntime::backend(
                Arc::new(AgentSandboxBackend::new(
                    self.kube_client().await?,
                    AgentSandboxConfig::try_from(self)?,
                )),
                spec.clone(),
            ),
        };
        Ok(Some(WorkflowHostSandboxRuntime::new(runtime, spec)))
    }

    fn workflow_host_spec(
        &self,
        bootstrap_iron_control_principal: Option<&str>,
    ) -> Result<SandboxSpec, ServerError> {
        let image = self
            .workflow_host_image
            .clone()
            .or_else(|| self.agent_image.clone())
            .unwrap_or_else(|| "centaur-agent:latest".to_owned());
        let command = self.workflow_host_command.clone().unwrap_or_else(|| {
            let path = match self.backend {
                SandboxBackendKind::Local => env::var("PYTHON_WORKFLOW_HOST_PATH")
                    .unwrap_or_else(|_| self.default_workflow_host_path()),
                SandboxBackendKind::AgentK8s => self.default_workflow_host_path(),
            };
            let interpreter =
                env::var("PYTHON_WORKFLOW_HOST_PYTHON").unwrap_or_else(|_| "python3".to_owned());
            format!("exec {interpreter} {path}")
        });
        let mut spec = SandboxSpec::new(image)
            .label("centaur.ai/component", "workflow-run")
            .env("CENTAUR_WORKLOAD", "workflow-host");
        spec = match self.backend {
            SandboxBackendKind::Local => spec.command(["/bin/sh", "-lc"]).args([command]),
            SandboxBackendKind::AgentK8s => spec.command(["/entrypoint.sh"]).args([
                "/bin/sh".to_owned(),
                "-lc".to_owned(),
                command,
            ]),
        };
        let agent_k8s_workflow_dirs = self.agent_k8s_workflow_dirs();
        if env::var_os("WORKFLOW_DIRS").is_none()
            && matches!(self.backend, SandboxBackendKind::AgentK8s)
        {
            spec = spec.env("WORKFLOW_DIRS", agent_k8s_workflow_dirs.clone());
        }
        if let Ok(value) =
            env::var("WORKFLOW_HOST_DATABASE_URL").or_else(|_| env::var("DATABASE_URL"))
        {
            spec = spec.env("DATABASE_URL", value);
        }
        for name in [
            "WORKFLOW_DIRS",
            "PYTHON_WORKFLOW_HOST_PATH",
            "PYTHON_WORKFLOW_HOST_PYTHON",
        ] {
            if let Ok(value) = env::var(name) {
                let value = match (name, self.backend) {
                    ("WORKFLOW_DIRS", SandboxBackendKind::AgentK8s) => {
                        agent_k8s_workflow_dirs.clone()
                    }
                    ("PYTHON_WORKFLOW_HOST_PATH", SandboxBackendKind::AgentK8s) => {
                        self.default_workflow_host_path()
                    }
                    _ => value,
                };
                spec = spec.env(name, value);
            }
        }
        if matches!(self.backend, SandboxBackendKind::AgentK8s)
            && let Some(repos_path) = clean_optional_value(self.repos_path.as_deref())
        {
            spec = spec.mount(
                Mount::new(self.repos_mount_kind(repos_path), SANDBOX_REPOS_MOUNT_PATH).read_only(),
            );
        }
        for (name, value) in self.workflow_host_env_template()? {
            upsert_spec_env(&mut spec, name, value);
        }
        if let Some(principal) = bootstrap_iron_control_principal {
            spec = spec.iron_control_principal(principal);
        }
        Ok(spec)
    }

    fn agent_k8s_workflow_dirs(&self) -> String {
        if let Some(value) = clean_optional_value(self.kubernetes_workflow_dirs.as_deref()) {
            return value;
        }
        let source_repos = self.tools_source.source_repos();
        if !source_repos.is_empty() {
            return source_repos
                .into_iter()
                .map(|repo| format!("{SANDBOX_REPOS_MOUNT_PATH}/{repo}/workflows"))
                .collect::<Vec<_>>()
                .join(":");
        }
        "/opt/centaur/workflows".to_owned()
    }

    fn default_workflow_host_path(&self) -> String {
        match self.backend {
            SandboxBackendKind::Local => default_workflow_host_path(),
            SandboxBackendKind::AgentK8s => "/usr/local/bin/workflow-host".to_owned(),
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
                    self.default_harness.clone(),
                );
                if let Some(repos_path) = clean_optional_value(self.repos_path.as_deref()) {
                    workload = workload.mount(
                        Mount::new(self.repos_mount_kind(repos_path), SANDBOX_REPOS_MOUNT_PATH)
                            .read_only(),
                    );
                }
                Ok(workload)
            }
        }
    }

    fn repos_mount_kind(&self, repos_path: String) -> MountKind {
        if let Some(claim_name) = clean_optional_value(self.repos_pvc.as_deref()) {
            return MountKind::NamedVolume(claim_name);
        }
        MountKind::Bind {
            source_path: repos_path,
        }
    }

    fn codex_app_server_env_template(&self) -> Result<Vec<(String, String)>, ServerError> {
        let mut envs = vec![("CENTAUR_API_URL".to_owned(), self.centaur_api_url())];

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

        // Inject the infra/harness placeholder credentials so env-based
        // consumers send the proxy_value iron-proxy replaces with the real
        // secret: codex's OPENAI_API_KEY (api_key mode → codex logs in and
        // hits api.openai.com instead of falling back to the ChatGPT
        // auth.json), git's GITHUB_TOKEN, and the rest of the infra set.
        for (name, value) in self.iron_proxy.sandbox_placeholder_env()? {
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
        if !envs
            .iter()
            .any(|(existing, _)| existing == "OPENROUTER_API_KEY")
        {
            envs.push((
                "OPENROUTER_API_KEY".to_owned(),
                "OPENROUTER_API_KEY".to_owned(),
            ));
        }
        // When Bedrock is enabled, codex's `amazon-bedrock` provider signs with
        // these placeholder AWS credentials and iron-proxy re-signs (SigV4) with
        // the real IAM keys. `aws_auth` is not a `secrets` transform, so the
        // placeholders are injected here rather than via sandbox_placeholder_env.
        for (name, value) in centaur_iron_proxy::bedrock_sandbox_env() {
            if !envs.iter().any(|(existing, _)| existing == &name) {
                envs.push((name, value));
            }
        }

        // OTLP trace wiring rides from this process into every sandbox (the
        // same hardcoded set the Python control plane forwarded). The harness
        // wrapper needs the endpoint + auth header to configure codex's OTLP
        // export — codex's `session_task.turn` spans carry the token usage
        // Laminar prices into cost. The headers value is a secret (Laminar
        // ingest key, ideally ingest-only): it reaches the api-rs process via
        // the chart's secret envFrom, never via values.
        for name in SANDBOX_OTLP_PASSTHROUGH_ENV_KEYS {
            if let Some(value) = clean_optional_value(env::var(name).ok().as_deref())
                && !envs.iter().any(|(existing, _)| existing == name)
            {
                envs.push((name.to_owned(), value));
            }
        }

        for name in self.passthrough_env_names() {
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

        // Operator extra env wins over template defaults (same precedence as
        // the Python control plane). Proxy wiring stays safe: the backend's
        // `apply_proxy_env` overrides the pinned proxy vars at create time and
        // merges NO_PROXY instead of replacing it.
        for (name, value) in self.sandbox_extra_env() {
            if let Some((_, existing_value)) = envs
                .iter_mut()
                .find(|(existing_name, _)| existing_name == &name)
            {
                *existing_value = value;
            } else {
                envs.push((name, value));
            }
        }

        Ok(envs)
    }

    /// `SESSION_SANDBOX_EXTRA_ENV` parsed as a JSON list of `{"name","value"}`
    /// objects. Invalid JSON or shapes are ignored (with a warning) rather than
    /// failing startup, matching the Python control plane's behavior.
    fn sandbox_extra_env(&self) -> Vec<(String, String)> {
        let Some(raw) = self
            .extra_env_json
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
        else {
            return Vec::new();
        };
        let parsed: serde_json::Value = match serde_json::from_str(raw) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn!(%error, "SESSION_SANDBOX_EXTRA_ENV is not valid JSON; ignoring");
                return Vec::new();
            }
        };
        let Some(items) = parsed.as_array() else {
            warn!("SESSION_SANDBOX_EXTRA_ENV is not a JSON array; ignoring");
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let name = item.get("name")?.as_str()?.trim();
                if name.is_empty() || name.contains('=') {
                    return None;
                }
                let value = match item.get("value") {
                    None | Some(serde_json::Value::Null) => String::new(),
                    Some(serde_json::Value::String(value)) => value.clone(),
                    Some(other) => other.to_string(),
                };
                Some((name.to_owned(), value))
            })
            .collect()
    }

    /// Per-sandbox proxy OTLP egress NetworkPolicy target, derived from the
    /// OTLP endpoint the codex sandbox env will carry. Only in-cluster service
    /// DNS endpoints (`<service>.<namespace>.svc[...]`) map to a namespace
    /// selector.
    fn sandbox_otlp_egress_target(&self) -> Result<Option<OtlpEgressTarget>, ServerError> {
        if !matches!(self.workload, SandboxWorkloadKind::CodexAppServer) {
            return Ok(None);
        }
        let envs = self.codex_app_server_env_template()?;
        let endpoint = [
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
        ]
        .into_iter()
        .find_map(|key| {
            envs.iter()
                .find(|(name, value)| name == key && !value.trim().is_empty())
                .map(|(_, value)| value.trim().to_owned())
        });
        let Some(endpoint) = endpoint else {
            return Ok(None);
        };
        match parse_otlp_egress_target(&endpoint) {
            Some(target) => {
                info!(
                    namespace = %target.namespace,
                    port = target.port,
                    endpoint = %endpoint,
                    "sandbox proxy OTLP egress enabled"
                );
                Ok(Some(target))
            }
            None => {
                warn!(
                    endpoint = %endpoint,
                    "sandbox OTLP endpoint is not an in-cluster service DNS name; \
                     no proxy egress NetworkPolicy rule will be created for it"
                );
                Ok(None)
            }
        }
    }

    fn workflow_host_env_template(&self) -> Result<Vec<(String, String)>, ServerError> {
        let mut envs = vec![("CENTAUR_API_URL".to_owned(), self.centaur_api_url())];

        for (name, value) in self.iron_proxy.sandbox_placeholder_env()? {
            envs.push((name, value));
        }

        if let Some(value) = clean_optional_value(self.tools.tool_dirs.as_deref()) {
            envs.push(("TOOL_DIRS".to_owned(), value));
        }
        if let Some(value) = self
            .tools
            .tools_path
            .as_deref()
            .map(|path| path.to_string_lossy().to_string())
            .and_then(|value| clean_optional_value(Some(value.as_str())))
        {
            envs.push(("TOOLS_PATH".to_owned(), value));
        }
        if let Some(value) = self
            .tools
            .tools_overlay_path
            .as_deref()
            .map(|path| path.to_string_lossy().to_string())
            .and_then(|value| clean_optional_value(Some(value.as_str())))
        {
            envs.push(("TOOLS_OVERLAY_PATH".to_owned(), value));
        }

        for name in self.passthrough_env_names() {
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

    fn centaur_api_url(&self) -> String {
        self.centaur_api_url_override
            .as_deref()
            .or(self.centaur_api_url.as_deref())
            .unwrap_or("http://api:8000")
            .to_owned()
    }

    fn passthrough_env_names(&self) -> impl Iterator<Item = &str> {
        self.passthrough_env
            .iter()
            .flat_map(|entry| entry.split(','))
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    fn discover_tool_proxy_fragment(
        &self,
    ) -> Result<Option<DiscoveredToolProxyFragment>, ServerError> {
        let tool_dirs = self.tool_proxy_dirs()?;
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

    fn tool_proxy_dirs(&self) -> Result<Vec<PathBuf>, ServerError> {
        if let Some(tools) = self.tools_source.to_config() {
            let sources = ToolGitSource::from_config(&tools);
            let mut dirs = Vec::with_capacity(sources.len());
            for source in sources {
                source.sync()?;
                let tools_dir = source.tools_dir();
                // Skip sources without a tools tree (chart-defaulted subdirs
                // make this a normal case for non-tool overlay repos).
                if !tools_dir.is_dir() {
                    continue;
                }
                dirs.push(tools_dir);
            }
            return Ok(dirs);
        }
        self.tools.resolve_tool_dirs()
    }

    fn warm_pool_config(&self) -> Option<WarmPoolConfig> {
        (self.warm_pool_size > 0).then(|| WarmPoolConfig {
            target_size: self.warm_pool_size,
            replenish_interval: Duration::from_secs(self.warm_pool_replenish_interval_secs),
            bootstrap_iron_control_principal: None,
            max_running_sandboxes: (self.sandbox_running_limit > 0)
                .then_some(self.sandbox_running_limit),
        })
    }

    fn sandbox_capacity_config(&self) -> Option<SandboxCapacityConfig> {
        (self.sandbox_running_limit > 0).then(|| SandboxCapacityConfig {
            max_running: self.sandbox_running_limit,
            hot_idle_grace: Duration::from_secs(self.sandbox_hot_idle_grace_secs),
        })
    }

    fn sandbox_reaper_config(&self) -> SandboxReaperConfig {
        let ttl = |secs: u64| (secs > 0).then(|| Duration::from_secs(secs));
        SandboxReaperConfig {
            interval: Duration::from_secs(self.sandbox_reap_interval_secs),
            max_lifetime: ttl(self.sandbox_max_lifetime_secs),
        }
    }

    fn sandbox_cleanup_config(&self) -> SessionSandboxCleanupConfig {
        let duration = |secs: u64| (secs > 0).then(|| Duration::from_secs(secs));
        SessionSandboxCleanupConfig {
            interval: duration(self.sandbox_cleanup_interval_secs),
            idle_backstop: duration(self.sandbox_idle_cleanup_backstop_secs),
        }
    }
}

const IRON_CONTROL_REGISTER_MAX_ATTEMPTS: u32 = 5;
const IRON_CONTROL_REGISTER_INITIAL_BACKOFF: Duration = Duration::from_millis(250);

async fn register_role_with_retry(
    client: &IronControlClient,
    namespace: &str,
    spec: &RoleSpec,
    fragment: &ProxyFragment,
    policy: &SourcePolicy,
) -> Result<String, RegisterError> {
    let mut backoff = IRON_CONTROL_REGISTER_INITIAL_BACKOFF;
    for attempt in 1..=IRON_CONTROL_REGISTER_MAX_ATTEMPTS {
        match register_role(client, namespace, spec, fragment, policy).await {
            Ok(role_id) => return Ok(role_id),
            Err(error)
                if attempt < IRON_CONTROL_REGISTER_MAX_ATTEMPTS
                    && should_retry_iron_control_register(&error) =>
            {
                warn!(
                    %error,
                    role = %spec.foreign_id,
                    attempt,
                    max_attempts = IRON_CONTROL_REGISTER_MAX_ATTEMPTS,
                    backoff_ms = backoff.as_millis(),
                    "iron-control role registration failed; retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("iron-control registration retry loop always returns");
}

fn should_retry_iron_control_register(error: &RegisterError) -> bool {
    match error {
        RegisterError::Translate(_) => false,
        RegisterError::Control(IronControlError::Transport { .. }) => true,
        RegisterError::Control(IronControlError::Decode { .. }) => false,
        RegisterError::Control(IronControlError::Status { status, .. }) => {
            *status == 429 || (500..600).contains(status)
        }
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
        config.image_pull_secrets = args
            .image_pull_secrets
            .iter()
            .map(|secret| secret.trim())
            .filter(|secret| !secret.is_empty())
            .map(str::to_owned)
            .collect();
        config.ready_timeout = Duration::from_secs(args.ready_timeout_secs);
        config.iron_proxy = args.iron_proxy.to_config()?;
        if let Some(proxy) = config.iron_proxy.as_mut() {
            // `to_config` only ships the harness fragment, so add infra and
            // discovered tool fragments for any static proxy placeholder
            // metadata the backend needs.
            let mut fragments = vec![args.iron_proxy.infra_fragment()?];
            if let Some(tool_fragment) = args.discover_tool_proxy_fragment()? {
                fragments.push(tool_fragment.fragment);
            }
            fragments.append(&mut proxy.fragments);
            proxy.fragments = fragments;
        }
        config.iron_control = args.iron_control.settings();
        config.tools = args.tools_source.to_config();
        // The chart label policy handles sandbox OTLP egress; keep the
        // per-sandbox proxy's own in-cluster OTLP egress explicit.
        config.otlp_egress = args.sandbox_otlp_egress_target()?;
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
struct ToolsArgs {
    // Tools are git-cloned into each sandbox at boot by a `tools-bootstrap` init
    // container (repo-cache-style) rather than baked into an image, so adding a
    // tool needs no image rebuild. Explicit `id`s avoid clap arg-id collisions
    // with sibling flattened structs.
    #[arg(
        id = "tools_source_repo",
        long = "kubernetes-tools-repo",
        env = "KUBERNETES_TOOLS_REPO"
    )]
    repo: Option<String>,
    #[arg(
        id = "tools_source_ref",
        long = "kubernetes-tools-ref",
        env = "KUBERNETES_TOOLS_REF"
    )]
    git_ref: Option<String>,
    #[arg(
        id = "tools_source_subdir",
        long = "kubernetes-tools-subdir",
        env = "KUBERNETES_TOOLS_SUBDIR",
        default_value = "tools"
    )]
    source_subdir: String,
    // Git-capable image the clone init container runs (the sandbox image carries git).
    #[arg(
        id = "tools_runner_image",
        long = "kubernetes-tools-runner-image",
        env = "KUBERNETES_TOOLS_RUNNER_IMAGE"
    )]
    image: Option<String>,
    #[arg(
        id = "tools_runner_image_pull_policy",
        long = "kubernetes-tools-runner-image-pull-policy",
        env = "KUBERNETES_TOOLS_RUNNER_IMAGE_PULL_POLICY"
    )]
    image_pull_policy: Option<String>,
    // Secret + key holding a GitHub token for private-repo clones (optional).
    #[arg(
        id = "tools_github_token_secret",
        long = "kubernetes-tools-github-token-secret",
        env = "KUBERNETES_TOOLS_GITHUB_TOKEN_SECRET"
    )]
    github_token_secret: Option<String>,
    #[arg(
        id = "tools_github_token_secret_key",
        long = "kubernetes-tools-github-token-secret-key",
        env = "KUBERNETES_TOOLS_GITHUB_TOKEN_SECRET_KEY",
        default_value = "token"
    )]
    github_token_secret_key: String,
    // Optional mounted repo-cache root. When present, sandboxes and api-rs copy
    // tools from `<path>/<repo>/<subdir>` instead of fetching GitHub directly.
    #[arg(
        id = "tools_repo_cache_path",
        long = "kubernetes-tools-repo-cache-path",
        env = "KUBERNETES_TOOLS_REPO_CACHE_PATH"
    )]
    repo_cache_path: Option<String>,
    // Optional PVC claim backing the repo-cache root. When set, sandbox pods mount
    // the repo cache from this PVC instead of using a hostPath.
    #[arg(
        id = "tools_repo_cache_pvc",
        long = "kubernetes-tools-repo-cache-pvc",
        env = "KUBERNETES_TOOLS_REPO_CACHE_PVC"
    )]
    repo_cache_pvc: Option<String>,
    #[arg(
        id = "tools_auto_reload",
        long = "kubernetes-tools-auto-reload",
        env = "KUBERNETES_TOOLS_AUTO_RELOAD",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    auto_reload: bool,
    #[arg(
        id = "tools_extra_sources",
        long = "kubernetes-tools-extra-sources",
        env = "KUBERNETES_TOOLS_EXTRA_SOURCES"
    )]
    extra_sources: Option<String>,
}

impl ToolsArgs {
    fn source_repos(&self) -> Vec<String> {
        let Some(repo) = clean_optional_value(self.repo.as_deref()) else {
            return Vec::new();
        };
        let mut repos = vec![repo];
        repos.extend(self.extra_sources().into_iter().map(|source| source.repo));
        repos
    }

    fn extra_sources(&self) -> Vec<ToolSource> {
        let Some(value) = clean_optional_value(self.extra_sources.as_deref()) else {
            return Vec::new();
        };
        match serde_json::from_str::<Vec<ToolSourceArg>>(&value) {
            Ok(sources) => sources
                .into_iter()
                .filter_map(ToolSourceArg::into_source)
                .collect(),
            Err(err) => {
                tracing::warn!(error = %err, "invalid KUBERNETES_TOOLS_EXTRA_SOURCES; ignoring extra tool sources");
                Vec::new()
            }
        }
    }

    /// `None` when no repo or runner image is configured (tools disabled).
    fn to_config(&self) -> Option<ToolsConfig> {
        let repo = clean_optional_value(self.repo.as_deref())?;
        let image = clean_optional_value(self.image.as_deref())?;
        let mut config = ToolsConfig::new(repo, image);
        config.image_pull_policy = self.image_pull_policy.clone();
        config.git_ref = clean_optional_value(self.git_ref.as_deref());
        if let Some(subdir) = clean_optional_value(Some(self.source_subdir.as_str())) {
            config.source_subdir = subdir;
        }
        if let Some(secret_name) = clean_optional_value(self.github_token_secret.as_deref()) {
            config.github_token = Some(GitHubTokenRef {
                secret_name,
                secret_key: clean_optional_value(Some(self.github_token_secret_key.as_str()))
                    .unwrap_or_else(|| "token".to_owned()),
            });
        }
        config.repo_cache_path = clean_optional_value(self.repo_cache_path.as_deref());
        config.repo_cache_pvc = clean_optional_value(self.repo_cache_pvc.as_deref());
        config.auto_reload = self.auto_reload;
        config.extra_sources = self.extra_sources();
        Some(config)
    }
}

#[derive(Debug, serde::Deserialize)]
struct ToolSourceArg {
    repo: String,
    #[serde(default, rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    subdir: Option<String>,
}

impl ToolSourceArg {
    fn into_source(self) -> Option<ToolSource> {
        Some(ToolSource {
            repo: clean_optional_value(Some(self.repo.as_str()))?,
            git_ref: self
                .git_ref
                .as_deref()
                .and_then(|value| clean_optional_value(Some(value))),
            source_subdir: self
                .subdir
                .as_deref()
                .and_then(|value| clean_optional_value(Some(value)))
                .unwrap_or_else(|| "tools".to_owned()),
        })
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

        let harness_fragments = self.harness.fragments()?;
        let mut config =
            IronProxyConfig::new(self.image.clone(), ca_cert_secret_name, ca_key_secret_name);
        config.image_pull_policy = self.image_pull_policy.clone();
        self.source.apply_to_config(&mut config);
        config.fragments = harness_fragments;
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
    /// infra and harness secrets, and every session principal is granted that
    /// role (see [`SessionRegistrar`]).
    fn roles_to_register(&self) -> Result<Vec<(RoleSpec, ProxyFragment)>, ServerError> {
        let infra = self.infra_fragment()?;
        Ok(vec![(RoleSpec::infra(), infra)])
    }

    /// The full infra fragment: the shared infra secrets plus every available
    /// harness auth fragment (also infra), selected by auth mode.
    fn infra_fragment(&self) -> Result<ProxyFragment, ServerError> {
        let mut infra = infra_fragment()?;
        for fragment in self.harness.fragments()? {
            merge_fragment(&mut infra, fragment);
        }
        Ok(infra)
    }

    /// Placeholder env (`PLACEHOLDER=PLACEHOLDER`) for the infra/harness
    /// secrets, whose consumers read credentials straight from the environment
    /// (codex's `OPENAI_API_KEY`, git's `GITHUB_TOKEN`, …). Discovered tool
    /// secrets contribute nothing here: tools read credentials through the SDK,
    /// whose `StubBackend` already returns the key name iron-proxy matches on,
    /// and the cloudwatch tool embeds its own throwaway SigV4 credentials.
    fn sandbox_placeholder_env(&self) -> Result<BTreeMap<String, String>, ServerError> {
        Ok(centaur_iron_proxy::placeholder_env(&[
            self.infra_fragment()?
        ]))
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

    /// Every harness auth fragment to register. The configured engine's
    /// fragment is required (startup fails without it, as before); the other
    /// engines' fragments are added when their engine/auth-mode pair has one,
    /// so sessions restarted onto another harness still get working
    /// credentials through the proxy.
    fn fragments(&self) -> Result<Vec<ProxyFragment>, ServerError> {
        let mut fragments = vec![self.fragment()?];
        for engine in [
            HarnessType::Codex,
            HarnessType::ClaudeCode,
            HarnessType::Amp,
        ] {
            if engine == self.engine {
                continue;
            }
            let auth_mode = harness_auth_mode_env(&engine).unwrap_or_else(|| "api_key".to_owned());
            if let Some(fragment) =
                harness_auth_fragment(harness_fragment_engine_name(&engine), &auth_mode)?
            {
                fragments.push(fragment);
            }
        }
        if let Some(fragment) = harness_auth_fragment("openrouter", "api_key")? {
            fragments.push(fragment);
        }
        // Bedrock is opt-in (not the default codex provider): only register its
        // SigV4 re-signing fragment when the operator has set CODEX_BEDROCK_REGION,
        // since the fragment expects AWS keys in the secrets backend.
        if bedrock_enabled()
            && let Some(fragment) = harness_auth_fragment("amazon-bedrock", "api_key")?
        {
            fragments.push(fragment);
        }
        Ok(fragments)
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

fn default_workflow_host_path() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("workflow-python")
        .join("workflow_host.py")
        .to_string_lossy()
        .to_string()
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

/// Map an OTLP endpoint URL onto a NetworkPolicy egress target. Only
/// in-cluster service DNS hosts (`<service>.<namespace>.svc[...]`) are mapped;
/// the namespace label is the policy's `kubernetes.io/metadata.name`
/// selector. Ports default by scheme when absent.
fn parse_otlp_egress_target(endpoint: &str) -> Option<OtlpEgressTarget> {
    let trimmed = endpoint.trim();
    let (scheme, rest) = trimmed.split_once("://").unwrap_or(("http", trimmed));
    let authority = rest.split('/').next()?.trim();
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, host_port)| host_port)
        .unwrap_or(authority);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (host_port, if scheme == "https" { 443 } else { 80 }),
    };
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 3 || labels[2] != "svc" || labels[0].is_empty() || labels[1].is_empty() {
        return None;
    }
    Some(OtlpEgressTarget {
        namespace: labels[1].to_owned(),
        port,
    })
}

fn clean_optional_value(value: Option<&str>) -> Option<String> {
    non_empty(value).map(ToOwned::to_owned)
}

fn upsert_spec_env(spec: &mut SandboxSpec, name: String, value: String) {
    if let Some(existing) = spec.env.iter_mut().find(|env| env.name == name) {
        existing.value = value;
    } else {
        spec.env
            .push(centaur_sandbox_core::EnvVar::new(name, value));
    }
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &'static str)]) -> Self {
            let saved = vars
                .iter()
                .map(|(name, _)| (*name, env::var(name).ok()))
                .collect();
            for (name, value) in vars {
                // SAFETY: tests that mutate process env hold ENV_LOCK for the
                // duration of the guard, so concurrent tests in this module
                // cannot observe partial mutations.
                unsafe {
                    env::set_var(name, value);
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..) {
                // SAFETY: see EnvGuard::set; the lock outlives the guard.
                unsafe {
                    if let Some(value) = value {
                        env::set_var(name, value);
                    } else {
                        env::remove_var(name);
                    }
                }
            }
        }
    }

    #[test]
    fn iron_control_registration_retry_policy_is_transient_only() {
        let status_error = |status| {
            RegisterError::Control(IronControlError::Status {
                method: "PUT".to_owned(),
                path: "/api/v1/static_secrets/example".to_owned(),
                status,
                body: String::new(),
            })
        };

        assert!(should_retry_iron_control_register(&status_error(500)));
        assert!(should_retry_iron_control_register(&status_error(503)));
        assert!(should_retry_iron_control_register(&status_error(429)));
        assert!(!should_retry_iron_control_register(&status_error(400)));
        assert!(!should_retry_iron_control_register(
            &RegisterError::Translate(centaur_iron_control::TranslateError::Unsupported {
                what: "unsupported transform".to_owned(),
            })
        ));
    }

    #[test]
    fn activity_summary_uses_direct_openai_key_by_default() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("OPENAI_API_KEY", "sk-test"),
            ("FIREWALL_MANAGER_SECRET_SOURCE", "env"),
            ("KUBERNETES_OP_CONNECT_HOST", ""),
            ("OP_CONNECT_TOKEN", ""),
            ("OP_VAULT", ""),
        ]);
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-activity-summary-enabled",
            "true",
        ])
        .unwrap();

        let config = args.activity_summary_config().unwrap();
        assert_eq!(config.api_key, "sk-test");
    }

    #[test]
    fn activity_summary_uses_mounted_openai_key_even_with_onepassword_connect_source() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            ("OPENAI_API_KEY", "sk-mounted"),
            ("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword-connect"),
            (
                "KUBERNETES_OP_CONNECT_HOST",
                "http://onepassword-connect:8080",
            ),
            ("OP_CONNECT_TOKEN", "op-token"),
            ("OP_VAULT", "centaur-agent"),
        ]);
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-activity-summary-enabled",
            "true",
        ])
        .unwrap();

        let config = args.activity_summary_config().unwrap();
        assert_eq!(config.api_key, "sk-mounted");
    }

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
    fn sandbox_reaper_defaults_delete_after_max_lifetime() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
        ])
        .unwrap();

        let config = args.sandbox_reaper_config();
        assert_eq!(config.max_lifetime, Some(Duration::from_secs(259_200)));
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
            "--session-sandbox-image-pull-secrets",
            "github-access-token-read-packages, extra-secret ",
            "--session-sandbox-ready-timeout-secs",
            "42",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        let config = AgentSandboxConfig::try_from(&args.sandbox).unwrap();
        assert_eq!(config.namespace, "centaur-test");
        assert_eq!(config.image_pull_policy.as_deref(), Some("IfNotPresent"));
        assert_eq!(
            config.image_pull_secrets,
            vec!["github-access-token-read-packages", "extra-secret"]
        );
        assert_eq!(config.ready_timeout, Duration::from_secs(42));
        assert!(config.iron_proxy.is_none());
    }

    #[test]
    fn tools_config_read_from_flags() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
            "--kubernetes-tools-repo",
            "paradigmxyz/centaur",
            "--kubernetes-tools-ref",
            "main",
            "--kubernetes-tools-runner-image",
            "centaur-agent:test",
            "--kubernetes-tools-repo-cache-path",
            "/var/lib/centaur/repos",
            "--kubernetes-tools-github-token-secret",
            "centaur-repo-cache-github-token",
        ])
        .unwrap();
        let config = AgentSandboxConfig::try_from(&args.sandbox).unwrap();
        let tools = config.tools.expect("tools should be Some");
        assert_eq!(tools.repo, "paradigmxyz/centaur");
        assert_eq!(tools.git_ref.as_deref(), Some("main"));
        assert_eq!(tools.source_subdir, "tools");
        assert_eq!(tools.image, "centaur-agent:test");
        assert_eq!(
            tools.repo_cache_path.as_deref(),
            Some("/var/lib/centaur/repos")
        );
        assert!(tools.auto_reload);
        let token = tools.github_token.expect("token should be Some");
        assert_eq!(token.secret_name, "centaur-repo-cache-github-token");
        assert_eq!(token.secret_key, "token");
    }

    #[test]
    fn tools_config_reads_auto_reload_flag() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
            "--kubernetes-tools-repo",
            "paradigmxyz/centaur",
            "--kubernetes-tools-runner-image",
            "centaur-agent:test",
            "--kubernetes-tools-auto-reload",
            "false",
        ])
        .unwrap();

        let config = AgentSandboxConfig::try_from(&args.sandbox).unwrap();
        let tools = config.tools.expect("tools should be Some");
        assert!(!tools.auto_reload);
    }

    #[test]
    fn agent_k8s_workflow_dirs_fan_out_across_extra_sources() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
            "--kubernetes-tools-repo",
            "paradigmxyz/centaur",
            "--kubernetes-tools-runner-image",
            "centaur-agent:test",
            "--kubernetes-tools-extra-sources",
            r#"[{"repo":"acme/overlay"},{"repo":"acme/other","subdir":"packages/tools"}]"#,
        ])
        .unwrap();

        // Every source contributes its repo-root `workflows/` tree, base first,
        // colon-joined. The tools `subdir` does not affect the workflows path.
        assert_eq!(
            args.sandbox.agent_k8s_workflow_dirs(),
            "/home/agent/github/paradigmxyz/centaur/workflows:\
             /home/agent/github/acme/overlay/workflows:\
             /home/agent/github/acme/other/workflows",
        );
    }

    #[test]
    fn agent_k8s_workflow_dirs_falls_back_when_tools_disabled() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        assert_eq!(
            args.sandbox.agent_k8s_workflow_dirs(),
            "/opt/centaur/workflows"
        );
    }

    #[test]
    fn agent_k8s_workflow_host_mounts_repos_and_tool_env() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
            "--repos-path",
            "/var/lib/centaur/repos",
            "--tools-path",
            "/home/agent/github/paradigmxyz/centaur/tools",
            "--tools-overlay-path",
            "/home/agent/github/tempoxyz/centaur-tempo/tools",
            "--kubernetes-workflow-dirs",
            "/home/agent/github/paradigmxyz/centaur/workflows:/home/agent/github/tempoxyz/centaur-tempo/workflows",
            "--session-sandbox-passthrough-env",
            "TOOLS_PATH,TOOLS_OVERLAY_PATH",
        ])
        .unwrap();

        let spec = args.sandbox.workflow_host_spec(None).unwrap();

        assert!(spec.mounts.iter().any(|mount| {
            mount.target_path == SANDBOX_REPOS_MOUNT_PATH
                && mount.read_only
                && mount.kind
                    == MountKind::Bind {
                        source_path: "/var/lib/centaur/repos".to_owned(),
                    }
        }));
        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "TOOLS_PATH")
                .map(|env| env.value.as_str()),
            Some("/home/agent/github/paradigmxyz/centaur/tools")
        );
        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "TOOLS_OVERLAY_PATH")
                .map(|env| env.value.as_str()),
            Some("/home/agent/github/tempoxyz/centaur-tempo/tools")
        );
        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "WORKFLOW_DIRS")
                .map(|env| env.value.as_str()),
            Some(
                "/home/agent/github/paradigmxyz/centaur/workflows:/home/agent/github/tempoxyz/centaur-tempo/workflows"
            )
        );
    }

    #[test]
    fn agent_k8s_workflow_host_mounts_repos_pvc_read_only() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
            "--repos-path",
            "/var/lib/centaur/repos",
            "--repos-pvc",
            "centaur-repo-cache",
        ])
        .unwrap();

        let spec = args.sandbox.workflow_host_spec(None).unwrap();

        assert!(spec.mounts.iter().any(|mount| {
            mount.target_path == SANDBOX_REPOS_MOUNT_PATH
                && mount.read_only
                && mount.kind == MountKind::NamedVolume("centaur-repo-cache".to_owned())
        }));
    }

    #[test]
    fn workflow_host_env_template_splits_passthrough_env_from_environment() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::set(&[
            (
                "SESSION_SANDBOX_PASSTHROUGH_ENV",
                "SLACK_ETL_ENABLED,SLACK_BACKFILL_ENABLED",
            ),
            ("SLACK_ETL_ENABLED", "true"),
            ("SLACK_BACKFILL_ENABLED", "true"),
        ]);
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-backend",
            "agent-k8s",
            "--session-sandbox-centaur-api-url",
            "http://centaur-api-rs:8080",
            "--kubernetes-sandbox-iron-proxy-mode",
            "disabled",
        ])
        .unwrap();

        let spec = args.sandbox.workflow_host_spec(None).unwrap();

        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "CENTAUR_API_URL")
                .map(|env| env.value.as_str()),
            Some("http://centaur-api-rs:8080")
        );
        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "SLACK_ETL_ENABLED")
                .map(|env| env.value.as_str()),
            Some("true")
        );
        assert_eq!(
            spec.env
                .iter()
                .find(|env| env.name == "SLACK_BACKFILL_ENABLED")
                .map(|env| env.value.as_str()),
            Some("true")
        );
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
        assert!(
            env.iter()
                .any(|(name, value)| name == "OPENROUTER_API_KEY" && value == "OPENROUTER_API_KEY")
        );
    }

    #[test]
    fn codex_app_server_env_template_applies_extra_env_last() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-workload",
            "codex-app-server",
            "--session-sandbox-extra-env",
            r#"[
                {"name":"OTEL_EXPORTER_OTLP_TRACES_ENDPOINT","value":"http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces"},
                {"name":"OTEL_SERVICE_NAME","value":"codex"},
                {"name":"CODEX_AUTH_MODE","value":"chatgpt"},
                {"name":"NULL_VALUE"},
                {"name":"  ","value":"skipped"},
                {"name":"BAD=NAME","value":"skipped"}
            ]"#,
        ])
        .unwrap();

        let env = args.sandbox.codex_app_server_env_template().unwrap();
        let value = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };

        assert_eq!(
            value("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            Some("http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces")
        );
        assert_eq!(value("OTEL_SERVICE_NAME"), Some("codex"));
        // Operator extra env overrides template defaults.
        assert_eq!(value("CODEX_AUTH_MODE"), Some("chatgpt"));
        // Null values become empty strings; invalid names are dropped.
        assert_eq!(value("NULL_VALUE"), Some(""));
        assert!(!env.iter().any(|(name, _)| name == "BAD=NAME"));
        // No duplicate entries for overridden names.
        assert_eq!(
            env.iter()
                .filter(|(name, _)| name == "CODEX_AUTH_MODE")
                .count(),
            1
        );
    }

    #[test]
    fn sandbox_extra_env_ignores_invalid_json() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-extra-env",
            "not-json",
        ])
        .unwrap();

        assert!(args.sandbox.sandbox_extra_env().is_empty());
    }

    #[test]
    fn sandbox_otlp_egress_target_derived_from_extra_env_endpoint() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-workload",
            "codex-app-server",
            "--session-sandbox-extra-env",
            r#"[{"name":"OTEL_EXPORTER_OTLP_TRACES_ENDPOINT","value":"http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces"}]"#,
        ])
        .unwrap();

        assert_eq!(
            args.sandbox.sandbox_otlp_egress_target().unwrap(),
            Some(OtlpEgressTarget {
                namespace: "laminar".to_owned(),
                port: 8000,
            })
        );
    }

    #[test]
    fn sandbox_otlp_egress_target_absent_for_mock_workload() {
        let mock = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-extra-env",
            r#"[{"name":"OTEL_EXPORTER_OTLP_TRACES_ENDPOINT","value":"http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces"}]"#,
        ])
        .unwrap();
        assert_eq!(mock.sandbox.sandbox_otlp_egress_target().unwrap(), None);
    }

    /// The only test that mutates the process-level OTLP env keys: keeps all
    /// assertions that depend on their presence or absence sequential so
    /// parallel tests never race on them.
    #[test]
    fn codex_app_server_env_template_forwards_process_otlp_env() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-workload",
            "codex-app-server",
        ])
        .unwrap();

        unsafe {
            for key in SANDBOX_OTLP_PASSTHROUGH_ENV_KEYS {
                env::remove_var(key);
            }
        }
        let envs = args.sandbox.codex_app_server_env_template().unwrap();
        assert!(!envs.iter().any(|(name, _)| name.starts_with("OTEL_")));
        assert_eq!(args.sandbox.sandbox_otlp_egress_target().unwrap(), None);

        unsafe {
            env::set_var(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces",
            );
            env::set_var("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer test");
        }
        let envs = args.sandbox.codex_app_server_env_template().unwrap();
        let value = |key: &str| {
            envs.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        // The harness wrapper reads these to configure codex's OTLP export
        // (endpoint + Laminar ingest auth header).
        assert_eq!(
            value("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            Some("http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces")
        );
        assert_eq!(
            value("OTEL_EXPORTER_OTLP_HEADERS"),
            Some("authorization=Bearer test")
        );
        // The egress target derives from the same forwarded endpoint.
        assert_eq!(
            args.sandbox.sandbox_otlp_egress_target().unwrap(),
            Some(OtlpEgressTarget {
                namespace: "laminar".to_owned(),
                port: 8000,
            })
        );
        unsafe {
            for key in SANDBOX_OTLP_PASSTHROUGH_ENV_KEYS {
                env::remove_var(key);
            }
        }
    }

    #[test]
    fn parse_otlp_egress_target_accepts_only_in_cluster_service_dns() {
        assert_eq!(
            parse_otlp_egress_target(
                "http://laminar-app-server.laminar.svc.cluster.local:8000/v1/traces"
            ),
            Some(OtlpEgressTarget {
                namespace: "laminar".to_owned(),
                port: 8000,
            })
        );
        assert_eq!(
            parse_otlp_egress_target("http://collector.observability.svc:4318"),
            Some(OtlpEgressTarget {
                namespace: "observability".to_owned(),
                port: 4318,
            })
        );
        assert_eq!(
            parse_otlp_egress_target("https://collector.observability.svc.cluster.local"),
            Some(OtlpEgressTarget {
                namespace: "observability".to_owned(),
                port: 443,
            })
        );
        // External hosts and bare service names never map to a namespace rule.
        assert_eq!(parse_otlp_egress_target("https://api.honeycomb.io"), None);
        assert_eq!(parse_otlp_egress_target("http://laminar:8000"), None);
        assert_eq!(
            parse_otlp_egress_target("http://laminar-app-server.laminar:8000"),
            None
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
    fn iron_control_infra_secret_sync_can_be_disabled() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--iron-control-url",
            "http://console.local",
            "--iron-control-api-key",
            "iak_test",
            "--iron-control-sync-infra-secrets",
            "false",
        ])
        .unwrap();

        assert!(!args.sandbox.iron_control_sync_infra_secrets);
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
        let SandboxWorkloadMode::CodexAppServer {
            harness, mounts, ..
        } = workload
        else {
            panic!("expected codex app server workload");
        };

        assert_eq!(harness, HarnessType::Codex);
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
    fn codex_workload_mounts_repos_pvc_read_only() {
        let args = Args::try_parse_from([
            "centaur-api-server",
            "--database-url",
            "postgres://postgres:postgres@localhost/centaur",
            "--session-sandbox-workload",
            "codex-app-server",
            "--repos-path",
            "/var/lib/centaur/repos",
            "--repos-pvc",
            "centaur-repo-cache",
        ])
        .unwrap();

        let workload = args.sandbox.container_workload_mode().unwrap();
        let SandboxWorkloadMode::CodexAppServer { mounts, .. } = workload else {
            panic!("expected codex app server workload");
        };

        assert!(mounts.iter().any(|mount| {
            mount.target_path == SANDBOX_REPOS_MOUNT_PATH
                && mount.read_only
                && mount.kind == MountKind::NamedVolume("centaur-repo-cache".to_owned())
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
