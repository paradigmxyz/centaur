import type { RustSessionStreamEvent } from "@centaur/harness-events";
import type { TelegramApi } from "./telegram-api";
import type {
  JsonObject,
  JsonValue,
  Logger,
  TelegramChat,
  TelegramMessage,
  TelegramPhotoSize,
  TelegrambotAppendMessagesRequest,
  TelegrambotCreateSessionRequest,
  TelegrambotExecuteSessionRequest,
  TelegrambotExecuteSessionResponse,
  TelegrambotOptions,
  TelegrambotRendererSource,
  TelegrambotSessionMessage,
  TelegrambotTrace,
} from "./types";
import {
  elapsedMs,
  errorMessage,
  isJsonObject,
  noopLogger,
  nowMs,
  stringValue,
  toAsyncIterable,
  traceLog,
} from "./utils";

export class SessionApiError extends Error {
  readonly action: string;
  readonly body: string;
  readonly retryable: boolean;
  readonly status: number;
  readonly statusText: string;

  constructor(input: {
    action: string;
    body: string;
    retryable: boolean;
    status: number;
    statusText: string;
  }) {
    // api-rs error bodies can carry internals; keep them out of the message,
    // which is surfaced verbatim into the user-facing Telegram chat.
    super(
      `Centaur session ${input.action} failed: ${input.status} ${input.statusText}`,
    );
    this.name = "SessionApiError";
    this.action = input.action;
    this.body = input.body;
    this.retryable = input.retryable;
    this.status = input.status;
    this.statusText = input.statusText;
  }
}

export function isRetryableSessionApiError(error: unknown): boolean {
  if (error instanceof SessionApiError) return error.retryable;
  if (!(error instanceof Error)) return false;
  return error.name === "AbortError" || error.name === "TypeError";
}

// ---------------------------------------------------------------------------
// Serialized message shapes (Telegram delta: no Chat SDK adapter exists for
// Telegram, so the intermediate ApiMessage/ApiAttachment shapes the discordbot
// keeps in types.ts live here, next to the only code that produces them).
// ---------------------------------------------------------------------------

export type TelegramApiAuthor = {
  fullName: string;
  isBot: boolean | "unknown";
  isMe: boolean;
  userId: string;
  userName: string;
};

export type TelegramApiAttachment = {
  dataBase64?: string;
  dataBase64Omitted?: string;
  fetchError?: string;
  height?: number;
  mimeType?: string;
  name?: string;
  size?: number;
  type: "image" | "file";
  url?: string;
  width?: number;
};

export type TelegramApiMessage = {
  attachments: TelegramApiAttachment[];
  author: TelegramApiAuthor;
  /** Stable idempotency key `telegram:{chatId}:{messageId}` (see below). */
  id: string;
  isMention: boolean;
  raw: unknown;
  text: string;
  threadId: string;
  timestamp: string;
};

/**
 * Descriptive/routing context sent with EVERY create-session call: api-rs
 * re-upserts the principal on each create, so omitting a field on a later
 * create would erase it. Principal *selection* comes from the typed thread
 * key, never from this metadata.
 */
export type TelegramSessionCreateContext = {
  /** Display name for the principal (chat title, or the DM peer's name). */
  conversationName: string;
  chatType: TelegramChat["type"];
  /** Telegram numeric user id of the triggering user, as a string. */
  userId: string;
  /** Forum/private topic id; omitted from metadata when absent. */
  messageThreadId?: number;
};

export type ForwardSessionInput = {
  afterEventId: number;
  create: TelegramSessionCreateContext;
  executionId?: string;
  executeMessage?: TelegramApiMessage;
  messages: TelegramApiMessage[];
  onEventId(eventId: number): void;
  openStream: boolean;
  threadKey: string;
  trace?: TelegrambotTrace;
};

type ForwardSessionApiCallbacks = {
  onExecutionStarted?(
    execution: TelegrambotExecuteSessionResponse,
  ): Promise<void>;
  /**
   * Telegram delta: discordbot ignores the append response body, but the
   * telegram poller must correlate `session.steering_delivered.message_ids`
   * (server-assigned `msg_…` ids) with the messages it appended, so the ids
   * returned by POST /messages are handed back here.
   */
  onMessagesAppended?(messageIds: string[]): Promise<void>;
};

/**
 * Stable append/execute idempotency key for a Telegram message. Must stay in
 * lockstep with the inbox `client_message_id` contract in types.ts
 * (`telegram:{chatId}:{messageId}`).
 */
export function telegramClientMessageId(message: TelegramMessage): string {
  return `telegram:${message.chat.id}:${message.message_id}`;
}

