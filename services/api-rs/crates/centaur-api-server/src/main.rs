use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use centaur_api_server::{SandboxRuntime, build_router_with_runtime};
use centaur_sandbox_agent_k8s::{AgentSandboxBackend, AgentSandboxConfig};
use centaur_sandbox_core::SandboxSpec;
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_session_core::ThreadKey;
use centaur_session_sqlx::PgSessionStore;
use clap::{Parser, ValueEnum};
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    init_tracing();

    let args = Args::parse();

    let store = PgSessionStore::connect(&args.database_url).await?;
    if args.run_migrations {
        store.run_migrations().await?;
    }
    let sandbox_runtime = sandbox_runtime_from_args(&args).await?;

    let listener = TcpListener::bind(args.bind_addr).await?;
    info!(bind_addr = %args.bind_addr, "starting centaur api-rs server");

    axum::serve(listener, build_router_with_runtime(store, sandbox_runtime))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).json().init();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn sandbox_runtime_from_args(args: &Args) -> Result<SandboxRuntime, ServerError> {
    match args.session_sandbox_backend {
        SandboxBackendKind::Mock => Ok(SandboxRuntime::Mock),
        SandboxBackendKind::Local => Ok(SandboxRuntime::backend(
            Arc::new(LocalSandboxBackend::new()),
            local_mock_app_server_spec(),
        )),
        SandboxBackendKind::AgentK8s => {
            let image = args
                .session_sandbox_image
                .clone()
                .unwrap_or_else(|| default_sandbox_image(args.session_sandbox_workload).to_owned());
            let mut config = AgentSandboxConfig::new(args.session_sandbox_k8s_namespace.clone());
            config.image_pull_policy = args.session_sandbox_image_pull_policy.clone();
            config.ready_timeout = Duration::from_secs(args.session_sandbox_ready_timeout_secs);

            let client = if let Some(context) = args.session_sandbox_k8s_context.as_deref() {
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

            match args.session_sandbox_workload {
                SandboxWorkloadKind::Mock => Ok(SandboxRuntime::backend(
                    Arc::new(backend),
                    agent_k8s_mock_app_server_spec(&image),
                )),
                SandboxWorkloadKind::CodexAppServer => {
                    let env_template = codex_app_server_env_template(args);
                    Ok(SandboxRuntime::backend_with_spec_factory(
                        Arc::new(backend),
                        move |thread_key, _execution_id| {
                            codex_app_server_spec(&image, thread_key, &env_template)
                        },
                    ))
                }
            }
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Run the Centaur API Rust session control plane")]
struct Args {
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    #[arg(long, env = "BIND_ADDR", default_value = "127.0.0.1:8080")]
    bind_addr: SocketAddr,
    #[arg(long, env = "RUN_MIGRATIONS", default_value_t = false)]
    run_migrations: bool,
    #[arg(
        long,
        env = "SESSION_SANDBOX_BACKEND",
        value_enum,
        default_value = "mock"
    )]
    session_sandbox_backend: SandboxBackendKind,
    #[arg(
        long,
        env = "SESSION_SANDBOX_WORKLOAD",
        value_enum,
        default_value = "mock"
    )]
    session_sandbox_workload: SandboxWorkloadKind,
    #[arg(
        long,
        env = "SESSION_SANDBOX_K8S_NAMESPACE",
        default_value = "centaur-sandbox-e2e"
    )]
    session_sandbox_k8s_namespace: String,
    #[arg(long, env = "SESSION_SANDBOX_IMAGE")]
    session_sandbox_image: Option<String>,
    #[arg(long, env = "SESSION_SANDBOX_IMAGE_PULL_POLICY")]
    session_sandbox_image_pull_policy: Option<String>,
    #[arg(long, env = "SESSION_SANDBOX_READY_TIMEOUT_SECS", default_value_t = 90)]
    session_sandbox_ready_timeout_secs: u64,
    #[arg(long, env = "SESSION_SANDBOX_K8S_CONTEXT")]
    session_sandbox_k8s_context: Option<String>,
    #[arg(long, env = "SESSION_SANDBOX_CENTAUR_API_URL")]
    session_sandbox_centaur_api_url: Option<String>,
    #[arg(long, env = "CENTAUR_API_URL")]
    centaur_api_url: Option<String>,
    #[arg(long, env = "SESSION_SANDBOX_CENTAUR_API_KEY")]
    session_sandbox_centaur_api_key: Option<String>,
    #[arg(long, env = "CENTAUR_API_KEY")]
    centaur_api_key: Option<String>,
    #[arg(long, env = "SESSION_SANDBOX_PASSTHROUGH_ENV", value_delimiter = ',')]
    session_sandbox_passthrough_env: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SandboxBackendKind {
    Mock,
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

fn local_mock_app_server_spec() -> SandboxSpec {
    SandboxSpec::new("/bin/sh")
        .command(["/bin/sh", "-lc"])
        .args([mock_app_server_script()])
}

fn agent_k8s_mock_app_server_spec(image: &str) -> SandboxSpec {
    SandboxSpec::new(image)
        .command(["/bin/sh", "-lc"])
        .args([mock_app_server_script()])
}

fn codex_app_server_spec(
    image: &str,
    thread_key: &ThreadKey,
    env_template: &[(String, String)],
) -> SandboxSpec {
    let mut spec = SandboxSpec::new(image).env("CENTAUR_THREAD_KEY", thread_key.as_str());
    for (name, value) in env_template {
        spec = spec.env(name.clone(), value.clone());
    }
    spec
}

fn codex_app_server_env_template(args: &Args) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    push_env(
        &mut envs,
        "CENTAUR_API_URL",
        args.session_sandbox_centaur_api_url
            .as_deref()
            .or(args.centaur_api_url.as_deref())
            .unwrap_or("http://api:8000")
            .to_owned(),
    );
    if let Some(api_key) = args
        .session_sandbox_centaur_api_key
        .as_deref()
        .or(args.centaur_api_key.as_deref())
    {
        push_env(&mut envs, "CENTAUR_API_KEY", api_key.to_owned());
    }

    for name in &args.session_sandbox_passthrough_env {
        if let Ok(value) = env::var(&name) {
            push_env(&mut envs, &name, value);
        }
    }

    envs
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

fn mock_app_server_script() -> &'static str {
    r#"while IFS= read -r line; do
printf '%s\n' '{"type":"system","subtype":"wrapper_heartbeat","phase":"startup"}'
sleep 0.2
printf '%s\n' '{"type":"system","subtype":"wrapper_heartbeat","phase":"app_server_started"}'
sleep 0.2
printf '%s\n' '{"type":"thread.started","thread_id":"mock-codex-thread"}'
sleep 0.2
turn_index=1
while [ "$turn_index" -le 3 ]; do
  turn_id="mock-turn-$turn_index"
  printf '{"type":"turn.started","turn_id":"%s"}\n' "$turn_id"
  sleep 0.2
  printf '{"type":"item.agentMessage.delta","turnId":"%s","session_id":"mock-codex-thread","delta":"PONG %s"}\n' "$turn_id" "$turn_index"
  sleep 0.2
  printf '{"type":"turn.completed","turn":{"id":"%s"},"usage":{"input_tokens":0,"output_tokens":1}}\n' "$turn_id"
  sleep 0.2
  turn_index=$((turn_index + 1))
done
done"#
}

#[derive(Debug, Error)]
enum ServerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] centaur_session_sqlx::SessionStoreError),
    #[error(transparent)]
    Sandbox(#[from] centaur_sandbox_core::SandboxError),
    #[error(transparent)]
    KubeConfig(#[from] kube::config::KubeconfigError),
    #[error(transparent)]
    KubeInferConfig(#[from] kube::config::InferConfigError),
    #[error(transparent)]
    Kube(#[from] kube::Error),
}
