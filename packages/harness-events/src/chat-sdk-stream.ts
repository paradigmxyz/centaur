import type { StreamChunk } from "chat";
import type { ServerNotification, ThreadItem, Turn, TurnPlanStepStatus } from "./index";

export type ChatSdkStreamValue = string | ChatSdkStreamChunk;
export type ChatSdkStreamChunk = StreamChunk;
export type ChatSdkMarkdownTextChunk = Extract<StreamChunk, { type: "markdown_text" }>;
export type ChatSdkTaskUpdateChunk = Extract<StreamChunk, { type: "task_update" }>;
export type ChatSdkPlanUpdateChunk = Extract<StreamChunk, { type: "plan_update" }>;
export type ChatSdkTaskStatus = ChatSdkTaskUpdateChunk["status"];
type OutputStreamLabel = "stdout" | "stderr" | "stdin";

export interface CodexAppServerToChatStreamOptions {
  /**
   * App-server may omit agent message phases. Treating unknown messages as final
   * preserves the user's answer instead of silently hiding text.
   */
  unknownAgentMessagePhase?: "final_answer" | "commentary";
  /**
   * Reasoning summaries are user-facing. Raw reasoning text is ignored unless
   * explicitly enabled.
   */
  includeReasoningSummaries?: boolean;
  includeReasoningText?: boolean;
  /**
   * Prevent enormous terminal logs from becoming unusable task updates.
   * Set to Infinity to disable clipping.
   */
  maxTaskOutputChars?: number;
  maxTaskDetailsChars?: number;
  maxTitleChars?: number;
}

interface ItemState {
  agentText: string;
  commandOutput: string;
  details?: string;
  emittedAgentText: string;
  lastOutputStream?: OutputStreamLabel;
  output?: string;
  phase?: "commentary" | "final_answer" | null;
  status: ChatSdkTaskStatus;
  title?: string;
  type?: ThreadItem["type"];
}

interface NormalizedOptions {
  includeReasoningSummaries: boolean;
  includeReasoningText: boolean;
  maxTaskDetailsChars: number;
  maxTaskOutputChars: number;
  maxTitleChars: number;
  unknownAgentMessagePhase: "final_answer" | "commentary";
}

const DEFAULT_OPTIONS: NormalizedOptions = {
  includeReasoningSummaries: true,
  includeReasoningText: false,
  maxTaskDetailsChars: 8_000,
  maxTaskOutputChars: 16_000,
  maxTitleChars: 120,
  unknownAgentMessagePhase: "final_answer",
};

export class CodexAppServerChatStreamMapper {
  private readonly options: NormalizedOptions;
  private readonly items = new Map<string, ItemState>();
  private readonly lastTaskById = new Map<string, string>();
  private readonly reasoningTextByItemId = new Map<string, string[]>();
  private readonly reasoningSummaryByItemId = new Map<string, string[]>();
  private currentTurnId = "";
  private lastPlanTitle = "";

  constructor(options: CodexAppServerToChatStreamOptions = {}) {
    this.options = { ...DEFAULT_OPTIONS, ...options };
  }