// Telegram analog of discordbot's isContentlessApiMessage: sticker-only,
// location, poll, and service messages serialize to empty text with no
// supported attachments; executing them would fabricate a synthetic
// "continue" turn. Callers skip execution instead.
export function isContentlessApiMessage(message: TelegramApiMessage): boolean {
  return message.text.trim() === "" && message.attachments.length === 0;
}

/**
 * Build the session ApiMessage from a raw Telegram message. Text falls back
 * to the media caption; author metadata comes from `from` (the threading
 * gate has already rejected `from`-less and `sender_chat` messages, so the
 * fallbacks here are defensive, not a policy path). `isMe` is always false:
 * self-messages are rejected upstream and Telegram bots never receive their
 * own sends via getUpdates.
 */
export function serializeTelegramMessage(
  message: TelegramMessage,
  threadKey: string,
  options: {
    attachments?: TelegramApiAttachment[];
    isMention?: boolean;
  } = {},
): TelegramApiMessage {
  const from = message.from;
  const fullName = [from?.first_name, from?.last_name]
    .filter(Boolean)
    .join(" ");
  const userId = from ? String(from.id) : "unknown";
  return {
    attachments: options.attachments ?? [],
    author: {
      fullName: fullName || from?.username || userId,
      isBot: from ? from.is_bot : "unknown",
      isMe: false,
      userId,
      userName: from?.username ?? userId,
    },
    id: telegramClientMessageId(message),
    isMention: options.isMention ?? true,
    raw: message,
    text: message.text ?? message.caption ?? "",
    threadId: threadKey,
    timestamp: new Date(message.date * 1000).toISOString(),
  };
}

// ---------------------------------------------------------------------------
// Attachment ingest
// ---------------------------------------------------------------------------

/**
 * Telegram delta: discordbot inlines up to 100MB because Discord CDN objects
 * are already vetted by Discord's own upload limits; Telegram `getFile` only
 * serves files ≤20MB anyway, and the ingest cap is deliberately tighter.
 */
export const MAX_TELEGRAM_ATTACHMENT_BYTES = 10 * 1024 * 1024;

/**
 * A single Telegram message carries at most one photo (the `photo` array is
 * one image at several sizes) and one document today, so this cap is
 * future-proofing against richer update kinds, not a live constraint.
 */
export const MAX_TELEGRAM_ATTACHMENTS = 5;

export type TelegramAttachmentCandidate = {
  fileId: string;
  fileUniqueId: string;
  height?: number;
  mimeType?: string;
  name: string;
  size?: number;
  type: "image" | "file";
  width?: number;
};

/**
 * Downloadable attachments on a message: the largest rendition of `photo`
 * (smaller ones are the same image) plus `document`. Exported so callers can
 * check for media before paying for downloads.
 */
export function telegramAttachmentCandidates(
  message: TelegramMessage,
): TelegramAttachmentCandidate[] {
  const candidates: TelegramAttachmentCandidate[] = [];
  const photo = largestPhotoSize(message.photo);
  if (photo) {
    candidates.push({
      fileId: photo.file_id,
      fileUniqueId: photo.file_unique_id,
      height: photo.height,
      // Telegram re-encodes all photos as JPEG.
      mimeType: "image/jpeg",
      name: `photo-${photo.file_unique_id}.jpg`,
      size: photo.file_size,
      type: "image",
      width: photo.width,
    });
  }
  const document = message.document;
  if (document) {
    candidates.push({
      fileId: document.file_id,
      fileUniqueId: document.file_unique_id,
      mimeType: document.mime_type,
      name: document.file_name ?? `document-${document.file_unique_id}`,
      size: document.file_size,
      type: "file",
    });
  }
  return candidates;
}

function largestPhotoSize(
  photo: TelegramPhotoSize[] | undefined,
): TelegramPhotoSize | undefined {
  if (!photo || photo.length === 0) return undefined;
  // Documented as ascending, but sort defensively rather than trust ordering.
  return [...photo].sort((a, b) => a.width * a.height - b.width * b.height)[
    photo.length - 1
  ];
}

/**
 * Download photo/document attachments via getFile + downloadFile and inline
 * them as base64 for the staged-attachment scheme in toCodexInputLines.
 * Per-attachment failures and oversize skips are recorded as `fetchError` on
 * the serialized attachment (mirroring discordbot) and never thrown, so one
 * broken file cannot drop the message. TelegramApiError messages are already
 * token-redacted by the api module.
 */
export async function serializeTelegramAttachments(
  api: TelegramApi,
  message: TelegramMessage,
  logger: Logger = noopLogger,
): Promise<TelegramApiAttachment[]> {
  return serializeTelegramAttachmentList(
    api,
    telegramAttachmentCandidates(message),
    logger,
  );
}

