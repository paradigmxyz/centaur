//! `centaur-console`: the console service (RFC 0006).
//!
//! - `serve`: prepare the database (create it if missing, then migrate),
//!   bootstrap the first operator, and serve HTTP on `PORT` (3000);
//! - `worker`: the background loops (bootstrap too once the schema is
//!   current);
//! - `migrate`, `bootstrap`: the startup steps alone;
//! - `service-token`: print a short-lived console service JWT for api-rs
//!   admin calls.

use std::{net::SocketAddr, process::ExitCode};

use centaur_console::{bootstrap, config, startup, worker};
use centaur_console_api::{
    AppState, Config, Routes, assets::Assets, env::Env, hosts::HostAllowlist, state::Inner,
};
use centaur_console_auth::{
    flow_cookie::FlowCookies, session::SessionConfig, state_token::StateSigner,
    tokens::SandboxJwtConfig,
};
use centaur_console_db::{Clock, Encryptor, migrate};
use clap::{Parser, Subcommand};
use sqlx::{PgPool, postgres::PgConnectOptions};
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "centaur-console", about = "The Centaur console", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare the database, bootstrap, and serve HTTP.
    Serve {
        /// Listen address (default 0.0.0.0:$PORT).
        #[arg(long)]
        bind: Option<SocketAddr>,
    },
    /// Run the background loops.
    Worker,
    /// Create the database if missing and apply migrations (adopting a
    /// database left at the baseline schema by the retired Rails console).
    Migrate,
    /// Create the initial operator from CENTAUR_CONSOLE_INITIAL_USER_*.
    Bootstrap,
    /// Print a short-lived console service token for api-rs admin routes,
    /// signed with CENTAUR_JWT_SIGNING_SECRET.
    ServiceToken,
}

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Migration(#[from] migrate::MigrationError),
    #[error(transparent)]
    Bootstrap(#[from] bootstrap::BootstrapError),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Startup(#[from] startup::StartupError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Telemetry(#[from] centaur_telemetry::TelemetryError),
}

fn main() -> ExitCode {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match runtime.block_on(run(Cli::parse())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            error!("{failure}");
            eprintln!("centaur-console: {failure}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Failure> {
    let mut telemetry_config = centaur_telemetry::TelemetryConfig::from_env();
    if std::env::var("OTEL_SERVICE_NAME").is_err() {
        telemetry_config.service_name = "centaur-console".into();
    }
    let telemetry = centaur_telemetry::init_telemetry(telemetry_config)?;
    let env = Env::from_process();
    let result = match cli.command {
        Command::Serve { bind } => serve(&env, bind).await,
        Command::Worker => run_worker(&env).await,
        Command::Migrate => {
            let pool = prepare_database(&env).await?;
            let outcome = migrate::migrate(&pool).await?;
            info!(?outcome, "console schema is current");
            Ok(())
        }
        Command::Bootstrap => {
            let pool = connect(&env).await?;
            let clock = Clock::system();
            let outcome = bootstrap::run(&pool, &env, &clock).await?;
            info!(?outcome, "bootstrap finished");
            Ok(())
        }
        Command::ServiceToken => {
            let secret = env.var("CENTAUR_JWT_SIGNING_SECRET").unwrap_or_default();
            let now = Clock::system().now();
            match centaur_console_auth::tokens::console_service(secret, now) {
                Ok(token) => {
                    println!("{token}");
                    Ok(())
                }
                Err(_) => {
                    Err(config::ConfigError("CENTAUR_JWT_SIGNING_SECRET is not set".into()).into())
                }
            }
        }
    };
    telemetry.shutdown();
    result
}

async fn connect(env: &Env) -> Result<PgPool, Failure> {
    let options = config::primary_database(env)?;
    Ok(config::pool_options(env).connect_with(options).await?)
}

/// Connects, creating the database first when it is missing.
async fn prepare_database(env: &Env) -> Result<PgPool, Failure> {
    Ok(startup::prepare_database(env).await?)
}

fn app_state(env: &Env, pool: PgPool, clock: Clock) -> Result<AppState, Failure> {
    let secret_key_base = config::secret_key_base(env)?;
    let keys = config::encryption_keys(env)?;
    let (public_dir, spa_index) = config::asset_paths(env);
    let api_db = config::api_database(env)
        .ok()
        .map(|options: PgConnectOptions| config::pool_options(env).connect_lazy_with(options));
    let config = Config {
        public_url: env.present("PUBLIC_URL").map(str::to_owned),
        jwt_signing_secret: env
            .var("CENTAUR_JWT_SIGNING_SECRET")
            .unwrap_or_default()
            .to_owned(),
        sandbox_jwt: SandboxJwtConfig::new(
            env.var("CENTAUR_SANDBOX_ENTITLEMENTS_JWT_AUDIENCE"),
            env.var("CENTAUR_SANDBOX_ENTITLEMENTS_JWT_ISSUER"),
        ),
        public_dir,
        spa_index,
        env: env.clone(),
    };
    Ok(AppState::new(Inner {
        db: pool,
        api_db,
        assets: Assets::new(&config.public_dir),
        http: centaur_console_api::http::client(env)
            .map_err(|error| config::ConfigError(error.to_string()))?,
        hosts: HostAllowlist::from_config(&config),
        config,
        encryptor: Encryptor::new(&keys.primary_key, &keys.key_derivation_salt),
        clock,
        sessions: SessionConfig::new(&secret_key_base),
        flow_cookies: FlowCookies::new(&secret_key_base),
        signer: StateSigner::new(&secret_key_base),
    }))
}

/// Every area's routes.
fn routes(env: &Env) -> Routes {
    use centaur_console_api::{automation, identity, misc, oauth, principals, secrets};
    let api_clients = identity::settings::api_clients_enabled(env);
    identity::routes()
        .merge(principals::routes())
        .merge(secrets::routes())
        .merge(misc::routes())
        .merge(oauth::routes())
        .merge(automation::routes(api_clients))
}

async fn serve(env: &Env, bind: Option<SocketAddr>) -> Result<(), Failure> {
    let clock = Clock::system();
    // Fail on missing secrets before touching the database.
    config::secret_key_base(env)?;
    config::encryption_keys(env)?;
    let pool = prepare_database(env).await?;
    let outcome = migrate::migrate(&pool).await?;
    info!(?outcome, "console schema is current");
    bootstrap::run(&pool, env, &clock).await?;
    let state = app_state(env, pool, clock)?;
    let app = centaur_console_api::app::router(state, routes(env));
    let address =
        bind.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], config::port(env).unwrap_or(3000))));
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(%address, "centaur-console listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn run_worker(env: &Env) -> Result<(), Failure> {
    let clock = Clock::system();
    let pool = connect(env).await?;
    // Bootstrap, but only once migrations are applied.
    if migrate::is_current(&pool).await? {
        bootstrap::run(&pool, env, &clock).await?;
    }
    let state = app_state(env, pool, clock)?;
    worker::run(state, shutdown_signal()).await;
    Ok(())
}

/// SIGINT or SIGTERM (Kubernetes' termination signal).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