  process(notification: ServerNotification): ChatSdkStreamChunk[] {
    switch (notification.method) {
      case "turn/started":
        this.resetTurn(notification.params.turn.id);
        return [];
      case "turn/plan/updated":
        return this.handleTurnPlanUpdated(notification.params);
      case "item/started":
        return this.handleItem(notification.params.item, "started");
      case "item/completed":
        return this.handleItem(notification.params.item, "completed");
      case "item/agentMessage/delta":
        return this.handleAgentMessageDelta(
          notification.params.itemId,
          notification.params.delta
        );
      case "item/commandExecution/outputDelta":
        return this.handleCommandOutputDelta(
          notification.params.itemId,
          notification.params.delta
        );
      case "item/commandExecution/terminalInteraction":
        return this.handleTerminalInteraction(notification.params);
      case "item/fileChange/outputDelta":
        return this.handleFileChangeOutputDelta(notification.params);
      case "item/fileChange/patchUpdated":
        return this.handleFileChangePatchUpdated(notification.params);
      case "item/mcpToolCall/progress":
        return this.handleMcpProgress(notification.params);
      case "item/plan/delta":
        return this.handlePlanText(notification.params.itemId, notification.params.delta);
      case "command/exec/outputDelta":
        return this.handleCommandExecOutputDelta(notification.params);
      case "process/outputDelta":
        return this.handleProcessOutputDelta(notification.params);
      case "process/exited":
        return this.handleProcessExited(notification.params);
      case "item/reasoning/summaryTextDelta":
        return this.handleReasoningDelta(
          notification.params.itemId,
          notification.params.summaryIndex,
          notification.params.delta,
          "summary"
        );
      case "item/reasoning/textDelta":
        return this.handleReasoningDelta(
          notification.params.itemId,
          notification.params.contentIndex,
          notification.params.delta,
          "text"
        );
      case "turn/completed":
        return this.handleTurnCompleted(notification.params.turn);
      case "error":
        return this.failOpenTasks(notification.params.error.message);
      default:
        return [];
    }
  }

  flush(): ChatSdkStreamChunk[] {
    return this.completeOpenTasks();
  }

  private resetTurn(turnId: string): void {
    this.currentTurnId = turnId;
    this.items.clear();
    this.lastTaskById.clear();
    this.reasoningTextByItemId.clear();
    this.reasoningSummaryByItemId.clear();
    this.lastPlanTitle = "";
  }

  private stateFor(itemId: string): ItemState {
    let state = this.items.get(itemId);
    if (!state) {
      state = {
        agentText: "",
        commandOutput: "",
        emittedAgentText: "",
        status: "in_progress",
      };
      this.items.set(itemId, state);
    }
    return state;
  }

  private handleItem(
    item: ThreadItem,
    lifecycle: "started" | "completed"
  ): ChatSdkStreamChunk[] {
    const state = this.stateFor(item.id);
    state.type = item.type;

    switch (item.type) {
      case "agentMessage":
        state.phase = item.phase;
        return lifecycle === "completed"
          ? this.handleCompletedAgentMessage(item.id, item.text, item.phase)
          : this.handleStartedAgentMessage(item.id, item.phase);
      case "reasoning":
        return this.handleCompletedReasoningItem(item);
      case "commandExecution":
        return this.handleCommandExecution(item, lifecycle);
      case "dynamicToolCall":
        return this.handleDynamicToolCall(item);
      case "mcpToolCall":
        return this.handleMcpToolCall(item);
      case "fileChange":
        return this.handleFileChange(item);
      case "plan":
        return lifecycle === "completed" ? this.handlePlanText(item.id, item.text, true) : [];
      case "webSearch":
        return this.emitItemTask(item.id, {
          title: this.title(`Search: ${item.query || "web"}`),
          status: lifecycle === "completed" ? "complete" : "in_progress",
          details: item.action ? this.jsonDetails("Action", item.action) : undefined,
        });
      case "imageView":
        return this.emitItemTask(item.id, {
          title: this.title(`View image: ${item.path}`),
          status: lifecycle === "completed" ? "complete" : "in_progress",
        });
      case "imageGeneration":
        return this.emitItemTask(item.id, {
          title: this.title("Generate image"),
          status: item.status.toLowerCase() === "failed" ? "error" : "complete",
          details: item.revisedPrompt ? `Prompt: ${item.revisedPrompt}` : undefined,
          output: item.savedPath ?? item.result,
        });
      case "contextCompaction":
        return this.emitItemTask(item.id, {
          title: "Compact context",
          status: lifecycle === "completed" ? "complete" : "in_progress",
        });
      default:
        return [];
    }
  }

