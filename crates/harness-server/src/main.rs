use std::time::{Duration, Instant};

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
/// out the pod's whole termination grace period before SIGKILL. On SIGTERM,
/// stop the harness processes, flush telemetry, and exit.
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
            if std::process::id() == 1 {
                stop_harness_processes();
            }
            harness_server::flush_telemetry();
            std::process::exit(143);
        });
    });
}

/// How long harnesses get to finish writing their session state on shutdown;
/// well inside the pod's 30s termination grace period.
const HARNESS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Forwards SIGTERM to every other process in the sandbox and reaps them until
/// none are left or the timeout passes. Only valid as PID 1: there, `kill(-1)`
/// reaches exactly the container's other processes.
fn stop_harness_processes() {
    // SAFETY: kill and waitpid take no pointers we own; a null status is allowed.
    unsafe {
        libc::kill(-1, libc::SIGTERM);
    }
    let deadline = Instant::now() + HARNESS_SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        // SAFETY: as above.
        match unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } {
            // No children left to wait for.
            pid if pid < 0 => return,
            0 => std::thread::sleep(Duration::from_millis(50)),
            _ => {}
        }
    }
}