/** Split from serializeTelegramAttachments so the count cap is testable (a real message today yields ≤2 candidates). */
export async function serializeTelegramAttachmentList(
  api: TelegramApi,
  candidates: TelegramAttachmentCandidate[],
  logger: Logger = noopLogger,
): Promise<TelegramApiAttachment[]> {
  const attachments: TelegramApiAttachment[] = [];
  for (const [index, candidate] of candidates.entries()) {
    if (index >= MAX_TELEGRAM_ATTACHMENTS) {
      logger.warn("telegrambot_attachment_skipped_cap", {
        file_unique_id: candidate.fileUniqueId,
        attachment_count: candidates.length,
        cap: MAX_TELEGRAM_ATTACHMENTS,
      });
      attachments.push({
        ...baseAttachment(candidate),
        fetchError: `attachment skipped: message exceeds the ${MAX_TELEGRAM_ATTACHMENTS}-attachment cap`,
      });
      continue;
    }
    attachments.push(await downloadAttachment(api, candidate, logger));
  }
  return attachments;
}

async function downloadAttachment(
  api: TelegramApi,
  candidate: TelegramAttachmentCandidate,
  logger: Logger,
): Promise<TelegramApiAttachment> {
  const serialized = baseAttachment(candidate);
  if (
    typeof candidate.size === "number" &&
    candidate.size > MAX_TELEGRAM_ATTACHMENT_BYTES
  ) {
    logger.warn("telegrambot_attachment_skipped_oversized", {
      file_unique_id: candidate.fileUniqueId,
      declared_bytes: candidate.size,
    });
    serialized.fetchError = attachmentTooLargeError(candidate.size);
    return serialized;
  }

  try {
    const file = await api.getFile(candidate.fileId);
    if (!file.file_path) {
      serialized.fetchError = "telegram getFile returned no file_path";
      return serialized;
    }
    const bytes = await api.downloadFile(file.file_path);
    // Re-check the actual byte count: Telegram size metadata can be absent.
    if (bytes.byteLength > MAX_TELEGRAM_ATTACHMENT_BYTES) {
      logger.warn("telegrambot_attachment_skipped_oversized", {
        file_unique_id: candidate.fileUniqueId,
        actual_bytes: bytes.byteLength,
      });
      serialized.fetchError = attachmentTooLargeError(bytes.byteLength);
      return serialized;
    }
    serialized.size = bytes.byteLength;
    serialized.dataBase64 = Buffer.from(bytes).toString("base64");
  } catch (error) {
    serialized.fetchError = errorMessage(error);
    logger.warn("telegrambot_attachment_fetch_failed", {
      file_unique_id: candidate.fileUniqueId,
      error: serialized.fetchError,
    });
  }
  return serialized;
}

function baseAttachment(
  candidate: TelegramAttachmentCandidate,
): TelegramApiAttachment {
  return {
    height: candidate.height,
    mimeType: candidate.mimeType,
    name: candidate.name,
    size: candidate.size,
    type: candidate.type,
    width: candidate.width,
  };
}

function attachmentTooLargeError(bytes: number): string {
  return `attachment too large to inline (${bytes} bytes > ${MAX_TELEGRAM_ATTACHMENT_BYTES} byte limit)`;
}

// ---------------------------------------------------------------------------
// Session API client
// ---------------------------------------------------------------------------

// Note: on Telegram (as on Discord) the execute/openStream tail below is dead
// code — the live path always calls with `executeMessage: undefined` and runs
// the execute via `executeSessionTurn` inside the render stream (after the 👀
// reaction lands). The tail is kept verbatim so 3-way syncs against
// discordbot/slackbotv2 diff cleanly.
export async function forwardToSessionApi(
  options: TelegrambotOptions,
  input: ForwardSessionInput,
  callbacks: ForwardSessionApiCallbacks = {},
): Promise<AsyncIterable<TelegrambotRendererSource> | null> {
  const createStartedAtMs = nowMs();
  await createSession(options, input.threadKey, input.create);
  traceLog(options.logger, "telegrambot_session_create_complete", input.trace, {
    phase_ms: elapsedMs(createStartedAtMs),
  });
  if (input.messages.length > 0) {
    const appendStartedAtMs = nowMs();
    const messageIds = await appendSessionMessages(
      options,
      input.threadKey,
      input.messages,
    );
    traceLog(
      options.logger,
      "telegrambot_session_append_complete",
      input.trace,
      {
        message_count: input.messages.length,
        phase_ms: elapsedMs(appendStartedAtMs),
      },
    );
    await callbacks.onMessagesAppended?.(messageIds);
  } else {
    traceLog(
      options.logger,
      "telegrambot_session_append_skipped",
      input.trace,
      { message_count: 0 },
    );
  }
  if (!input.executeMessage) return null;

  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadKey,
    input.executeMessage,
  );
  traceLog(
    options.logger,
    "telegrambot_session_execute_complete",
    input.trace,
    {
      execution_id: execution.execution_id,
      phase_ms: elapsedMs(executeStartedAtMs),
    },
  );
  await callbacks.onExecutionStarted?.(execution);
  if (!input.openStream) return null;

  return openSessionEventStream(options, input);
}