  private handleStartedAgentMessage(
    itemId: string,
    phase: "commentary" | "final_answer" | null
  ): ChatSdkStreamChunk[] {
    if (this.agentPhase(phase) !== "commentary") {
      return [];
    }
    return this.emitItemTask(itemId, {
      title: "Thinking",
      status: "in_progress",
    });
  }

  private handleAgentMessageDelta(itemId: string, delta: string): ChatSdkStreamChunk[] {
    if (!delta) {
      return [];
    }
    const state = this.stateFor(itemId);
    state.agentText += delta;
    const phase = this.agentPhase(state.phase);
    if (phase === "commentary") {
      state.details = clip(state.agentText, this.options.maxTaskDetailsChars);
      return this.emitItemTask(itemId, {
        title: "Thinking",
        status: "in_progress",
        details: state.details,
      });
    }

    state.emittedAgentText += delta;
    return [{ type: "markdown_text", text: delta }];
  }

  private handleCompletedAgentMessage(
    itemId: string,
    text: string,
    phase: "commentary" | "final_answer" | null
  ): ChatSdkStreamChunk[] {
    const state = this.stateFor(itemId);
    state.phase = phase;
    state.agentText = text || state.agentText;

    if (this.agentPhase(phase) === "commentary") {
      const details = clip(state.agentText, this.options.maxTaskDetailsChars);
      state.details = details;
      return this.emitItemTask(itemId, {
        title: "Thinking",
        status: "complete",
        details,
      });
    }

    const alreadyEmitted = state.emittedAgentText;
    if (!state.agentText || state.agentText === alreadyEmitted) {
      state.emittedAgentText = state.agentText;
      return [];
    }
    if (alreadyEmitted && state.agentText.startsWith(alreadyEmitted)) {
      const suffix = state.agentText.slice(alreadyEmitted.length);
      state.emittedAgentText = state.agentText;
      return suffix ? [{ type: "markdown_text", text: suffix }] : [];
    }
    if (!alreadyEmitted) {
      state.emittedAgentText = state.agentText;
      return [{ type: "markdown_text", text: state.agentText }];
    }

    // Chat SDK streams are append-only. If Codex's completed item corrects
    // previously streamed text instead of extending it, avoid duplicating the
    // answer. Persisted/replay views can use the canonical turn item separately.
    state.emittedAgentText = state.agentText;
    return [];
  }

  private handleCommandExecution(
    item: Extract<ThreadItem, { type: "commandExecution" }>,
    lifecycle: "started" | "completed"
  ): ChatSdkStreamChunk[] {
    const state = this.stateFor(item.id);
    const output = item.aggregatedOutput ?? state.commandOutput;
    if (item.aggregatedOutput) {
      state.commandOutput = item.aggregatedOutput;
    }
    return this.emitItemTask(item.id, {
      title: this.title(`Run: ${displayCommand(item.command)}`),
      status: commandStatus(item.status, item.exitCode, lifecycle),
      details: this.commandDetails(item.command, item.cwd),
      output: this.commandOutput(output, item.exitCode),
    });
  }

  private handleCommandOutputDelta(itemId: string, delta: string): ChatSdkStreamChunk[] {
    if (!delta) {
      return [];
    }
    const state = this.stateFor(itemId);
    state.commandOutput += delta;
    return this.emitItemTask(itemId, {
      title: state.title ?? "Command output",
      status: "in_progress",
      details: state.details,
      output: this.commandOutput(state.commandOutput),
    });
  }

  private handleTerminalInteraction(
    params: Extract<
      ServerNotification,
      { method: "item/commandExecution/terminalInteraction" }
    >["params"]
  ): ChatSdkStreamChunk[] {
    const state = this.stateFor(params.itemId);
    appendStreamOutput(state, "stdin", params.stdin, false);
    return this.emitItemTask(params.itemId, {
      title: state.title ?? this.title(`Run: ${params.processId}`),
      status: "in_progress",
      details: state.details,
      output: this.commandOutput(state.commandOutput),
    });
  }

