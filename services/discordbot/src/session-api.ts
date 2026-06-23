import type { RustSessionStreamEvent } from "@centaur/harness-events";
import type { Attachment, Message } from "chat";
import { withDiscordEmbedText } from "./discord-starter";
import type {
  DiscordbotApiAttachment,
  DiscordbotApiMessage,
  DiscordbotAppendMessagesRequest,
  DiscordbotCreateSessionRequest,
  DiscordbotExecuteSessionRequest,
  DiscordbotExecuteSessionResponse,
  DiscordbotOptions,
  DiscordbotRendererSource,
  DiscordbotSessionMessage,
  ForwardSessionInput,
  JsonObject,
  JsonValue,
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
    // which is surfaced verbatim into the user-facing Discord thread.
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

type ForwardSessionApiCallbacks = {
  onExecutionStarted?(
    execution: DiscordbotExecuteSessionResponse,
  ): Promise<void>;
  onMessagesAppended?(): Promise<void>;
};

export async function collectInitialContext(
  thread: { allMessages: AsyncIterable<Message> },
  currentMessage: Message,
): Promise<DiscordbotApiMessage[]> {
  const messages: Message[] = [];
  try {
    for await (const message of thread.allMessages) {
      messages.push(message);
    }
  } catch (error) {
    if (!isDiscordThreadNotFoundError(error)) throw error;
    return [await serializeMessage(currentMessage)];
  }

  const currentIndex = messages.findIndex(
    (message) => message.id === currentMessage.id,
  );
  if (currentIndex >= 0) {
    messages[currentIndex] = currentMessage;
  } else {
    messages.push(currentMessage);
  }

  const serialized: DiscordbotApiMessage[] = [];
  for (const message of messages) {
    serialized.push(await serializeMessage(message));
  }
  return serialized;
}

// Discord analog of slackbotv2's isSlackThreadNotFoundError: the Discord
// adapter throws a NetworkError carrying the raw Discord API body, e.g.
// `Discord API error: 404 {"message": "Unknown Channel", "code": 10003}`.
// The JSON portion is parsed for the error code (serializer spacing varies);
// the substring checks remain as a fallback.
function isDiscordThreadNotFoundError(error: unknown): boolean {
  const code = discordApiErrorCode(error);
  if (code === 10003 || code === 10008) return true;
  if (!(error instanceof Error)) return false;
  return (
    error.message.includes("Unknown Channel") ||
    error.message.includes("Unknown Message") ||
    error.message.includes('"code": 10003')
  );
}

// Discord delta (no slackbotv2 analog): a 403 while reading channel history
// (50001 Missing Access / 50013 Missing Permissions) is NOT a thread-not-found
// and previously propagated with total user silence; callers surface it.
export function isDiscordPermissionError(error: unknown): boolean {
  const code = discordApiErrorCode(error);
  if (code === 50001 || code === 50013) return true;
  if (!(error instanceof Error)) return false;
  return (
    error.message.includes("Missing Access") ||
    error.message.includes("Missing Permissions")
  );
}

/** Best-effort extraction of the Discord error `code` from an adapter error message. */
function discordApiErrorCode(error: unknown): number | undefined {
  if (!(error instanceof Error)) return undefined;
  const jsonStart = error.message.indexOf("{");
  if (jsonStart === -1) return undefined;
  try {
    const payload: unknown = JSON.parse(error.message.slice(jsonStart));
    if (isJsonObject(payload) && typeof payload.code === "number") {
      return payload.code;
    }
  } catch {
    // Not a JSON body; fall back to substring checks.
  }
  return undefined;
}

// Discord delta (no slackbotv2 analog): sticker-only/forwarded/poll/system
// mentions serialize to empty text with no attachments; executing them would
// fabricate a synthetic "continue" turn. Callers skip execution and react ❓.
export function isContentlessApiMessage(
  message: DiscordbotApiMessage,
): boolean {
  return message.text.trim() === "" && message.attachments.length === 0;
}

export async function serializeMessage(
  message: Message,
): Promise<DiscordbotApiMessage> {
  const attachments: DiscordbotApiAttachment[] = [];
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
    raw: message.raw,
    // Discord delta: webhook-style messages (Sentry alerts etc.) carry their
    // payload in embeds, which the chat adapter drops from `text`.
    text: withDiscordEmbedText(message.text, message.raw),
    threadId: message.threadId,
    timestamp: message.metadata.dateSent.toISOString(),
  };
}

