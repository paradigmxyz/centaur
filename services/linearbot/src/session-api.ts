import type { RustSessionStreamEvent } from "@centaur/harness-events";
import type { Attachment, Message } from "chat";
import type {
  ForwardSessionInput,
  JsonObject,
  JsonValue,
  LinearbotApiAttachment,
  LinearbotApiMessage,
  LinearbotAppendMessagesRequest,
  LinearbotCreateSessionRequest,
  LinearbotExecuteSessionRequest,
  LinearbotExecuteSessionResponse,
  LinearbotOptions,
  LinearbotRendererSource,
  LinearbotSessionMessage,
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
    // which is surfaced verbatim into the user-facing Linear session.
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
    execution: LinearbotExecuteSessionResponse,
  ): Promise<void>;
  onMessagesAppended?(): Promise<void>;
  /**
   * Fires when session creation restarted the thread onto a new harness
   * (explicit --claude/--amp/--codex on a thread pinned to another harness).
   * Runs before append/execute, so the callback may set `input.contextPreamble`
   * to re-feed the issue + comment history to the fresh harness.
   */
  onSessionRestarted?(): Promise<void>;
};

export async function collectInitialContext(
  thread: { allMessages: AsyncIterable<Message> },
  currentMessage: Message,
): Promise<LinearbotApiMessage[]> {
  const messages: Message[] = [];
  try {
    for await (const message of thread.allMessages) {
      messages.push(message);
    }
  } catch (error) {
    if (!isLinearThreadNotFoundError(error)) throw error;
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

  const serialized: LinearbotApiMessage[] = [];
  for (const message of messages) {
    serialized.push(await serializeMessage(message));
  }
  return serialized;
}

// Linear analog of slackbotv2's isSlackThreadNotFoundError: the Linear SDK
// surfaces GraphQL failures for deleted/inaccessible entities as errors whose
// message carries "Entity not found" (and the adapter wraps some in
// AdapterError text mentioning the missing session/comment).
function isLinearThreadNotFoundError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return (
    error.message.includes("Entity not found") ||
    error.message.includes("entity not found") ||
    error.message.includes("Could not find referenced") ||
    error.message.includes("is missing a root comment")
  );
}

export async function serializeMessage(
  message: Message,
): Promise<LinearbotApiMessage> {
  const attachments: LinearbotApiAttachment[] = [];
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
    text: message.text,
    threadId: message.threadId,
    timestamp: message.metadata.dateSent.toISOString(),
  };
}

// Note: on Linear the execute/openStream tail below is dead code on the live
// path — index.ts always calls with `executeMessage: undefined` and runs the
// execute via `executeSessionTurn` inside the render stream (after the
// working-thought ack lands; the execute call blocks on cold sandbox
// spin-up). The tail is kept verbatim so 3-way syncs against slackbotv2 diff
// cleanly.
export async function forwardToSessionApi(
  options: LinearbotOptions,
  input: ForwardSessionInput,
  callbacks: ForwardSessionApiCallbacks = {},
): Promise<AsyncIterable<LinearbotRendererSource> | null> {
  const createStartedAtMs = nowMs();
  const created = await createSession(
    options,
    input.threadId,
    input.harnessType,
    input.conversationName,
  );
  traceLog(options, "linearbot_session_create_complete", input.trace, {
    harness_switched: created.harnessSwitched,
    phase_ms: elapsedMs(createStartedAtMs),
  });
  if (created.harnessSwitched) {
    await callbacks.onSessionRestarted?.();
  }
  if (input.messages.length > 0) {
    const appendStartedAtMs = nowMs();
    await appendSessionMessages(options, input.threadId, input.messages);
    traceLog(options, "linearbot_session_append_complete", input.trace, {
      message_count: input.messages.length,
      phase_ms: elapsedMs(appendStartedAtMs),
    });
    await callbacks.onMessagesAppended?.();
  } else {
    traceLog(options, "linearbot_session_append_skipped", input.trace, {
      message_count: 0,
    });
  }
  if (!input.executeMessage) return null;

  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadId,
    input.executeMessage,
    input.model,
    input.provider,
    input.contextPreamble,
  );
  traceLog(options, "linearbot_session_execute_complete", input.trace, {
    execution_id: execution.execution_id,
    phase_ms: elapsedMs(executeStartedAtMs),
  });
  await callbacks.onExecutionStarted?.(execution);
  if (!input.openStream) return null;

  return openSessionEventStream(options, input);
}

/**
 * Execute the session turn on its own (start the agent run), returning the
 * execution. Split out of forwardToSessionApi (mirrors discordbot) so the
 * render stream can run it AFTER the working ack lands — the execute call
 * blocks on cold sandbox spin-up. Idempotent via the request's
 * idempotency_key, so a render retry won't re-spawn.
 */