  private handleCommandExecOutputDelta(
    params: Extract<ServerNotification, { method: "command/exec/outputDelta" }>["params"]
  ): ChatSdkStreamChunk[] {
    const taskId = commandExecTaskId(params.processId);
    const state = this.stateFor(taskId);
    appendStreamOutput(
      state,
      params.stream,
      decodeBase64Utf8(params.deltaBase64),
      params.capReached
    );
    return this.emitItemTask(taskId, {
      title: this.title(`Command exec: ${params.processId}`),
      status: "in_progress",
      output: this.commandOutput(state.commandOutput),
    });
  }

  private handleProcessOutputDelta(
    params: Extract<ServerNotification, { method: "process/outputDelta" }>["params"]
  ): ChatSdkStreamChunk[] {
    const taskId = processTaskId(params.processHandle);
    const state = this.stateFor(taskId);
    appendStreamOutput(
      state,
      params.stream,
      decodeBase64Utf8(params.deltaBase64),
      params.capReached
    );
    return this.emitItemTask(taskId, {
      title: this.title(`Process: ${params.processHandle}`),
      status: "in_progress",
      output: this.commandOutput(state.commandOutput),
    });
  }

  private handleProcessExited(
    params: Extract<ServerNotification, { method: "process/exited" }>["params"]
  ): ChatSdkStreamChunk[] {
    const taskId = processTaskId(params.processHandle);
    const state = this.stateFor(taskId);
    const bufferedOutput = processBufferedOutput(params);
    const output = state.commandOutput || bufferedOutput;
    return this.emitItemTask(taskId, {
      title: this.title(`Process: ${params.processHandle}`),
      status: params.exitCode === 0 ? "complete" : "error",
      output: this.commandOutput(output, params.exitCode),
    });
  }

  private handleDynamicToolCall(
    item: Extract<ThreadItem, { type: "dynamicToolCall" }>
  ): ChatSdkStreamChunk[] {
    const toolName = item.namespace ? `${item.namespace}.${item.tool}` : item.tool;
    return this.emitItemTask(item.id, {
      title: this.title(`Tool: ${toolName}`),
      status: genericStatus(item.status, item.success === false),
      details: this.jsonDetails("Arguments", item.arguments),
      output: dynamicToolOutput(item.contentItems, this.options.maxTaskOutputChars),
    });
  }

  private handleMcpToolCall(
    item: Extract<ThreadItem, { type: "mcpToolCall" }>
  ): ChatSdkStreamChunk[] {
    return this.emitItemTask(item.id, {
      title: this.title(`MCP: ${item.server}.${item.tool}`),
      status: genericStatus(item.status, Boolean(item.error)),
      details: this.jsonDetails("Arguments", item.arguments),
      output: mcpOutput(item.result, item.error, this.options.maxTaskOutputChars),
    });
  }

  private handleFileChange(
    item: Extract<ThreadItem, { type: "fileChange" }>
  ): ChatSdkStreamChunk[] {
    return this.emitFileChangeTask(item.id, item.changes, patchStatus(item.status));
  }

  private handleFileChangePatchUpdated(
    params: Extract<ServerNotification, { method: "item/fileChange/patchUpdated" }>["params"]
  ): ChatSdkStreamChunk[] {
    return this.emitFileChangeTask(params.itemId, params.changes, "in_progress");
  }

  private handleFileChangeOutputDelta(
    params: Extract<ServerNotification, { method: "item/fileChange/outputDelta" }>["params"]
  ): ChatSdkStreamChunk[] {
    if (!params.delta) {
      return [];
    }
    const state = this.stateFor(params.itemId);
    state.commandOutput += params.delta;
    return this.emitItemTask(params.itemId, {
      title: state.title ?? "Apply file changes",
      status: "in_progress",
      details: state.details,
      output: this.commandOutput(state.commandOutput),
    });
  }

