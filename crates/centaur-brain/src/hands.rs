//! Hands: a Linux machine that a nanocode turn checks out on demand.
//!
//! A hand is a single-use pod from a warm pool. api-rs leases one with the
//! turn's own token, and the brain connects to it over nanocode's hand
//! protocol: `centaur_hand_checkout` connects through the runner pool,
//! selects the hand for this session's cells, and writes the lease's
//! placeholder environment to `~/.centaur/env.sh` on the hand.
//! `centaur_hand_release` disconnects and gives the lease back.
//!
//! The tools run on nanocode's executor, which is not tokio, while reqwest
//! needs a tokio reactor; each HTTP call therefore runs as a task on the
//! brain's tokio runtime (see `codemode`). Everything the runner pool does
//! stays on nanocode's executor, where its reactor and timers live.

use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use nanocode::{
    CodeModeHost, ExecutorSelection, HandAuthorizer, HandCallDecision, HostFuture, Runner,
    RunnerPool,
    tool_contract::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult},
    turnlog::{EffectClass, Effects},
};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::codemode::{self, TurnToken, Turns};

/// The api-rs base URL; hands are off when unset.
pub const URL_ENV: &str = "CENTAUR_HANDS_URL";
/// Seconds without hand activity before the lease is released; `0` never
/// releases automatically.
pub const IDLE_SECONDS_ENV: &str = "CENTAUR_HANDS_IDLE_SECONDS";
pub const CHECKOUT: &str = "centaur_hand_checkout";
pub const RELEASE: &str = "centaur_hand_release";

const DEFAULT_IDLE_SECONDS: u64 = 600;
/// Long enough for a pool miss, which may create and wait for a hand.
const LEASE_TIMEOUT: Duration = Duration::from_secs(120);
const RENEW_TIMEOUT: Duration = Duration::from_secs(30);
const RELEASE_TIMEOUT: Duration = Duration::from_secs(30);
const CURRENT_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the idle timer checks, at most.
const IDLE_CHECK: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

const LEASES_PATH: &str = "/api/hands/leases";
const CURRENT_PATH: &str = "/api/hands/leases/current";

/// The environment file the hand image sources from every shell.
const ENV_FILE: &str = "$HOME/.centaur/env.sh";
const ENV_HEREDOC: &str = "CENTAUR_HAND_ENV_EOF";

pub const UNAVAILABLE: &str = "Hands are not available for this conversation.";
pub const EXPIRED: &str = "This turn's hand access expired, so nothing was done to the hand. \
     Access is renewed with the next message in this conversation.";
pub const NOT_READY: &str = "No hand is ready right now; try again in a few seconds.";

pub const INSTRUCTIONS: &str = "Hands: this conversation can check out a hand, a Linux machine with a shell, files, git, and network access whose credentials are injected for the conversation's principal, by calling `centaur_hand_checkout`. While a hand is checked out, `exec` cells run on it, and its tools are called from inside a cell, for example `return await tools.exec_command({ cmd: \"pwd\" })`; `tools.hands()` lists the attached hand and its tools, and passing `hand: \"<name>\"` to a tool call runs that call on the hand. `centaur_hand_release` gives it back, and an idle hand is released automatically. Check one out only when the task needs a machine of its own.";

type Error = Box<dyn std::error::Error + Send + Sync>;

/// How the session log treats the hand tools. Checkout and release both act on
/// the pool, whose effects a crash cannot take back, so their calls are
/// committed before they run and an interrupted call is recorded as unknown.
/// Checkout is idempotent per thread, so re-running it after a crash finds the
/// same lease.
pub fn effects() -> Effects {
    codemode::effects()
        .declare(CHECKOUT, EffectClass::External)
        .declare(RELEASE, EffectClass::External)
}

/// The brain's hand configuration, from its environment.
pub fn from_env() -> Result<Option<Arc<Hands>>, Error> {
    let Some((base_url, idle)) = settings(|name| std::env::var(name).ok())? else {
        return Ok(None);
    };
    Ok(Some(Arc::new(Hands::new(
        &base_url,
        idle,
        tokio::runtime::Handle::current(),
    )?)))
}

/// The api-rs base URL and idle period: `None` when hands are off.
fn settings(lookup: impl Fn(&str) -> Option<String>) -> Result<Option<(String, Duration)>, Error> {
    let get = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
    let Some(base_url) = get(URL_ENV) else {
        return Ok(None);
    };
    let idle = match get(IDLE_SECONDS_ENV) {
        Some(seconds) => seconds.trim().parse::<u64>().map_err(|_| {
            format!("{IDLE_SECONDS_ENV} must be a number of seconds (0 disables auto-release)")
        })?,
        None => DEFAULT_IDLE_SECONDS,
    };
    Ok(Some((
        base_url.trim_end_matches('/').to_owned(),
        Duration::from_secs(idle),
    )))
}

/// The lease API and the settings shared by every thread.
pub struct Hands {
    base_url: String,
    client: reqwest::Client,
    idle: Duration,
    runtime: tokio::runtime::Handle,
}

impl Hands {
    pub fn new(
        base_url: &str,
        idle: Duration,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, reqwest::Error> {
        // In-cluster calls: never through an ambient egress proxy.
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            base_url: base_url.to_owned(),
            client,
            idle,
            runtime,
        })
    }

    /// How long a silent hand is kept before it is released; zero means it is
    /// never released automatically.
    pub const fn idle(&self) -> Duration {
        self.idle
    }

    /// One thread's hands, bound to the session whose cells they run.
    pub fn thread(
        self: &Arc<Self>,
        thread_key: &str,
        session_id: &str,
        log: Arc<dyn Turns>,
    ) -> Arc<ThreadHands> {
        Arc::new(ThreadHands {
            hands: Arc::clone(self),
            thread_key: thread_key.to_owned(),
            session_id: session_id.to_owned(),
            log,
            pool: OnceLock::new(),
            lifecycle: tokio::sync::Mutex::new(()),
            attached: Mutex::default(),
            relay: Mutex::default(),
            token: Mutex::default(),
            last_used: Arc::new(Mutex::new(Instant::now())),
            checked_current: AtomicBool::new(false),
        })
    }
}

/// Which hand size to lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Default,
    Large,
}

impl Size {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Large => "large",
        }
    }
}

/// A live lease. `token` is the hand's bearer and `env` its placeholders:
/// neither is ever logged or returned to the model.
#[derive(Clone, Deserialize)]
pub struct Lease {
    lease_id: String,
    hand_id: String,
    url: String,
    token: String,
    #[serde(default)]
    size: Option<Size>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    expires_at: Option<String>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Lease")
            .field("lease_id", &self.lease_id)
            .field("hand_id", &self.hand_id)
            .field("url", &self.url)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct LeaseRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<Size>,
}

/// The hand this thread has attached.
#[derive(Clone, Debug)]
struct Attached {
    lease_id: String,
    hand_id: String,
    size: Option<Size>,
    expires_at: Option<String>,
    /// The hand's own description: platform, shell, working directory, tools.
    description: Option<Value>,
}

