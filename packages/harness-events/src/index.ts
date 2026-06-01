export { splitThreadKey, normalizeThreadKey } from "./thread-key";

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type RequestId = string | number;
export type MessagePhase = "commentary" | "final_answer";
export type TurnPlanStepStatus = "pending" | "inProgress" | "completed";
export type OutputStream = "stdout" | "stderr" | "stdin";

export type ClientNotification = { method: "initialized" };

export type ClientRequest =
  | { method: "initialize"; id: RequestId; params: InitializeParams }
  | { method: "thread/start"; id: RequestId; params: ThreadStartParams }
  | { method: "turn/start"; id: RequestId; params: TurnStartParams }
  | { method: string; id: RequestId; params?: unknown };

export interface InitializeParams {
  clientInfo: Record<string, unknown>;
  capabilities: Record<string, unknown> | null;
}

export interface InitializeResponse {
  userAgent: string;
  codexHome: string;
  platformFamily: string;
  platformOs: string;
}

export type ServerRequest = { method: string; id: RequestId; params?: unknown };

export type UserInput =
  | { type: "text"; text: string; text_elements?: Array<Record<string, unknown>> }
  | { type: "image"; detail?: "auto" | "low" | "high"; url: string }
  | { type: "localImage"; detail?: "auto" | "low" | "high"; path: string }
  | { type: "skill"; name: string; path: string }
  | { type: "mention"; name: string; path: string };

export type ThreadStartParams = Record<string, unknown>;

export type ThreadStartResponse = {
  thread: Thread;
} & Record<string, unknown>;

export type TurnStartParams = {
  threadId: string;
  input: UserInput[];
} & Record<string, unknown>;

export interface TurnStartResponse {
  turn: Turn;
}

export interface Thread {
  id: string;
  [key: string]: unknown;
}

export type TurnStatus = "inProgress" | "completed" | "failed" | "cancelled";

export interface TurnError {
  message: string;
  [key: string]: unknown;
}

export interface Turn {
  id: string;
  items: ThreadItem[];
  itemsView: "full" | "partial" | string;
  status: TurnStatus;
  error: TurnError | null;
  startedAt: number | null;
  completedAt: number | null;
  durationMs: number | null;
}

export interface FileUpdateChange {
  path: string;
  kind?: string;
  diff: string;
}

export type DynamicToolCallOutputContentItem =
  | { type: "inputText"; text: string }
  | { type: "inputImage"; imageUrl: string };

export interface McpToolCallResult {
  content: unknown[];
  structuredContent: unknown | null;
  _meta?: unknown | null;
}

export interface McpToolCallError {
  message: string;
}

export type ThreadItem =
  | { type: "userMessage"; id: string; content: UserInput[] }
  | { type: "hookPrompt"; id: string; fragments: unknown[] }
  | {
      type: "agentMessage";
      id: string;
      text: string;
      phase: MessagePhase | null;
      memoryCitation?: unknown | null;
    }
  | { type: "plan"; id: string; text: string }
  | { type: "reasoning"; id: string; summary: string[]; content: string[] }
  | {
      type: "commandExecution";
      id: string;
      command: string;
      cwd: string;
      processId: string | null;
      source?: string;
      status: "inProgress" | "completed" | "failed" | "declined" | string;
      commandActions?: unknown[];
      aggregatedOutput: string | null;
      exitCode: number | null;
      durationMs?: number | null;
    }
  | {
      type: "fileChange";
      id: string;
      changes: FileUpdateChange[];
      status: "inProgress" | "completed" | "failed" | "declined" | string;
    }
  | {
      type: "mcpToolCall";
      id: string;
      server: string;
      tool: string;
      status: string;
      arguments: unknown;
      mcpAppResourceUri?: string;
      pluginId?: string | null;
      result: McpToolCallResult | null;
      error: McpToolCallError | null;
      durationMs?: number | null;
    }
  | {
      type: "dynamicToolCall";
      id: string;
      namespace: string | null;
      tool: string;
      arguments: unknown;
      status: string;
      contentItems: DynamicToolCallOutputContentItem[] | null;
      success: boolean | null;
      durationMs?: number | null;
    }
  | { type: "collabAgentToolCall"; id: string; [key: string]: unknown }
  | { type: "webSearch"; id: string; query: string; action: unknown | null }
  | { type: "imageView"; id: string; path: string }
  | {
      type: "imageGeneration";
      id: string;
      status: string;
      revisedPrompt: string | null;
      result: string;
      savedPath?: string;
    }
  | { type: "enteredReviewMode"; id: string; review: string }
  | { type: "exitedReviewMode"; id: string; review: string }
  | { type: "contextCompaction"; id: string };