  private emitFileChangeTask(
    itemId: string,
    changes: Extract<ThreadItem, { type: "fileChange" }>["changes"],
    status: ChatSdkTaskStatus
  ): ChatSdkStreamChunk[] {
    const paths = Array.from(new Set(changes.map((change) => change.path).filter(Boolean)));
    const diff = changes
      .map((change) => change.diff?.trim())
      .filter(Boolean)
      .join("\n\n");
    return this.emitItemTask(itemId, {
      title: this.title(
        paths.length === 1
          ? `Edit: ${paths[0]}`
          : paths.length > 1
            ? `Edit ${paths.length} files`
            : "Apply file changes"
      ),
      status,
      details: paths.length
        ? `Files: ${paths.map((path) => inlineCode(path)).join(", ")}`
        : undefined,
      output: diff ? codeBlock(clip(diff, this.options.maxTaskOutputChars), "diff") : undefined,
    });
  }

  private handleMcpProgress(
    params: Extract<ServerNotification, { method: "item/mcpToolCall/progress" }>["params"]
  ): ChatSdkStreamChunk[] {
    if (!params.message) {
      return [];
    }
    const state = this.stateFor(params.itemId);
    state.details = [state.details, params.message].filter(Boolean).join("\n");
    return this.emitItemTask(params.itemId, {
      title: state.title ?? "MCP tool",
      status: "in_progress",
      details: state.details,
      output: state.output,
    });
  }

  private handleTurnPlanUpdated(
    params: Extract<ServerNotification, { method: "turn/plan/updated" }>["params"]
  ): ChatSdkStreamChunk[] {
    const chunks: ChatSdkStreamChunk[] = [];
    const title = this.title(params.explanation?.trim() || "Plan updated");
    if (title && title !== this.lastPlanTitle) {
      this.lastPlanTitle = title;
      chunks.push({ type: "plan_update", title });
    }

    params.plan.forEach((step, index) => {
      chunks.push(
        ...this.emitTaskUpdate({
          id: planTaskId(params.turnId, index),
          title: this.title(cleanPlanStep(step.step)),
          status: planStatus(step.status),
        })
      );
    });
    return chunks;
  }

  private handlePlanText(
    itemId: string,
    text: string,
    completed = false
  ): ChatSdkStreamChunk[] {
    if (!text.trim()) {
      return [];
    }
    const state = this.stateFor(itemId);
    state.agentText = completed ? text : state.agentText + text;
    const steps = parsePlanText(state.agentText);
    const chunks: ChatSdkStreamChunk[] = [];
    const title = this.title(firstNonEmptyLine(state.agentText) || "Plan updated");
    if (title && title !== this.lastPlanTitle) {
      this.lastPlanTitle = title;
      chunks.push({ type: "plan_update", title });
    }
    steps.forEach((step, index) => {
      chunks.push(
        ...this.emitTaskUpdate({
          id: planTaskId(this.currentTurnId || itemId, index),
          title: this.title(step.title),
          status: completed && step.status === "pending" ? "complete" : step.status,
        })
      );
    });
    return chunks;
  }

  private handleReasoningDelta(
    itemId: string,
    index: number,
    delta: string,
    kind: "summary" | "text"
  ): ChatSdkStreamChunk[] {
    if (!delta) {
      return [];
    }
    if (kind === "summary" && !this.options.includeReasoningSummaries) {
      return [];
    }
    if (kind === "text" && !this.options.includeReasoningText) {
      return [];
    }
    const bucket =
      kind === "summary" ? this.reasoningSummaryByItemId : this.reasoningTextByItemId;
    const values = bucket.get(itemId) ?? [];
    values[index] = `${values[index] ?? ""}${delta}`;
    bucket.set(itemId, values);
    return this.emitReasoningTask(itemId, "in_progress");
  }