/**
 * Execute the session turn on its own (start the agent run), returning the
 * execution. Split out of forwardToSessionApi so the render stream can run it
 * AFTER the 👀 working reaction lands — the execute call blocks on cold
 * sandbox spin-up. Idempotent via the request's idempotency_key (the stable
 * `telegram:{chatId}:{messageId}` message id), so a render retry or inbox
 * recovery won't re-spawn the sandbox.
 */
export async function executeSessionTurn(
  options: TelegrambotOptions,
  input: ForwardSessionInput,
): Promise<TelegrambotExecuteSessionResponse | null> {
  if (!input.executeMessage) return null;
  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadKey,
    input.executeMessage,
  );
  traceLog(
    options.logger,
    "telegrambot_session_execute_complete",
    input.trace,
    {
      execution_id: execution.execution_id,
      phase_ms: elapsedMs(executeStartedAtMs),
    },
  );
  return execution;
}

export async function openSessionEventStream(
  options: TelegrambotOptions,
  input: Pick<
    ForwardSessionInput,
    "afterEventId" | "executionId" | "onEventId" | "threadKey" | "trace"
  >,
): Promise<AsyncIterable<TelegrambotRendererSource>> {
  const streamStartedAtMs = nowMs();
  const stream = await streamSessionNotifications(
    options,
    input.threadKey,
    input.afterEventId,
    input.executionId,
    input.onEventId,
  );
  traceLog(options.logger, "telegrambot_session_events_opened", input.trace, {
    after_event_id: input.afterEventId,
    execution_id: input.executionId,
    phase_ms: elapsedMs(streamStartedAtMs),
  });
  return stream;
}

// Deliberate delta from slackbotv2 (kept from the Discord port): the synthetic
// starting item primes the mapper's task state so answer deltas stream
// immediately instead of waiting out the pre-stream grace period.
export function startingStreamNotification(threadKey: string): JsonObject {
  return {
    method: "item/started",
    params: {
      threadId: threadKey,
      turnId: "telegrambot-starting-turn",
      startedAtMs: Date.now(),
      item: {
        id: "telegrambot-starting",
        memoryCitation: null,
        phase: "commentary",
        text: "",
        type: "agentMessage",
      },
    },
  };
}

export function sessionStreamError(error: unknown): RustSessionStreamEvent {
  return {
    data: { error: error instanceof Error ? error.message : String(error) },
    event: "session.stream_error",
    eventKind: "session.stream_error",
  };
}

// ---------------------------------------------------------------------------
// Steering events
// ---------------------------------------------------------------------------

/**
 * Event names verified against api-rs
 * `centaur-session-runtime/src/lib.rs` (`forward_messages_to_active_execution`
 * / `record_steering_failure`):
 *
 * - `session.steering_delivered` data:
 *   `{ execution_id, thread_key, message_ids, input_line_count }`
 * - `session.steering_failed` data:
 *   `{ execution_id, thread_key, error }`
 *
 * `message_ids` are the server-assigned `msg_…` ids the append route returns
 * as `message_ids` — NOT the client_message_ids — which is why
 * appendSessionMessages surfaces the append response ids to its caller.
 */
export const SESSION_STEERING_DELIVERED_EVENT = "session.steering_delivered";
export const SESSION_STEERING_FAILED_EVENT = "session.steering_failed";

export function isSteeringDeliveredEvent(
  source: TelegrambotRendererSource,
): boolean {
  return sourceEventKind(source) === SESSION_STEERING_DELIVERED_EVENT;
}

export function isSteeringFailedEvent(
  source: TelegrambotRendererSource,
): boolean {
  return sourceEventKind(source) === SESSION_STEERING_FAILED_EVENT;
}

/** Server-assigned session message ids a steering_delivered event covers ([] otherwise). */
export function steeringDeliveredMessageIds(
  source: TelegrambotRendererSource,
): string[] {
  if (!isSteeringDeliveredEvent(source)) return [];
  const data = sourceEventData(source);
  if (!isJsonObject(data) || !Array.isArray(data.message_ids)) return [];
  return data.message_ids.filter(
    (value): value is string => typeof value === "string",
  );
}