/// One thread's hands: the runner pool it attaches them to, the lease it has,
/// and when a hand last ran something.
pub struct ThreadHands {
    hands: Arc<Hands>,
    thread_key: String,
    session_id: String,
    log: Arc<dyn Turns>,
    /// The thread runtime's pool. The runtime exists only after the tools are
    /// built, so the pool is bound once it does.
    pool: OnceLock<RunnerPool>,
    /// Serializes attaching and detaching, so a reattach racing a checkout
    /// cannot open two relays or leave two hands registered for one thread.
    lifecycle: tokio::sync::Mutex<()>,
    attached: Mutex<Option<Attached>>,
    /// The loopback relay in front of the attached hand, closed on release.
    relay: Mutex<Option<Relay>>,
    /// The most recent hands token, so an idle release can still ask the API.
    token: Mutex<Option<TurnToken>>,
    last_used: Arc<Mutex<Instant>>,
    /// Whether a live lease from a previous brain process was looked for.
    checked_current: AtomicBool,
}

impl ThreadHands {
    /// The tools of this thread, as native nanocode tools.
    pub fn tools(self: &Arc<Self>) -> Vec<HandTool> {
        Kind::ALL
            .into_iter()
            .map(|kind| HandTool {
                hands: Arc::clone(self),
                kind,
            })
            .collect()
    }

    /// Bind the thread runtime's runner pool, once the runtime is built.
    pub fn bind(&self, pool: &RunnerPool) {
        pool.code_mode_executors()
            .set_authorizer(Some(Arc::new(Activity {
                last_used: Arc::clone(&self.last_used),
            })));
        if self.pool.set(pool.clone()).is_err() {
            warn!(thread_key = %self.thread_key, "the hand pool was bound twice");
        }
    }

    fn pool(&self) -> Result<&RunnerPool, String> {
        self.pool
            .get()
            .ok_or_else(|| "The hand pool is not ready yet; retry in a few seconds.".to_owned())
    }

    fn executors(&self) -> Result<nanocode::CodeModeExecutors, String> {
        Ok(self.pool()?.code_mode_executors())
    }

    fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elapsed()
    }

    fn attached(&self) -> Option<Attached> {
        self.attached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn remember(&self, token: Option<&TurnToken>) {
        if let Some(token) = token {
            *self
                .token
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(token.clone());
        }
    }

    fn last_token(&self) -> Option<TurnToken> {
        self.token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A new turn started: rebind the lease to this requester, and reconnect to
    /// a lease a previous brain process left behind.
    ///
    /// The lease calls run detached and best effort: a slow or unavailable
    /// hands API must not hold up the turn's submission.
    pub fn turn_started(self: &Arc<Self>, token: Option<&TurnToken>) {
        self.remember(token);
        let Some(token) = token.filter(|token| token.expires_at > now_secs()).cloned() else {
            return;
        };
        let hands = Arc::clone(self);
        nanocode_executor::spawn(async move { hands.resume(token).await });
    }

    /// Renew the attached lease, or reattach a lease left by a previous brain
    /// process.
    async fn resume(&self, token: TurnToken) {
        if let Some(attached) = self.attached() {
            match self.renew(&token, &attached.lease_id).await {
                Ok(expires_at) => {
                    if let Some(expires_at) = expires_at
                        && let Some(current) = self
                            .attached
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_mut()
                            .filter(|current| current.lease_id == attached.lease_id)
                    {
                        // A release may have run while the renew was in
                        // flight; only the lease we renewed may be updated, so
                        // a finished release is never resurrected.
                        current.expires_at = Some(expires_at);
                    }
                    self.touch();
                }
                Err(RenewError::Gone) => {
                    warn!(
                        thread_key = %self.thread_key,
                        lease_id = %attached.lease_id,
                        "the hand lease is gone; detaching"
                    );
                    self.detach(&attached.lease_id, &attached.hand_id).await;
                }
                Err(RenewError::Failed(message)) => {
                    warn!(
                        thread_key = %self.thread_key,
                        lease_id = %attached.lease_id,
                        %message,
                        "could not renew the hand lease"
                    );
                }
            }
            return;
        }
        // Only a hands token authorizes the lease API, so a lease left by a
        // previous brain process is found on this thread's first turn.
        if self.checked_current.swap(true, Ordering::AcqRel) {
            return;
        }
        match self.current(&token).await {
            Ok(Some(lease)) => {
                info!(
                    thread_key = %self.thread_key,
                    hand_id = %lease.hand_id,
                    "reconnecting to the thread's live hand"
                );
                if let Err(message) = self.attach(lease).await {
                    // A hand that could not be reached may be reachable on the
                    // next turn; try again then.
                    self.checked_current.store(false, Ordering::Release);
                    warn!(thread_key = %self.thread_key, %message, "hand reattach failed");
                }
            }
            Ok(None) => {}
            Err(CurrentError::Final(message)) => {
                warn!(thread_key = %self.thread_key, %message, "cannot look up the thread's hand");
            }
            Err(CurrentError::Transient(message)) => {
                self.checked_current.store(false, Ordering::Release);
                warn!(thread_key = %self.thread_key, %message, "cannot look up the thread's hand");
            }
        }
    }

    /// `centaur_hand_checkout`.
    async fn checkout(&self, size: Option<Size>, turn: &codemode::Turn) -> Result<String, String> {
        let token = live_token(turn)?;
        if let Some(attached) = self.attached() {
            // Already attached: the same hand, without leasing another.
            return Ok(description(&attached));
        }
        let lease = self.lease(token, size).await?;
        match self.attach(lease.clone()).await {
            Ok(attached) => Ok(description(&attached)),
            Err(message) => {
                // The hand is unusable; do not leave it leased.
                self.delete(token, &lease.lease_id).await;
                Err(message)
            }
        }
    }

    /// `centaur_hand_release`.
    async fn release(&self, turn: &codemode::Turn) -> Result<String, String> {
        let token = live_token(turn)?;
        let Some(attached) = self.attached() else {
            return Ok("No hand is checked out for this conversation.".to_owned());
        };
        self.detach(&attached.lease_id, &attached.hand_id).await;
        self.delete(token, &attached.lease_id).await;
        Ok(format!("Hand `{}` was released.", attached.hand_id))
    }

    /// Connect to a leased hand, route this session's cells to it, and give it
    /// the lease's placeholder environment. The caller owns `lease` so it can
    /// release it when attaching fails.
    async fn attach(&self, lease: Lease) -> Result<Attached, String> {
        let _guard = self.lifecycle.lock().await;
        self.attach_locked(lease).await
    }

    /// [`Self::attach`], with the thread's lifecycle already held.
    async fn attach_locked(&self, lease: Lease) -> Result<Attached, String> {
        if let Some(attached) = self.attached() {
            return Ok(attached);
        }
        // nanocode refuses a plaintext attachment whose host is not literal
        // loopback, and a hand is addressed by its pod IP: relay through a
        // local listener.
        let relay = self.hands.relay(&lease.url).await?;
        let pool = self.pool()?;
        let runner = Runner::connect(relay.endpoint.as_str())
            .token(lease.token.as_str())
            .name(lease.hand_id.as_str());
        let _handle = pool
            .connect(runner)
            .await
            .map_err(|error| attach_error(&lease, &error))?;
        let executors = pool.code_mode_executors();
        let name: Arc<str> = Arc::from(lease.hand_id.as_str());
        if executors.get(&name).is_none() {
            // A runner that is not a Code Mode executor cannot run cells, and
            // a hand's tools live there; do not leave it registered.
            pool.disconnect(&lease.hand_id).await.ok();
            return Err(format!(
                "the hand at {} does not speak the hand protocol; nothing was attached",
                lease.url
            ));
        }
        executors.select(&self.session_id, Some(ExecutorSelection::Executor(name)));
        let description = executors
            .get(&lease.hand_id)
            .and_then(|host| host.describe())
            .and_then(|description| serde_json::to_value(description).ok());
        let attached = Attached {
            lease_id: lease.lease_id.clone(),
            hand_id: lease.hand_id.clone(),
            size: lease.size,
            expires_at: lease.expires_at.clone(),
            description,
        };
        if let Err(message) = self.write_env(&lease).await {
            self.detach_locked(None, &lease.hand_id).await;
            return Err(message);
        }
        *self
            .relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(relay);
        *self
            .attached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(attached.clone());
        self.touch();
        info!(
            thread_key = %self.thread_key,
            hand_id = %attached.hand_id,
            lease_id = %attached.lease_id,
            size = attached.size.map_or("default", Size::as_str),
            "hand checked out"
        );
        Ok(attached)
    }

    /// Write the lease's placeholders where the hand image's shells source
    /// them. Values are placeholders, never real credentials.
    async fn write_env(&self, lease: &Lease) -> Result<(), String> {
        let executors = self.executors()?;
        let host = executors
            .connected(&lease.hand_id)
            .map_err(|error| error.to_string())?;
        let definitions = host
            .tool_definitions(&self.session_id)
            .map_err(|error| error.to_string())?;
        let (name, argument) = shell_tool(&definitions).ok_or_else(|| {
            "The hand has no shell tool, so its environment could not be set up; nothing was attached."
                .to_owned()
        })?;
        let call_id = format!("{}-env", lease.lease_id);
        let context = ToolContext::new("centaur-hand", &self.session_id, &call_id, &[], usize::MAX);
        let output = executors
            .call_on(
                &lease.hand_id,
                name,
                json!({ argument: env_script(&lease.env) }),
                context,
            )
            .await;
        if !output.success {
            return Err(format!(
                "The hand could not write its environment: {}; nothing was attached.",
                output_text(&output)
            ));
        }
        Ok(())
    }

    /// Disconnect the hand and stop routing this session's cells to it. Only
    /// the expected lease is detached, so a release that overlapped a renew
    /// cannot take down a newer hand.
    async fn detach(&self, lease_id: &str, hand_id: &str) {
        let _guard = self.lifecycle.lock().await;
        self.detach_locked(Some(lease_id), hand_id).await;
    }

    /// [`Self::detach`], with the thread's lifecycle already held.
    async fn detach_locked(&self, expected_lease: Option<&str>, hand_id: &str) {
        {
            let mut attached = self
                .attached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(expected) = expected_lease
                && attached
                    .as_ref()
                    .is_some_and(|current| current.lease_id != expected)
            {
                return;
            }
            attached.take();
        }
        self.relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Ok(pool) = self.pool() else {
            return;
        };
        pool.code_mode_executors().select(&self.session_id, None);
        if let Err(error) = pool.disconnect(hand_id).await {
            warn!(thread_key = %self.thread_key, hand_id, %error, "hand disconnect failed");
        }
    }

    /// Release a hand that has been silent for the idle period, so a
    /// forgotten hand is not held (and billed) forever. One timer per open
    /// thread; its sleep is capped, so a long idle setting still reacts. An
    /// idle period of zero turns the timer off.
    pub fn spawn_idle(self: &Arc<Self>) {
        if self.hands.idle.is_zero() {
            return;
        }
        // The timer must not keep the thread state alive: a dropped thread ends
        // the loop.
        let hands = Arc::downgrade(self);
        nanocode_executor::spawn(async move {
            loop {
                nanocode_executor::sleep(idle_check(
                    hands
                        .upgrade()
                        .map_or(Duration::from_secs(1), |hands| hands.hands.idle),
                ))
                .await;
                let Some(hands) = hands.upgrade() else {
                    return;
                };
                if hands.attached().is_none() {
                    continue;
                }
                // A running turn may be using the hand through a long cell.
                if matches!(hands.log.running().await, Ok(Some(_))) {
                    hands.touch();
                    continue;
                }
                if hands.idle_for() < hands.hands.idle {
                    continue;
                }
                let Some(attached) = hands.attached() else {
                    continue;
                };
                info!(
                    thread_key = %hands.thread_key,
                    hand_id = %attached.hand_id,
                    idle_seconds = hands.hands.idle.as_secs(),
                    "releasing an idle hand"
                );
                hands.detach(&attached.lease_id, &attached.hand_id).await;
                match hands.last_token() {
                    Some(token) if token.expires_at > now_secs() => {
                        hands.delete(&token, &attached.lease_id).await;
                    }
                    _ => warn!(
                        thread_key = %hands.thread_key,
                        lease_id = %attached.lease_id,
                        "no live turn token for the idle release; the lease expires on its own"
                    ),
                }
            }
        });
    }

    async fn lease(&self, token: &TurnToken, size: Option<Size>) -> Result<Lease, String> {
        let url = format!("{}{LEASES_PATH}", self.hands.base_url);
        let body =
            serde_json::to_value(LeaseRequest { size }).map_err(|error| error.to_string())?;
        let reply = self
            .hands
            .request(
                reqwest::Method::POST,
                &url,
                token,
                Some(body),
                LEASE_TIMEOUT,
            )
            .await?;
        match reply {
            Reply::Ok(body) => serde_json::from_value(body)
                .map_err(|error| format!("The hand service answered an unreadable lease: {error}")),
            Reply::Status(status, message) => Err(lease_failure(status, &message)),
        }
    }

    async fn current(&self, token: &TurnToken) -> Result<Option<Lease>, CurrentError> {
        let url = format!("{}{CURRENT_PATH}", self.hands.base_url);
        let reply = self
            .hands
            .request(reqwest::Method::GET, &url, token, None, CURRENT_TIMEOUT)
            .await
            .map_err(CurrentError::Transient)?;
        match reply {
            Reply::Ok(body) => match serde_json::from_value::<Lease>(body) {
                Ok(lease) => Ok(Some(lease)),
                // An empty body is no lease.
                Err(_) => Ok(None),
            },
            Reply::Status(StatusCode::NOT_FOUND, _) => Ok(None),
            // A refused token will not start working again this process.
            Reply::Status(status @ (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN), message) => {
                Err(CurrentError::Final(lease_failure(status, &message)))
            }
            Reply::Status(status, message) => {
                Err(CurrentError::Transient(lease_failure(status, &message)))
            }
        }
    }

    async fn renew(&self, token: &TurnToken, lease_id: &str) -> Result<Option<String>, RenewError> {
        let url = format!(
            "{}{LEASES_PATH}/{}/renew",
            self.hands.base_url,
            urlencoding::encode(lease_id)
        );
        let reply = self
            .hands
            .request(reqwest::Method::POST, &url, token, None, RENEW_TIMEOUT)
            .await
            .map_err(RenewError::Failed)?;
        match reply {
            Reply::Ok(body) => Ok(body
                .get("expires_at")
                .and_then(Value::as_str)
                .map(str::to_owned)),
            Reply::Status(StatusCode::NOT_FOUND | StatusCode::GONE, _) => Err(RenewError::Gone),
            Reply::Status(status, message) => {
                Err(RenewError::Failed(lease_failure(status, &message)))
            }
        }
    }

    /// Best effort: a lease that cannot be released here expires on its own.
    async fn delete(&self, token: &TurnToken, lease_id: &str) {
        let url = format!(
            "{}{LEASES_PATH}/{}",
            self.hands.base_url,
            urlencoding::encode(lease_id)
        );
        match self
            .hands
            .request(reqwest::Method::DELETE, &url, token, None, RELEASE_TIMEOUT)
            .await
        {
            Ok(Reply::Ok(_)) => {
                info!(thread_key = %self.thread_key, lease_id, "hand lease released");
            }
            Ok(Reply::Status(StatusCode::NOT_FOUND, _)) => {
                info!(thread_key = %self.thread_key, lease_id, "hand lease was already gone");
            }
            Ok(Reply::Status(status, message)) => warn!(
                thread_key = %self.thread_key,
                lease_id,
                status = status.as_u16(),
                %message,
                "hand lease release was refused; it expires on its own"
            ),
            Err(message) => warn!(
                thread_key = %self.thread_key,
                lease_id,
                %message,
                "hand lease release failed; it expires on its own"
            ),
        }
    }
}

enum RenewError {
    /// The lease is no longer live.
    Gone,
    Failed(String),
}

enum CurrentError {
    /// Retrying this turn's token will not help (authentication).
    Final(String),
    /// Worth another try on the next turn.
    Transient(String),
}

/// Which of the two tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Checkout,
    Release,
}

