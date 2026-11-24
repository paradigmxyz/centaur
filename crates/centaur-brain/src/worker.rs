//! The nanocode side of the brain. nanocode brings its own executor, so every
//! agent lives on one dedicated thread; the HTTP server talks to it through
//! executor-agnostic channels.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use nanocode::{
    AgentEvents, ClaudeClient, ModelSelection, Nanocode, OpenAi, PromptRequest, Thinking, Tools,
    agent::{PromptRoute, events::AgentEvent},
    durability::{MemoryStore, SessionStore, SqlStore},
};
use nanocode_store_sql::{PgOptions, PgPool, PgSql};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    blocks::{self, Input, UserTurn},
    registry::Registry,
    translate::{self, Turn},
};

const PG_CONNECTIONS: usize = 8;
/// How long a failed turn waits for nanocode's own terminal event before the
/// brain writes one, so every turn ends exactly once.
const TERMINAL_GRACE: Duration = Duration::from_millis(250);

pub enum Command {
    Input { sandbox: String, line: String },
    Stop { sandbox: String },
}

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: Option<String>,
    pub default_model: String,
    pub thinking: String,
    pub openai_base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
}

type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn spawn(
    config: Config,
    registry: Registry,
    commands: mpsc::UnboundedReceiver<Command>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("nanocode".into())
        .spawn(move || {
            let outcome = nanocode_executor::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(Error::from)
                .and_then(|runtime| runtime.block_on(run(config, registry, commands)));
            if let Err(error) = outcome {
                tracing::error!(%error, "nanocode worker stopped");
                std::process::exit(1);
            }
        })
}

async fn run(
    config: Config,
    registry: Registry,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> Result<(), Error> {
    let default_model: ModelSelection = config.default_model.parse()?;
    let thinking: Thinking = config.thinking.parse()?;
    let store = session_store(config.database_url.as_deref())?;
    let runtime = Nanocode::builder()
        .openai(openai(config.openai_base_url.as_deref())?)
        .claude(claude(config.anthropic_base_url.as_deref()))
        .tools(Tools::builder().without_defaults().build()?)
        .without_cua()
        .runner_management(false)
        .durability(store)
        .thinking(thinking)
        .build_async()
        .await?;
    info!(model = %default_model, ?thinking, "nanocode runtime ready");
    let mut brain = Brain {
        runtime,
        registry,
        default_model,
        thinking,
        sessions: HashMap::new(),
    };
    while let Some(command) = commands.recv().await {
        match command {
            Command::Input { sandbox, line } => brain.input(&sandbox, &line).await,
            Command::Stop { sandbox } => brain.stop(&sandbox).await,
        }
    }
    brain.runtime.shutdown().await?;
    Ok(())
}

fn session_store(database_url: Option<&str>) -> Result<SessionStore, Error> {
    Ok(match database_url {
        Some(url) => {
            let pool = PgPool::new(PgOptions::parse(url)?, PG_CONNECTIONS);
            nanocode::session_store(SqlStore::new(PgSql::reading(pool.clone(), pool)))?
        }
        None => {
            warn!("BRAIN_DATABASE_URL is unset; sessions do not survive a restart");
            nanocode::session_store(MemoryStore::new()?)?
        }
    })
}

/// HTTPS SSE: brain overhead to first token measures ~3 ms over it, and the
/// WebSocket transport does not follow a regional `OPENAI_BASE_URL`.
fn openai(base_url: Option<&str>) -> Result<OpenAi, Error> {
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| "OPENAI_API_KEY".to_owned());
    let mut builder = OpenAi::builder(key)
        .transport(nanocode::oai::transport::ResponsesTransport::Https)
        .websocket_warmup(false);
    if let Some(base_url) = base_url {
        builder = builder.api_base_url(base_url.trim_end_matches('/'));
    }
    Ok(builder.build()?)
}

fn claude(base_url: Option<&str>) -> ClaudeClient {
    let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| "ANTHROPIC_API_KEY".to_owned());
    let base = base_url.map_or("https://api.anthropic.com/v1", |url| {
        url.trim_end_matches('/')
    });
    ClaudeClient::with_api_key(format!("{base}/messages"), key)
}

struct Brain {
    runtime: Nanocode,
    registry: Registry,
    default_model: ModelSelection,
    thinking: Thinking,
    sessions: HashMap<String, Session>,
}

struct Session {
    agent: nanocode::Agent,
    output: Arc<Output>,
}

/// Where a session's output goes, and the translation state of its turn.
struct Output {
    thread_id: String,
    state: Mutex<OutputState>,
}

struct OutputState {
    /// The sandbox whose socket receives this session's output.
    sandbox: String,
    turn: Option<Turn>,
    thread_announced: bool,
    /// Turns ended so far. Turns run one at a time per session, so a turn
    /// that fails without this moving never reported its own end.
    ended: u64,
}

impl Output {
    fn new(thread_id: &str, sandbox: &str) -> Self {
        Self {
            thread_id: thread_id.to_owned(),
            state: Mutex::new(OutputState {
                sandbox: sandbox.to_owned(),
                turn: None,
                thread_announced: false,
                ended: 0,
            }),
        }
    }

    fn route_to(&self, sandbox: &str) {
        sandbox.clone_into(&mut lock(&self.state).sandbox);
    }

    fn sandbox(&self) -> String {
        lock(&self.state).sandbox.clone()
    }

    fn ended(&self) -> u64 {
        lock(&self.state).ended
    }

