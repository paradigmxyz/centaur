import type { RustSessionStreamEvent } from "@centaur/harness-events";
import type { CodexAppServerToChatStreamOptions } from "@centaur/rendering";
import type { Hono } from "hono";
import type { Pool } from "pg";

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export type JsonObject = { [key: string]: JsonValue | undefined };

/**
 * Telegram delta: the other bots take their Logger from the Chat SDK, but
 * telegrambot has no Chat SDK adapter (none exists for Telegram), so the same
 * shape is declared locally instead of importing the `chat` package for one
 * type.
 */
export type Logger = {
  debug(message: string, data?: unknown): void;
  info(message: string, data?: unknown): void;
  warn(message: string, data?: unknown): void;
  error(message: string, data?: unknown): void;
  child(bindings?: Record<string, unknown>): Logger;
};

// ---------------------------------------------------------------------------
// Telegram Bot API payloads (subset the service consumes; unknown fields pass
// through as raw JSON in the inbox).
// ---------------------------------------------------------------------------

export type TelegramUser = {
  id: number;
  is_bot: boolean;
  first_name: string;
  last_name?: string;
  username?: string;
};

export type TelegramChat = {
  id: number;
  type: "private" | "group" | "supergroup" | "channel";
  title?: string;
  username?: string;
  first_name?: string;
  last_name?: string;
  is_forum?: boolean;
};

export type TelegramMessageEntity = {
  type: string;
  offset: number;
  length: number;
  url?: string;
  user?: TelegramUser;
  language?: string;
};

export type TelegramPhotoSize = {
  file_id: string;
  file_unique_id: string;
  width: number;
  height: number;
  file_size?: number;
};

export type TelegramDocument = {
  file_id: string;
  file_unique_id: string;
  file_name?: string;
  mime_type?: string;
  file_size?: number;
};

export type TelegramMessage = {
  message_id: number;
  message_thread_id?: number;
  from?: TelegramUser;
  sender_chat?: TelegramChat;
  chat: TelegramChat;
  date: number;
  text?: string;
  caption?: string;
  entities?: TelegramMessageEntity[];
  caption_entities?: TelegramMessageEntity[];
  photo?: TelegramPhotoSize[];
  document?: TelegramDocument;
  reply_to_message?: TelegramMessage;
  is_topic_message?: boolean;
  via_bot?: TelegramUser;
};

export type TelegramUpdate = {
  update_id: number;
  message?: TelegramMessage;
  // Other update kinds are filtered server-side via allowed_updates; anything
  // that still arrives is durably recorded as ignored, never processed.
  [key: string]: unknown;
};

export type TelegramFile = {
  file_id: string;
  file_unique_id: string;
  file_size?: number;
  file_path?: string;
};

// ---------------------------------------------------------------------------
// Service options / composition
// ---------------------------------------------------------------------------

export type TelegrambotFetch = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export type TelegrambotOptions = {
  /** Edit cadence for the in-progress answer message (default ~1.5s). */
  answerEditIntervalMs?: number;
  apiKey?: string;
  apiUrl: string;
  botToken: string;
  /** Group/supergroup chat ids allowed to trigger sessions. Empty = inert. */
  chatAllowlist?: readonly string[];
  fetch?: TelegrambotFetch;
  harnessType?: string;
  idleTimeoutMs?: number;
  /** Lease TTL for fenced ownership (default 30s; renewed at TTL/3). */
  leaseTtlMs?: number;
  logger?: Logger;
  mapper?: CodexAppServerToChatStreamOptions;
  /** Bounded cross-thread worker concurrency (default 4). */
  maxConcurrentThreads?: number;
  maxDurationMs?: number;
  /** Long-poll timeout passed to getUpdates (default 50s). */
  pollTimeoutSeconds?: number;
  postgresUrl?: string;
  /** Injected pool for tests; otherwise built from postgresUrl. */
  pool?: Pool;
  recoverRenderObligationsOnStart?: boolean;
  /** Terminal inbox rows older than this are eligible for pruning (default 72h). */
  retentionHours?: number;
  /** Base URL override for the Bot API (default https://api.telegram.org). */
  telegramApiUrl?: string;
  /** Telegram user ids allowed to use private (DM) chats. Empty = no DMs. */
  userAllowlist?: readonly string[];
  userName?: string;
};

