//! The nanocode side of the brain. Each Centaur thread is a nanocode session
//! driven by a durable `TurnInbox` over its own session log; the log holds
//! every turn's lifecycle and events. nanocode brings its own executor, so all
//! of this runs on one dedicated thread and the HTTP server talks to it through
//! executor-agnostic channels.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use nanocode::{
    ClaudeClient, ClaudeHarness, CodexHarness, HostFuture, ModelSelection, Nanocode, OpenAi,
    Thinking, Tools,
    backend::Prompt,
    claude::ClaudeEnvironment,
    durability::SessionRecord,
    session_host::{
        Driven, InboxHost, LoggedEvent, MovedQueued, Submission, Submitted, TurnInbox, TurnState,
    },
    tools::ToolExposure,
    turnlog::{LogHost, MemoryLog, OneLog, Policy, SqlLog},
};
use nanocode_store_sql::{PgOptions, PgPool, PgSql};
use serde_json::{Map, Value, json, value::to_raw_value};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{info, warn};

use crate::{
    blocks::{self, Input},
    codemode::{self, CodeMode},
    prompt,
};

const PG_CONNECTIONS: usize = 8;
/// How long the inbox batches events before writing them to the log.
const LOG_WINDOW: Duration = Duration::from_millis(20);
/// How long an idle driver waits before checking for more work.
const DRIVE_POLL: Duration = Duration::from_millis(50);
pub const PAGE_LIMIT: usize = 500;
/// How many times a turn interrupted by a crash is resumed before it fails,
/// so a turn that crashes the brain cannot crash-loop it.
const TURN_RESTARTS: u64 = 3;
/// The host record that commits the log up to a published event.
const DURABLE_KEY: &str = "centaur.durable";

type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
pub struct Config {
    pub database_url: Option<String>,
    pub default_model: String,
    pub thinking: String,
    pub openai_base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    /// The Code Mode tools every thread gets.
    pub code_mode: Arc<CodeMode>,
}

/// A thread's custom system prompt, from the Console: `None` when there is
/// none. A submission carries `Option<Custom>`, where `None` means the
/// submitter did not say, so the thread keeps the prompt it has.
pub type Custom = Option<String>;

/// The turn metadata key that holds the custom system prompt a turn was
/// submitted with. An empty string records that it had none.
const SYSTEM_PROMPT_KEY: &str = "system_prompt";

/// A Centaur thread and the nanocode session that holds it.
#[derive(Clone, Debug)]
pub struct ThreadRef {
    pub key: String,
    pub session_id: String,
}

