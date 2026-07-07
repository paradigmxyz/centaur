import type { RustSessionStreamEvent } from "@centaur/harness-events";
import type { CodexAppServerToChatStreamOptions } from "@centaur/rendering";
import type { Attachment, Chat, Logger, StateAdapter } from "chat";
import type { Hono } from "hono";

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export type JsonObject = { [key: string]: JsonValue | undefined };

export type LinearbotApiAuthor = {
  fullName: string;
  isBot: boolean | "unknown";
  isMe: boolean;
  userId: string;
  userName: string;
};

export type LinearbotApiAttachment = {
  dataBase64?: string;
  dataBase64Omitted?: string;
  fetchError?: string;
  fetchMetadata?: Record<string, string>;
  height?: number;
  mimeType?: string;
  name?: string;
  size?: number;
  type: Attachment["type"];
  url?: string;
  width?: number;
};

// Linear delta: no Slack `teamId` (Linear scopes by organization, which the
// adapter resolves from the install; sessions are keyed by thread id alone).
export type LinearbotApiMessage = {
  attachments: LinearbotApiAttachment[];
  author: LinearbotApiAuthor;
  id: string;
  isMention: boolean;
  raw: unknown;
  text: string;
  threadId: string;
  timestamp: string;
};

export type LinearbotSessionMessageRole =
  | "user"
  | "assistant"
  | "system"
  | "tool";

export type LinearbotSessionMessage = {
  client_message_id?: string;
  metadata: JsonObject;
  parts: JsonValue[];
  role: LinearbotSessionMessageRole;
};

export type LinearbotAppendMessagesRequest = {
  messages: LinearbotSessionMessage[];
};

export type LinearbotCreateSessionRequest = {
  harness_type: string;
  metadata: JsonObject;
  /** 'restart': switch the thread to harness_type if it's pinned to another harness. */
  on_harness_conflict?: "reject" | "restart";
};

export type LinearbotExecuteSessionRequest = {
  idempotency_key?: string;
  idle_timeout_ms?: number;
  input_lines: string[];
  max_duration_ms?: number;
  metadata: JsonObject;
};

export type LinearbotExecuteSessionResponse = {
  execution_id: string;
  ok: boolean;
  status: string;
  thread_key: string;
};

export type LinearbotFetch = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export type LinearbotOptions = {
  apiKey?: string;
  apiUrl: string;
  /**
   * Connect the Postgres state (and initialize the adapter) at startup.
   * Defaults to true; tests pass false to skip the live connect against mock
   * backends.
   */
  connectStateOnStart?: boolean;
  /**
   * Harness for new threads when no --claude/--amp/--codex flag is given
   * (HarnessType wire value: codex | amp | claudecode). Defaults to codex.
   */
  defaultHarnessType?: string;
  fetch?: LinearbotFetch;
  idleTimeoutMs?: number;
  /** OAuth access token from an actor=app install (the bot runs as an app). */
  linearAccessToken?: string;
  /**
   * Personal API key fallback: runs the same comment-thread model as a regular
   * Linear user instead of an app (no OAuth install required).
   */
  linearApiKey?: string;
  /** Override the Linear GraphQL API base URL (tests/emulation). */
  linearApiUrl?: string;
  /** Webhook signing secret from the Linear webhook settings page. */
  linearWebhookSecret: string;
  logger?: Logger;
  mapper?: CodexAppServerToChatStreamOptions;
  maxDurationMs?: number;
  postgresUrl?: string;
  state?: StateAdapter;
  stateKeyPrefix?: string;
  userName?: string;
};

export type Linearbot = {
  app: Hono;
  chat: Chat;
};

export type LinearbotThreadState = {
  /** Set once the thread's first turn has run (gates follow-up ingestion). */
  historyForwarded?: boolean;
  /**
   * Set once the full issue context (with description) has ridden a turn's
   * execute; later turns prepend only the compact id/title header instead.
   */
  contextSeeded?: boolean;
  /** Highest session-event id seen, used as the replay watermark. */
  lastEventId?: number;
  /**
   * Centaur-forward model: ids of comments this thread has already answered, so
   * a webhook redelivery never double-replies. Capped FIFO.
   */
  repliedCommentIds?: string[];
  /**
   * Centaur-forward model: ids of non-mention comments this thread has already
   * appended to its session as context, so a webhook redelivery never
   * double-appends. Separate from repliedCommentIds so the high-volume followup
   * stream can't evict mention ids from their dedup window. Capped FIFO.
   */
  ingestedCommentIds?: string[];
  /**
   * Centaur-forward model: the last assignment trigger (issue `updatedAt`) the
   * bot ran a turn for, so a redelivered Issue webhook doesn't re-run.
   */
  lastAssignmentTrigger?: string;
};

export type LinearbotRendererSource = RustSessionStreamEvent | JsonObject;

export type LinearbotTrace = {
  includeContext: boolean;
  messageId: string;
  mode: "execute";
  openStream: boolean;
  startedAtMs: number;
  threadId: string;
};

export type ForwardSessionInput = {
  afterEventId: number;
  /**
   * Human-readable issue name (identifier/title) carried in the create-session
   * metadata as `linear_conversation_name`; api-rs uses it as the session
   * principal's display name.
   */
  conversationName?: string;
  /**
   * Prepended to the execute message content as a text part. Set when a harness
   * restart discards the previous harness's conversation state so the new
   * harness still sees the issue + comment history.
   */
  contextPreamble?: string;
  executionId?: string;
  executeMessage?: LinearbotApiMessage;
  /** Harness override parsed from message flags (--claude/--amp/--codex). */
  harnessType?: string;
  messages: LinearbotApiMessage[];
  /** Per-turn model override parsed from message flags (--model/--opus/...). */
  model?: string;
  /** Per-turn model provider override parsed from message flags (--meta). */
  provider?: string;
  onEventId(eventId: number): void;
  openStream: boolean;
  threadId: string;
  trace?: LinearbotTrace;
};

/**
 * Minimal slice of the Linear chat adapter the narrator uses to emit typed
 * agent activities (thought/action/response/error) directly — the unified
 * Chat SDK surface only exposes Response posts and ephemeral thoughts.
 */
export type LinearAgentActivityContent =
  | { type: "thought"; body: string }
  | { type: "action"; action: string; parameter: string; result?: string }
  | { type: "response"; body: string }
  | { type: "error"; body: string };

export type LinearActivityClient = {
  createAgentActivity(input: {
    agentSessionId: string;
    content: LinearAgentActivityContent;
    ephemeral?: boolean;
  }): Promise<unknown>;
};

export type LinearSessionCapableAdapter = {
  /** App user id of the bot; the getter throws before initialize. */
  botUserId?: string;
  linearClient?: LinearActivityClient & LinearRawRequestClient;
  startTyping?(threadId: string, status?: string): Promise<void>;
};

/**
 * Raw GraphQL escape hatch on the Linear SDK client, used by the issue-status
 * plumbing (linear-status.ts) — the typed SDK surface differs across versions
 * for agent-era fields like `delegate`.
 */
export type LinearRawRequestClient = {
  client?: {
    rawRequest<Data>(
      query: string,
      variables?: Record<string, unknown>,
    ): Promise<{ data?: Data | null }>;
  };
};
