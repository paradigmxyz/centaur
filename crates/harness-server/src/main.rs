use clap::{Parser, Subcommand, ValueEnum};
use harness_server::{
    HarnessKind, Result, run_blocks_server, run_harness_server, run_hermes_blocks_server,
    run_nanocodex_blocks_server, run_validate_agent_deltas, run_validate_jsonrpc,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Serve agent harnesses over Centaur's streaming protocols."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum CliCommand {
    Codex(HarnessCommand),
    #[command(alias = "claude")]
    ClaudeCode(HarnessCommand),
    Amp(HarnessCommand),
    /// Run Nanocodex directly as a library and stream its native typed events.
    Nanocodex,
    /// Drive Hermes Agent's long-lived JSON-RPC gateway (sessions, memory,
    /// skills, crons survive across turns).
    Hermes,
    /// Drive Pi's long-lived RPC mode.
    Pi(HarnessCommand),
    ValidateJsonrpc,
    ValidateAgentDeltas,
}

#[derive(Debug, Parser)]
struct HarnessCommand {
    #[arg(long, value_enum, default_value_t = ServerMode::Blocks)]
    mode: ServerMode,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ServerMode {
    Blocks,
    Jsonrpc,
}

fn main() {
    exit_on_sigterm();
    if let Err(error) = run() {
        eprintln!("harness-server: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse()
        .command
        .unwrap_or(CliCommand::Codex(HarnessCommand {
            mode: ServerMode::Blocks,
        })) {
        CliCommand::Codex(command) => run_mode(HarnessKind::Codex, command.mode),
        CliCommand::ClaudeCode(command) => run_mode(HarnessKind::ClaudeCode, command.mode),
        CliCommand::Amp(command) => run_mode(HarnessKind::Amp, command.mode),
        CliCommand::Pi(command) => run_mode(HarnessKind::Pi, command.mode),
        CliCommand::Nanocodex => run_nanocodex_blocks_server(),
        CliCommand::Hermes => run_hermes_blocks_server(),
        CliCommand::ValidateJsonrpc => run_validate_jsonrpc(),
        CliCommand::ValidateAgentDeltas => run_validate_agent_deltas(),
    }
}

fn run_mode(kind: HarnessKind, mode: ServerMode) -> Result<()> {
    match mode {
        ServerMode::Blocks => run_blocks_server(kind),
        ServerMode::Jsonrpc => run_harness_server(kind),
    }
}

/// harness-server is PID 1 in the sandbox, and the kernel ignores SIGTERM for
/// PID 1 unless it installs a handler. Without one, stopping a sandbox waits
/// out the pod's whole termination grace period before SIGKILL. Exit on
/// SIGTERM instead; the container runtime then stops the harness with it.
fn exit_on_sigterm() {
    std::thread::spawn(|| {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            let Ok(mut terminate) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            else {
                return;
            };
            terminate.recv().await;
            std::process::exit(143);
        });
    });
}