export async function executeSessionTurn(
  options: LinearbotOptions,
  input: ForwardSessionInput,
): Promise<LinearbotExecuteSessionResponse | null> {
  if (!input.executeMessage) return null;
  const executeStartedAtMs = nowMs();
  const execution = await executeSession(
    options,
    input.threadId,
    input.executeMessage,
    input.model,
    input.provider,
    input.contextPreamble,
  );
  traceLog(options, "linearbot_session_execute_complete", input.trace, {
    execution_id: execution.execution_id,
    phase_ms: elapsedMs(executeStartedAtMs),
  });
  return execution;
}

export async function openSessionEventStream(
  options: LinearbotOptions,
  input: Pick<
    ForwardSessionInput,
    "afterEventId" | "executionId" | "onEventId" | "threadId" | "trace"
  >,
): Promise<AsyncIterable<LinearbotRendererSource>> {
  const streamStartedAtMs = nowMs();
  const stream = await streamSessionNotifications(
    options,
    input.threadId,
    input.afterEventId,
    input.executionId,
    input.onEventId,
  );
  traceLog(options, "linearbot_session_events_opened", input.trace, {
    after_event_id: input.afterEventId,
    execution_id: input.executionId,
    phase_ms: elapsedMs(streamStartedAtMs),
  });
  return stream;
}

// Adopted from discordbot (slackbotv2 removed it): the synthetic starting item
// primes the mapper's task state so answer deltas stream immediately instead
// of waiting out the pre-stream grace period.
export function startingStreamNotification(threadId: string): JsonObject {
  return {
    method: "item/started",
    params: {
      threadId,
      turnId: "linearbot-starting-turn",
      startedAtMs: Date.now(),
      item: {
        id: "linearbot-starting",
        memoryCitation: null,
        phase: "commentary",
        text: "",
        type: "agentMessage",
      },
    },
  };
}

const RESTART_CONTEXT_MAX_CHARS = 24_000;

/**
 * Transcript of the Linear issue thread, fed to a freshly restarted harness as
 * a context preamble (the old harness's conversation state dies with its
 * sandbox). The current message is excluded — it rides in the same input line
 * as the actual user turn. The synthetic issue-context message (author
 * "Linear") is part of the history, so the new harness still sees the issue.
 */
