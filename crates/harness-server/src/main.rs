use clap::{Parser, Subcommand, ValueEnum};
use harness_server::{
    HarnessKind, Result, run_blocks_server, run_harness_server, run_nanocodex_blocks_server,
    run_validate_agent_deltas, run_validate_jsonrpc,
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
        CliCommand::Nanocodex => run_nanocodex_blocks_server(),
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
