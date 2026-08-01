use std::{
    collections::HashMap,
    sync::{
        Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocodex::{
    AgentEventKind, AgentEvents, AgentHandle, Nanocodex, Tool, ToolContext, ToolDefinition,
    ToolExecution, ToolInput, ToolResult, Tools, ToolsBuildError, async_trait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTask {
    role: String,
    task: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowUpTask {
    agent_id: u64,
    task: String,
}

#[derive(Serialize)]
struct WorkerResult {
    agent_id: u64,
    kind: &'static str,
    role: String,
    report: String,
}

#[derive(Serialize)]
struct FollowUpResult {
    agent_id: u64,
    report: String,
}

struct ChildSession {
    agent: Nanocodex,
    event_task: JoinHandle<()>,
}

#[derive(Default)]
pub(crate) struct ChildAgents {
    next_id: AtomicU64,
    agents: tokio::sync::Mutex<HashMap<u64, ChildSession>>,
}

impl ChildAgents {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn insert(&self, id: u64, agent: Nanocodex, event_task: JoinHandle<()>) {
        self.agents
            .lock()
            .await
            .insert(id, ChildSession { agent, event_task });
    }

    async fn get(&self, id: u64) -> Option<Nanocodex> {
        self.agents
            .lock()
            .await
            .get(&id)
            .map(|session| session.agent.clone())
    }

    pub(crate) async fn shutdown(&self) {
        let sessions = std::mem::take(&mut *self.agents.lock().await);
        let mut event_tasks = Vec::with_capacity(sessions.len());
        for session in sessions.into_values() {
            event_tasks.push(session.event_task);
            drop(session.agent);
        }
        for event_task in event_tasks {
            drop(event_task.await);
        }
    }
}

#[derive(Clone, Copy)]
enum ChildKind {
    Spawn,
    Fork,
}

impl ChildKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Spawn => "spawn_agent",
            Self::Fork => "fork_agent",
        }
    }

    const fn result_name(self) -> &'static str {
        match self {
            Self::Spawn => "independent",
            Self::Fork => "fork",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Spawn => {
                "Starts a reusable clean child agent without the invoking agent's conversation history, runs its first task, and returns its agent_id and report. The child may inspect the shared workspace but is instructed not to modify it."
            }
            Self::Fork => {
                "Starts a reusable read-only child agent from the invoking agent's latest completed checkpoint, runs its first task, and returns its agent_id and report. This is unavailable until the invoking agent has completed at least one turn."
            }
        }
    }

    fn prompt(self, task: &str) -> String {
        let context = match self {
            Self::Spawn => "You have no inherited conversation context.",
            Self::Fork => "Use the inherited conversation only as context for this delegation.",
        };
        format!(
            "Act as a read-only specialist child agent. {context} Inspect the shared workspace as \
             needed, but do not modify files or run destructive commands. Return a compact, \
             evidence-backed report to the parent agent.\n\nDelegated task:\n{task}"
        )
    }
}

struct ChildAgent {
    agent: AgentHandle,
    agents: Weak<ChildAgents>,
    kind: ChildKind,
}

impl ChildAgent {
    const fn new(agent: AgentHandle, agents: Weak<ChildAgents>, kind: ChildKind) -> Self {
        Self {
            agent,
            agents,
            kind,
        }
    }
}

fn drain_events(
    agent_id: u64,
    role: String,
    kind: &'static str,
    mut events: AgentEvents,
) -> JoinHandle<()> {
    let log_jsonl = std::env::var_os("NANOCODEX_SUBAGENT_JSONL").is_some();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if log_jsonl
                && matches!(
                    event.kind,
                    AgentEventKind::RunStarted
                        | AgentEventKind::RunCompleted
                        | AgentEventKind::RunFailed
                )
            {
                eprintln!(
                    "{}",
                    json!({
                        "agent_id": agent_id,
                        "role": role,
                        "kind": kind,
                        "event": event,
                    })
                );
            }
        }
    })
}

#[async_trait]
impl Tool for ChildAgent {
    fn name(&self) -> &'static str {
        self.kind.name()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            self.kind.description(),
            json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "description": "A short worker role for result attribution."
                    },
                    "task": {
                        "type": "string",
                        "description": "A complete, focused task for the child agent."
                    }
                },
                "required": ["role", "task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let AgentTask { role, task } = input.decode_json()?;
        let agents = self
            .agents
            .upgrade()
            .ok_or_else(|| std::io::Error::other("child-agent registry stopped"))?;
        let agent_id = agents.next_id();
        let (child, events) = match self.kind {
            ChildKind::Spawn => self.agent.spawn().await,
            ChildKind::Fork => self.agent.fork().await,
        }?;
        let event_task = drain_events(agent_id, role.clone(), self.kind.result_name(), events);

        let result = child
            .prompt(self.kind.prompt(&task))
            .await?
            .result()
            .await?;
        agents.insert(agent_id, child, event_task).await;
        Ok(ToolExecution::json(&WorkerResult {
            agent_id,
            kind: self.kind.result_name(),
            role,
            report: result.final_message,
        }))
    }
}

struct PromptAgent {
    agents: Weak<ChildAgents>,
}

#[async_trait]
impl Tool for PromptAgent {
    fn name(&self) -> &'static str {
        "prompt_agent"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            "Runs a follow-up turn on a previously spawned or forked child, preserving that child's conversation, response chain, cache lineage, WebSocket, and tools.",
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "The agent_id returned by spawn_agent or fork_agent."
                    },
                    "task": {
                        "type": "string",
                        "description": "The next prompt for that child agent."
                    }
                },
                "required": ["agent_id", "task"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let FollowUpTask { agent_id, task } = input.decode_json()?;
        let agents = self
            .agents
            .upgrade()
            .ok_or_else(|| std::io::Error::other("child-agent registry stopped"))?;
        let child = agents
            .get(agent_id)
            .await
            .ok_or_else(|| std::io::Error::other(format!("unknown agent_id {agent_id}")))?;
        let result = child.prompt(task).await?.result().await?;
        Ok(ToolExecution::json(&FollowUpResult {
            agent_id,
            report: result.final_message,
        }))
    }
}

pub(crate) fn with_subagents(
    tools: Tools,
    agent: AgentHandle,
    agents: Weak<ChildAgents>,
) -> Result<Tools, ToolsBuildError> {
    tools
        .into_builder()
        .tool(ChildAgent::new(
            agent.clone(),
            agents.clone(),
            ChildKind::Spawn,
        ))
        .tool(ChildAgent::new(agent, agents.clone(), ChildKind::Fork))
        .tool(PromptAgent { agents })
        .build()
}

#[cfg(test)]
mod tests {
    use super::ChildKind;

    #[test]
    fn child_tool_names_are_stable() {
        assert_eq!(ChildKind::Spawn.name(), "spawn_agent");
        assert_eq!(ChildKind::Fork.name(), "fork_agent");
    }
}
