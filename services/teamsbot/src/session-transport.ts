export type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export type SessionStreamEvent = {
  data: unknown;
  event: string;
  eventId?: number;
  eventKind: string;
};

export type RequestRetryEvent = {
  action: string;
  attempt: number;
  maxRetries: number;
  message: string;
};

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
    super(`Centaur session ${input.action} failed: ${input.status} ${input.statusText}`);
    this.name = "SessionApiError";
    this.action = input.action;
    this.body = input.body;
    this.retryable = input.retryable;
    this.status = input.status;
    this.statusText = input.statusText;
  }
}

export async function requestWithRetries<T>(input: {
  action: string;
  maxRetries?: number;
  onRetry?: (event: RequestRetryEvent) => void;
  operation: () => Promise<T>;
  retryDelayMs?: number;
}): Promise<T> {
  const maxRetries = input.maxRetries ?? 2;
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await input.operation();
    } catch (error) {
      if (attempt >= maxRetries || !isRetryableError(error)) {
        throw error;
      }
      const delayMs = input.retryDelayMs ?? 250;
      input.onRetry?.({
        action: input.action,
        attempt: attempt + 1,
        maxRetries,
        message: error instanceof Error ? error.message : String(error),
      });
      if (delayMs > 0) {
        await sleep(delayMs);
      }
    }
  }
}

export async function ensureSessionResponseOk(response: Response, action: string): Promise<void> {
  if (response.ok) {
    return;
  }
  let body = "";
  try {
    body = await response.text();
  } catch {
    body = "";
  }
  throw new SessionApiError({
    action,
    body,
    retryable: response.status === 408 || response.status === 425 || response.status === 429 || response.status >= 500,
    status: response.status,
    statusText: response.statusText,
  });
}

export async function streamSessionEvents(input: {
  afterEventId: number;
  apiUrl: string;
  executionId?: string;
  fetch: FetchLike;
  headers?: HeadersInit;
  maxRetries?: number;
  onRetry?: (event: RequestRetryEvent) => void;
  onEventId(eventId: number): void;
  retryDelayMs?: number;
  signal?: AbortSignal;
  threadId: string;
}): Promise<AsyncIterable<SessionStreamEvent>> {
  const url = new URL(apiSessionUrl(input.apiUrl, input.threadId, "events"));
  url.searchParams.set("after_event_id", String(input.afterEventId));
  if (input.executionId) {
    url.searchParams.set("execution_id", input.executionId);
  }
  return requestWithRetries({
    action: "stream events",
    maxRetries: input.maxRetries,
    onRetry: input.onRetry,
    retryDelayMs: input.retryDelayMs,
    operation: async () => {
      const response = await input.fetch(url.toString(), {
        method: "GET",
        headers: input.headers,
        signal: input.signal,
      });
      await ensureSessionResponseOk(response, "stream events");
      if (!response.body) {
        return toAsyncIterable([]);
      }
      return parseSessionEventStream(response.body, input.onEventId);
    },
  });
}

export function apiSessionUrl(apiUrl: string, threadId: string, suffix?: "messages" | "execute" | "events"): string {
  const path = `/api/session/${encodeURIComponent(threadId)}${suffix ? `/${suffix}` : ""}`;
  return new URL(path, apiUrl.endsWith("/") ? apiUrl : `${apiUrl}/`).toString();
}