export function steeringFailedError(
  source: TelegrambotRendererSource,
): string | undefined {
  if (!isSteeringFailedEvent(source)) return undefined;
  const data = sourceEventData(source);
  if (!isJsonObject(data)) return undefined;
  return stringValue(data.error);
}

function sourceEventKind(
  source: TelegrambotRendererSource,
): string | undefined {
  if (!source || typeof source !== "object") return undefined;
  const record = source as { event?: unknown; eventKind?: unknown };
  if (typeof record.eventKind === "string") return record.eventKind;
  if (typeof record.event === "string") return record.event;
  return undefined;
}

function sourceEventData(source: TelegrambotRendererSource): unknown {
  if (!source || typeof source !== "object") return undefined;
  return (source as { data?: unknown }).data;
}

// ---------------------------------------------------------------------------
// HTTP calls
// ---------------------------------------------------------------------------

async function createSession(
  options: TelegrambotOptions,
  threadKey: string,
  create: TelegramSessionCreateContext,
): Promise<void> {
  const fetchFn = options.fetch ?? fetch;
  const name = create.conversationName.trim();
  const body: TelegrambotCreateSessionRequest = {
    harness_type: options.harnessType ?? "codex",
    metadata: {
      source: "telegrambot",
      platform: "telegram",
      thread_id: threadKey,
      // api-rs reads this as the session principal's display name; a blank
      // name is omitted rather than upserted over a previous good one.
      ...(name ? { telegram_conversation_name: name } : {}),
      telegram_chat_type: create.chatType,
      user_id: create.userId,
      ...(create.messageThreadId === undefined
        ? {}
        : { message_thread_id: create.messageThreadId }),
    },
  };
  const response = await fetchFn(apiSessionUrl(options.apiUrl, threadKey), {
    method: "POST",
    headers: apiHeaders(options),
    body: JSON.stringify(body),
  });
  await ensureApiOk(response, "create session", options);
}

/** Returns the server-assigned `msg_…` ids (see steering doc above); [] when the response body is unreadable. */
async function appendSessionMessages(
  options: TelegrambotOptions,
  threadKey: string,
  messages: TelegramApiMessage[],
): Promise<string[]> {
  const fetchFn = options.fetch ?? fetch;
  const body: TelegrambotAppendMessagesRequest = {
    messages: messages.map(toSessionMessage),
  };
  const response = await fetchFn(
    apiSessionUrl(options.apiUrl, threadKey, "messages"),
    {
      method: "POST",
      headers: apiHeaders(options),
      body: JSON.stringify(body),
    },
  );
  await ensureApiOk(response, "append session messages", options);
  try {
    const payload: unknown = await response.json();
    if (isJsonObject(payload) && Array.isArray(payload.message_ids)) {
      return payload.message_ids.filter(
        (value): value is string => typeof value === "string",
      );
    }
  } catch {
    // Correlation degrades gracefully: without ids the poller falls back to
    // resolving steering_pending rows on execution termination.
  }
  return [];
}

async function executeSession(
  options: TelegrambotOptions,
  threadKey: string,
  message: TelegramApiMessage,
): Promise<TelegrambotExecuteSessionResponse> {
  const fetchFn = options.fetch ?? fetch;
  const body: TelegrambotExecuteSessionRequest = {
    idempotency_key: message.id,
    metadata: sessionMetadata(message, { action: "execute" }),
    input_lines: toCodexInputLines(message, threadKey),
    ...(options.idleTimeoutMs === undefined
      ? {}
      : { idle_timeout_ms: options.idleTimeoutMs }),
    ...(options.maxDurationMs === undefined
      ? {}
      : { max_duration_ms: options.maxDurationMs }),
  };
  const response = await fetchFn(
    apiSessionUrl(options.apiUrl, threadKey, "execute"),
    {
      method: "POST",
      headers: apiHeaders(options),
      body: JSON.stringify(body),
    },
  );
  await ensureApiOk(response, "execute session", options);
  return (await response.json()) as TelegrambotExecuteSessionResponse;
}

async function ensureApiOk(
  response: Response,
  action: string,
  options: TelegrambotOptions,
): Promise<void> {
  if (response.ok) return;
  let body = "";
  try {
    body = await response.text();
  } catch {
    body = "";
  }
  // api-rs is internal and unauthenticated; its error bodies can carry stack
  // traces, internal hostnames, or echoed payloads. Log the full body
  // server-side, but the thrown message stays generic — it is surfaced
  // verbatim into the user-facing chat via sessionStreamError.
  if (body) {
    (options.logger ?? noopLogger).warn("telegrambot_session_api_error", {
      action,
      status: response.status,
      status_text: response.statusText,
      body,
    });
  }
  throw new SessionApiError({
    action,
    body,
    retryable: isRetryableApiStatus(response.status),
    status: response.status,
    statusText: response.statusText,
  });
}

