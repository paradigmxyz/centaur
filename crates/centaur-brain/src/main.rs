//! centaur-brain: runs agent loops for harness `nanocode` outside sandboxes.

mod blocks;
mod migrate;
mod projector;
mod server;
mod translate;
mod worker;

use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Runs Centaur agent loops outside sandboxes")]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Serve the brain API for api-rs.
    Serve(ServeArgs),
    /// Apply the brain's schema migrations with the database owner's
    /// credentials, then exit.
    Migrate(MigrateArgs),
}

#[derive(clap::Args)]
struct MigrateArgs {
    /// Postgres DSN of a role that may create schemas and roles.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,
}

#[derive(clap::Args)]
struct ServeArgs {
    #[arg(long, env = "BRAIN_LISTEN", default_value = "0.0.0.0:8790")]
    listen: String,
    /// Postgres DSN for durable sessions; in-memory when unset.
    #[arg(long, env = "BRAIN_DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,
    #[arg(long, env = "BRAIN_DEFAULT_MODEL", default_value = "luna")]
    default_model: String,
    #[arg(long, env = "BRAIN_DEFAULT_THINKING", default_value = "low")]
    thinking: String,
    #[arg(long, env = "OPENAI_BASE_URL")]
    openai_base_url: Option<String>,
    #[arg(long, env = "ANTHROPIC_BASE_URL")]
    anthropic_base_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = match Cli::parse().command {
        CliCommand::Serve(args) => args,
        CliCommand::Migrate(args) => {
            migrate::run(&args.database_url).await?;
            tracing::info!("brain migrations applied");
            return Ok(());
        }
    };
    let (commands, receiver) = mpsc::unbounded_channel();
    let _worker = worker::spawn(
        worker::Config {
            database_url: args.database_url,
            default_model: args.default_model,
            thinking: args.thinking,
            openai_base_url: args.openai_base_url,
            anthropic_base_url: args.anthropic_base_url,
        },
        receiver,
    )?;
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    tracing::info!(listen = %args.listen, "centaur-brain listening");
    axum::serve(listener, server::router(server::AppState { commands }))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
