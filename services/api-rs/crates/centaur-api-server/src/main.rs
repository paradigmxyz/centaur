mod activity_summary;
mod args;

use centaur_api_server::{AppState, build_router_with_app_state};
use centaur_session_runtime::SessionRuntime;
use centaur_session_sqlx::PgSessionStore;
use centaur_telemetry::{TelemetryConfig, init_telemetry};
use centaur_workflows::WorkflowRuntime;
use clap::Parser;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::info;

use args::Args;

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    init_crypto_provider();
    let telemetry = init_telemetry(TelemetryConfig::from_env())?;

    let args = Args::parse();
    let listener = TcpListener::bind(args.server.bind_addr).await?;
    info!(
        bind_addr = %args.server.bind_addr,
        "starting centaur api-rs server"
    );

    let app_state = AppState::unready();
    let app = build_router_with_app_state(app_state.clone());
    let shutdown_state = app_state.clone();
    let drain_timeout = args.shutdown_execution_drain_timeout();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_signal().await;
                info!("shutdown signal received; handing off in-flight executions");
                // Hand off before axum starts draining connections: open SSE
                // streams can keep the server future alive until SIGKILL, and
                // the lease release must not be lost to that.
                if let Some(runtime) = shutdown_state.session_runtime() {
                    runtime.handoff_owned_executions(drain_timeout).await;
                }
            })
            .await
    });

    tokio::select! {
        result = &mut server => {
            result??;
            telemetry.shutdown();
            return Ok(());
        }
        result = initialize_runtime(args, app_state) => {
            if let Err(error) = result {
                server.abort();
                telemetry.shutdown();
                return Err(error);
            }
        }
    }

    server.await??;
    telemetry.shutdown();
    Ok(())
}

async fn initialize_runtime(args: Args, app_state: AppState) -> Result<(), ServerError> {
    let store = PgSessionStore::connect(&args.server.database_url).await?;
    if args.server.run_migrations {
        store.run_migrations().await?;
    }
    if let Some(config) = args.activity_summary_config() {
        let worker = activity_summary::ActivitySummaryWorker::new(store.clone(), config)?;
        tokio::spawn(worker.run());
    }
    let pool = store.pool().clone();
    let sandbox_runtime = args.sandbox_runtime().await?;
    let mut runtime = SessionRuntime::new(store.clone(), sandbox_runtime)
        .with_openai_session_title_generator_from_env();
    let mut warm_pool_bootstrap_principal = None;
    let mut workflow_host_principal = None;
    let mut workflow_principal_registrar = None;
    if let Some(iron_control) = args.iron_control_runtime().await? {
        info!("iron-control session registration enabled");
        warm_pool_bootstrap_principal = Some(iron_control.warm_pool_bootstrap_principal);
        workflow_host_principal = Some(iron_control.workflow_host_principal);
        workflow_principal_registrar = Some(iron_control.workflow_principal_registrar);
        runtime = runtime.with_iron_control(iron_control.registrar);
    }
    runtime = runtime.with_personas(args.persona_registry()?);
    let sandbox_capacity_config = args.sandbox_capacity_config();
    if let Some(config) = sandbox_capacity_config {
        runtime = runtime.with_sandbox_capacity(config);
    }
    if let Some(mut config) = args.warm_pool_config() {
        config.bootstrap_iron_control_principal = warm_pool_bootstrap_principal.clone();
        runtime = runtime.with_warm_pool(config);
    }
    runtime = runtime.with_sandbox_reaper(args.sandbox_reaper_config());
    runtime = runtime.with_sandbox_cleanup(args.sandbox_cleanup_config());
    let workflow_host_sandbox = args
        .workflow_host_sandbox_runtime(workflow_host_principal.as_deref())
        .await?;
    let workflows = Some(
        WorkflowRuntime::new_with_workflow_host_sandbox_and_principal_registrar(
            store,
            runtime.clone(),
            workflow_host_sandbox,
            workflow_principal_registrar,
        )
        .await?,
    );

    // Adopt executions orphaned by another control plane process
    // (deploy/crash): recover finished turns from recorded sandbox output,
    // re-attach still running sandboxes, and fail the rest so their threads
    // unwedge. The scan re-runs periodically because executions can be
    // orphaned after startup — e.g. a rolling deploy terminates the previous
    // pod mid-turn after this pod's startup scan already ran.
    match args.execution_adoption_interval() {
        Some(interval) => runtime.spawn_orphan_adoption(interval),
        None => {
            let adoption_runtime = runtime.clone();
            tokio::spawn(async move {
                adoption_runtime.adopt_orphaned_executions().await;
            });
        }
    }

    app_state.mark_ready(runtime, workflows, Some(pool));
    info!("centaur api-rs runtime initialized");
    Ok(())
}

fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Resolves on SIGINT (Ctrl-C) or, on Unix, SIGTERM — the signal Kubernetes
/// sends on pod termination. The binary runs as PID 1 in its container, and
/// PID 1 ignores signals without installed handlers: without the SIGTERM arm
/// every rollout burned the full termination grace period and ended in
/// SIGKILL, never reaching the graceful shutdown path.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(sigterm) => sigterm,
            Err(error) => {
                tracing::warn!(%error, "failed to install SIGTERM handler; using ctrl-c only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Store(#[from] centaur_session_sqlx::SessionStoreError),
    #[error(transparent)]
    Workflows(#[from] centaur_workflows::WorkflowRuntimeError),
    #[error(transparent)]
    KubeConfig(#[from] kube::config::KubeconfigError),
    #[error(transparent)]
    KubeInferConfig(#[from] kube::config::InferConfigError),
    #[error(transparent)]
    Kube(#[from] kube::Error),
    #[error(transparent)]
    IronProxy(#[from] centaur_iron_proxy::IronProxyConfigError),
    #[error(transparent)]
    IronControl(#[from] centaur_iron_control::IronControlError),
    #[error(transparent)]
    IronControlRegister(#[from] centaur_iron_control::RegisterError),
    #[error(transparent)]
    Telemetry(#[from] centaur_telemetry::TelemetryError),
    #[error(transparent)]
    ToolDiscovery(#[from] centaur_api_server::ToolDiscoveryError),
    #[error(transparent)]
    ActivitySummary(#[from] activity_summary::ActivitySummaryError),
    #[error("tool source error: {0}")]
    ToolSource(String),
    #[error("iron-proxy requires both firewall CA cert and key Secret names")]
    MissingIronProxyCaSecret,
    #[error("{0}")]
    UnsupportedConfig(String),
}
