import { describe, expect, it } from "vitest";
import type { ServerNotification, ThreadItem } from "./index";
import {
  CodexAppServerChatStreamMapper,
  type ChatSdkStreamChunk,
  type ChatSdkTaskUpdateChunk,
} from "./chat-sdk-stream";

function turnStarted(turnId = "turn-1"): ServerNotification {
  return {
    method: "turn/started",
    params: {
      threadId: "thread-1",
      turn: {
        id: turnId,
        items: [],
        itemsView: "full",
        status: "inProgress",
        error: null,
        startedAt: 1,
        completedAt: null,
        durationMs: null,
      },
    },
  };
}

function commandItem(
  overrides: Partial<Extract<ThreadItem, { type: "commandExecution" }>> = {}
) {
  return {
    type: "commandExecution",
    id: "cmd-1",
    command: "pnpm test",
    cwd: "/repo",
    processId: null,
    source: "agent",
    status: "inProgress",
    commandActions: [],
    aggregatedOutput: null,
    exitCode: null,
    durationMs: null,
    ...overrides,
  } satisfies Extract<ThreadItem, { type: "commandExecution" }>;
}

function task(chunk: ChatSdkStreamChunk | undefined): ChatSdkTaskUpdateChunk {
  if (!chunk || chunk.type !== "task_update") {
    throw new Error(`expected task_update, got ${JSON.stringify(chunk)}`);
  }
  return chunk;
}

function b64(value: string): string {
  return globalThis.btoa(value);
}