impl Kind {
    const ALL: [Self; 2] = [Self::Checkout, Self::Release];

    const fn name(self) -> &'static str {
        match self {
            Self::Checkout => CHECKOUT,
            Self::Release => RELEASE,
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Checkout => {
                "Checks out a hand: a Linux machine with a shell, files, git, and network access \
                 whose credentials are injected for this conversation's principal. While a hand is \
                 checked out, `exec` cells run on it and its tools are called from inside a cell, \
                 for example `return await tools.exec_command({ cmd: \"pwd\" })`. The hand stays \
                 checked out until `centaur_hand_release` or until it goes idle; a conversation has \
                 at most one, and checking out again returns the one already attached."
            }
            Self::Release => {
                "Releases the hand checked out for this conversation. The machine is deleted, so \
                 anything left on it is lost; call this when the work that needed a machine is done."
            }
        }
    }

    fn schema(self) -> Value {
        match self {
            Self::Checkout => json!({
                "type": "object",
                "properties": {
                    "size": {
                        "type": "string",
                        "enum": ["default", "large"],
                        "description": "The machine's size; `large` for heavy builds. Defaults to `default`."
                    }
                },
                "additionalProperties": false
            }),
            Self::Release => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }
}

/// One hand tool of one thread, as a native nanocode tool.
pub struct HandTool {
    hands: Arc<ThreadHands>,
    kind: Kind,
}

impl Tool for HandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.kind.name(),
            self.kind.description(),
            self.kind.schema(),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        // One hand per thread: these two are ordered against each other.
        false
    }

    fn execute<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        _context: ToolContext<'life1>,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let args = input.decode_json::<Value>();
        Box::pin(async move {
            // Read here, on nanocode's executor, where the session log lives.
            let running = match self.hands.log.running().await {
                Ok(Some(running)) => running,
                Ok(None) => return Ok(ToolOutput::error(UNAVAILABLE)),
                Err(error) => {
                    warn!(thread_key = %self.hands.thread_key, %error, "cannot read the running turn");
                    return Ok(ToolOutput::error(
                        "The hand tools could not load this turn's access; retry in a few seconds.",
                    ));
                }
            };
            let turn = codemode::Turn {
                thread_key: self.hands.thread_key.clone(),
                request_id: running.request_id,
                // The lease API takes the hands token, never the Code Mode
                // token that goes to the executor and proxy.
                token: running.hands_token,
            };
            let result = match (self.kind, args) {
                (Kind::Checkout, Ok(args)) => match serde_json::from_value::<CheckoutArgs>(args) {
                    Ok(args) => self.hands.checkout(args.size, &turn).await,
                    Err(error) => Err(format!("Invalid arguments for {CHECKOUT}: {error}")),
                },
                (Kind::Release, _) => self.hands.release(&turn).await,
                (Kind::Checkout, Err(error)) => {
                    Err(format!("Invalid arguments for {CHECKOUT}: {error}"))
                }
            };
            Ok(match result {
                Ok(text) => ToolOutput::text(text),
                Err(message) => ToolOutput::error(message),
            })
        })
    }
}

#[derive(Deserialize)]
struct CheckoutArgs {
    #[serde(default)]
    size: Option<Size>,
}

/// The session's hand activity, so the idle timer sees every routed call.
struct Activity {
    last_used: Arc<Mutex<Instant>>,
}

impl HandAuthorizer for Activity {
    fn authorize_call<'a>(
        &'a self,
        _session_id: &'a str,
        _hand: &'a str,
        _name: &'a str,
        _input: &'a Value,
    ) -> HostFuture<'a, HandCallDecision> {
        *self
            .last_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        Box::pin(async { HandCallDecision::Allow })
    }
}

/// A live token for the lease API.
fn live_token(turn: &codemode::Turn) -> Result<&TurnToken, String> {
    let token = turn.token.as_ref().ok_or_else(|| UNAVAILABLE.to_owned())?;
    if token.expires_at <= now_secs() {
        return Err(EXPIRED.to_owned());
    }
    Ok(token)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn idle_check(idle: Duration) -> Duration {
    (idle / 4).clamp(Duration::from_secs(1), IDLE_CHECK)
}

/// The shell tool to run the environment setup with, and the name of its
/// command argument.
fn shell_tool(definitions: &[ToolDefinition]) -> Option<(&'static str, &'static str)> {
    const CANDIDATES: [(&str, &str); 2] = [("exec_command", "cmd"), ("Bash", "command")];
    CANDIDATES.into_iter().find(|(name, _)| {
        definitions
            .iter()
            .any(|definition| definition.name() == *name)
    })
}

/// The shell script that writes the lease's placeholders to the hand's
/// environment file. A quoted heredoc keeps every value literal.
fn env_script(env: &BTreeMap<String, String>) -> String {
    let mut script =
        format!("mkdir -p \"$(dirname {ENV_FILE})\" && cat > {ENV_FILE} <<'{ENV_HEREDOC}'\n");
    for (name, value) in env {
        if !valid_name(name) {
            warn!(name, "skipping an environment name the hand cannot export");
            continue;
        }
        script.push_str(&format!("export {name}={}\n", quoted(value)));
    }
    script.push_str(ENV_HEREDOC);
    script.push('\n');
    script
}

fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn output_text(output: &ToolOutput) -> &str {
    match &output.output {
        nanocode::tool_contract::ToolOutputBody::Text(text) => text,
        nanocode::tool_contract::ToolOutputBody::Content(_) => "non-text output",
    }
}

fn description(attached: &Attached) -> String {
    let mut text = format!("Hand `{}` is checked out", attached.hand_id);
    if let Some(size) = attached.size
        && size != Size::Default
    {
        text.push_str(&format!(" ({})", size.as_str()));
    }
    if let Some(expires_at) = &attached.expires_at {
        text.push_str(&format!("; its lease expires at {expires_at}"));
    }
    text.push_str(
        ".\nThis session's `exec` cells now run on it; call its tools from inside a cell, for \
         example `return await tools.exec_command({ cmd: \"ls\" })`, or name it with \
         `hand: \"",
    );
    text.push_str(&attached.hand_id);
    text.push_str("\"` to run one call there.");
    if let Some(description) = &attached.description {
        if let Some(workspace) = description.get("workspace").and_then(Value::as_str) {
            text.push_str(&format!("\nWorking directory: {workspace}"));
        }
        if let Some(tools) = description.get("tools").and_then(Value::as_array) {
            let names = tools.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if !names.is_empty() {
                text.push_str(&format!("\nTools: {}", names.join(", ")));
            }
        }
        if let Some(environment) = description.get("environment")
            && let (Some(platform), Some(shell)) = (
                environment.get("platform").and_then(Value::as_str),
                environment.get("shell").and_then(Value::as_str),
            )
        {
            text.push_str(&format!("\n{platform}, {shell}"));
        }
    }
    text
}

fn attach_error(lease: &Lease, error: &nanocode::AttachmentError) -> String {
    match error {
        nanocode::AttachmentError::Authentication(_) => {
            "The hand refused this lease's token; it was not attached.".to_owned()
        }
        nanocode::AttachmentError::Transport(_) | nanocode::AttachmentError::Timeout { .. } => {
            format!(
                "The hand at {} is not reachable; it was not attached.",
                lease.url
            )
        }
        other => format!("The hand could not be attached: {other}"),
    }
}

fn lease_failure(status: StatusCode, message: &str) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => EXPIRED.to_owned(),
        StatusCode::SERVICE_UNAVAILABLE => NOT_READY.to_owned(),
        StatusCode::NOT_FOUND => {
            "Hands are not enabled on the hand service, so no hand was checked out.".to_owned()
        }
        _ if message.is_empty() => {
            format!(
                "The hand service failed this request (HTTP {}).",
                status.as_u16()
            )
        }
        _ => format!(
            "The hand service failed this request (HTTP {}): {message}",
            status.as_u16()
        ),
    }
}

