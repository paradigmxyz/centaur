import type { RustSessionStreamEvent } from "@centaur/harness-events";
import type { CodexAppServerToChatStreamOptions } from "@centaur/rendering";
import type { TelegramAdapter } from "@chat-adapter/telegram";
import type { Attachment, Chat, Logger, StateAdapter } from "chat";
import type { Hono } from "hono";

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export type JsonObject = { [key: string]: JsonValue | undefined };

export type TelegrambotApiAuthor = {
  fullName: string;
  isBot: boolean | "unknown";
  isMe: boolean;
  userId: string;
  userName: string;
};

export type TelegrambotApiAttachment = {
  dataBase64?: string;
  dataBase64Omitted?: string;
  fetchError?: string;
  height?: number;
  mimeType?: string;
  name?: string;
  size?: number;
  type: Attachment["type"];
  width?: number;
};

export type TelegrambotApiMessage = {
  attachments: TelegrambotApiAttachment[];
  author: TelegrambotApiAuthor;
  id: string;
  isMention: boolean;
  text: string;
  threadId: string;
  timestamp: string;
  /** How the message reached the agent (see telegram-policy.ts). */
  trigger?: TelegramTrigger;
};

export type TelegrambotSessionMessageRole =
  "user" | "assistant" | "system" | "tool";

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

export type TelegrambotFetch = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

/** How updates reach the bot. Polling needs no public endpoint. */
export type TelegrambotMode = "polling" | "webhook";

export type TelegrambotOptions = {
  /**
   * TTL after which a persisted `activeExecution` flag is treated as stale, so
   * a crash between marking and clearing it cannot wedge the chat forever.
   */
  activeExecutionTtlMs?: number;
  /** Floor for answer edits; defaults to 1500 ms in DMs and 3100 ms in groups. */
  answerEditIntervalMs?: number;
  apiKey?: string;
  apiUrl: string;
  botToken: string;
  /** Group/supergroup chat ids allowed to use the bot. Fail-closed when empty. */
  chatAllowlist?: readonly string[];
  fetch?: TelegrambotFetch;
  idleTimeoutMs?: number;
  logger?: Logger;
  mapper?: CodexAppServerToChatStreamOptions;
  maxDurationMs?: number;
  /** Defaults to "polling". */
  mode?: TelegrambotMode;
  /** getUpdates long-poll timeout in seconds (polling mode only). */
  pollTimeoutSeconds?: number;
  postgresUrl?: string;
  recoverRenderObligationsOnStart?: boolean;
  state?: StateAdapter;
  stateKeyPrefix?: string;
  /** Bot API base URL override (tests and self-hosted Bot API servers). */
  telegramApiUrl?: string;
  /** Private-chat user ids allowed to use the bot. Fail-closed when empty. */
  userAllowlist?: readonly string[];
  /** Bot username; resolved from getMe when omitted. */
  userName?: string;
  /** Required in webhook mode; checked against X-Telegram-Bot-Api-Secret-Token. */
  webhookSecretToken?: string;
};

export type Telegrambot = {
  adapter: TelegramAdapter;
  app: Hono;
  chat: Chat;
};

export type TelegrambotThreadState = {
  activeExecution?: boolean;
  /** Epoch ms when `activeExecution` was last (re)confirmed; cleared with it. */
  activeExecutionStartedAt?: number | null;
  executedMessageIds?: string[];
  forwardedMessageIds?: string[];
  lastEventId?: number;
  renderObligation?: TelegrambotRenderObligation | null;
};

export type TelegrambotRenderObligation = {
  afterEventId: number;
  executionId: string;
  message: TelegrambotApiMessage;
};

export type TelegrambotMessageMode = "append" | "execute";

export type TelegrambotRendererSource = RustSessionStreamEvent | JsonObject;

export type TelegrambotTrace = {
  messageId: string;
  mode: TelegrambotMessageMode;
  startedAtMs: number;
  threadId: string;
};

export type ForwardSessionInput = {
  afterEventId: number;
  /**
   * Chat title (group) or user name (DM), carried in the create-session
   * metadata as `telegram_conversation_name`; api-rs uses it as the session
   * principal's display name.
   */
  conversationName?: string;
  executionId?: string;
  executeMessage?: TelegrambotApiMessage;
  messages: TelegrambotApiMessage[];
  onEventId(eventId: number): void;
  threadId: string;
  trace?: TelegrambotTrace;
};

/**
 * Why an allowed message reaches the agent: every DM message, a group reply
 * to one of the bot's own messages, or the addressed `/ask` command.
 */
export type TelegramTrigger = "dm" | "reply" | "command";