describe("CodexAppServerChatStreamMapper", () => {
  it("maps turn plans to plan_update plus stable task updates", () => {
    const mapper = new CodexAppServerChatStreamMapper();
    expect(mapper.process(turnStarted())).toEqual([]);

    const chunks = mapper.process({
      method: "turn/plan/updated",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        explanation: "Implementation plan",
        plan: [
          { step: "Inspect the stream protocol", status: "completed" },
          { step: "Map command events", status: "inProgress" },
          { step: "Wire Chat SDK chunks", status: "pending" },
        ],
      },
    });

    expect(chunks).toEqual([
      { type: "plan_update", title: "Implementation plan" },
      {
        type: "task_update",
        id: "plan:turn-1:1",
        title: "Inspect the stream protocol",
        status: "complete",
      },
      {
        type: "task_update",
        id: "plan:turn-1:2",
        title: "Map command events",
        status: "in_progress",
      },
      {
        type: "task_update",
        id: "plan:turn-1:3",
        title: "Wire Chat SDK chunks",
        status: "pending",
      },
    ]);
  });

  it("streams command lifecycle and live output as task updates", () => {
    const mapper = new CodexAppServerChatStreamMapper();
    mapper.process(turnStarted());

    const started = mapper.process({
      method: "item/started",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        startedAtMs: 1,
        item: commandItem(),
      },
    });
    expect(started).toHaveLength(1);
    const startedTask = task(started[0]);
    expect(startedTask).toMatchObject({
      type: "task_update",
      id: "cmd-1",
      title: "Run: pnpm test",
      status: "in_progress",
    });
    expect(startedTask.details).toContain("```sh\npnpm test");

    const output = mapper.process({
      method: "item/commandExecution/outputDelta",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "cmd-1",
        delta: "first line\n",
      },
    });
    expect(output).toHaveLength(1);
    const outputTask = task(output[0]);
    expect(outputTask).toMatchObject({
      type: "task_update",
      id: "cmd-1",
      status: "in_progress",
    });
    expect(outputTask.output).toContain("first line");

    const stdin = mapper.process({
      method: "item/commandExecution/terminalInteraction",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        itemId: "cmd-1",
        processId: "proc-1",
        stdin: "y\n",
      },
    });
    expect(stdin).toHaveLength(1);
    const stdinTask = task(stdin[0]);
    expect(stdinTask).toMatchObject({
      type: "task_update",
      id: "cmd-1",
      status: "in_progress",
    });
    expect(stdinTask.output).toContain("[stdin]\ny");

    const completedItem = commandItem({
      status: "completed",
      aggregatedOutput: "first line\nsecond line\n",
      exitCode: 0,
      durationMs: 20,
    });
    const completed = mapper.process({
      method: "item/completed",
      params: {
        threadId: "thread-1",
        turnId: "turn-1",
        completedAtMs: 2,
        item: completedItem,
      },
    });
    expect(completed).toHaveLength(1);
    const completedTask = task(completed[0]);
    expect(completedTask).toMatchObject({
      type: "task_update",
      id: "cmd-1",
      status: "complete",
    });
    expect(completedTask.output).toContain("second line");

    const replayedTurn = mapper.process({
      method: "turn/completed",
      params: {
        threadId: "thread-1",
        turn: {
          id: "turn-1",
          items: [completedItem],
          itemsView: "full",
          status: "completed",
          error: null,
          startedAt: 1,
          completedAt: 2,
          durationMs: 1000,
        },
      },
    });
    expect(replayedTurn).toEqual([]);
  });

  it("maps connection command and process output notifications to task updates", () => {
    const mapper = new CodexAppServerChatStreamMapper();
    mapper.process(turnStarted());

    const commandExec = mapper.process({
      method: "command/exec/outputDelta",
      params: {
        processId: "exec-1",
        stream: "stdout",
        deltaBase64: b64("installing\n"),
        capReached: false,
      },
    });
    expect(commandExec).toHaveLength(1);
    const commandExecTask = task(commandExec[0]);
    expect(commandExecTask).toMatchObject({
      type: "task_update",
      id: "command-exec:exec-1",
      title: "Command exec: exec-1",
      status: "in_progress",
    });
    expect(commandExecTask.output).toContain("[stdout]\ninstalling");

    const processStdout = mapper.process({
      method: "process/outputDelta",
      params: {
        processHandle: "proc-1",
        stream: "stdout",
        deltaBase64: b64("ready\n"),
        capReached: false,
      },
    });
    expect(processStdout).toHaveLength(1);
    const processStdoutTask = task(processStdout[0]);
    expect(processStdoutTask).toMatchObject({
      type: "task_update",
      id: "process:proc-1",
      title: "Process: proc-1",
      status: "in_progress",
    });
    expect(processStdoutTask.output).toContain("[stdout]\nready");

    const processStderr = mapper.process({
      method: "process/outputDelta",
      params: {
        processHandle: "proc-1",
        stream: "stderr",
        deltaBase64: b64("warning\n"),
        capReached: true,
      },
    });
    const processStderrTask = task(processStderr[0]);
    expect(processStderrTask.output).toContain("[stderr]\nwarning");
    expect(processStderrTask.output).toContain("[stderr output capped]");

    const processExited = mapper.process({
      method: "process/exited",
      params: {
        processHandle: "proc-1",
        exitCode: 1,
        stdout: "",
        stdoutCapReached: false,
        stderr: "",
        stderrCapReached: false,
      },
    });
    const processExitedTask = task(processExited[0]);
    expect(processExitedTask).toMatchObject({
      type: "task_update",
      id: "process:proc-1",
      status: "error",
    });
    expect(processExitedTask.output).toContain("exit code 1");
    expect(processExitedTask.output).toContain("warning");

    const flushed = mapper.flush();
    expect(task(flushed[0])).toMatchObject({
      id: "command-exec:exec-1",
      status: "complete",
    });
  });

  it("keeps commentary as a Thinking task and final answer as markdown text", () => {
    const mapper = new CodexAppServerChatStreamMapper();
    mapper.process(turnStarted());

    expect(
      mapper.process({
        method: "item/started",
        params: {
          threadId: "thread-1",
          turnId: "turn-1",
          startedAtMs: 1,
          item: {
            type: "agentMessage",
            id: "commentary-1",
            text: "",
            phase: "commentary",
            memoryCitation: null,
          },
        },
      })
    ).toEqual([
      {
        type: "task_update",
        id: "commentary-1",
        title: "Thinking",
        status: "in_progress",
      },
    ]);

    expect(
      mapper.process({
        method: "item/agentMessage/delta",
        params: {
          threadId: "thread-1",
          turnId: "turn-1",
          itemId: "commentary-1",
          delta: "Checking the command output",
        },
      })
    ).toEqual([
      {
        type: "task_update",
        id: "commentary-1",
        title: "Thinking",
        status: "in_progress",
        details: "Checking the command output",
      },
    ]);

    expect(
      mapper.process({
        method: "item/reasoning/summaryTextDelta",
        params: {
          threadId: "thread-1",
          turnId: "turn-1",
          itemId: "reasoning-1",
          summaryIndex: 0,
          delta: "Inspecting the event stream",
        },
      })
    ).toEqual([
      {
        type: "task_update",
        id: "reasoning-1",
        title: "Thinking",
        status: "in_progress",
        details: "Inspecting the event stream",
      },
    ]);

    expect(
      mapper.process({
        method: "item/completed",
        params: {
          threadId: "thread-1",
          turnId: "turn-1",
          completedAtMs: 2,
          item: {
            type: "agentMessage",
            id: "commentary-1",
            text: "Checking the command output",
            phase: "commentary",
            memoryCitation: null,
          },
        },
      })
    ).toEqual([
      {
        type: "task_update",
        id: "commentary-1",
        title: "Thinking",
        status: "complete",
        details: "Checking the command output",
      },
    ]);

    expect(
      mapper.process({
        method: "item/agentMessage/delta",
        params: {
          threadId: "thread-1",
          turnId: "turn-1",
          itemId: "answer-1",
          delta: "Done",
        },
      })
    ).toEqual([{ type: "markdown_text", text: "Done" }]);

    expect(
      mapper.process({
        method: "item/completed",
        params: {
          threadId: "thread-1",
          turnId: "turn-1",
          completedAtMs: 3,
          item: {
            type: "agentMessage",
            id: "answer-1",
            text: "Done.",
            phase: "final_answer",
            memoryCitation: null,
          },
        },
      })
    ).toEqual([{ type: "markdown_text", text: "." }]);
  });
});
