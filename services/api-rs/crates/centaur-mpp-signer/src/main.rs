use std::{sync::Arc, time::Duration};

use centaur_mpp_signer::{
    AppState, PgSignerStore, Registry, TempoChargeSigner, build_router, config::Args,
    describe_metrics,
};
use centaur_session_sqlx::PgSessionStore;
use centaur_telemetry::{TelemetryConfig, init_telemetry};
use clap::Parser as _;
use time::Duration as TimeDuration;
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let telemetry = init_telemetry(TelemetryConfig::from_env())?;
    describe_metrics();

    let args = Args::parse();
    args.validate()?;
    let session_store = PgSessionStore::connect(&args.database_url).await?;
    if args.run_migrations {
        session_store.run_migrations().await?;
    }
    let store = Arc::new(PgSignerStore::new(session_store.pool().clone()));
    let registry = Arc::new(Registry::new(
        store.clone(),
        &args.registry_url,
        TimeDuration::seconds(args.registry_cache_ttl_seconds),
        TimeDuration::seconds(args.registry_max_stale_seconds),
    )?);
    let signer = Arc::new(TempoChargeSigner::new(args.signer()?, &args.rpc_url)?);
    let policy_rules = args.policy_rules()?;
    let state = AppState::new(
        args.signer_token,
        store,
        registry,
        args.default_methods,
        policy_rules,
        signer,
        args.max_per_charge_atomic,
        args.max_daily_atomic,
    )?;

    if state.budgets_disabled() {
        tracing::warn!("MPP software budgets are disabled; wallet balance is the hard boundary");
        metrics::gauge!("centaur_mpp_budget_configured").set(0.0);
    } else {
        metrics::gauge!("centaur_mpp_budget_configured").set(1.0);
    }

    let listener = TcpListener::bind(args.bind_addr).await?;
    info!(bind_addr = %args.bind_addr, "starting Centaur MPP signer");
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    telemetry.shutdown();
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(signal) = sigterm.as_mut() {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
}