pub enum Command {
    Submit {
        thread: ThreadRef,
        request_id: String,
        input_line: Value,
        steer: bool,
        /// The turn's verified Code Mode token, kept in its own metadata.
        tool_token: Option<codemode::TurnToken>,
        /// The custom system prompt for this turn, also kept in its metadata.
        system_prompt: Option<Custom>,
        reply: oneshot::Sender<Result<SubmitOutcome, SubmitError>>,
    },
    Interrupt {
        thread: ThreadRef,
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
    Page {
        thread: ThreadRef,
        after: u64,
        reply: oneshot::Sender<Result<Page, String>>,
    },
    Status {
        thread: ThreadRef,
        reply: oneshot::Sender<Result<ThreadStatus, String>>,
    },
}

/// The turns a thread has not finished, from its durable log.
#[derive(Debug)]
pub struct ThreadStatus {
    pub running: Option<String>,
    pub queued: Vec<String>,
}

#[derive(Debug)]
pub struct SubmitOutcome {
    pub status: &'static str,
    pub created: bool,
    pub steered: bool,
}

#[derive(Debug)]
pub enum SubmitError {
    BadRequest(String),
    Conflict(String),
    Internal(String),
}

pub struct Page {
    pub session_id: String,
    pub events: Vec<LoggedEvent>,
    pub next_cursor: u64,
    pub has_more: bool,
    /// Changes whenever the session log gains events.
    pub wake: watch::Receiver<u64>,
}

pub fn spawn(
    config: Config,
    commands: mpsc::UnboundedReceiver<Command>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nanocode".into())
        .spawn(move || {
            let outcome = nanocode_executor::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(Error::from)
                .and_then(|runtime| runtime.block_on(run(config, commands)));
            if let Err(error) = outcome {
                tracing::error!(%error, "nanocode worker stopped");
                std::process::exit(1);
            }
        })
}

async fn run(config: Config, mut commands: mpsc::UnboundedReceiver<Command>) -> Result<(), Error> {
    let brain = Arc::new(Brain {
        log_host: log_host(config.database_url.as_deref())?,
        default_model: config.default_model.parse()?,
        thinking: config.thinking.parse()?,
        openai_base_url: config.openai_base_url,
        anthropic_base_url: config.anthropic_base_url,
        code_mode: config.code_mode,
        threads: Default::default(),
        opening: Default::default(),
    });
    info!(model = %brain.default_model, thinking = ?brain.thinking, "brain ready");
    while let Some(command) = commands.recv().await {
        let brain = Arc::clone(&brain);
        nanocode_executor::spawn(async move { brain.handle(command).await });
    }
    Ok(())
}

fn log_host(database_url: Option<&str>) -> Result<Arc<dyn LogHost>, Error> {
    Ok(match database_url {
        Some(url) => {
            let pool = PgPool::new(PgOptions::parse(url)?, PG_CONNECTIONS);
            Arc::new(SqlLog::new(Arc::new(PgSql::reading(pool.clone(), pool))))
        }
        None => {
            warn!("BRAIN_DATABASE_URL is unset; sessions do not survive a restart");
            Arc::new(MemoryLog::new())
        }
    })
}

struct Brain {
    log_host: Arc<dyn LogHost>,
    default_model: ModelSelection,
    thinking: Thinking,
    openai_base_url: Option<String>,
    anthropic_base_url: Option<String>,
    code_mode: Arc<CodeMode>,
    threads: Mutex<HashMap<String, Arc<Thread>>>,
    /// Serializes opening, so a thread is opened once.
    opening: tokio::sync::Mutex<()>,
}

/// One Centaur thread: a nanocode session and its durable inbox.
///
/// nanocode fixes a session's instructions when it opens the session, so a
/// thread whose custom system prompt changes is retired and reopened with the
/// new one once it is idle (see [`Brain::retire`]). The replacement keeps the
/// same `wake` channel, so event streams following the thread carry on.
struct Thread {
    session_id: String,
    inbox: TurnInbox<OneLog>,
    wake: Arc<watch::Sender<u64>>,
    driving: AtomicBool,
    /// The custom system prompt the session was opened with.
    custom: Custom,
    /// Submissions hold it shared; retiring the thread holds it exclusively,
    /// so no turn is submitted to a thread that is being retired.
    gate: tokio::sync::RwLock<()>,
    /// Set once the thread is retired; a submission that finds it set looks
    /// the thread up again.
    retired: AtomicBool,
    /// Kept alive for the inbox's lifetime.
    _runtime: Nanocode,
}

impl Brain {
    async fn handle(self: Arc<Self>, command: Command) {
        match command {
            Command::Submit {
                thread,
                request_id,
                input_line,
                steer,
                tool_token,
                system_prompt,
                reply,
            } => {
                let outcome = self
                    .submit(
                        &thread,
                        &request_id,
                        &input_line,
                        steer,
                        tool_token,
                        system_prompt,
                    )
                    .await;
                let _ = reply.send(outcome);
            }
            Command::Interrupt { thread, reply } => {
                let outcome = match self.thread(&thread, None, None).await {
                    Ok(thread) => thread.interrupt().await,
                    Err(error) => Err(error.to_string()),
                };
                let _ = reply.send(outcome);
            }
            Command::Status { thread, reply } => {
                let outcome = match self.thread(&thread, None, None).await {
                    Ok(thread) => thread.status().await,
                    Err(error) => Err(error.to_string()),
                };
                let _ = reply.send(outcome);
            }
            Command::Page {
                thread,
                after,
                reply,
            } => {
                let outcome = match self.thread(&thread, None, None).await {
                    Ok(thread) => thread.page(after).await,
                    Err(error) => Err(error.to_string()),
                };
                let _ = reply.send(outcome);
            }
        }
    }