/// A loopback relay in front of a hand.
///
/// nanocode's plaintext attachment targets must be literal loopback hosts,
/// while the lease names the hand by its pod IP. The relay forwards the
/// WebSocket bytes unchanged; the hand token still authenticates the
/// handshake. Dropping it closes the listener.
struct Relay {
    /// `ws://127.0.0.1:<port>/`, what the runner pool connects to.
    endpoint: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Hands {
    /// Open a loopback relay to the hand at `url`.
    async fn relay(&self, url: &str) -> Result<Relay, String> {
        let (host, port) = endpoint_parts(url)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let url = url.to_owned();
        let task = self.runtime.spawn(async move {
            let Ok(addresses) = tokio::net::lookup_host((host.as_str(), port)).await else {
                let _ = sender.send(Err(format!("The hand at {url} cannot be resolved.")));
                return;
            };
            let Some(upstream) = addresses.into_iter().next() else {
                let _ = sender.send(Err(format!("The hand at {url} has no address.")));
                return;
            };
            let listener =
                match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = sender.send(Err(format!("The hand relay cannot listen: {error}")));
                        return;
                    }
                };
            let Ok(local) = listener.local_addr() else {
                let _ = sender.send(Err("The hand relay has no address.".to_owned()));
                return;
            };
            if sender.send(Ok(local)).is_err() {
                return;
            }
            loop {
                let Ok((mut downstream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(upstream).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
                });
            }
        });
        let local = receiver
            .await
            .map_err(|_| "The hand relay stopped before it listened.".to_owned())??;
        Ok(Relay {
            endpoint: format!("ws://{local}/"),
            task,
        })
    }
}

