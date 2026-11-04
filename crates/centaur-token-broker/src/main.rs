mod broker;
mod client;
mod grants;
mod identifiers;
#[cfg(test)]
mod integration_tests;
mod schedule;
mod store;
mod telemetry;

use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use active_record_encryption::ActiveRecordEncryption;
use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};
use broker::Broker;
use clap::Parser;
use client::TokenClient;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::{signal::unix::SignalKind, sync::watch};
use tracing::{error, info};

#[derive(Parser)]
#[command(
    name = "centaur-token-broker",
    about = "Refresh Console's broker credentials before their access tokens expire"
)]
struct Config {
    #[arg(long, env = "IRON_CONTROL_DATABASE_URL", hide_env_values = true)]
    database_url: String,
    #[arg(long, env = "IRON_CONTROL_DATABASE_NAME")]
    database_name: Option<String>,
    #[arg(
        long,
        env = "IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY",
        hide_env_values = true
    )]
    active_record_primary_key: String,
    #[arg(
        long,
        env = "IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT",
        hide_env_values = true
    )]
    active_record_key_derivation_salt: String,
    /// Seconds between checks for credentials due for refresh.
    #[arg(long, env = "TOKEN_BROKER_POLL_INTERVAL_SECONDS", default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
    poll_interval_seconds: u64,
    /// Credentials refreshed at once. Each holds a database connection for the
    /// duration of its token request.
    #[arg(long, env = "TOKEN_BROKER_CONCURRENCY", default_value_t = 4, value_parser = clap::value_parser!(u64).range(1..))]
    concurrency: u64,
    #[arg(long, env = "DATABASE_MAX_CONNECTIONS", default_value_t = 5)]
    database_max_connections: u32,
    /// Serves /healthz and /metrics.
    #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
    bind_addr: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Config::parse();
    let metrics = telemetry::init_metrics()?;
    // Starts the last-poll clock, so a service that never completes a poll
    // still falls behind.
    telemetry::record_tick_completed();

    let mut connect_options = PgConnectOptions::from_str(&config.database_url)?;
    if let Some(name) = config
        .database_name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        connect_options = connect_options.database(name);
    }
    let broker = Broker {
        pool: PgPoolOptions::new()
            .max_connections(config.database_max_connections)
            .connect_with(connect_options)
            .await?,
        encryption: Arc::new(ActiveRecordEncryption::new(
            &config.active_record_primary_key,
            &config.active_record_key_derivation_salt,
        )),
        client: TokenClient::new()?,
        concurrency: config.concurrency as usize,
    };

    let (shutdown_sender, shutdown) = watch::channel(false);
    let mut refresh_loop = tokio::spawn(run(
        broker,
        Duration::from_secs(config.poll_interval_seconds),
        shutdown.clone(),
    ));
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route(
            "/metrics",
            get(move || async move {
                (
                    [("Content-Type", "text/plain; version=0.0.4; charset=utf-8")],
                    metrics.render(),
                )
                    .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    info!(bind = %config.bind_addr, "token broker listening");
    let mut server_shutdown = shutdown.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = server_shutdown.wait_for(|stopping| *stopping).await;
            })
            .await
    });

    // Exit, so Kubernetes restarts the pod, if the refresh loop or the server
    // stops on its own, for example on a panic.
    let mut terminate = tokio::signal::unix::signal(SignalKind::terminate())?;
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
        result = &mut refresh_loop => return Err(format!("refresh loop stopped: {result:?}").into()),
        result = &mut server => return Err(format!("server stopped: {result:?}").into()),
    }
    info!("shutting down after in-flight refreshes");
    shutdown_sender.send_replace(true);
    refresh_loop.await?;
    server.await??;
    Ok(())
}

async fn run(broker: Broker, poll_interval: Duration, mut shutdown: watch::Receiver<bool>) {
    loop {
        if let Err(error) = broker.tick(&shutdown).await {
            error!(%error, "broker credential refresh tick failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(poll_interval) => {}
            _ = shutdown.wait_for(|stopping| *stopping) => return,
        }
    }
}