    async fn submit(
        self: &Arc<Self>,
        thread_ref: &ThreadRef,
        request_id: &str,
        input_line: &Value,
        steer: bool,
        tool_token: Option<codemode::TurnToken>,
        system_prompt: Option<Custom>,
    ) -> Result<SubmitOutcome, SubmitError> {
        let line = serde_json::to_string(input_line)
            .map_err(|error| SubmitError::BadRequest(error.to_string()))?;
        let turn = match blocks::parse(&line).map_err(SubmitError::BadRequest)? {
            Input::User(turn) => turn,
            _ => {
                return Err(SubmitError::BadRequest(
                    "input_line must be a user line".into(),
                ));
            }
        };
        let model: Option<ModelSelection> =
            turn.model.as_deref().and_then(|model| model.parse().ok());
        loop {
            let thread = self
                .thread(thread_ref, model.clone(), system_prompt.as_ref())
                .await
                .map_err(|error| SubmitError::Internal(error.to_string()))?;
            let _gate = thread.gate.read().await;
            if thread.retired.load(Ordering::Acquire) {
                continue;
            }
            let outcome = thread
                .submit(
                    request_id,
                    &turn.text,
                    &thread_ref.key,
                    steer,
                    tool_token.clone(),
                    system_prompt.as_ref(),
                )
                .await?;
            // Under the gate, so a thread with work is never retired.
            thread.drive();
            return Ok(outcome);
        }
    }

    /// The open thread, opening it (and resuming its pending turns) if needed.
    ///
    /// With `custom`, a thread open with another custom system prompt is
    /// reopened with this one if it is idle. A busy thread keeps its prompt,
    /// and the change applies to the first turn submitted once it is idle.
    async fn thread(
        self: &Arc<Self>,
        thread: &ThreadRef,
        model: Option<ModelSelection>,
        custom: Option<&Custom>,
    ) -> Result<Arc<Thread>, Error> {
        let session_id = thread.session_id.clone();
        let current = |open: &Thread| custom.is_none_or(|custom| *custom == open.custom);
        if let Some(open) = self.open_thread(&session_id)
            && current(&open)
        {
            return Ok(open);
        }
        let _opening = self.opening.lock().await;
        let mut wake = None;
        if let Some(open) = self.open_thread(&session_id) {
            if current(&open) || !self.retire(&open).await {
                return Ok(open);
            }
            info!(session_id, "reopening the session with a new system prompt");
            wake = Some(Arc::clone(&open.wake));
        }
        let thread = Arc::new(
            self.open(&session_id, &thread.key, model, custom, wake)
                .await?,
        );
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id, Arc::clone(&thread));
        thread.drive();
        Ok(thread)
    }

