import type { Logger } from "chat";
import type { JsonObject, LinearbotOptions, LinearbotTrace } from "./types";

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
  options: LinearbotOptions,
  event: string,
  trace?: LinearbotTrace,
  fields: JsonObject = {},
): void {
  const logger = options.logger ?? noopLogger;
  logger.info(event, {
    ...(trace
      ? {
          elapsed_ms: elapsedMs(trace.startedAtMs),
          include_context: trace.includeContext,
          message_id: trace.messageId,
          mode: trace.mode,
          open_stream: trace.openStream,
          thread_id: trace.threadId,
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
