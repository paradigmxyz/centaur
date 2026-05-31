use clap::{Parser, Subcommand};
use harness_server::{
    HarnessKind, Result, run_harness_server, run_validate_agent_deltas, run_validate_jsonrpc,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Serve harness CLIs through the Codex App Server V2 protocol."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum CliCommand {
    Codex,
    #[command(alias = "claude")]
    ClaudeCode,
    Amp,
    ValidateJsonrpc,
    ValidateAgentDeltas,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("harness-server: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command.unwrap_or(CliCommand::Codex) {
        CliCommand::Codex => run_harness_server(HarnessKind::Codex),
        CliCommand::ClaudeCode => run_harness_server(HarnessKind::ClaudeCode),
        CliCommand::Amp => run_harness_server(HarnessKind::Amp),
        CliCommand::ValidateJsonrpc => run_validate_jsonrpc(),
        CliCommand::ValidateAgentDeltas => run_validate_agent_deltas(),
    }
}