    /// Retire an idle thread so it can be reopened: no submission in flight
    /// and no unfinished turn. Returns whether it was retired. A driver that is
    /// still polling the empty inbox stops once it sees the thread retired.
    async fn retire(&self, thread: &Thread) -> bool {
        let Ok(_gate) = thread.gate.try_write() else {
            return false;
        };
        match thread.inbox.log().pending().await {
            Ok(pending) if pending.is_empty() => {}
            _ => return false,
        }
        thread.retired.store(true, Ordering::Release);
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&thread.session_id);
        if let Err(error) = thread.inbox.shutdown().await {
            warn!(session_id = %thread.session_id, %error, "shutting down a retired session failed");
        }
        true
    }

    fn open_thread(&self, session_id: &str) -> Option<Arc<Thread>> {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
    }

    /// Open a thread's session. Its custom system prompt is the one its
    /// unfinished turn was submitted with, so a turn resumed after a restart
    /// keeps its prompt; with no such turn, it is `custom`.
    async fn open(
        &self,
        session_id: &str,
        thread_key: &str,
        model: Option<ModelSelection>,
        custom: Option<&Custom>,
        wake: Option<Arc<watch::Sender<u64>>>,
    ) -> Result<Thread, Error> {
        let policy = Policy {
            restarts: TURN_RESTARTS,
            effects: codemode::effects(),
            ..Policy::default()
        };
        let parts = nanocode::turnlog(Arc::clone(&self.log_host), session_id, policy).await?;
        // Direct function tools. By default nanocode offers only its own code
        // mode (`exec`, calling the tools from JavaScript) and refuses direct
        // calls; with direct tools, the subagent tools are direct functions
        // too.
        let mut tools = Tools::builder()
            .without_defaults()
            .effect_gate(Arc::clone(&parts.gate))
            .cell_journals(parts.journals.clone())
            .exposure(ToolExposure::DirectOnly);
        let log: Arc<dyn codemode::Turns> = parts.log.clone();
        for tool in self.code_mode.tools(thread_key, &log) {
            tools = tools.tool(tool);
        }
        let tools = tools.build()?;
        let custom = match unfinished_custom(&parts.log).await? {
            Some(stored) => stored,
            None => custom.cloned().flatten(),
        };
        let instructions: Arc<str> = prompt::compose(custom.as_deref()).into();
        // With direct tools the model would also see nanocode's subagent
        // tools; a brain turn gets exactly the Code Mode tools.
        let runtime = Nanocode::builder()
            .openai(openai(
                self.openai_base_url.as_deref(),
                Arc::clone(&instructions),
            )?)
            .claude(claude(self.anthropic_base_url.as_deref(), instructions))
            .tools(tools)
            .without_cua()
            .runner_management(false)
            .thinking(self.thinking)
            .durability(parts.store.clone())
            .without_subagents()
            .build_async()
            .await?;
        // Everything already in the log is durable; later events become
        // visible only once the host has committed them.
        let durable = last_seq(&parts.log).await?;
        let wake = match wake {
            Some(wake) => {
                wake.send_if_modified(|latest| {
                    let newer = durable > *latest;
                    *latest = (*latest).max(durable);
                    newer
                });
                wake
            }
            None => Arc::new(watch::channel(durable).0),
        };
        let host = Arc::new(Host {
            log: Arc::clone(&parts.log),
            wake: Arc::clone(&wake),
        });
        let record = SessionRecord::root(
            session_id,
            model.unwrap_or_else(|| self.default_model.clone()),
            None,
        );
        let (inbox, pump) = TurnInbox::open(
            (*runtime).clone(),
            &parts.store,
            record,
            parts.log,
            host,
            LOG_WINDOW,
        )
        .await?;
        nanocode_executor::spawn(pump);
        info!(
            session_id,
            custom_system_prompt = custom.is_some(),
            "opened session"
        );
        Ok(Thread {
            session_id: session_id.to_owned(),
            inbox,
            wake,
            driving: AtomicBool::new(false),
            custom,
            gate: tokio::sync::RwLock::new(()),
            retired: AtomicBool::new(false),
            _runtime: runtime,
        })
    }
}