  private handleCompletedReasoningItem(
    item: Extract<ThreadItem, { type: "reasoning" }>
  ): ChatSdkStreamChunk[] {
    if (this.options.includeReasoningSummaries && item.summary.length) {
      this.reasoningSummaryByItemId.set(item.id, item.summary);
    }
    if (this.options.includeReasoningText && item.content.length) {
      this.reasoningTextByItemId.set(item.id, item.content);
    }
    return this.emitReasoningTask(item.id, "complete");
  }

  private emitReasoningTask(itemId: string, status: ChatSdkTaskStatus): ChatSdkStreamChunk[] {
    const summary = this.reasoningSummaryByItemId.get(itemId) ?? [];
    const text = this.reasoningTextByItemId.get(itemId) ?? [];
    const details = [...summary, ...text].filter(Boolean).join("\n\n").trim();
    if (!details) {
      return [];
    }
    return this.emitItemTask(itemId, {
      title: "Thinking",
      status,
      details: clip(details, this.options.maxTaskDetailsChars),
    });
  }

  private handleTurnCompleted(turn: Turn): ChatSdkStreamChunk[] {
    const chunks: ChatSdkStreamChunk[] = [];
    for (const item of turn.items) {
      chunks.push(...this.handleItem(item, "completed"));
    }
    if (turn.status === "failed" && turn.error?.message) {
      chunks.push(...this.failOpenTasks(turn.error.message));
    } else {
      chunks.push(...this.completeOpenTasks());
    }
    return chunks;
  }

  private completeOpenTasks(): ChatSdkStreamChunk[] {
    const chunks: ChatSdkStreamChunk[] = [];
    for (const [id, state] of this.items) {
      if (!state.title || (state.status !== "pending" && state.status !== "in_progress")) {
        continue;
      }
      chunks.push(
        ...this.emitTaskUpdate({
          id,
          title: state.title,
          status: "complete",
          details: state.details,
          output: state.output,
        })
      );
    }
    return chunks;
  }

  private failOpenTasks(message: string): ChatSdkStreamChunk[] {
    const chunks: ChatSdkStreamChunk[] = [];
    let failedAny = false;
    for (const [id, state] of this.items) {
      if (!state.title || (state.status !== "pending" && state.status !== "in_progress")) {
        continue;
      }
      failedAny = true;
      chunks.push(
        ...this.emitTaskUpdate({
          id,
          title: state.title,
          status: "error",
          details: state.details,
          output: state.output,
        })
      );
    }
    if (!failedAny && message) {
      chunks.push(
        ...this.emitTaskUpdate({
          id: `turn-error:${this.currentTurnId || "unknown"}`,
          title: "Turn failed",
          status: "error",
          output: message,
        })
      );
    }
    return chunks;
  }

  private emitItemTask(
    itemId: string,
    update: Omit<ChatSdkTaskUpdateChunk, "id" | "type">
  ): ChatSdkStreamChunk[] {
    const state = this.stateFor(itemId);
    state.title = update.title;
    state.status = update.status;
    state.details = update.details;
    state.output = update.output;
    return this.emitTaskUpdate({ id: itemId, ...update });
  }

  private emitTaskUpdate(update: Omit<ChatSdkTaskUpdateChunk, "type">): ChatSdkStreamChunk[] {
    const chunk: ChatSdkTaskUpdateChunk = {
      type: "task_update",
      id: update.id,
      title: this.title(update.title),
      status: update.status,
      ...(update.details
        ? { details: clip(update.details, this.options.maxTaskDetailsChars) }
        : {}),
      ...(update.output ? { output: clip(update.output, this.options.maxTaskOutputChars) } : {}),
    };
    const signature = JSON.stringify(chunk);
    if (this.lastTaskById.get(chunk.id) === signature) {
      return [];
    }
    this.lastTaskById.set(chunk.id, signature);
    return [chunk];
  }

  private agentPhase(
    phase: "commentary" | "final_answer" | null | undefined
  ): "commentary" | "final_answer" {
    return phase ?? this.options.unknownAgentMessagePhase;
  }

