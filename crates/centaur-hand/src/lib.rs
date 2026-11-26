//! `centaur-hand`: the daemon side of a Centaur hand.
//!
//! A hand is a single-use Linux box that a nanocode brain drives over the hand
//! protocol. This daemon owns three things:
//!
//! - the nanocode hand listener, which executes the brain's tool calls in the
//!   workspace;
//! - a plaintext public listener for that protocol, on
//!   `CENTAUR_HAND_LISTEN`/`0.0.0.0:7070`;
//! - `GET /healthz` on `CENTAUR_HAND_HEALTH_LISTEN`, ready once the public
//!   listener is serving.
//!
//! nanocode's plaintext listener only binds a loopback address (a remote hand
//! is expected to sit behind a TLS terminator). A hand pod is addressed
//! directly by its pod IP, so the daemon listens on an ephemeral loopback port
//! and forwards the public listener to it byte for byte. The hand token still
//! authenticates the WebSocket handshake in nanocode.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tracing::{info, warn};

/// Everything the daemon needs to serve one hand.
#[derive(Clone, Debug)]
pub struct Config {
    /// Public hand-protocol address, normally `0.0.0.0:7070`.
    pub listen: SocketAddr,
    /// Readiness probe address, normally `0.0.0.0:7071`.
    pub health_listen: SocketAddr,
    /// File holding the hand token. It is read once and never logged.
    pub token_file: PathBuf,
    /// Directory the brain's tools run in.
    pub workspace: PathBuf,
    /// Name the brain sees; nanocode derives one from the hostname when unset.
    pub name: Option<String>,
}

/// A running hand daemon.
///
/// Dropping this stops the health and public listeners. The nanocode listener
/// runs on its own executor thread for the life of the process.
pub struct RunningHand {
    listen: SocketAddr,
    health: SocketAddr,
    ready: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
    stopped: tokio::sync::mpsc::UnboundedReceiver<Result<SocketAddr, String>>,
}

impl RunningHand {
    /// The bound public hand-protocol address.
    #[must_use]
    pub const fn listen(&self) -> SocketAddr {
        self.listen
    }

    /// The bound readiness probe address.
    #[must_use]
    pub const fn health(&self) -> SocketAddr {
        self.health
    }

    /// Wait until nanocode's listener stops, and return why. Readiness drops
    /// first, so a hand that can no longer serve never looks healthy.
    pub async fn stopped(&mut self) -> String {
        let reason = loop {
            match self.stopped.recv().await {
                Some(Ok(_)) => {}
                Some(Err(reason)) => break reason,
                None => break "the hand listener thread exited".to_owned(),
            }
        };
        self.ready.store(false, Ordering::SeqCst);
        reason
    }
}

impl Drop for RunningHand {
    fn drop(&mut self) {
        self.ready.store(false, Ordering::SeqCst);
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Read the hand token, refusing a missing, empty, or unreadable file.
pub fn read_token(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read hand token from {}", path.display()))?;
    let token = raw.trim();
    if token.is_empty() {
        bail!("hand token file {} is empty", path.display());
    }
    Ok(token.to_owned())
}

/// Bind the listeners, start the hand, and return once it is serving.
///
/// # Errors
///
/// Fails if the token is missing or empty, either socket cannot bind, the
/// workspace cannot be created, or nanocode's listener fails to start.
pub async fn start(config: Config) -> Result<RunningHand> {
    let token = read_token(&config.token_file)?;
    std::fs::create_dir_all(&config.workspace)
        .with_context(|| format!("create workspace {}", config.workspace.display()))?;

    let ready = Arc::new(AtomicBool::new(false));
    let health_listener = TcpListener::bind(config.health_listen)
        .await
        .with_context(|| format!("bind health listener on {}", config.health_listen))?;
    let health_addr = health_listener.local_addr()?;
    let health_task = tokio::spawn(serve_health(health_listener, Arc::clone(&ready)));

    let public_listener = match TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            health_task.abort();
            return Err(error).with_context(|| format!("bind hand listener on {}", config.listen));
        }
    };
    let public_addr = public_listener.local_addr()?;

    let (internal, stopped) = match spawn_hand(config.clone(), token).await {
        Ok(started) => started,
        Err(error) => {
            health_task.abort();
            return Err(error);
        }
    };

    let proxy_task = tokio::spawn(serve_public(public_listener, internal));
    ready.store(true, Ordering::SeqCst);
    info!(
        listen = %public_addr,
        health = %health_addr,
        workspace = %config.workspace.display(),
        "centaur-hand serving"
    );
    Ok(RunningHand {
        listen: public_addr,
        health: health_addr,
        ready,
        tasks: vec![health_task, proxy_task],
        stopped,
    })
}

