/**
 * A session API `fetch` that answers create/append/execute and streams one
 * Codex turn: a reasoning line, a completed `gh api` command, then `answer`.
 * The command lands in the reply's chain of thought.
 */
export function fakeSessionApiFetch(answer: string): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input));
    if ((init?.method ?? "GET") === "GET" && url.pathname.endsWith("/events")) {
      const body = codexTurnLines(answer)
        .map(
          (line, index) =>
            `event: session.output.line\nid: ${index + 1}\ndata: ${line}\n\n`,
        )
        .join("");
      return new Response(body, {
        headers: { "content-type": "text/event-stream" },
      });
    }
    if (url.pathname.endsWith("/execute")) {
      return Response.json({ execution_id: "exec-1", ok: true, status: "running" });
    }
    return Response.json({ ok: true });
  }) as typeof fetch;
}

export const FAKE_TURN_COMMAND = "gh api repos/o/other-repo/contents/notes.md";

function codexTurnLines(answer: string): string[] {
  const ids = { threadId: "thread-1", turnId: "turn-1" };
  const command = {
    type: "commandExecution",
    id: "cmd-1",
    command: FAKE_TURN_COMMAND,
    cwd: "/repo",
    processId: "proc-1",
    source: "agent",
    commandActions: [],
  };
  return [
    {
      method: "turn/started",
      params: {
        threadId: "thread-1",
        turn: { id: "turn-1", items: [], status: "inProgress", startedAt: 1 },
      },
    },
    {
      method: "item/started",
      params: {
        ...ids,
        startedAtMs: 2,
        item: { type: "agentMessage", id: "answer-1", text: "", phase: "final_answer" },
      },
    },
    {
      method: "item/reasoning/summaryTextDelta",
      params: { ...ids, itemId: "reasoning-1", summaryIndex: 0, delta: "Reading the notes" },
    },
    {
      method: "item/started",
      params: {
        ...ids,
        startedAtMs: 2,
        item: { ...command, status: "inProgress", aggregatedOutput: null, exitCode: null },
      },
    },
    {
      method: "item/completed",
      params: {
        ...ids,
        completedAtMs: 3,
        item: { ...command, status: "completed", aggregatedOutput: "notes\n", exitCode: 0 },
      },
    },
    {
      method: "item/agentMessage/delta",
      params: { ...ids, itemId: "answer-1", delta: answer },
    },
    { type: "turn.completed", turn: { id: "turn-1", items: [] } },
  ].map((line) => JSON.stringify(line));
}