export async function* parseSessionEventStream(
  stream: ReadableStream<Uint8Array>,
  onEventId: (eventId: number) => void,
): AsyncIterable<SessionStreamEvent> {
  for await (const event of parseSseEvents(stream)) {
    if (typeof event.id === "number") {
      onEventId(event.id);
    }
    if (event.event === "session.output.line") {
      yield {
        data: event.data,
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      };
      if (isTerminalCodexOutputLine(event.data)) {
        return;
      }
      continue;
    }
    if (event.event === "session.execution_failed" || event.event === "session.stream_error") {
      yield {
        data: { error: sessionErrorMessage(event) },
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      };
      return;
    }
    if (event.event === "session.execution_cancelled") {
      yield {
        data: { error: sessionErrorMessage(event, "Execution cancelled") },
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      };
      return;
    }
    if (event.event === "session.execution_completed") {
      yield {
        data: sessionEventData(event),
        event: event.event,
        eventId: event.id,
        eventKind: event.event,
      };
      return;
    }
    yield {
      data: event.data,
      event: event.event ?? "message",
      eventId: event.id,
      eventKind: event.event ?? "message",
    };
  }
}

async function* parseSseEvents(stream: ReadableStream<Uint8Array>): AsyncIterable<{
  data: string;
  event?: string;
  id?: number;
}> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let eventName: string | undefined;
  let eventId: number | undefined;
  let data: string[] = [];

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer += decoder.decode(value, { stream: true });
      let newlineIndex: number;
      while ((newlineIndex = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newlineIndex).replace(/\r$/, "");
        buffer = buffer.slice(newlineIndex + 1);
        if (line === "") {
          if (eventName || eventId !== undefined || data.length > 0) {
            yield { data: data.join("\n"), event: eventName, id: eventId };
          }
          eventName = undefined;
          eventId = undefined;
          data = [];
          continue;
        }
        if (line.startsWith("event:")) {
          eventName = line.slice("event:".length).trim();
        } else if (line.startsWith("id:")) {
          const parsed = Number.parseInt(line.slice("id:".length).trim(), 10);
          eventId = Number.isFinite(parsed) ? parsed : undefined;
        } else if (line.startsWith("data:")) {
          data.push(line.slice("data:".length).trimStart());
        }
      }
    }
    buffer += decoder.decode();
    const trailingLine = buffer.replace(/\r$/, "");
    if (trailingLine) {
      if (trailingLine.startsWith("event:")) {
        eventName = trailingLine.slice("event:".length).trim();
      } else if (trailingLine.startsWith("id:")) {
        const parsed = Number.parseInt(trailingLine.slice("id:".length).trim(), 10);
        eventId = Number.isFinite(parsed) ? parsed : undefined;
      } else if (trailingLine.startsWith("data:")) {
        data.push(trailingLine.slice("data:".length).trimStart());
      }
    }
    if (eventName || eventId !== undefined || data.length > 0) {
      yield { data: data.join("\n"), event: eventName, id: eventId };
    }
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function isTerminalCodexOutputLine(line: string): boolean {
  let payload: unknown;
  try {
    payload = JSON.parse(line);
  } catch {
    return false;
  }
  if (!isJsonObject(payload)) {
    return false;
  }
  return (
    payload.type === "turn.completed" ||
    payload.type === "turn.failed" ||
    payload.type === "turn.done" ||
    payload.method === "error" ||
    payload.method === "turn/completed"
  );
}

function sessionEventData(event: { data: string }): unknown {
  try {
    return JSON.parse(event.data);
  } catch {
    return event.data;
  }
}

function sessionErrorMessage(event: { data: string; event?: string }, fallback?: string): string {
  let message = fallback ?? `${event.event ?? "session error"}`;
  try {
    const payload = JSON.parse(event.data);
    if (isJsonObject(payload)) {
      message = stringValue(payload.error) ?? stringValue(payload.message) ?? message;
    }
  } catch {
    if (event.data.trim()) {
      message = event.data.trim();
    }
  }
  return message;
}

function isRetryableError(error: unknown): boolean {
  if (error instanceof SessionApiError) {
    return error.retryable;
  }
  return error instanceof TypeError || (error instanceof Error && error.name === "AbortError");
}

function toAsyncIterable<T>(items: T[]): AsyncIterable<T> {
  return {
    async *[Symbol.asyncIterator]() {
      yield* items;
    },
  };
}

function isJsonObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value ? value : undefined;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
