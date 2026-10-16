import type { RustSessionStreamEvent } from "@centaur/harness-events";
import { isRetryableCodexErrorNotification } from "@centaur/rendering";
import type { Attachment, Message } from "chat";
import type {
  JsonObject,
  JsonValue,
  TelegramTrigger,
  TelegrambotApiAttachment,
  TelegrambotApiMessage,
  TelegrambotAppendMessagesRequest,
  TelegrambotCreateSessionRequest,
  TelegrambotExecuteSessionRequest,
  TelegrambotExecuteSessionResponse,
  TelegrambotOptions,
  TelegrambotRendererSource,
  TelegrambotSessionMessage,
  ForwardSessionInput,
} from "./types";
import {
  elapsedMs,
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

// Telegram delta: a message that serializes to empty text with no
// attachments would fabricate a synthetic "continue" turn; callers skip it.
export function isContentlessApiMessage(
  message: TelegrambotApiMessage,
): boolean {
  return message.text.trim() === "" && message.attachments.length === 0;
}

export async function serializeMessage(
  message: Message,
  input: { text?: string; trigger?: TelegramTrigger } = {},
): Promise<TelegrambotApiMessage> {
  const attachments: TelegrambotApiAttachment[] = [];
  for (const attachment of message.attachments) {
    attachments.push(await serializeAttachment(attachment));
  }

  return {
    attachments,
    author: {
      fullName: message.author.fullName,
      isBot: message.author.isBot,
      isMe: message.author.isMe,
      userId: message.author.userId,
      userName: message.author.userName,
    },
    id: message.id,
    isMention: message.isMention === true,
    text: input.text ?? message.text,
    threadId: message.threadId,
    timestamp: message.metadata.dateSent.toISOString(),
    ...(input.trigger ? { trigger: input.trigger } : {}),
  };
}

export async function executeSessionTurn(
  options: TelegrambotOptions,
  input: ForwardSessionInput,
): Promise<TelegrambotExecuteSessionResponse | null> {
  if (!input.executeMessage) return null;
  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadId,
    input.executeMessage,
  );
  traceLog(options, "telegrambot_session_execute_complete", input.trace, {
    execution_id: execution.execution_id,
    phase_ms: elapsedMs(executeStartedAtMs),
  });
  return execution;
}

export async function openSessionEventStream(
  options: TelegrambotOptions,
  input: Pick<
    ForwardSessionInput,
    "afterEventId" | "executionId" | "onEventId" | "threadId" | "trace"
  >,
): Promise<AsyncIterable<TelegrambotRendererSource>> {
  const streamStartedAtMs = nowMs();
  const stream = await streamSessionNotifications(
    options,
    input.threadId,
    input.afterEventId,
    input.executionId,
    input.onEventId,
  );
  traceLog(options, "telegrambot_session_events_opened", input.trace, {
    after_event_id: input.afterEventId,
    execution_id: input.executionId,
    phase_ms: elapsedMs(streamStartedAtMs),
  });
  return stream;
}