export type Telegrambot = {
  app: Hono;
  /** Starts ownership, migrations, webhook reconciliation, poller, workers. */
  start(): Promise<void>;
  shutdown(): Promise<void>;
};

// ---------------------------------------------------------------------------
// Session API contracts (mirrors the discordbot session client shapes)
// ---------------------------------------------------------------------------

export type TelegrambotSessionMessageRole =
  | "user"
  | "assistant"
  | "system"
  | "tool";

export type TelegrambotSessionMessage = {
  client_message_id?: string;
  metadata: JsonObject;
  parts: JsonValue[];
  role: TelegrambotSessionMessageRole;
};

export type TelegrambotAppendMessagesRequest = {
  messages: TelegrambotSessionMessage[];
};

export type TelegrambotCreateSessionRequest = {
  harness_type: string;
  metadata: JsonObject;
};

export type TelegrambotExecuteSessionRequest = {
  idempotency_key?: string;
  idle_timeout_ms?: number;
  input_lines: string[];
  max_duration_ms?: number;
  metadata: JsonObject;
};

export type TelegrambotExecuteSessionResponse = {
  execution_id: string;
  ok: boolean;
  status: string;
  thread_key: string;
};

export type TelegrambotRendererSource = RustSessionStreamEvent | JsonObject;

// ---------------------------------------------------------------------------
// Durable inbox / poll state (owned tables, see src/migrations.ts)
// ---------------------------------------------------------------------------

/**
 * Processing stages an accepted update moves through. Receipt
 * (`receive_offset`) is tracked separately in telegram_poll_state and is never
 * gated on these stages.
 *
 * - received: durably stored, not yet dispatched
 * - message_appended: durable session message appended (stable client_message_id)
 * - steering_pending: appended while an execution was active; awaiting
 *   steering_delivered / execution-terminal resolution
 * - execution_accepted: api-rs accepted an idempotent execution
 * - render_obligation_persisted: replayable render state is discoverable
 * - completed | steered | ignored | rejected | failed: terminal (ignored /
 *   rejected / failed always carry a durable reason)
 */
export type TelegramInboxStatus =
  | "received"
  | "message_appended"
  | "steering_pending"
  | "execution_accepted"
  | "render_obligation_persisted"
  | "completed"
  | "steered"
  | "ignored"
  | "rejected"
  | "failed";

export const TERMINAL_INBOX_STATUSES: readonly TelegramInboxStatus[] = [
  "completed",
  "steered",
  "ignored",
  "rejected",
  "failed",
];

export type TelegramInboxRow = {
  botUserId: string;
  updateId: number;
  payload: TelegramUpdate;
  status: TelegramInboxStatus;
  /** Typed durable key; null until the update is accepted for processing. */
  threadKey: string | null;
  /** Stable append/execute idempotency key: telegram:{chatId}:{messageId}. */
  clientMessageId: string | null;
  executionId: string | null;
  statusReason: string | null;
  receivedAt: Date;
  updatedAt: Date;
};

/**
 * Fenced ownership + receipt cursor, one row per bot user id. Every cursor
 * update and inbox transition must match holder_id + generation on an
 * unexpired lease in the same statement/transaction (see src/ownership.ts).
 */
export type TelegramPollState = {
  botUserId: string;
  receiveOffset: number | null;
  holderId: string | null;
  generation: number;
  leaseExpiresAt: Date | null;
};

export type OwnershipLease = {
  botUserId: string;
  holderId: string;
  generation: number;
};

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/**
 * Durable render obligation: persisted before any Telegram send so a crashed
 * or restarted worker can resume terminal delivery (at-least-once; a send
 * whose response was never recorded may be duplicated on recovery).
 */
export type TelegramRenderObligation = {
  threadKey: string;
  executionId: string;
  chatId: string;
  /** Forum/private topic the output must stay in, when present. */
  messageThreadId: number | null;
  /** message_id of the triggering Telegram message (reactions target it). */
  triggerMessageId: number;
  afterEventId: number;
  /** message_ids already posted for this answer, oldest first. */
  postedMessageIds: number[];
  /** Rendered text already durably delivered into postedMessageIds. */
  deliveredText: string;
};

export type TelegramNarratorOutcome = "done" | "failed" | "retrying";

export type TelegrambotMessageMode = "append" | "execute" | "steer";

export type TelegrambotTrace = {
  messageId: string;
  mode: TelegrambotMessageMode;
  openStream: boolean;
  startedAtMs: number;
  threadKey: string;
};