/// The host and port of a hand's `ws://` endpoint.
fn endpoint_parts(url: &str) -> Result<(String, u16), String> {
    let invalid = || format!("The lease names an unusable hand endpoint: {url}");
    let parsed = url::Url::parse(url).map_err(|_| invalid())?;
    if parsed.scheme() != "ws" {
        return Err(format!(
            "The lease's hand endpoint uses {}; the brain relays plaintext ws:// only.",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host_str()
        .map(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned()
        })
        .ok_or_else(invalid)?;
    let port = parsed.port_or_known_default().ok_or_else(invalid)?;
    Ok((host, port))
}

enum Reply {
    Ok(Value),
    /// A non-200 status and the body's error message.
    Status(StatusCode, String),
}

impl Hands {
    /// One lease API call, on the tokio runtime, from any executor.
    async fn request(
        &self,
        method: reqwest::Method,
        url: &str,
        token: &TurnToken,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<Reply, String> {
        let client = self.client.clone();
        let url = url.to_owned();
        let bearer = token.token.clone();
        let task = self.runtime.spawn(async move {
            let mut request = client
                .request(method, &url)
                .bearer_auth(&bearer)
                .timeout(timeout);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let unreachable = |error: reqwest::Error| {
                if error.is_timeout() {
                    format!(
                        "The hand service did not answer within {} s.",
                        timeout.as_secs()
                    )
                } else {
                    format!("The hand service is unreachable: {error}")
                }
            };
            let mut response = request.send().await.map_err(unreachable)?;
            let status = response.status();
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
            {
                return Err(too_large());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(unreachable)? {
                if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Err(too_large());
                }
                bytes.extend_from_slice(&chunk);
            }
            // A release answers 204 No Content; any 2xx is success.
            if !status.is_success() {
                return Ok(Reply::Status(status, error_message(&bytes)));
            }
            if bytes.iter().all(u8::is_ascii_whitespace) {
                return Ok(Reply::Ok(Value::Null));
            }
            serde_json::from_slice(&bytes)
                .map(Reply::Ok)
                .map_err(|error| format!("The hand service answered unreadable JSON: {error}"))
        });
        AbortOnDrop(task)
            .await
            .map_err(|error| format!("The hand service call did not finish: {error}"))?
    }
}

fn too_large() -> String {
    format!(
        "The hand service's answer is larger than {} KiB.",
        MAX_RESPONSE_BYTES / 1024
    )
}

/// The `error` of a JSON error body, or its text, shortened.
fn error_message(bytes: &[u8]) -> String {
    let text = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|body| body.get("error").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
    let text = text.trim();
    match text.char_indices().nth(500) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// A tokio task the dropped future cancels.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codemode::Running;
    use axum::{
        Json, Router,
        extract::{Path as AxumPath, State},
        http::{HeaderMap, StatusCode as AxumStatus},
        response::IntoResponse,
        routing::{delete, get, post},
    };
    use std::{path::PathBuf, sync::Mutex};

    /// A thread's running turn, fixed so the tools always see a live token.
    struct Fixed(Option<Running>);

    impl Turns for Fixed {
        fn running(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Running>, String>> + Send + '_>> {
            let running = self.0.clone();
            Box::pin(async move { Ok(running) })
        }
    }

    fn turn(thread_key: &str, token: Option<(&str, i64)>) -> codemode::Turn {
        codemode::Turn {
            thread_key: thread_key.to_owned(),
            request_id: "exe_1".into(),
            token: token.map(|(token, expires_at)| TurnToken {
                token: token.into(),
                expires_at,
            }),
        }
    }

    fn test_idle() -> Duration {
        Duration::from_secs(600)
    }

    #[test]
    fn the_environment_script_is_literal_and_shell_quoted() {
        let env = BTreeMap::from([
            ("GITHUB_TOKEN".to_owned(), "ghp_placeholder".to_owned()),
            ("ODD".to_owned(), "it's".to_owned()),
            ("not-a-name".to_owned(), "x".to_owned()),
        ]);
        let script = env_script(&env);
        assert!(
            script.starts_with("mkdir -p \"$(dirname $HOME/.centaur/env.sh)\" && cat > $HOME/.centaur/env.sh <<'CENTAUR_HAND_ENV_EOF'\n"),
            "{script}"
        );
        assert!(script.contains("export GITHUB_TOKEN='ghp_placeholder'\n"));
        assert!(script.contains("export ODD='it'\\''s'\n"));
        assert!(!script.contains("not-a-name"), "{script}");
        assert!(script.ends_with("CENTAUR_HAND_ENV_EOF\n"));
    }

    #[test]
    fn the_shell_tool_is_found_by_name() {
        let definitions = |names: &[&str]| {
            names
                .iter()
                .map(|name| ToolDefinition::function(*name, "test", json!({"type": "object"})))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shell_tool(&definitions(&["Read", "Bash"])),
            Some(("Bash", "command"))
        );
        assert_eq!(
            shell_tool(&definitions(&["exec_command"])),
            Some(("exec_command", "cmd"))
        );
        assert_eq!(shell_tool(&definitions(&["Read"])), None);
    }

    #[test]
    fn hands_are_off_when_the_url_is_unset() {
        assert!(settings(|_| None).unwrap().is_none());
        let (base, idle) = settings(|name| match name {
            URL_ENV => Some("http://api/".into()),
            IDLE_SECONDS_ENV => Some("45".into()),
            _ => None,
        })
        .unwrap()
        .unwrap();
        assert_eq!(base, "http://api");
        assert_eq!(idle, Duration::from_secs(45));
        assert_eq!(
            settings(|name| match name {
                URL_ENV => Some("http://api".into()),
                IDLE_SECONDS_ENV => Some("0".into()),
                _ => None,
            })
            .unwrap()
            .unwrap()
            .1,
            Duration::ZERO,
            "zero disables auto-release"
        );
        assert_eq!(
            settings(|name| (name == URL_ENV).then(|| "http://api".to_owned()))
                .unwrap()
                .unwrap()
                .1,
            Duration::from_secs(DEFAULT_IDLE_SECONDS)
        );
        assert!(
            settings(|name| match name {
                URL_ENV => Some("http://api".into()),
                IDLE_SECONDS_ENV => Some("soon".into()),
                _ => None,
            })
            .is_err()
        );
    }

    #[test]
    fn lease_failures_are_readable() {
        assert_eq!(lease_failure(StatusCode::UNAUTHORIZED, ""), EXPIRED);
        assert_eq!(
            lease_failure(StatusCode::SERVICE_UNAVAILABLE, ""),
            NOT_READY
        );
        assert!(lease_failure(StatusCode::INTERNAL_SERVER_ERROR, "boom").contains("boom"));
    }

    /// A successful release is `204 No Content`, not a JSON 200.
    #[test]
    fn a_no_content_release_is_a_success() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let seen = Seen::default();
        let address = tokio_runtime.block_on(fake_api(None, 404, seen.clone()));
        let hands = Arc::new(
            Hands::new(
                &format!("http://{address}"),
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let token = TurnToken {
            token: "hands-token".into(),
            expires_at: now_secs() + 600,
        };
        let reply = tokio_runtime.block_on(async {
            hands
                .request(
                    reqwest::Method::DELETE,
                    &format!("http://{address}{LEASES_PATH}/hls_1"),
                    &token,
                    None,
                    RELEASE_TIMEOUT,
                )
                .await
                .unwrap()
        });
        match reply {
            Reply::Ok(Value::Null) => {}
            _ => panic!("a 204 release is a success"),
        }
        assert_eq!(seen.calls()[0].0, "DELETE /api/hands/leases/{lease_id}");
    }

    /// A transient failure to look up a previous lease is retried on the next
    /// turn; authentication and 404 are final for this process.
    #[test]
    fn a_transient_reattach_is_retried_but_a_refusal_is_not() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        for (statuses, expected_calls) in [
            (vec![500, 404], 2),
            (vec![401, 404], 1),
            (vec![404, 500], 1),
        ] {
            let seen = Seen::default();
            let address = tokio_runtime.block_on(fake_current(statuses, seen.clone()));
            let hands = Arc::new(
                Hands::new(
                    &format!("http://{address}"),
                    test_idle(),
                    tokio_runtime.handle().clone(),
                )
                .unwrap(),
            );
            let thread = hands.thread("thread-1", "session-1", Arc::new(Fixed(None)));
            let token = TurnToken {
                token: "hands-token".into(),
                expires_at: now_secs() + 600,
            };
            executor.block_on(async {
                thread.resume(token.clone()).await;
                thread.resume(token.clone()).await;
                thread.resume(token).await;
            });
            assert_eq!(seen.calls().len(), expected_calls, "{seen:?}");
        }
    }

    /// A renew runs detached, and a release during it is not undone when the
    /// slower renew finally answers.
    #[test]
    fn a_renew_runs_detached_and_a_release_is_not_undone() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let seen = Seen::default();
        let (address, release, finished) = tokio_runtime.block_on(fake_slow_renew(seen.clone()));
        let hands = Arc::new(
            Hands::new(
                &format!("http://{address}"),
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let thread = hands.thread("thread-1", "session-1", Arc::new(Fixed(None)));
        *thread
            .attached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Attached {
            lease_id: "hls_1".into(),
            hand_id: "hnd_1".into(),
            size: None,
            expires_at: None,
            description: None,
        });
        let token = TurnToken {
            token: "hands-token".into(),
            expires_at: now_secs() + 600,
        };
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        executor.block_on(async {
            let started = Instant::now();
            thread.turn_started(Some(&token));
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "submit does not wait for the lease API"
            );
            for _ in 0..500 {
                if !seen.calls().is_empty() {
                    break;
                }
                nanocode_executor::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(seen.calls()[0].0, "POST /api/hands/leases/{lease_id}/renew");
            thread.detach("hls_1", "hnd_1").await;
            release.add_permits(1);
            for _ in 0..500 {
                if finished.load(Ordering::Acquire) {
                    break;
                }
                nanocode_executor::sleep(Duration::from_millis(10)).await;
            }
            // Let the detached task consume the answer.
            nanocode_executor::sleep(Duration::from_millis(100)).await;
            assert!(
                thread.attached().is_none(),
                "the earlier renew resurrected a released hand"
            );
        });
    }

    /// The idle timer holds only a weak reference, so a closed thread is not
    /// kept alive by its own timer.
    #[test]
    fn the_idle_timer_does_not_keep_the_thread_state_alive() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let hands = Arc::new(
            Hands::new(
                "http://127.0.0.1:1",
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let weak = executor.block_on(async {
            let thread = hands.thread("thread-1", "session-1", Arc::new(Fixed(None)));
            let weak = Arc::downgrade(&thread);
            thread.spawn_idle();
            drop(thread);
            weak
        });
        assert!(weak.upgrade().is_none());
    }

    /// The hand tools read this turn's hands token, never its Code Mode token,
    /// and a turn without one gets the unavailable message without a call.
    #[test]
    fn hand_tools_use_the_hands_token_and_refuse_a_turn_without_one() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let seen = Seen::default();
        let address = tokio_runtime.block_on(fake_api(
            Some(lease_json("ws://127.0.0.1:1")),
            200,
            seen.clone(),
        ));
        let hands = Arc::new(
            Hands::new(
                &format!("http://{address}"),
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let token = |token: &str| TurnToken {
            token: token.into(),
            expires_at: now_secs() + 600,
        };
        let thread = hands.thread(
            "thread-1",
            "session-1",
            Arc::new(Fixed(Some(Running {
                request_id: "exe_1".into(),
                token: Some(token("tool-token")),
                hands_token: Some(token("hands-token")),
            }))),
        );
        let pool = RunnerPool::new();
        thread.bind(&pool);
        let tool = thread
            .tools()
            .into_iter()
            .find(|tool| tool.kind == Kind::Checkout)
            .unwrap();
        let output = executor.block_on(async {
            tool.execute(
                ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
                ToolContext::new("luna", "session-1", "call-1", &[], usize::MAX),
            )
            .await
            .unwrap()
        });
        assert!(!output.success);
        let calls = seen.calls();
        assert!(
            calls.iter().all(|call| call.1 == "Bearer hands-token"),
            "every lease call uses the hands token: {calls:?}"
        );
        assert!(calls.iter().any(|call| call.0.starts_with("POST")));

        let before = calls.len();
        let thread = hands.thread(
            "thread-1",
            "session-1",
            Arc::new(Fixed(Some(Running {
                request_id: "exe_1".into(),
                token: Some(token("tool-token")),
                hands_token: None,
            }))),
        );
        thread.bind(&pool);
        let tool = thread
            .tools()
            .into_iter()
            .find(|tool| tool.kind == Kind::Checkout)
            .unwrap();
        let output = executor.block_on(async {
            tool.execute(
                ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
                ToolContext::new("luna", "session-1", "call-1", &[], usize::MAX),
            )
            .await
            .unwrap()
        });
        assert!(!output.success);
        assert_eq!(output.output.structured_result(), json!(UNAVAILABLE));
        assert_eq!(seen.calls().len(), before, "no call without a hands token");
    }

    #[test]
    fn a_dead_token_is_refused_before_any_call() {
        let expired = turn("thread-1", Some(("t", now_secs() - 1)));
        assert_eq!(live_token(&expired).unwrap_err(), EXPIRED);
        assert_eq!(
            live_token(&turn("thread-1", None)).unwrap_err(),
            UNAVAILABLE
        );
        assert!(live_token(&turn("thread-1", Some(("t", now_secs() + 60)))).is_ok());
    }

    /// What the fake lease API saw.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct Seen(Arc<Mutex<Vec<(String, String, Value)>>>);

    impl Seen {
        fn push(&self, method: &str, path: &str, headers: &HeaderMap, body: Value) {
            let authorization = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            self.0
                .lock()
                .unwrap()
                .push((format!("{method} {path}"), authorization, body));
        }

        pub(crate) fn calls(&self) -> Vec<(String, String, Value)> {
            self.0.lock().unwrap().clone()
        }
    }

    /// The fake lease API's settings, shared by its handlers.
    #[derive(Clone)]
    struct Api {
        lease: Option<Value>,
        status: u16,
        seen: Seen,
    }

    pub(crate) async fn fake_api(
        lease: Option<Value>,
        status: u16,
        seen: Seen,
    ) -> std::net::SocketAddr {
        async fn checkout(
            State(api): State<Api>,
            headers: HeaderMap,
            body: Option<Json<Value>>,
        ) -> impl IntoResponse {
            api.seen.push(
                "POST",
                LEASES_PATH,
                &headers,
                body.map(|Json(body)| body).unwrap_or(Value::Null),
            );
            (
                AxumStatus::from_u16(api.status).unwrap(),
                Json(
                    api.lease
                        .unwrap_or_else(|| json!({"error": "no hand is ready"})),
                ),
            )
        }
        async fn current(State(api): State<Api>, headers: HeaderMap) -> impl IntoResponse {
            api.seen.push("GET", CURRENT_PATH, &headers, Value::Null);
            match api.lease {
                Some(lease) => (AxumStatus::OK, Json(lease)),
                None => (AxumStatus::NOT_FOUND, Json(json!({"error": "no lease"}))),
            }
        }
        async fn renew(
            State(api): State<Api>,
            AxumPath(lease_id): AxumPath<String>,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            api.seen.push(
                "POST",
                "/api/hands/leases/{lease_id}/renew",
                &headers,
                json!(lease_id),
            );
            match api.lease {
                Some(lease) => (AxumStatus::OK, Json(lease)),
                None => (AxumStatus::NOT_FOUND, Json(json!({"error": "no lease"}))),
            }
        }
        async fn release(
            State(api): State<Api>,
            AxumPath(lease_id): AxumPath<String>,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            api.seen.push(
                "DELETE",
                "/api/hands/leases/{lease_id}",
                &headers,
                json!(lease_id),
            );
            // A release has no body, like api-rs's 204 No Content.
            AxumStatus::NO_CONTENT
        }
        let router = Router::new()
            .route(LEASES_PATH, post(checkout))
            .route(CURRENT_PATH, get(current))
            .route("/api/hands/leases/{lease_id}/renew", post(renew))
            .route("/api/hands/leases/{lease_id}", delete(release))
            .with_state(Api {
                lease,
                status,
                seen: seen.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        address
    }

    /// A fake lease API whose `current` answers a scripted sequence of statuses.
    #[derive(Clone)]
    struct CurrentApi {
        statuses: Arc<Mutex<Vec<u16>>>,
        seen: Seen,
    }

    pub(crate) async fn fake_current(statuses: Vec<u16>, seen: Seen) -> std::net::SocketAddr {
        async fn current(State(api): State<CurrentApi>, headers: HeaderMap) -> impl IntoResponse {
            api.seen.push("GET", CURRENT_PATH, &headers, Value::Null);
            let status = {
                let mut statuses = api.statuses.lock().unwrap();
                if statuses.is_empty() {
                    404
                } else {
                    statuses.remove(0)
                }
            };
            let reply = if status == 200 {
                lease_json("ws://127.0.0.1:1")
            } else {
                json!({"error": "current failed"})
            };
            (AxumStatus::from_u16(status).unwrap(), Json(reply))
        }
        let router = Router::new()
            .route(CURRENT_PATH, get(current))
            .with_state(CurrentApi {
                statuses: Arc::new(Mutex::new(statuses)),
                seen,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        address
    }

    /// A fake lease API whose renew waits until the test lets it answer.
    #[derive(Clone)]
    struct SlowRenew {
        release: Arc<tokio::sync::Semaphore>,
        finished: Arc<AtomicBool>,
        seen: Seen,
    }

    pub(crate) async fn fake_slow_renew(
        seen: Seen,
    ) -> (
        std::net::SocketAddr,
        Arc<tokio::sync::Semaphore>,
        Arc<AtomicBool>,
    ) {
        async fn renew(
            State(api): State<SlowRenew>,
            AxumPath(lease_id): AxumPath<String>,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            api.seen.push(
                "POST",
                "/api/hands/leases/{lease_id}/renew",
                &headers,
                json!(lease_id),
            );
            api.release.acquire().await.unwrap().forget();
            api.finished.store(true, Ordering::Release);
            (
                AxumStatus::OK,
                Json(json!({"expires_at": "2026-11-25T19:00:00Z"})),
            )
        }
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let router = Router::new()
            .route("/api/hands/leases/{lease_id}/renew", post(renew))
            .with_state(SlowRenew {
                release: Arc::clone(&release),
                finished: Arc::clone(&finished),
                seen,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (address, release, finished)
    }

    pub(crate) fn lease_json(url: &str) -> Value {
        json!({
            "lease_id": "hls_1",
            "hand_id": "hnd_1",
            "size": "default",
            "url": url,
            "token": "hand-token",
            "env": {"GITHUB_TOKEN": "ghp_placeholder"},
            "expires_at": "2026-11-25T19:00:00Z",
        })
    }

    /// The lease API is called with the turn's own token, and failures become
    /// tool messages rather than panics.
    #[test]
    fn the_lease_api_carries_the_turn_token() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let seen = Seen::default();
        let address = tokio_runtime.block_on(fake_api(
            Some(lease_json("ws://127.0.0.1:1")),
            200,
            seen.clone(),
        ));
        let hands = Arc::new(
            Hands::new(
                &format!("http://{address}"),
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let thread = hands.thread("thread-1", "session-1", Arc::new(Fixed(None)));
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let lease = executor.block_on(async {
            thread
                .lease(
                    &TurnToken {
                        token: "turn-token".into(),
                        expires_at: now_secs() + 600,
                    }
                    .clone(),
                    Some(Size::Large),
                )
                .await
                .unwrap()
        });
        assert_eq!(lease.lease_id, "hls_1");
        assert_eq!(lease.hand_id, "hnd_1");
        assert_eq!(lease.env["GITHUB_TOKEN"], "ghp_placeholder");
        assert!(
            !format!("{lease:?}").contains("hand-token"),
            "the debug form hides the hand token"
        );
        let calls = seen.calls();
        assert_eq!(calls[0].0, format!("POST {LEASES_PATH}"));
        assert_eq!(calls[0].1, "Bearer turn-token");
        assert_eq!(calls[0].2, json!({"size": "large"}));
    }

    /// A refused lease reads as a tool error, not a panic.
    #[test]
    fn a_refused_lease_is_a_tool_error() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let seen = Seen::default();
        let address = tokio_runtime.block_on(fake_api(None, 503, seen.clone()));
        let hands = Arc::new(
            Hands::new(
                &format!("http://{address}"),
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let thread = hands.thread("thread-1", "session-1", Arc::new(Fixed(None)));
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let message = executor.block_on(async {
            thread
                .lease(
                    &TurnToken {
                        token: "turn-token".into(),
                        expires_at: now_secs() + 600,
                    },
                    None,
                )
                .await
                .unwrap_err()
        });
        assert_eq!(message, NOT_READY);
        assert_eq!(seen.calls()[0].2, json!({}));
    }

    /// The whole path: a real nanocode hand on localhost, a lease from a fake
    /// API, checkout through the brain's tools, a cell routed to the hand, and
    /// release. The hand's shell runs with `HOME` pointed at a temp directory
    /// so the environment file lands there.
    #[test]
    fn checkout_attaches_runs_a_cell_and_releases() {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // The hand's shell must not touch this test's real home.
        let home_path = home.path().to_path_buf();

        // nanocode's process tools refuse to spawn until a shell is chosen;
        // the hand daemon selects bash, so the test hand does the same.
        let shell = if std::path::Path::new("/bin/bash").exists() {
            PathBuf::from("/bin/bash")
        } else {
            PathBuf::from("/bin/sh")
        };
        let _ = nanocode_home::shell::choose(nanocode_home::shell::Shell::psh(shell, Vec::new()));

        let hand_token = "hand-token";
        let (address_sender, address_receiver) = std::sync::mpsc::channel();
        let executor = nanocode_executor::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let tools = nanocode::hand::tools_with(
            workspace.path().to_path_buf(),
            nanocode::hand::HandFiles::Machine,
        )
        .unwrap()
        .into_builder()
        .process_environment([("HOME", home_path.as_os_str())])
        .build()
        .unwrap();
        let hand = nanocode::hand::Hand::listen("127.0.0.1:0".parse().unwrap(), hand_token, tools)
            .name("hand-test")
            .directory(workspace.path().to_path_buf());
        executor.spawn(async move {
            let _ = hand
                .run(move |event| {
                    if let nanocode::hand::HandEvent::Listening { endpoint } = event {
                        let _ = address_sender.send(endpoint);
                    }
                })
                .await;
        });
        let endpoint = address_receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the hand listens");

        let seen = Seen::default();
        let api_address =
            tokio_runtime.block_on(fake_api(Some(lease_json(&endpoint)), 200, seen.clone()));
        let hands = Arc::new(
            Hands::new(
                &format!("http://{api_address}"),
                test_idle(),
                tokio_runtime.handle().clone(),
            )
            .unwrap(),
        );
        let running = Running {
            request_id: "exe_1".into(),
            token: Some(TurnToken {
                token: "tool-token".into(),
                expires_at: now_secs() + 600,
            }),
            hands_token: Some(TurnToken {
                token: "hands-token".into(),
                expires_at: now_secs() + 600,
            }),
        };
        let thread = hands.thread(
            "thread-1",
            "session-1",
            Arc::new(Fixed(Some(running.clone()))),
        );
        let pool = RunnerPool::new();
        thread.bind(&pool);

        let probe_home = home_path.clone();
        let outcome = executor.block_on(async move {
            // The hand's shell, not this test's, must own `$HOME`; check it
            // before checkout writes the environment file anywhere.
            let probe = Runner::connect(endpoint.as_str())
                .token(hand_token)
                .name("probe");
            pool.connect(probe).await.expect("the hand attaches");
            let probe_context = ToolContext::new("luna", "session-1", "probe", &[], usize::MAX);
            let home_seen = pool
                .code_mode_executors()
                .call_on(
                    "probe",
                    "exec_command",
                    json!({"cmd": "printf %s \"$HOME\""}),
                    probe_context,
                )
                .await;
            let home_seen = output_text(&home_seen).to_owned();
            pool.disconnect("probe").await.expect("the probe detaches");
            assert!(
                home_seen.contains(probe_home.to_string_lossy().as_ref()),
                "the hand's shell keeps this test's home: {home_seen}"
            );

            let turn = turn("thread-1", None);
            let turn = codemode::Turn {
                token: running.hands_token.clone(),
                ..turn
            };
            let checked_out = thread.checkout(None, &turn).await.unwrap();
            assert!(checked_out.contains("hnd_1"), "{checked_out}");
            assert!(checked_out.contains("exec_command"), "{checked_out}");
            assert_eq!(
                pool.code_mode_executors().selection("session-1"),
                ExecutorSelection::Executor("hnd_1".into())
            );

            // A cell, exactly as the model's `exec` would run on the hand.
            let context = ToolContext::new("luna", "session-1", "cell-1", &[], usize::MAX);
            let cell = pool
                .code_mode_executors()
                .execute(
                    "await tools.exec_command({ cmd: 'touch ran' }); 'done'",
                    context,
                )
                .await
                .unwrap();
            assert!(cell.success, "the cell ran on the hand: {:?}", cell.output);

            let released = thread.release(&turn).await.unwrap();
            assert!(released.contains("released"), "{released}");
            assert!(pool.code_mode_executors().names().is_empty());
            assert_eq!(
                pool.code_mode_executors().selection("session-1"),
                ExecutorSelection::Local
            );
            checked_out
        });
        assert!(!outcome.is_empty());

        // The lease's placeholders are on the hand, and nothing secret is.
        let env = std::fs::read_to_string(home_path.join(".centaur/env.sh")).unwrap();
        assert!(
            env.contains("export GITHUB_TOKEN='ghp_placeholder'"),
            "{env}"
        );
        assert!(!env.contains(hand_token), "{env}");
        assert!(workspace.path().join("ran").exists());

        let calls = seen.calls();
        assert_eq!(calls[0].0, format!("POST {LEASES_PATH}"));
        let released = calls
            .iter()
            .find(|call| call.0.starts_with("DELETE"))
            .expect("the lease is released");
        assert_eq!(released.1, "Bearer hands-token");
        assert_eq!(released.2, json!("hls_1"));
        assert!(
            calls.iter().all(|call| call.1 != "Bearer tool-token"),
            "no lease call uses the Code Mode token: {calls:?}"
        );
    }
}