  private title(value: string): string {
    return clipOneLine(value || "Working", this.options.maxTitleChars);
  }

  private commandDetails(command: string, cwd: string): string {
    return clip(
      [`cwd: ${inlineCode(cwd)}`, codeBlock(command, "sh")].filter(Boolean).join("\n\n"),
      this.options.maxTaskDetailsChars
    );
  }

  private commandOutput(output: string, exitCode?: number | null): string | undefined {
    const normalized =
      exitCode !== null && exitCode !== undefined && exitCode !== 0
        ? `exit code ${exitCode}${output ? `\n${output}` : ""}`
        : output;
    return normalized
      ? codeBlock(clip(normalized, this.options.maxTaskOutputChars), "text")
      : undefined;
  }

  private jsonDetails(label: string, value: unknown): string | undefined {
    const body = stableJson(value);
    return body === "{}"
      ? undefined
      : `${label}:\n\n${codeBlock(clip(body, this.options.maxTaskDetailsChars), "json")}`;
  }
}

export async function* codexAppServerToChatSdkStream(
  notifications: AsyncIterable<ServerNotification>,
  options: CodexAppServerToChatStreamOptions = {}
): AsyncIterable<ChatSdkStreamChunk> {
  const mapper = new CodexAppServerChatStreamMapper(options);
  for await (const notification of notifications) {
    for (const chunk of mapper.process(notification)) {
      yield chunk;
    }
  }
  for (const chunk of mapper.flush()) {
    yield chunk;
  }
}

function genericStatus(status: string, failed = false): ChatSdkTaskStatus {
  if (failed) {
    return "error";
  }
  switch (status) {
    case "completed":
      return "complete";
    case "failed":
    case "declined":
      return "error";
    case "inProgress":
      return "in_progress";
    default:
      return "pending";
  }
}

function commandStatus(
  status: string,
  exitCode: number | null,
  lifecycle: "started" | "completed"
): ChatSdkTaskStatus {
  if (status === "failed" || status === "declined") {
    return "error";
  }
  if (exitCode !== null && exitCode !== undefined && exitCode !== 0) {
    return "error";
  }
  if (status === "completed" || lifecycle === "completed") {
    return "complete";
  }
  return status === "inProgress" ? "in_progress" : "pending";
}

function patchStatus(status: string): ChatSdkTaskStatus {
  return genericStatus(status);
}

function planStatus(status: TurnPlanStepStatus): ChatSdkTaskStatus {
  switch (status) {
    case "completed":
      return "complete";
    case "inProgress":
      return "in_progress";
    case "pending":
      return "pending";
  }
}

function planTaskId(turnId: string, index: number): string {
  return `plan:${turnId || "turn"}:${index + 1}`;
}

function commandExecTaskId(processId: string): string {
  return `command-exec:${processId}`;
}

function processTaskId(processHandle: string): string {
  return `process:${processHandle}`;
}

function parsePlanText(value: string): Array<{ title: string; status: ChatSdkTaskStatus }> {
  const steps: Array<{ title: string; status: ChatSdkTaskStatus }> = [];
  for (const line of value.split("\n")) {
    const trimmed = line.trim();
    const match = /^(?:[-*]|\d+[.)])\s+(?:\[([ xX])\]\s+)?(.+)$/.exec(trimmed);
    if (!match) {
      continue;
    }
    const marker = match[1];
    steps.push({
      title: cleanPlanStep(match[2] ?? ""),
      status: marker?.toLowerCase() === "x" ? "complete" : "pending",
    });
  }
  return steps;
}

function cleanPlanStep(value: string): string {
  return value
    .replace(/^\s*(?:[-*]|\d+[.)])\s+/, "")
    .replace(/^\[[ xX]\]\s+/, "")
    .trim();
}

function firstNonEmptyLine(value: string): string {
  return value
    .split("\n")
    .map((line) => line.trim())
    .find(Boolean) ?? "";
}

