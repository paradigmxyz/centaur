import type { JsonObject, Logger, TelegrambotTrace } from "./types";

export const noopLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => noopLogger,
};

export function nowMs(): number {
  return globalThis.performance?.now?.() ?? Date.now();
}

export function elapsedMs(startedAtMs: number): number {
  return Math.max(0, Math.round(nowMs() - startedAtMs));
}

export function traceLog(
  logger: Logger | undefined,
  event: string,
  trace?: TelegrambotTrace,
  fields: JsonObject = {},
): void {
  (logger ?? noopLogger).info(event, {
    ...(trace
      ? {
          elapsed_ms: elapsedMs(trace.startedAtMs),
          message_id: trace.messageId,
          mode: trace.mode,
          open_stream: trace.openStream,
          thread_key: trace.threadKey,
        }
      : {}),
    ...fields,
  });
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

export function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

export function isJsonObject(value: unknown): value is JsonObject {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

export async function* toAsyncIterable<T>(
  source: Iterable<T>,
): AsyncIterable<T> {
  for await (const item of source) {
    yield item;
  }
}

// Telegram delta (same motivation as the Discord port): Telegram counts
// message length in UTF-16 code units and rejects payloads that cut a
// surrogate pair in half, so every truncation of user-visible text must back
// off the cut when it lands mid-pair.
/** Surrogate-safe prefix: never cuts between a UTF-16 surrogate pair. */
export function sliceSurrogateSafe(value: string, maxUnits: number): string {
  if (maxUnits <= 0) return "";
  if (value.length <= maxUnits) return value;
  const tail = value.charCodeAt(maxUnits - 1);
  const end = tail >= 0xd800 && tail <= 0xdbff ? maxUnits - 1 : maxUnits;
  return value.slice(0, end);
}

/**
 * Single-consumer async queue bridging a producer loop to an AsyncIterable
 * consumer (e.g. the answer streamer). push() never blocks; end() lets the
 * consumer drain the remaining items and finish.
 */
export class AsyncTextQueue implements AsyncIterable<string> {
  private readonly values: string[] = [];
  private done = false;
  private wake: (() => void) | null = null;

  push(value: string): void {
    this.values.push(value);
    this.wake?.();
  }

  end(): void {
    this.done = true;
    this.wake?.();
  }

  async *[Symbol.asyncIterator](): AsyncIterator<string> {
    while (true) {
      const value = this.values.shift();
      if (value !== undefined) {
        yield value;
        continue;
      }
      if (this.done) return;
      await new Promise<void>((resolve) => {
        this.wake = () => {
          this.wake = null;
          resolve();
        };
      });
    }
  }
}