function isRetryableApiStatus(status: number): boolean {
  return status === 408 || status === 425 || status === 429 || status >= 500;
}

async function streamSessionNotifications(
  options: TelegrambotOptions,
  threadKey: string,
  afterEventId: number,
  executionId: string | undefined,
  onEventId: (eventId: number) => void,
): Promise<AsyncIterable<TelegrambotRendererSource>> {
  const fetchFn = options.fetch ?? fetch;
  const url = new URL(apiSessionUrl(options.apiUrl, threadKey, "events"));
  url.searchParams.set("after_event_id", String(afterEventId));
  if (executionId) url.searchParams.set("execution_id", executionId);
  const response = await fetchFn(url.toString(), {
    method: "GET",
    headers: apiHeaders(options, false),
  });
  await ensureApiOk(response, "stream events", options);
  if (!response.body) return toAsyncIterable([]);
  return parseSessionEventStream(response.body, onEventId);
}

function apiSessionUrl(
  apiUrl: string,
  threadKey: string,
  suffix?: "messages" | "execute" | "events",
): string {
  const path = `/api/session/${encodeURIComponent(threadKey)}${suffix ? `/${suffix}` : ""}`;
  return new URL(path, ensureTrailingSlash(apiUrl)).toString();
}

function ensureTrailingSlash(value: string): string {
  return value.endsWith("/") ? value : `${value}/`;
}

function apiHeaders(
  options: TelegrambotOptions,
  jsonBody = true,
): HeadersInit {
  const apiKey = options.apiKey ?? process.env.TELEGRAMBOT_API_KEY;
  return {
    ...(jsonBody ? { "content-type": "application/json" } : {}),
    ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {}),
  };
}

function toSessionMessage(
  message: TelegramApiMessage,
): TelegrambotSessionMessage {
  return {
    client_message_id: message.id,
    role: message.author.isMe ? "assistant" : "user",
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message),
  };
}

function sessionMessageParts(message: TelegramApiMessage): JsonValue[] {
  const parts: JsonValue[] = [];
  if (message.text.trim()) {
    parts.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    parts.push(sessionAttachmentPart(attachment));
  }
  return parts.length > 0 ? parts : [{ type: "text", text: "" }];
}

function sessionAttachmentPart(attachment: TelegramApiAttachment): JsonObject {
  const part: JsonObject = {
    ...attachment,
    attachment_type: attachment.type,
    type: "attachment",
  };
  // Don't persist megabytes of base64 in the stored session message; the
  // executing turn delivers the bytes separately (inline or staged chunks).
  if (
    typeof attachment.dataBase64 === "string" &&
    attachment.dataBase64.length > MAX_CODEX_INPUT_LINE_CHARS
  ) {
    delete part.dataBase64;
    part.dataBase64Omitted = `${attachment.dataBase64.length} base64 chars omitted from stored session message`;
  }
  return part;
}

function sessionMetadata(
  message: TelegramApiMessage,
  extra: JsonObject = {},
): JsonObject {
  return {
    source: "telegrambot",
    platform: "telegram",
    message_id: message.id,
    thread_id: message.threadId,
    is_mention: message.isMention,
    timestamp: message.timestamp,
    user_id: message.author.userId,
    user_name: message.author.userName,
    ...extra,
  };
}

/**
 * Largest JSON codex input line we will emit. A `data:` URL inlined directly
 * in the user message blows past this for larger images, so anything bigger
 * is delivered out-of-band as `attachment.chunk` lines and referenced by a
 * staged attachment id (mirrors discordbot/slackbotv2).
 */
const MAX_CODEX_INPUT_LINE_CHARS = 900 * 1024;
const STAGED_ATTACHMENT_CHUNK_CHARS = 700 * 1024;

/**
 * Build the codex input lines for an execute turn. Attachments whose inlined
 * `data:` URL would push the user-message line past `MAX_CODEX_INPUT_LINE_CHARS`
 * are streamed ahead of it as `attachment.chunk` lines and referenced by a
 * staged attachment id; everything else stays inline.
 */