export interface AgentMessageDeltaNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  delta: string;
}

export interface ItemStartedNotification {
  item: ThreadItem;
  threadId: string;
  turnId: string;
  startedAtMs: number;
}

export interface ItemCompletedNotification {
  item: ThreadItem;
  threadId: string;
  turnId: string;
  completedAtMs: number;
}

export interface ThreadStartedNotification {
  thread: Thread;
}

export interface TurnStartedNotification {
  threadId: string;
  turn: Turn;
}

export interface TurnCompletedNotification {
  threadId: string;
  turn: Turn;
}

export interface TurnPlanStep {
  step: string;
  status: TurnPlanStepStatus;
}

export interface TurnPlanUpdatedNotification {
  threadId: string;
  turnId: string;
  explanation: string | null;
  plan: TurnPlanStep[];
}

export interface ThreadNameUpdatedNotification {
  threadId: string;
  threadName: string;
}

export interface ErrorNotification {
  error: TurnError;
  willRetry?: boolean;
  threadId?: string;
  turnId?: string;
}

export interface CommandExecutionOutputDeltaNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  delta: string;
}

export interface TerminalInteractionNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  processId: string;
  stdin: string;
}

export interface CommandExecOutputDeltaNotification {
  processId: string;
  stream: OutputStream;
  deltaBase64: string;
  capReached: boolean;
}

export interface ProcessOutputDeltaNotification {
  processHandle: string;
  stream: OutputStream;
  deltaBase64: string;
  capReached: boolean;
}

export interface ProcessExitedNotification {
  processHandle: string;
  exitCode: number;
  stdout: string;
  stdoutCapReached: boolean;
  stderr: string;
  stderrCapReached: boolean;
}

export interface FileChangePatchUpdatedNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  changes: FileUpdateChange[];
}

export interface FileChangeOutputDeltaNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  delta: string;
}

export interface McpToolCallProgressNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  message: string;
}

export interface PlanDeltaNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  delta: string;
}

export interface ReasoningSummaryTextDeltaNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  delta: string;
  summaryIndex: number;
}

export interface ReasoningTextDeltaNotification {
  threadId: string;
  turnId: string;
  itemId: string;
  delta: string;
  contentIndex: number;
}

export type ServerNotification =
  | { method: "error"; params: ErrorNotification }
  | { method: "thread/started"; params: ThreadStartedNotification }
  | { method: "thread/name/updated"; params: ThreadNameUpdatedNotification }
  | { method: "turn/started"; params: TurnStartedNotification }
  | { method: "turn/completed"; params: TurnCompletedNotification }
  | { method: "turn/plan/updated"; params: TurnPlanUpdatedNotification }
  | { method: "item/started"; params: ItemStartedNotification }
  | { method: "item/completed"; params: ItemCompletedNotification }
  | { method: "item/agentMessage/delta"; params: AgentMessageDeltaNotification }
  | {
      method: "item/commandExecution/outputDelta";
      params: CommandExecutionOutputDeltaNotification;
    }
  | {
      method: "item/commandExecution/terminalInteraction";
      params: TerminalInteractionNotification;
    }
  | { method: "command/exec/outputDelta"; params: CommandExecOutputDeltaNotification }
  | { method: "process/outputDelta"; params: ProcessOutputDeltaNotification }
  | { method: "process/exited"; params: ProcessExitedNotification }
  | { method: "item/fileChange/patchUpdated"; params: FileChangePatchUpdatedNotification }
  | { method: "item/fileChange/outputDelta"; params: FileChangeOutputDeltaNotification }
  | { method: "item/mcpToolCall/progress"; params: McpToolCallProgressNotification }
  | { method: "item/plan/delta"; params: PlanDeltaNotification }
  | {
      method: "item/reasoning/summaryTextDelta";
      params: ReasoningSummaryTextDeltaNotification;
    }
  | { method: "item/reasoning/textDelta"; params: ReasoningTextDeltaNotification };

export type RustSessionStreamEvent = {
  eventId?: number;
  eventKind?: string;
  event?: string;
  data?: unknown;
};
