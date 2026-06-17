import type { Logger } from 'chat'
import type { JsonObject, SlackbotV2Options, SlackbotV2Trace } from './types'

const PENDING_OPERATION_WARN_AFTER_MS = 5_000
const PENDING_OPERATION_REPEAT_MS = 30_000
export type TraceLogLevel = 'debug' | 'info' | 'warn' | 'error'

export const noopLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => noopLogger
}

export function nowMs(): number {
  return globalThis.performance?.now?.() ?? Date.now()
}

export function elapsedMs(startedAtMs: number): number {
  return Math.max(0, Math.round(nowMs() - startedAtMs))
}

export function traceLog(
  options: SlackbotV2Options,
  event: string,
  trace?: SlackbotV2Trace,
  fields: JsonObject = {},
  level: TraceLogLevel = 'info'
): void {
  const logger = options.logger ?? noopLogger
  logger[level](event, {
    ...traceFields(trace),
    ...fields
  })
}

export function traceWarn(
  options: SlackbotV2Options,
  event: string,
  trace?: SlackbotV2Trace,
  fields: JsonObject = {}
): void {
  traceLog(options, event, trace, fields, 'warn')
}

export function startPendingOperationLog(
  options: SlackbotV2Options,
  event: string,
  trace?: SlackbotV2Trace,
  fields: JsonObject = {},
  startedAtMs: number = nowMs(),
  warnAfterMs = PENDING_OPERATION_WARN_AFTER_MS,
  repeatEveryMs = PENDING_OPERATION_REPEAT_MS
): () => void {
  let stopped = false
  let timer: ReturnType<typeof globalThis.setTimeout> | undefined
  const emit = () => {
    if (stopped) return
    traceWarn(options, event, trace, {
      ...fields,
      pending_ms: elapsedMs(startedAtMs)
    })
    timer = schedulePendingLog(emit, repeatEveryMs)
  }
  timer = schedulePendingLog(emit, warnAfterMs)
  return () => {
    stopped = true
    if (timer !== undefined) globalThis.clearTimeout(timer)
  }
}

function traceFields(trace?: SlackbotV2Trace): JsonObject {
  return trace
    ? {
        elapsed_ms: elapsedMs(trace.startedAtMs),
        include_context: trace.includeContext,
        message_id: trace.messageId,
        mode: trace.mode,
        open_stream: trace.openStream,
        thread_id: trace.threadId
      }
    : {}
}

function schedulePendingLog(
  fn: () => void,
  delayMs: number
): ReturnType<typeof globalThis.setTimeout> {
  const timer = globalThis.setTimeout(fn, delayMs)
  const unref = (timer as { unref?: () => void }).unref
  if (unref) unref.call(timer)
  return timer
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}

export function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

export function isJsonObject(value: unknown): value is JsonObject {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

export async function* toAsyncIterable<T>(source: Iterable<T>): AsyncIterable<T> {
  for await (const item of source) {
    yield item
  }
}