export function toCodexInputLines(
  message: TelegramApiMessage,
  threadKey: string,
): string[] {
  const staged = new Map<TelegramApiAttachment, string>();
  const lines: string[] = [];
  for (const attachment of message.attachments) {
    if (!attachment.dataBase64) continue;
    const inlineLine = toCodexInputLineWithStaged(message, threadKey, staged);
    if (
      inlineLine.length <= MAX_CODEX_INPUT_LINE_CHARS &&
      attachment.dataBase64.length <= MAX_CODEX_INPUT_LINE_CHARS
    ) {
      continue;
    }
    const stagedAttachmentId = `att-${message.id}-${staged.size + 1}`;
    staged.set(attachment, stagedAttachmentId);
    lines.push(...stagedAttachmentInputLines(attachment, stagedAttachmentId));
  }
  lines.push(toCodexInputLineWithStaged(message, threadKey, staged));
  return lines;
}

function toCodexInputLineWithStaged(
  message: TelegramApiMessage,
  threadKey: string,
  staged: Map<TelegramApiAttachment, string>,
): string {
  return JSON.stringify({
    type: "user",
    thread_key: threadKey,
    trace_metadata: sessionMetadata(message, { action: "execute" }),
    message: {
      role: "user",
      content: codexInputContent(message, staged),
    },
  });
}

function stagedAttachmentInputLines(
  attachment: TelegramApiAttachment,
  stagedAttachmentId: string,
): string[] {
  const dataBase64 = attachment.dataBase64;
  if (!dataBase64) return [];
  const lines: string[] = [];
  // Keep chunks on a base64 boundary (multiple of 4) so each decodes cleanly.
  const chunkSize =
    STAGED_ATTACHMENT_CHUNK_CHARS - (STAGED_ATTACHMENT_CHUNK_CHARS % 4);
  for (
    let offset = 0, index = 0;
    offset < dataBase64.length;
    offset += chunkSize, index += 1
  ) {
    const chunk = dataBase64.slice(offset, offset + chunkSize);
    lines.push(
      JSON.stringify({
        type: "attachment.chunk",
        attachmentId: stagedAttachmentId,
        name: attachment.name,
        mimeType: attachment.mimeType,
        attachmentType: attachment.type,
        chunkIndex: index,
        final: offset + chunkSize >= dataBase64.length,
        dataBase64: chunk,
      }),
    );
  }
  return lines;
}

function codexInputContent(
  message: TelegramApiMessage,
  staged: Map<TelegramApiAttachment, string> = new Map(),
): JsonValue[] {
  const content: JsonValue[] = [];
  if (message.text.trim()) {
    content.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment, staged.get(attachment)));
  }
  return content.length > 0 ? content : [{ type: "text", text: "continue" }];
}

export function codexAttachmentInput(
  attachment: TelegramApiAttachment,
  stagedAttachmentId?: string,
): JsonValue {
  if (stagedAttachmentId) {
    return {
      type: "attachment",
      attachment_type: attachment.type,
      stagedAttachmentId,
      name: attachment.name,
      mimeType: attachment.mimeType,
      size: attachment.size,
    };
  }
  const dataUrl =
    attachment.dataBase64 && attachment.mimeType
      ? `data:${attachment.mimeType};base64,${attachment.dataBase64}`
      : undefined;
  if (attachment.type === "image" && (dataUrl || attachment.url)) {
    return {
      type: "image",
      url: dataUrl ?? attachment.url,
      detail: "auto",
      name: attachment.name,
    };
  }
  if (attachment.dataBase64) {
    return {
      type: "attachment",
      attachment_type: attachment.type,
      dataBase64: attachment.dataBase64,
      mimeType: attachment.mimeType,
      name: attachment.name,
      size: attachment.size,
    };
  }
  return {
    type: "text",
    text: attachmentDescription(attachment),
  };
}

function attachmentDescription(attachment: TelegramApiAttachment): string {
  const fields = [
    `name=${attachment.name ?? "attachment"}`,
    `type=${attachment.type}`,
    attachment.mimeType ? `mime=${attachment.mimeType}` : undefined,
    attachment.dataBase64Omitted
      ? `content=${attachment.dataBase64Omitted}`
      : undefined,
    attachment.fetchError ? `fetch_error=${attachment.fetchError}` : undefined,
  ].filter(Boolean);
  return `[Telegram attachment: ${fields.join(" ")}]`;
}

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

type ParsedSessionEvent = {
  data: string;
  event?: string;
  id?: number;
};