// Note: on Discord the execute/openStream tail below is dead code — the live
// path always calls with `executeMessage: undefined` and runs the execute via
// `executeSessionTurn` inside the render stream (after the 👀 reaction lands).
// The tail is kept verbatim so 3-way syncs against slackbotv2 diff cleanly.
export async function forwardToSessionApi(
  options: DiscordbotOptions,
  input: ForwardSessionInput,
  callbacks: ForwardSessionApiCallbacks = {},
): Promise<AsyncIterable<DiscordbotRendererSource> | null> {
  const createStartedAtMs = nowMs();
  await createSession(options, input.threadId, input.conversationName);
  traceLog(options, "discordbot_session_create_complete", input.trace, {
    phase_ms: elapsedMs(createStartedAtMs),
  });
  if (input.messages.length > 0) {
    const appendStartedAtMs = nowMs();
    await appendSessionMessages(options, input.threadId, input.messages);
    traceLog(options, "discordbot_session_append_complete", input.trace, {
      message_count: input.messages.length,
      phase_ms: elapsedMs(appendStartedAtMs),
    });
    await callbacks.onMessagesAppended?.();
  } else {
    traceLog(options, "discordbot_session_append_skipped", input.trace, {
      message_count: 0,
    });
  }
  if (!input.executeMessage) return null;

  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadId,
    input.executeMessage,
  );
  traceLog(options, "discordbot_session_execute_complete", input.trace, {
    execution_id: execution.execution_id,
    phase_ms: elapsedMs(executeStartedAtMs),
  });
  await callbacks.onExecutionStarted?.(execution);
  if (!input.openStream) return null;

  return openSessionEventStream(options, input);
}

/**
 * Execute the session turn on its own (start the agent run), returning the
 * execution. Split out of forwardToSessionApi so the render stream can run it
 * AFTER the 👀 working reaction lands — the execute call blocks on cold
 * sandbox spin-up. Idempotent via the request's idempotency_key, so a render
 * retry won't re-spawn the sandbox.
 */
export async function executeSessionTurn(
  options: DiscordbotOptions,
  input: ForwardSessionInput,
): Promise<DiscordbotExecuteSessionResponse | null> {
  if (!input.executeMessage) return null;
  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadId,
    input.executeMessage,
  );
  traceLog(options, "discordbot_session_execute_complete", input.trace, {
    execution_id: execution.execution_id,
    phase_ms: elapsedMs(executeStartedAtMs),
  });
  return execution;
}