impl Thread {
    async fn submit(
        &self,
        request_id: &str,
        text: &str,
        thread_key: &str,
        steer: bool,
        tool_token: Option<codemode::TurnToken>,
        system_prompt: Option<&Custom>,
    ) -> Result<SubmitOutcome, SubmitError> {
        let internal = |error: &dyn std::fmt::Display| SubmitError::Internal(error.to_string());
        let running = self.running().await.map_err(|error| internal(&error))?;
        // The turn's Code Mode token lives in its own record, and only there:
        // nanocode stores, resumes, and retires it with this turn. A retry of
        // the same request keeps the first submission's metadata.
        let mut metadata = json!({"thread_key": thread_key});
        if let Some(token) = tool_token {
            metadata["tool_token"] = json!(token);
        }
        if let Some(custom) = system_prompt {
            metadata[SYSTEM_PROMPT_KEY] = json!(custom.as_deref().unwrap_or_default());
        }
        let submission = Submission {
            request_id: request_id.to_owned(),
            prompt: Prompt::new(text),
            hand: None,
            metadata: to_raw_value(&metadata).map_err(|error| internal(&error))?,
        };
        let submitted = self
            .inbox
            .log()
            .submit(&submission)
            .await
            .map_err(|error| internal(&error))?;
        let record = match submitted {
            Submitted::Accepted { record, event } => {
                self.wake(event.seq);
                record
            }
            Submitted::Existing(record) => {
                return Ok(SubmitOutcome {
                    status: record.state.as_str(),
                    created: false,
                    steered: false,
                });
            }
            Submitted::Conflict(_) => {
                return Err(SubmitError::Conflict(format!(
                    "request {request_id} was already submitted with different input"
                )));
            }
            Submitted::TooLarge { bytes, limit } => {
                return Err(SubmitError::BadRequest(format!(
                    "the turn is {bytes} bytes; the limit is {limit}"
                )));
            }
        };
        let mut steered = false;
        if steer && let Some(running) = running {
            steered = matches!(
                self.inbox
                    .move_queued(request_id, Some(&running))
                    .await
                    .map_err(|error| internal(&error))?,
                MovedQueued::Moved
            );
        }
        Ok(SubmitOutcome {
            status: record.state.as_str(),
            created: true,
            steered,
        })
    }

    async fn running(&self) -> Result<Option<String>, String> {
        let pending = self
            .inbox
            .log()
            .pending()
            .await
            .map_err(|error| error.to_string())?;
        Ok(pending
            .into_iter()
            .find(|record| record.state == TurnState::Running)
            .map(|record| record.request_id))
    }

    async fn status(&self) -> Result<ThreadStatus, String> {
        let pending = self
            .inbox
            .log()
            .pending()
            .await
            .map_err(|error| error.to_string())?;
        let mut status = ThreadStatus {
            running: None,
            queued: Vec::new(),
        };
        for record in pending {
            match record.state {
                TurnState::Running => status.running = Some(record.request_id),
                TurnState::Queued => status.queued.push(record.request_id),
                _ => {}
            }
        }
        Ok(status)
    }

    async fn interrupt(&self) -> Result<Option<String>, String> {
        let Some(running) = self.running().await? else {
            return Ok(None);
        };
        self.inbox
            .cancel(&running, &Map::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(running))
    }

    async fn page(&self, after: u64) -> Result<Page, String> {
        let page = self
            .inbox
            .log()
            .page(after, PAGE_LIMIT)
            .await
            .map_err(|error| error.to_string())?;
        // Serve only committed events: a client must never see an event that
        // a crash could take back.
        let durable = *self.wake.borrow();
        let complete = page.events.iter().all(|event| event.seq <= durable);
        let events = page
            .events
            .into_iter()
            .filter(|event| event.seq <= durable)
            .map(|event| event.expanded(&self.session_id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(Page {
            session_id: self.session_id.clone(),
            next_cursor: events.last().map_or(after, |event| event.seq),
            has_more: complete && page.has_more,
            events,
            wake: self.wake.subscribe(),
        })
    }

    fn wake(&self, seq: u64) {
        self.wake.send_if_modified(|latest| {
            let newer = seq > *latest;
            *latest = (*latest).max(seq);
            newer
        });
    }

    /// Run queued turns until the inbox is idle. One driver per thread.
    fn drive(self: &Arc<Self>) {
        if self.driving.swap(true, Ordering::AcqRel) {
            return;
        }
        let thread = Arc::clone(self);
        nanocode_executor::spawn(async move {
            loop {
                if thread.retired.load(Ordering::Acquire) {
                    break;
                }
                match thread.inbox.drive(None).await {
                    Ok(Driven::Idle) => {
                        let pending = thread.inbox.log().pending().await;
                        if pending.is_ok_and(|pending| pending.is_empty()) {
                            break;
                        }
                        nanocode_executor::sleep(DRIVE_POLL).await;
                    }
                    Ok(Driven::Busy) => nanocode_executor::sleep(DRIVE_POLL).await,
                    Ok(_) => {}
                    Err(error) => {
                        warn!(session_id = %thread.session_id, %error, "drive failed");
                        nanocode_executor::sleep(DRIVE_POLL).await;
                    }
                }
            }
            thread.driving.store(false, Ordering::Release);
            // A turn submitted between the last check and the flag reset.
            if !thread.retired.load(Ordering::Acquire)
                && thread
                    .inbox
                    .log()
                    .pending()
                    .await
                    .is_ok_and(|pending| !pending.is_empty())
            {
                thread.drive();
            }
        });
    }
}

/// Commits what the inbox logs, then wakes event streams up to it. The log
/// buffers events between its own commit points and replays a turn from the
/// last one after a crash; committing on publish keeps every event a client
/// sees durable, so a client never sees one that a crash takes back.
struct Host {
    log: Arc<OneLog>,
    /// The highest committed seq.
    wake: Arc<watch::Sender<u64>>,
}

impl InboxHost for Host {
    fn publish(&self, events: Vec<LoggedEvent>) -> HostFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let Some(seq) = events.iter().map(|event| event.seq).max() else {
                return Ok(());
            };
            if seq <= *self.wake.borrow() {
                return Ok(());
            }
            let marker = to_raw_value(&seq).map_err(|error| error.to_string())?;
            self.log
                .effect(DURABLE_KEY, marker)
                .await
                .map_err(|error| error.to_string())?;
            self.wake.send_if_modified(|latest| {
                let newer = seq > *latest;
                *latest = (*latest).max(seq);
                newer
            });
            Ok(())
        })
    }
}