/// Start nanocode's hand listener on an ephemeral loopback port and wait for
/// it to report that the port is bound.
async fn spawn_hand(
    config: Config,
    token: String,
) -> Result<(
    SocketAddr,
    tokio::sync::mpsc::UnboundedReceiver<Result<SocketAddr, String>>,
)> {
    let (startup_tx, mut startup_rx) =
        tokio::sync::mpsc::unbounded_channel::<Result<SocketAddr, String>>();
    let event_tx = startup_tx.clone();
    let thread = std::thread::Builder::new()
        .name("centaur-hand".into())
        .spawn(move || {
            let runtime = match nanocode_executor::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = startup_tx.send(Err(format!("nanocode executor: {error}")));
                    return;
                }
            };
            choose_shell();
            let workspace = config.workspace;
            let name = config.name;
            let outcome: Result<(), String> = runtime.block_on(async move {
                let mut hand = match nanocode::hand::listen(
                    SocketAddr::from(([127, 0, 0, 1], 0)),
                    token,
                    workspace,
                ) {
                    Ok(hand) => hand,
                    Err(error) => return Err(error.to_string()),
                };
                if let Some(name) = name {
                    hand = hand.name(name);
                }
                let mut reported = false;
                hand.run(move |event| {
                    log_event(&event);
                    if let nanocode::hand::HandEvent::Listening { endpoint } = &event
                        && !reported
                    {
                        reported = true;
                        let _ = event_tx.send(endpoint_address(endpoint).ok_or_else(|| {
                            format!("nanocode reported an unparsable listener endpoint {endpoint}")
                        }));
                    }
                })
                .await
                .map_err(|error| error.to_string())
            });
            match outcome {
                Ok(()) => {
                    let _ = startup_tx.send(Err("the hand listener stopped".into()));
                }
                Err(error) => {
                    warn!(%error, "hand listener failed");
                    let _ = startup_tx.send(Err(error));
                }
            }
        })
        .context("spawn hand listener thread")?;
    // The thread is detached: a hand serves for the life of the process.
    drop(thread);
    match startup_rx.recv().await {
        Some(Ok(internal)) => Ok((internal, startup_rx)),
        Some(Err(error)) => bail!("hand listener failed to start: {error}"),
        None => bail!("hand listener thread exited before reporting its address"),
    }
}

/// nanocode's workspace process tools refuse to run until the process selects
/// the shell they spawn. A hand image ships bash, so select it directly: the
/// `psh` shell is the nanocode CLI's own binary and has no place in this daemon.
fn choose_shell() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let program = if Path::new("/bin/bash").is_file() {
            PathBuf::from("/bin/bash")
        } else {
            PathBuf::from("/bin/sh")
        };
        let shell = nanocode_home::shell::Shell::psh(program, Vec::new());
        if let Err(chosen) = nanocode_home::shell::choose(shell) {
            info!(shell = %chosen.path().display(), "hand shell already selected");
        }
    });
}

fn endpoint_address(endpoint: &str) -> Option<SocketAddr> {
    endpoint
        .strip_prefix("ws://")?
        .trim_end_matches('/')
        .parse()
        .ok()
}

fn log_event(event: &nanocode::hand::HandEvent) {
    use nanocode::hand::HandEvent;
    match event {
        HandEvent::Connecting { endpoint } => info!(%endpoint, "hand connecting"),
        HandEvent::Connected { endpoint, .. } => info!(%endpoint, "hand attached"),
        HandEvent::Moving { endpoint } => info!(%endpoint, "hand moving"),
        HandEvent::Listening { endpoint } => info!(%endpoint, "hand listener bound"),
        HandEvent::BrainConnected { peer } => info!(%peer, "brain connected"),
        HandEvent::Disconnected { reason } => info!(%reason, "brain disconnected"),
        HandEvent::Retrying { delay } => info!(?delay, "hand retrying"),
        HandEvent::Revoked => info!("hand revoked by the brain"),
        HandEvent::Replaced => info!("hand replaced by another hand"),
        HandEvent::Rejected { status } => warn!(status, "hand attachment rejected"),
        HandEvent::Unconfirmed { status, retry } => {
            warn!(status, ?retry, "hand attachment unconfirmed");
        }
        other => info!(?other, "hand event"),
    }
}

async fn serve_health(listener: TcpListener, ready: Arc<AtomicBool>) {
    let app = Router::new()
        .route("/healthz", get(healthz))
        .with_state(ready);
    if let Err(error) = axum::serve(listener, app).await {
        warn!(%error, "health server stopped");
    }
}

async fn healthz(State(ready): State<Arc<AtomicBool>>) -> impl IntoResponse {
    if ready.load(Ordering::SeqCst) {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting")
    }
}

async fn serve_public(listener: TcpListener, internal: SocketAddr) {
    loop {
        match listener.accept().await {
            Ok((inbound, peer)) => {
                tokio::spawn(proxy_connection(inbound, internal, peer));
            }
            Err(error) => {
                warn!(%error, "hand listener accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn proxy_connection(mut inbound: TcpStream, internal: SocketAddr, peer: SocketAddr) {
    let mut outbound = match TcpStream::connect(internal).await {
        Ok(outbound) => outbound,
        Err(error) => {
            warn!(%error, %peer, "connect to internal hand listener failed");
            return;
        }
    };
    let _ = inbound.set_nodelay(true);
    let _ = outbound.set_nodelay(true);
    if let Err(error) = copy_bidirectional(&mut inbound, &mut outbound).await {
        tracing::debug!(%error, %peer, "hand connection ended");
    }
}