export async function openSessionEventStream(
  options: DiscordbotOptions,
  input: Pick<
    ForwardSessionInput,
    "afterEventId" | "executionId" | "onEventId" | "threadId" | "trace"
  >,
): Promise<AsyncIterable<DiscordbotRendererSource>> {
  const streamStartedAtMs = nowMs();
  const stream = await streamSessionNotifications(
    options,
    input.threadId,
    input.afterEventId,
    input.executionId,
    input.onEventId,
  );
  traceLog(options, "discordbot_session_events_opened", input.trace, {
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
      turnId: "discordbot-starting-turn",
      startedAtMs: Date.now(),
      item: {
        id: "discordbot-starting",
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

/** Largest attachment we are willing to buffer in memory and inline as base64. */
export const MAX_INLINE_ATTACHMENT_BYTES = 100 * 1024 * 1024;

async function serializeAttachment(
  attachment: Attachment,
): Promise<DiscordbotApiAttachment> {
  const serialized: DiscordbotApiAttachment = {
    fetchMetadata: attachment.fetchMetadata,
    height: attachment.height,
    mimeType: attachment.mimeType,
    name: attachment.name,
    size: attachment.size,
    type: attachment.type,
    url: attachment.url,
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
      // Re-check the actual byte count: Discord size metadata can be absent.
      const byteLength = Buffer.isBuffer(data) ? data.length : data.size;
      if (byteLength > MAX_INLINE_ATTACHMENT_BYTES) {
        serialized.fetchError = attachmentTooLargeError(byteLength);
        return serialized;
      }
      serialized.dataBase64 = await bytesToBase64(data);
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

async function bytesToBase64(data: Buffer | Blob): Promise<string> {
  if (Buffer.isBuffer(data)) return data.toString("base64");
  const bytes = await data.arrayBuffer();
  return Buffer.from(bytes).toString("base64");
}

async function createSession(
  options: DiscordbotOptions,
  threadId: string,
  conversationName?: string,
): Promise<void> {
  const fetchFn = options.fetch ?? fetch;
  const name = conversationName?.trim();
  const body: DiscordbotCreateSessionRequest = {
    harness_type: "codex",
    metadata: {
      source: "discordbot",
      platform: "discord",
      thread_id: threadId,
      // api-rs reads this as the session principal's display name.
      ...(name ? { discord_conversation_name: name } : {}),
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
  options: DiscordbotOptions,
  threadId: string,
  messages: DiscordbotApiMessage[],
): Promise<void> {
  const fetchFn = options.fetch ?? fetch;
  const body: DiscordbotAppendMessagesRequest = {
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
  options: DiscordbotOptions,
  threadId: string,
  message: DiscordbotApiMessage,
): Promise<DiscordbotExecuteSessionResponse> {
  const fetchFn = options.fetch ?? fetch;
  const body: DiscordbotExecuteSessionRequest = {
    idempotency_key: message.id,
    metadata: sessionMetadata(message, { action: "execute" }),
    input_lines: [toCodexInputLine(message, threadId)],
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
  return (await response.json()) as DiscordbotExecuteSessionResponse;
}

async function ensureApiOk(
  response: Response,
  action: string,
  options: DiscordbotOptions,
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
  // generic — it is surfaced verbatim into the user-facing Discord thread via sessionStreamError.
  if (body) {
    (options.logger ?? noopLogger).warn("discordbot_session_api_error", {
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
  options: DiscordbotOptions,
  threadId: string,
  afterEventId: number,
  executionId: string | undefined,
  onEventId: (eventId: number) => void,
): Promise<AsyncIterable<DiscordbotRendererSource>> {
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

function apiHeaders(options: DiscordbotOptions, jsonBody = true): HeadersInit {
  const apiKey = options.apiKey ?? process.env.DISCORDBOT_API_KEY;
  return {
    ...(jsonBody ? { "content-type": "application/json" } : {}),
    ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {}),
  };
}

function toSessionMessage(
  message: DiscordbotApiMessage,
): DiscordbotSessionMessage {
  return {
    client_message_id: message.id,
    role: message.author.isMe ? "assistant" : "user",
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message),
  };
}

function sessionMessageParts(message: DiscordbotApiMessage): JsonValue[] {
  const parts: JsonValue[] = [];
  if (message.text.trim()) {
    parts.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    parts.push({
      ...attachment,
      attachment_type: attachment.type,
      type: "attachment",
    });
  }
  return parts.length > 0 ? parts : [{ type: "text", text: "" }];
}

function sessionMetadata(
  message: DiscordbotApiMessage,
  extra: JsonObject = {},
): JsonObject {
  return {
    source: "discordbot",
    platform: "discord",
    message_id: message.id,
    thread_id: message.threadId,
    is_mention: message.isMention,
    timestamp: message.timestamp,
    user_id: message.author.userId,
    user_name: message.author.userName,
    ...extra,
  };
}

function toCodexInputLine(
  message: DiscordbotApiMessage,
  threadId: string,
): string {
  return JSON.stringify({
    type: "user",
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: "execute" }),
    message: {
      role: "user",
      content: codexInputContent(message),
    },
  });
}

function codexInputContent(message: DiscordbotApiMessage): JsonValue[] {
  const content: JsonValue[] = [];
  if (message.text.trim()) {
    content.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment));
  }
  return content.length > 0 ? content : [{ type: "text", text: "continue" }];
}

function codexAttachmentInput(attachment: DiscordbotApiAttachment): JsonValue {
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
  return {
    type: "text",
    text: attachmentDescription(attachment),
  };
}

function attachmentDescription(attachment: DiscordbotApiAttachment): string {
  const fields = [
    `name=${attachment.name ?? "attachment"}`,
    `type=${attachment.type}`,
    attachment.mimeType ? `mime=${attachment.mimeType}` : undefined,
    attachment.url ? `url=${attachment.url}` : undefined,
    attachment.dataBase64 ? `base64=${attachment.dataBase64}` : undefined,
    attachment.fetchError ? `fetch_error=${attachment.fetchError}` : undefined,
  ].filter(Boolean);
  return `[Discord attachment: ${fields.join(" ")}]`;
}

type ParsedSessionEvent = {
  data: string;
  event?: string;
  id?: number;
};

async function* parseSessionEventStream(
  stream: ReadableStream<Uint8Array>,
  onEventId: (eventId: number) => void,
): AsyncIterable<DiscordbotRendererSource> {
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

  // Discord delta (no slackbotv2 analog): the consumer returns early on
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