async function* parseSessionEventStream(
  stream: ReadableStream<Uint8Array>,
  onEventId: (eventId: number) => void,
): AsyncIterable<TelegrambotRendererSource> {
  for await (const event of parseSseEvents(stream)) {
    if (typeof event.id === "number") onEventId(event.id);
    if (event.event === "session.output.line") {
      yield {
        data: event.data,
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      } satisfies RustSessionStreamEvent;
      if (isTerminalCodexOutputLine(event.data)) return;
      continue;
    }
    if (event.event === "session.activity_summary") {
      yield {
        data: sessionEventData(event),
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      } satisfies RustSessionStreamEvent;
      continue;
    }
    // Telegram delta: discordbot drops steering events on the floor; here
    // they must reach the consumer so the inbox can resolve steering_pending
    // rows (delivered) or reschedule the message (failed). Non-terminal.
    if (
      event.event === SESSION_STEERING_DELIVERED_EVENT ||
      event.event === SESSION_STEERING_FAILED_EVENT
    ) {
      yield {
        data: sessionEventData(event),
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      } satisfies RustSessionStreamEvent;
      continue;
    }
    if (
      event.event === "session.execution_failed" ||
      event.event === "session.stream_error"
    ) {
      yield {
        data: { error: sessionErrorMessage(event) },
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      } satisfies RustSessionStreamEvent;
      return;
    }
    if (event.event === "session.execution_cancelled") {
      yield {
        data: { error: sessionErrorMessage(event, "Execution cancelled") },
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      } satisfies RustSessionStreamEvent;
      return;
    }
    if (event.event === "session.execution_completed") {
      yield {
        data: sessionEventData(event),
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      } satisfies RustSessionStreamEvent;
      return;
    }
  }
}

async function* parseSseEvents(
  stream: ReadableStream<Uint8Array>,
): AsyncIterable<ParsedSessionEvent> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let eventName: string | undefined;
  let eventId: number | undefined;
  let data: string[] = [];

  // The consumer returns early on terminal events, abandoning this generator
  // at a yield point. Without the finally, the reader lock is never released
  // and the HTTP response body is never cancelled, leaking the SSE connection
  // on every completed run (kept from the Discord port).
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? "";

      for (const line of lines) {
        const emitted = parseSseLine(line, { data, eventId, eventName });
        data = emitted.state.data;
        eventId = emitted.state.eventId;
        eventName = emitted.state.eventName;
        if (emitted.event) yield emitted.event;
      }
    }

    buffer += decoder.decode();
    if (buffer) {
      const emitted = parseSseLine(buffer, { data, eventId, eventName });
      data = emitted.state.data;
      eventId = emitted.state.eventId;
      eventName = emitted.state.eventName;
      if (emitted.event) yield emitted.event;
    }
    if (data.length > 0) {
      yield { data: data.join("\n"), event: eventName, id: eventId };
    }
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function parseSseLine(
  line: string,
  state: {
    data: string[];
    eventId?: number;
    eventName?: string;
  },
): {
  event?: ParsedSessionEvent;
  state: { data: string[]; eventId?: number; eventName?: string };
} {
  if (!line.trim()) {
    const event =
      state.data.length > 0
        ? {
            data: state.data.join("\n"),
            event: state.eventName,
            id: state.eventId,
          }
        : undefined;
    return { event, state: { data: [] } };
  }
  if (line.startsWith(":")) return { state };

  const separator = line.indexOf(":");
  const field = separator >= 0 ? line.slice(0, separator) : line;
  const value =
    separator >= 0 ? line.slice(separator + 1).replace(/^ /, "") : "";
  if (field === "event") return { state: { ...state, eventName: value } };
  if (field === "id") {
    const id = Number.parseInt(value, 10);
    return {
      state: { ...state, eventId: Number.isFinite(id) ? id : undefined },
    };
  }
  if (field === "data" && value !== "[DONE]") {
    return { state: { ...state, data: [...state.data, value] } };
  }

  return { state };
}

function isTerminalCodexOutputLine(line: string): boolean {
  let payload: unknown;
  try {
    payload = JSON.parse(line);
  } catch {
    // Non-JSON stdout lines (e.g. sandbox bootstrap notices) are noise, not a
    // signal that the turn finished; treating them as terminal drops the answer.
    return false;
  }
  if (!isJsonObject(payload)) return false;

  return (
    payload.type === "turn.completed" ||
    payload.type === "turn.failed" ||
    payload.type === "turn.done" ||
    payload.method === "error" ||
    payload.method === "turn/completed"
  );
}

function sessionEventData(event: ParsedSessionEvent): unknown {
  try {
    return JSON.parse(event.data);
  } catch {
    return event.data;
  }
}

function sessionErrorMessage(
  event: ParsedSessionEvent,
  fallback?: string,
): string {
  let message = fallback ?? `${event.event ?? "session error"}`;
  try {
    const payload = JSON.parse(event.data);
    if (isJsonObject(payload)) {
      message =
        stringValue(payload.error) ?? stringValue(payload.message) ?? message;
    }
  } catch {
    if (event.data.trim()) message = event.data.trim();
  }
  return message;
}