/// The last seq in a session log.
async fn last_seq(log: &OneLog) -> Result<u64, Error> {
    let mut cursor = 0;
    loop {
        let page = log.page(cursor, PAGE_LIMIT).await?;
        cursor = page.next_cursor;
        if !page.has_more {
            return Ok(cursor);
        }
    }
}

/// The custom system prompt the thread's unfinished turn was submitted with:
/// the running turn's, or else the first queued one's. `None` when there is no
/// unfinished turn or it was submitted without saying.
async fn unfinished_custom(log: &OneLog) -> Result<Option<Custom>, Error> {
    let pending = log.pending().await?;
    let Some(record) = pending
        .iter()
        .find(|record| record.state == TurnState::Running)
        .or_else(|| pending.first())
    else {
        return Ok(None);
    };
    #[derive(serde::Deserialize)]
    struct Metadata {
        #[serde(default)]
        system_prompt: Option<String>,
    }
    Ok(serde_json::from_str::<Metadata>(record.metadata.get())
        .ok()
        .and_then(|metadata| metadata.system_prompt)
        .map(|stored| prompt::custom(Some(&stored))))
}

/// HTTPS SSE: the WebSocket transport does not follow a regional
/// `OPENAI_BASE_URL`. The thread's instructions follow nanocode's base system
/// prompt as its additional (developer) instructions.
fn openai(base_url: Option<&str>, instructions: Arc<str>) -> Result<CodexHarness, Error> {
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| "OPENAI_API_KEY".to_owned());
    let mut builder = OpenAi::builder(key)
        .transport(nanocode::oai::transport::ResponsesTransport::Https)
        .websocket_warmup(false);
    if let Some(base_url) = base_url {
        builder = builder.api_base_url(base_url.trim_end_matches('/'));
    }
    Ok(CodexHarness::new(builder.build()?)
        .configure(move |builder| Ok(builder.additional_instructions(Arc::clone(&instructions)))))
}

/// The thread's instructions are the session's project instructions, which
/// follow the base system prompt. That replaces the environment nanocode
/// would derive from the process's working directory, which the brain's
/// agents cannot reach anyway.
fn claude(base_url: Option<&str>, instructions: Arc<str>) -> ClaudeHarness {
    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| "ANTHROPIC_API_KEY".to_owned());
    let base = base_url.map_or("https://api.anthropic.com/v1", |url| {
        url.trim_end_matches('/')
    });
    ClaudeHarness::new(ClaudeClient::with_api_key(format!("{base}/messages"), key)).configure(
        move |builder| {
            Ok(builder.environment(
                ClaudeEnvironment::new().project_instructions(instructions.to_string()),
            ))
        },
    )
}