// Deliberate delta from slackbotv2 (which removed this entirely): the
// synthetic starting item primes the mapper's task state so answer deltas
// stream immediately instead of waiting out the pre-stream grace period.
export function startingStreamNotification(threadId: string): JsonObject {
  return {
    method: "item/started",
    params: {
      threadId,
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

/**
 * Largest attachment we buffer and inline. Bots can only download files up to
 * 20 MB through the Bot API (the adapter enforces 25 MB); anything above is
 * described to the agent instead of fetched.
 */
export const MAX_INLINE_ATTACHMENT_BYTES = 20 * 1024 * 1024;

/**
 * Largest JSON codex input line we will emit. A `data:` URL inlined directly in
 * the user message blows past this for larger images, so anything bigger is
 * delivered out-of-band as `attachment.chunk` lines and referenced by a staged
 * attachment id (mirrors slackbotv2).
 */
const MAX_CODEX_INPUT_LINE_CHARS = 900 * 1024;
const STAGED_ATTACHMENT_CHUNK_CHARS = 700 * 1024;

/**
 * The Telegram adapter exposes inbound files only through a lazy `fetchData`
 * closure (the download URL embeds the bot token, so it is never surfaced).
 * Bytes are inlined as base64 so every model provider can read them.
 */
export async function serializeAttachment(
  attachment: Attachment,
): Promise<TelegrambotApiAttachment> {
  const serialized: TelegrambotApiAttachment = {
    height: attachment.height,
    mimeType: attachment.mimeType,
    name: attachment.name,
    size: attachment.size,
    type: attachment.type,
    width: attachment.width,
  };

  if (
    typeof attachment.size === "number" &&
    attachment.size > MAX_INLINE_ATTACHMENT_BYTES
  ) {
    serialized.fetchError = attachmentTooLargeError(attachment.size);
    return serialized;
  }

  try {
    const data = attachment.data ?? (await attachment.fetchData?.());
    if (data) {
      const bytes = await toBuffer(data);
      if (bytes.length > MAX_INLINE_ATTACHMENT_BYTES) {
        serialized.fetchError = attachmentTooLargeError(bytes.length);
        return serialized;
      }
      serialized.dataBase64 = bytes.toString("base64");
    }
  } catch (error) {
    serialized.fetchError =
      error instanceof Error ? error.message : String(error);
  }

  return serialized;
}

function attachmentTooLargeError(bytes: number): string {
  return `attachment too large to inline (${bytes} bytes > ${MAX_INLINE_ATTACHMENT_BYTES} byte limit)`;
}

async function toBuffer(data: Buffer | Blob | ArrayBuffer): Promise<Buffer> {
  if (Buffer.isBuffer(data)) return data;
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  return Buffer.from(await data.arrayBuffer());
}

/**
 * Create (or reuse) the session and durably append `messages`. Execution is
 * started separately (executeSessionTurn) inside the render stream, after the
 * working reaction lands.
 */
export async function forwardToSessionApi(
  options: TelegrambotOptions,
  input: ForwardSessionInput,
  callbacks: { onMessagesAppended?(): Promise<void> } = {},
): Promise<void> {
  const createStartedAtMs = nowMs();
  await createSession(options, input.threadId, input.conversationName);
  traceLog(options, "telegrambot_session_create_complete", input.trace, {
    phase_ms: elapsedMs(createStartedAtMs),
  });
  if (input.messages.length === 0) {
    traceLog(options, "telegrambot_session_append_skipped", input.trace, {
      message_count: 0,
    });
    return;
  }
  const appendStartedAtMs = nowMs();
  await appendSessionMessages(options, input.threadId, input.messages);
  traceLog(options, "telegrambot_session_append_complete", input.trace, {
    message_count: input.messages.length,
    phase_ms: elapsedMs(appendStartedAtMs),
  });
  await callbacks.onMessagesAppended?.();
}

async function createSession(
  options: TelegrambotOptions,
  threadId: string,
  conversationName?: string,
): Promise<void> {
  const fetchFn = options.fetch ?? fetch;
  const name = conversationName?.trim();
  const body: TelegrambotCreateSessionRequest = {
    harness_type: "codex",
    metadata: {
      source: "telegrambot",
      platform: "telegram",
      thread_id: threadId,
      // api-rs reads this as the session principal's display name.
      ...(name ? { telegram_conversation_name: name } : {}),
    },
  };
  const response = await fetchFn(apiSessionUrl(options.apiUrl, threadId), {
    method: "POST",
    headers: apiHeaders(options),
    body: JSON.stringify(body),
  });
  await ensureApiOk(response, "create session", options);
}

async function appendSessionMessages(
  options: TelegrambotOptions,
  threadId: string,
  messages: TelegrambotApiMessage[],
): Promise<void> {
  const fetchFn = options.fetch ?? fetch;
  const body: TelegrambotAppendMessagesRequest = {
    messages: messages.map(toSessionMessage),
  };
  const response = await fetchFn(
    apiSessionUrl(options.apiUrl, threadId, "messages"),
    {
      method: "POST",
      headers: apiHeaders(options),
      body: JSON.stringify(body),
    },
  );
  await ensureApiOk(response, "append session messages", options);
}

async function executeSession(
  options: TelegrambotOptions,
  threadId: string,
  message: TelegrambotApiMessage,
): Promise<TelegrambotExecuteSessionResponse> {
  const fetchFn = options.fetch ?? fetch;
  const body: TelegrambotExecuteSessionRequest = {
    idempotency_key: message.id,
    metadata: sessionMetadata(message, { action: "execute" }),
    input_lines: toCodexInputLines(message, threadId),
    ...(options.idleTimeoutMs === undefined
      ? {}
      : { idle_timeout_ms: options.idleTimeoutMs }),
    ...(options.maxDurationMs === undefined
      ? {}
      : { max_duration_ms: options.maxDurationMs }),
  };
  const response = await fetchFn(
    apiSessionUrl(options.apiUrl, threadId, "execute"),
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
  // api-rs is internal and unauthenticated; its error bodies can carry stack traces, internal
  // hostnames, or echoed payloads. Log the full body server-side, but the thrown message stays
  // generic — it is surfaced verbatim into the user-facing Telegram chat via sessionStreamError.
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
  threadId: string,
  afterEventId: number,
  executionId: string | undefined,
  onEventId: (eventId: number) => void,
): Promise<AsyncIterable<TelegrambotRendererSource>> {
  const fetchFn = options.fetch ?? fetch;
  const url = new URL(apiSessionUrl(options.apiUrl, threadId, "events"));
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
  threadId: string,
  suffix?: "messages" | "execute" | "events",
): string {
  const path = `/api/session/${encodeURIComponent(threadId)}${suffix ? `/${suffix}` : ""}`;
  return new URL(path, ensureTrailingSlash(apiUrl)).toString();
}

function ensureTrailingSlash(value: string): string {
  return value.endsWith("/") ? value : `${value}/`;
}

function apiHeaders(options: TelegrambotOptions, jsonBody = true): HeadersInit {
  const apiKey = options.apiKey ?? process.env.TELEGRAMBOT_API_KEY;
  return {
    ...(jsonBody ? { "content-type": "application/json" } : {}),
    ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {}),
  };
}

function toSessionMessage(
  message: TelegrambotApiMessage,
): TelegrambotSessionMessage {
  return {
    client_message_id: message.id,
    role: message.author.isMe ? "assistant" : "user",
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message),
  };
}

function sessionMessageParts(message: TelegrambotApiMessage): JsonValue[] {
  const parts: JsonValue[] = [];
  if (message.text.trim()) {
    parts.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    parts.push(sessionAttachmentPart(attachment));
  }
  return parts.length > 0 ? parts : [{ type: "text", text: "" }];
}

function sessionAttachmentPart(
  attachment: TelegrambotApiAttachment,
): JsonObject {
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
  message: TelegrambotApiMessage,
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
    ...(message.trigger ? { trigger: message.trigger } : {}),
    ...extra,
  };
}

/**
 * Build the codex input lines for an execute turn. Attachments whose inlined
 * `data:` URL would push the user-message line past `MAX_CODEX_INPUT_LINE_CHARS`
 * are streamed ahead of it as `attachment.chunk` lines and referenced by a
 * staged attachment id; everything else stays inline. Mirrors slackbotv2.
 */
export function toCodexInputLines(
  message: TelegrambotApiMessage,
  threadId: string,
): string[] {
  const staged = new Map<TelegrambotApiAttachment, string>();
  const lines: string[] = [];
  for (const attachment of message.attachments) {
    if (!attachment.dataBase64) continue;
    const inlineLine = toCodexInputLineWithStaged(message, threadId, staged);
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
  lines.push(toCodexInputLineWithStaged(message, threadId, staged));
  return lines;
}

function toCodexInputLineWithStaged(
  message: TelegrambotApiMessage,
  threadId: string,
  staged: Map<TelegrambotApiAttachment, string>,
): string {
  return JSON.stringify({
    type: "user",
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: "execute" }),
    message: {
      role: "user",
      content: codexInputContent(message, staged),
    },
  });
}

function stagedAttachmentInputLines(
  attachment: TelegrambotApiAttachment,
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
  message: TelegrambotApiMessage,
  staged: Map<TelegrambotApiAttachment, string> = new Map(),
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
  attachment: TelegrambotApiAttachment,
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
  if (attachment.type === "image" && dataUrl) {
    return {
      type: "image",
      url: dataUrl,
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

function attachmentDescription(attachment: TelegrambotApiAttachment): string {
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

  // Ported from discordbot: the consumer returns early on
  // terminal events, abandoning this generator at a yield point. Without the
  // finally, the reader lock is never released and the HTTP response body is
  // never cancelled, leaking the SSE connection on every completed run.
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
  if (isRetryableCodexErrorNotification(payload)) return false;

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