function displayCommand(command: string): string {
  const trimmed = command.trim();
  const shellPrefix = /^\/bin\/(?:ba|z|)sh\s+-lc\s+/.exec(trimmed);
  if (!shellPrefix) {
    return trimmed || "command";
  }
  return trimmed.slice(shellPrefix[0].length).replace(/^['"]|['"]$/g, "") || trimmed;
}

function appendStreamOutput(
  state: ItemState,
  stream: OutputStreamLabel,
  delta: string,
  capReached: boolean
): void {
  if (!delta && !capReached) {
    return;
  }
  const header =
    state.lastOutputStream === stream && state.commandOutput
      ? ""
      : `${state.commandOutput ? "\n" : ""}[${stream}]\n`;
  const capNotice = capReached
    ? `${delta.endsWith("\n") || !delta ? "" : "\n"}[${stream} output capped]\n`
    : "";
  state.commandOutput += `${header}${delta}${capNotice}`;
  state.lastOutputStream = stream;
}

function processBufferedOutput(
  params: Extract<ServerNotification, { method: "process/exited" }>["params"]
): string {
  const stdout = appendCapNotice(params.stdout, "stdout", params.stdoutCapReached);
  const stderr = appendCapNotice(params.stderr, "stderr", params.stderrCapReached);
  const sections = [
    stdout ? `[stdout]\n${stdout}` : "",
    stderr ? `[stderr]\n${stderr}` : "",
  ].filter(Boolean);
  return sections.join("\n\n");
}

function appendCapNotice(value: string, stream: "stdout" | "stderr", capReached: boolean): string {
  if (!capReached) {
    return value;
  }
  return `${value}${value.endsWith("\n") || !value ? "" : "\n"}[${stream} output capped]\n`;
}

function decodeBase64Utf8(value: string): string {
  try {
    const binary = globalThis.atob(value);
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  } catch {
    return value;
  }
}

function dynamicToolOutput(
  items: Extract<ThreadItem, { type: "dynamicToolCall" }>["contentItems"],
  maxChars: number
): string | undefined {
  if (!items?.length) {
    return undefined;
  }
  const parts = items.map((item) =>
    item.type === "inputText" ? item.text : `[image] ${item.imageUrl}`
  );
  return clip(parts.join("\n\n"), maxChars);
}

function mcpOutput(
  result: Extract<ThreadItem, { type: "mcpToolCall" }>["result"],
  error: Extract<ThreadItem, { type: "mcpToolCall" }>["error"],
  maxChars: number
): string | undefined {
  if (error?.message) {
    return error.message;
  }
  if (!result) {
    return undefined;
  }
  const content = result.content
    .map((item) => {
      if (isRecord(item) && typeof item.text === "string") {
        return item.text;
      }
      return stableJson(item);
    })
    .filter(Boolean)
    .join("\n\n");
  const structured =
    result.structuredContent === null ? "" : `\n\nStructured content:\n${stableJson(result.structuredContent)}`;
  return clip(`${content}${structured}`.trim(), maxChars) || undefined;
}

function stableJson(value: unknown): string {
  return JSON.stringify(value, null, 2) ?? "";
}

function codeBlock(value: string, language: string): string {
  const fence = value.includes("```") ? "````" : "```";
  return `${fence}${language}\n${value.trimEnd()}\n${fence}`;
}

function inlineCode(value: string): string {
  return `\`${value.replace(/`/g, "\\`")}\``;
}

function clipOneLine(value: string, maxChars: number): string {
  return clip(value.replace(/\s+/g, " ").trim(), maxChars);
}

function clip(value: string, maxChars: number): string {
  if (!Number.isFinite(maxChars) || value.length <= maxChars) {
    return value;
  }
  if (maxChars <= 3) {
    return ".".repeat(Math.max(maxChars, 0));
  }
  return `${value.slice(0, maxChars - 3)}...`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
