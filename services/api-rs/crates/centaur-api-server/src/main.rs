mod args;

use centaur_api_server::build_router_with_runtime;
use centaur_session_sqlx::PgSessionStore;
use clap::Parser;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt as tracing_fmt};

use args::Args;

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    init_crypto_provider();
    init_tracing();

    let args = Args::parse();

    let store = PgSessionStore::connect(&args.server.database_url).await?;
    if args.server.run_migrations {
        store.run_migrations().await?;
    }
    let sandbox_runtime = args.sandbox_runtime().await?;

    let listener = TcpListener::bind(args.server.bind_addr).await?;
    info!(bind_addr = %args.server.bind_addr, "starting centaur api-rs server");

    axum::serve(listener, build_router_with_runtime(store, sandbox_runtime))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_fmt().with_env_filter(filter).json().init();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
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