    /// Translate one nanocode event and send the resulting lines.
    fn event(&self, registry: &Registry, event: &AgentEvent) {
        let payload: Value = serde_json::from_str(event.payload.get()).unwrap_or_default();
        let mut state = lock(&self.state);
        let mut out = Vec::new();
        if let Some(turn_id) = payload["turn_id"].as_str()
            && state.turn.as_ref().is_none_or(|turn| turn.id() != turn_id)
        {
            let announce = !state.thread_announced;
            let (turn, opening) = Turn::start(&self.thread_id, turn_id, announce);
            state.thread_announced = true;
            state.turn = Some(turn);
            out.extend(opening);
        }
        let Some(turn) = state.turn.as_mut() else {
            return;
        };
        out.extend(turn.event(event));
        if turn.finished() {
            state.turn = None;
            state.ended += 1;
        }
        for line in out {
            registry.emit(&state.sandbox, line);
        }
    }

    /// End a turn that failed without reporting its own end.
    fn fail(&self, registry: &Registry, ended_before: u64, error: &str) {
        let mut state = lock(&self.state);
        if state.ended != ended_before {
            return;
        }
        let out = match state.turn.take() {
            Some(mut turn) => turn.fail(error),
            None => translate::failed_turn(
                &self.thread_id,
                &nanocode::platform::uuid_v7().to_string(),
                error,
            ),
        };
        state.ended += 1;
        for line in out {
            registry.emit(&state.sandbox, line);
        }
    }
}

impl Brain {
    async fn input(&mut self, sandbox: &str, line: &str) {
        match blocks::parse(line) {
            Ok(Input::User(turn)) => self.user(sandbox, turn).await,
            Ok(Input::Interrupt { thread_key }) => {
                let Some(thread_key) = thread_key.or_else(|| self.registry.thread_key(sandbox))
                else {
                    return;
                };
                if let Some(session) = self.sessions.get(&blocks::session_id(&thread_key))
                    && let Err(error) = session.agent.cancel_active_turn().await
                {
                    warn!(%error, "interrupt failed");
                }
            }
            Ok(Input::Ignored { kind }) => info!(kind, "ignoring unsupported input"),
            Err(error) => warn!(%error, "dropping malformed input"),
        }
    }

    async fn user(&mut self, sandbox: &str, turn: UserTurn) {
        let request_id = turn
            .request_id
            .clone()
            .unwrap_or_else(|| nanocode::platform::uuid_v7().to_string());
        let Some(thread_key) = turn
            .thread_key
            .clone()
            .or_else(|| self.registry.thread_key(sandbox))
        else {
            self.fail(sandbox, "brain", "input has no thread_key");
            return;
        };
        self.registry.set_thread_key(sandbox, &thread_key);
        let model = turn.model.as_deref().and_then(|model| model.parse().ok());
        let (agent, output) = match self.session(sandbox, &thread_key, model).await {
            Ok(session) => (session.agent.clone(), Arc::clone(&session.output)),
            Err(error) => {
                let thread_id = blocks::session_id(&thread_key);
                self.fail(
                    sandbox,
                    &thread_id,
                    &format!("cannot open session: {error}"),
                );
                return;
            }
        };
        let ended_before = output.ended();
        let route = agent
            .route_prompt_request(PromptRequest::new(turn.text).request_id(&request_id))
            .await;
        match route {
            Ok(PromptRoute::Started(started)) => {
                let registry = self.registry.clone();
                nanocode_executor::spawn(async move {
                    if let Err(error) = started.result().await {
                        nanocode_executor::sleep(TERMINAL_GRACE).await;
                        output.fail(&registry, ended_before, &error.to_string());
                    }
                });
            }
            Ok(PromptRoute::Steered) => info!(%thread_key, "steered the active turn"),
            Err(error) => output.fail(&self.registry, ended_before, &error.to_string()),
        }
    }

    async fn session(
        &mut self,
        sandbox: &str,
        thread_key: &str,
        model: Option<ModelSelection>,
    ) -> Result<&Session, Error> {
        let id = blocks::session_id(thread_key);
        if !self.sessions.contains_key(&id) {
            let (agent, events) = match self.runtime.open_session(&id).await {
                Ok(opened) => opened,
                Err(_) => {
                    self.runtime
                        .spawn()
                        .harness_model(model.clone().unwrap_or_else(|| self.default_model.clone()))
                        .thinking(self.thinking)
                        .session_id(id.clone())
                        .build()
                        .await?
                }
            };
            let output = Arc::new(Output::new(&id, sandbox));
            pump(events, self.registry.clone(), Arc::clone(&output));
            self.sessions.insert(id.clone(), Session { agent, output });
        }
        let session = &self.sessions[&id];
        session.output.route_to(sandbox);
        if let Some(model) = model
            && session
                .agent
                .settings()
                .await
                .is_ok_and(|(current, _, _)| current != model)
            && let Err(error) = session.agent.set_harness_model(model).await
        {
            warn!(%error, "keeping the session's model");
        }
        Ok(session)
    }

    async fn stop(&mut self, sandbox: &str) {
        let stopped: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.output.sandbox() == sandbox)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stopped {
            if let Some(session) = self.sessions.remove(&id) {
                let _ = session.agent.cancel_active_turn().await;
                let _ = session.agent.shutdown().await;
            }
        }
    }

    /// End a turn that failed before it had a session to run in.
    fn fail(&self, sandbox: &str, thread_id: &str, error: &str) {
        warn!(sandbox, error, "turn failed before it started");
        let turn_id = nanocode::platform::uuid_v7().to_string();
        for line in translate::failed_turn(thread_id, &turn_id, error) {
            self.registry.emit(sandbox, line);
        }
    }
}

fn pump(mut events: AgentEvents, registry: Registry, output: Arc<Output>) {
    nanocode_executor::spawn(async move {
        while let Some(event) = events.recv().await {
            output.event(&registry, &event);
        }
    });
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