export function harnessRestartPreamble(
  history: LinearbotApiMessage[],
  currentMessageId: string,
): string | undefined {
  const lines: string[] = [];
  for (const item of history) {
    if (item.id === currentMessageId) continue;
    const text = item.text.trim();
    if (!text) continue;
    const author = item.author.isMe
      ? "assistant"
      : item.author.userName || item.author.fullName || "user";
    lines.push(`[${author}]: ${text}`);
  }
  if (lines.length === 0) return undefined;
  let transcript = lines.join("\n");
  if (transcript.length > RESTART_CONTEXT_MAX_CHARS) {
    transcript = `…(earlier messages truncated)\n${transcript.slice(-RESTART_CONTEXT_MAX_CHARS)}`;
  }
  return (
    "This Linear issue thread was just restarted on a different agent harness, " +
    "so the previous agent's working state is gone. Transcript of the thread so " +
    `far, for context:\n${transcript}`
  );
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
const MAX_CODEX_INPUT_LINE_CHARS = 900 * 1024;
const STAGED_ATTACHMENT_CHUNK_CHARS = 700 * 1024;

async function serializeAttachment(
  attachment: Attachment,
): Promise<LinearbotApiAttachment> {
  const serialized: LinearbotApiAttachment = {
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
      // Re-check the actual byte count: size metadata can be absent.
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

const DEFAULT_HARNESS_TYPE = "codex";

type CreateSessionOutcome = {
  /** The API restarted the thread onto the requested harness. */
  harnessSwitched: boolean;
};

async function createSession(
  options: LinearbotOptions,
  threadId: string,
  harnessType?: string,
  conversationName?: string,
): Promise<CreateSessionOutcome> {
  const requested =
    harnessType ?? options.defaultHarnessType ?? DEFAULT_HARNESS_TYPE;
  // An explicit --claude/--amp/--codex restarts a thread pinned to another
  // harness; the implicit default never forces a switch.
  const response = await postCreateSession(
    options,
    threadId,
    requested,
    harnessType ? "restart" : undefined,
    conversationName,
  );
  if (response.ok) {
    return { harnessSwitched: await harnessSwitchedFromResponse(response) };
  }

  let body = "";
  try {
    body = await response.text();
  } catch {
    body = "";
  }
  // A thread is pinned to the harness it was created with; the API rejects a
  // differing harness_type with 409. A plain message on a thread created with a
  // non-default harness lands here: keep the thread alive on its existing
  // harness instead of failing the message.
  const existing =
    response.status === 409 ? existingHarnessFromConflict(body) : undefined;
  if (existing && existing !== requested) {
    const retry = await postCreateSession(
      options,
      threadId,
      existing,
      undefined,
      conversationName,
    );
    await ensureApiOk(retry, "create session", options);
    return { harnessSwitched: false };
  }
  logApiErrorBody(options, "create session", response, body);
  throw new SessionApiError({
    action: "create session",
    body,
    retryable: isRetryableApiStatus(response.status),
    status: response.status,
    statusText: response.statusText,
  });
}

async function postCreateSession(
  options: LinearbotOptions,
  threadId: string,
  harnessType: string,
  onHarnessConflict?: "reject" | "restart",
  conversationName?: string,
): Promise<Response> {
  const fetchFn = options.fetch ?? fetch;
  const name = conversationName?.trim();
  const body: LinearbotCreateSessionRequest = {
    harness_type: harnessType,
    metadata: {
      source: "linearbot",
      platform: "linear",
      thread_id: threadId,
      // api-rs reads this as the session principal's display name.
      ...(name ? { linear_conversation_name: name } : {}),
    },
    ...(onHarnessConflict ? { on_harness_conflict: onHarnessConflict } : {}),
  };
  return fetchFn(apiSessionUrl(options.apiUrl, threadId), {
    method: "POST",
    headers: apiHeaders(options),
    body: JSON.stringify(body),
  });
}

async function harnessSwitchedFromResponse(
  response: Response,
): Promise<boolean> {
  try {
    const payload = await response.json();
    return isJsonObject(payload) && payload.harness_switched === true;
  } catch {
    return false;
  }
}

function existingHarnessFromConflict(body: string): string | undefined {
  try {
    const payload = JSON.parse(body);
    if (isJsonObject(payload)) {
      const existing = stringValue(payload.existing_harness);
      if (existing) return existing;
    }
  } catch {
    // fall through to message parsing
  }
  return /already exists with harness_type ([A-Za-z0-9_-]+)/.exec(body)?.[1];
}

async function appendSessionMessages(
  options: LinearbotOptions,
  threadId: string,
  messages: LinearbotApiMessage[],
): Promise<void> {
  const fetchFn = options.fetch ?? fetch;
  const body: LinearbotAppendMessagesRequest = {
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
  options: LinearbotOptions,
  threadId: string,
  message: LinearbotApiMessage,
  model?: string,
  provider?: string,
  contextPreamble?: string,
): Promise<LinearbotExecuteSessionResponse> {
  const fetchFn = options.fetch ?? fetch;
  const body: LinearbotExecuteSessionRequest = {
    idempotency_key: message.id,
    metadata: sessionMetadata(message, { action: "execute" }),
    input_lines: toCodexInputLines(message, threadId, model, provider, contextPreamble),
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
  return (await response.json()) as LinearbotExecuteSessionResponse;
}

async function ensureApiOk(
  response: Response,
  action: string,
  options: LinearbotOptions,
): Promise<void> {
  if (response.ok) return;
  let body = "";
  try {
    body = await response.text();
  } catch {
    body = "";
  }
  logApiErrorBody(options, action, response, body);
  throw new SessionApiError({
    action,
    body,
    retryable: isRetryableApiStatus(response.status),
    status: response.status,
    statusText: response.statusText,
  });
}

// api-rs error bodies can carry stack traces, internal hostnames, or echoed
// payloads. Log the full body server-side; the thrown message stays generic —
// it is surfaced verbatim into the user-facing Linear session.
function logApiErrorBody(
  options: LinearbotOptions,
  action: string,
  response: Response,
  body: string,
): void {
  if (!body) return;
  (options.logger ?? noopLogger).warn("linearbot_session_api_error", {
    action,
    status: response.status,
    status_text: response.statusText,
    body,
  });
}

function isRetryableApiStatus(status: number): boolean {
  return status === 408 || status === 425 || status === 429 || status >= 500;
}

async function streamSessionNotifications(
  options: LinearbotOptions,
  threadId: string,
  afterEventId: number,
  executionId: string | undefined,
  onEventId: (eventId: number) => void,
): Promise<AsyncIterable<LinearbotRendererSource>> {
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

function apiHeaders(options: LinearbotOptions, jsonBody = true): HeadersInit {
  const apiKey =
    options.apiKey ??
    process.env.LINEARBOT_API_KEY ??
    process.env.CENTAUR_API_KEY;
  return {
    ...(jsonBody ? { "content-type": "application/json" } : {}),
    ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {}),
  };
}

function toSessionMessage(
  message: LinearbotApiMessage,
): LinearbotSessionMessage {
  return {
    client_message_id: message.id,
    role: message.author.isMe ? "assistant" : "user",
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message),
  };
}

function sessionMessageParts(message: LinearbotApiMessage): JsonValue[] {
  const parts: JsonValue[] = [];
  if (message.text.trim()) {
    parts.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    parts.push(sessionAttachmentPart(attachment));
  }
  return parts.length > 0 ? parts : [{ type: "text", text: "" }];
}

function sessionAttachmentPart(attachment: LinearbotApiAttachment): JsonObject {
  const part: JsonObject = {
    ...attachment,
    attachment_type: attachment.type,
    type: "attachment",
  };
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
  message: LinearbotApiMessage,
  extra: JsonObject = {},
): JsonObject {
  return {
    source: "linearbot",
    platform: "linear",
    message_id: message.id,
    thread_id: message.threadId,
    is_mention: message.isMention,
    timestamp: message.timestamp,
    user_id: message.author.userId,
    user_name: message.author.userName,
    ...extra,
  };
}

function toCodexInputLines(
  message: LinearbotApiMessage,
  threadId: string,
  model?: string,
  provider?: string,
  contextPreamble?: string,
): string[] {
  const staged = new Map<LinearbotApiAttachment, string>();
  const lines: string[] = [];
  for (const attachment of message.attachments) {
    if (!attachment.dataBase64) continue;
    const inlineLine = toCodexInputLineWithStaged(
      message,
      threadId,
      staged,
      model,
      provider,
      contextPreamble,
    );
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
  lines.push(
    toCodexInputLineWithStaged(
      message,
      threadId,
      staged,
      model,
      provider,
      contextPreamble,
    ),
  );
  return lines;
}

function toCodexInputLineWithStaged(
  message: LinearbotApiMessage,
  threadId: string,
  staged: Map<LinearbotApiAttachment, string>,
  model?: string,
  provider?: string,
  contextPreamble?: string,
): string {
  return JSON.stringify({
    type: "user",
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: "execute" }),
    ...(model ? { model } : {}),
    ...(provider ? { provider } : {}),
    message: {
      role: "user",
      content: codexInputContent(message, staged, contextPreamble),
    },
  });
}

function stagedAttachmentInputLines(
  attachment: LinearbotApiAttachment,
  stagedAttachmentId: string,
): string[] {
  const dataBase64 = attachment.dataBase64;
  if (!dataBase64) return [];
  const lines: string[] = [];
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
  message: LinearbotApiMessage,
  staged: Map<LinearbotApiAttachment, string> = new Map(),
  contextPreamble?: string,
): JsonValue[] {
  const content: JsonValue[] = [];
  // On a harness restart the fresh sandbox has no conversation state, so the
  // thread transcript rides in front of this turn's text (see
  // harnessRestartPreamble); on the normal path the issue context arrives as
  // its own prepended session message instead.
  if (contextPreamble?.trim()) {
    content.push({ type: "text", text: contextPreamble });
  }
  if (message.text.trim()) {
    content.push({ type: "text", text: message.text });
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment, staged.get(attachment)));
  }
  return content.length > 0 ? content : [{ type: "text", text: "continue" }];
}

function codexAttachmentInput(
  attachment: LinearbotApiAttachment,
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

function attachmentDescription(attachment: LinearbotApiAttachment): string {
  const fields = [
    `name=${attachment.name ?? "attachment"}`,
    `type=${attachment.type}`,
    attachment.mimeType ? `mime=${attachment.mimeType}` : undefined,
    attachment.url ? `url=${attachment.url}` : undefined,
    attachment.dataBase64Omitted
      ? `content=${attachment.dataBase64Omitted}`
      : undefined,
    attachment.fetchError ? `fetch_error=${attachment.fetchError}` : undefined,
  ].filter(Boolean);
  return `[Linear attachment: ${fields.join(" ")}]`;
}

type ParsedSessionEvent = {
  data: string;
  event?: string;
  id?: number;
};

async function* parseSessionEventStream(
  stream: ReadableStream<Uint8Array>,
  onEventId: (eventId: number) => void,
): AsyncIterable<LinearbotRendererSource> {
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

  // Adopted from discordbot: the consumer returns early on terminal events,
  // abandoning this generator at a yield point. Without the finally, the
  // reader lock is never released and the HTTP response body is never
  // cancelled, leaking the SSE connection on every completed run.
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
