//! `centaur-codemode`: run agent-written JavaScript cells against TypeScript
//! connectors.
//!
//! ```text
//! centaur-codemode exec  [-e CODE | -]   run one cell and print the JSON response
//! centaur-codemode brief                 print the typed catalog an agent reads
//! centaur-codemode serve                 HTTP API for api-rs
//! ```

mod catalog;
mod engine;
mod server;

use std::{io::Read, net::SocketAddr, path::PathBuf, sync::Arc};

use clap::{Args, Parser, Subcommand};
use eyre::{Result, WrapErr, bail};

use crate::{
    catalog::Catalog,
    engine::{Engine, ExecRequest},
};

#[derive(Parser)]
#[command(version, about = "Run Code Mode cells against TypeScript connectors")]
struct Cli {
    #[command(flatten)]
    common: Common,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct Common {
    /// Connector roots, in overlay order (later roots shadow earlier ones).
    #[arg(
        long = "connectors",
        env = "CENTAUR_CODEMODE_CONNECTORS",
        value_delimiter = ':',
        default_value = "connectors",
        global = true
    )]
    roots: Vec<PathBuf>,
    /// Extra PEM roots to trust, such as the iron-proxy CA.
    #[arg(long, env = "CENTAUR_CODEMODE_CA_FILE", global = true)]
    ca_file: Option<PathBuf>,
    /// Sandbox context file, given to connectors as `ctx.context`. Defaults
    /// to `~/.centaur/sandbox.json`, where harness-server writes it.
    #[arg(long, env = "CENTAUR_SANDBOX_CONTEXT_FILE", global = true)]
    context_file: Option<PathBuf>,
    /// Connectors to expose (comma-separated); all when unset.
    #[arg(long, value_delimiter = ',', global = true)]
    tools: Option<Vec<String>>,
}

#[derive(Subcommand)]
enum Command {
    /// Run one cell and print the JSON response.
    Exec {
        /// Cell source; `-` (or nothing) reads standard input.
        #[arg(short = 'e', long = "code")]
        code: Option<String>,
        /// Print only the result (or the error) instead of the full response.
        #[arg(long)]
        result_only: bool,
    },
    /// Print the typed catalog an agent reads before writing a cell.
    Brief,
    /// Serve the HTTP API.
    Serve {
        #[arg(
            long,
            env = "CENTAUR_CODEMODE_LISTEN",
            default_value = "127.0.0.1:8790"
        )]
        listen: SocketAddr,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    let Cli { common, command } = cli;
    let catalog = Arc::new(Catalog::load(&common.roots)?);
    let allowed = common.tools.as_deref();
    match command {
        Command::Brief => {
            print!("{}", catalog.brief(allowed));
            Ok(())
        }
        Command::Exec { code, result_only } => {
            let code = match code.as_deref() {
                Some("-") | None => {
                    let mut code = String::new();
                    std::io::stdin().read_to_string(&mut code)?;
                    code
                }
                Some(code) => code.to_owned(),
            };
            let engine = engine(&common, catalog)?;
            let response = engine
                .exec(ExecRequest {
                    code,
                    tools: common.tools.clone(),
                })
                .await;
            let ok = response.ok;
            if result_only {
                match (&response.result, &response.error) {
                    (Some(result), _) => println!("{}", serde_json::to_string_pretty(result)?),
                    (None, Some(error)) => eprintln!("{}", error.message),
                    (None, None) => {}
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            if !ok {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Serve { listen } => {
            let engine = engine(&common, catalog)?;
            let listener = tokio::net::TcpListener::bind(listen)
                .await
                .wrap_err_with(|| format!("binding {listen}"))?;
            tracing::warn!(%listen, "centaur-codemode serving");
            axum::serve(listener, server::router(engine))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            Ok(())
        }
    }
}

fn engine(common: &Common, catalog: Arc<Catalog>) -> Result<Engine> {
    if catalog.connectors.is_empty() {
        bail!(
            "no connectors found under {}",
            common
                .roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(":")
        );
    }
    let context_file = common.context_file.clone().or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".centaur/sandbox.json"))
    });
    Engine::new(catalog, common.ca_file.as_deref(), context_file)
}
