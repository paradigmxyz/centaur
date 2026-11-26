//! `centaur-hand`: serve one Centaur hand.

use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(about = "Serve a Centaur hand that a nanocode brain drives")]
struct Cli {
    /// Address for the hand protocol.
    #[arg(
        long,
        env = "CENTAUR_HAND_LISTEN",
        default_value = "0.0.0.0:7070",
        value_name = "ADDR"
    )]
    listen: SocketAddr,
    /// Address for `GET /healthz`.
    #[arg(
        long,
        env = "CENTAUR_HAND_HEALTH_LISTEN",
        default_value = "0.0.0.0:7071",
        value_name = "ADDR"
    )]
    health_listen: SocketAddr,
    /// File holding the hand's bearer token.
    #[arg(
        long,
        env = "CENTAUR_HAND_TOKEN_FILE",
        default_value = "/var/run/centaur-hand/token",
        value_name = "PATH"
    )]
    token_file: PathBuf,
    /// Directory the brain's tools run in.
    #[arg(
        long,
        env = "CENTAUR_HAND_WORKSPACE",
        default_value = "/home/agent/workspace",
        value_name = "PATH"
    )]
    workspace: PathBuf,
    /// Name the brain sees. Defaults to the hostname.
    #[arg(long, env = "CENTAUR_HAND_NAME", value_name = "NAME")]
    name: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let config = centaur_hand::Config {
        listen: cli.listen,
        health_listen: cli.health_listen,
        token_file: cli.token_file,
        workspace: cli.workspace,
        name: cli.name,
    };
    let mut hand = centaur_hand::start(config).await?;
    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal?,
        reason = hand.stopped() => anyhow::bail!("hand stopped serving: {reason}"),
    }
    Ok(())
}
