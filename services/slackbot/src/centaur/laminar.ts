import { randomUUID } from 'node:crypto'
import { Laminar, type LaminarSpanContext } from '@lmnr-ai/lmnr'

let initialized = false
let unavailable = false

export type CentaurTrace = {
  trace_id?: unknown
  thread_key?: unknown
  traceparent?: unknown
}

export async function withLaminarSpan<T>(
  name: string,
  trace: CentaurTrace | undefined,
  fn: () => Promise<T>
): Promise<T> {
  if (!initializeLaminar()) return fn()
  const traceId = normalizeUuid(trace?.trace_id)
  const threadKey = String(trace?.thread_key ?? '').trim()
  const span = Laminar.startActiveSpan({
    name,
    sessionId: traceId,
    parentSpanContext: traceId
      ? ({
          traceId,
          spanId: randomUUID(),
          isRemote: true,
          sessionId: traceId,
          metadata: {
            service: 'slackbot',
            trace_id: traceId,
            thread_key: threadKey
          }
        } as LaminarSpanContext)
      : undefined,
    metadata: {
      service: 'slackbot',
      ...(traceId ? { trace_id: traceId } : {}),
      ...(threadKey ? { thread_key: threadKey } : {})
    }
  })
  try {
    return await fn()
  } finally {
    span.end()
  }
}

function initializeLaminar(): boolean {
  if (initialized) return true
  if (unavailable) return false
  const projectApiKey = process.env.LMNR_PROJECT_API_KEY?.trim()
  if (!projectApiKey) return false
  try {
    Laminar.initialize({
      projectApiKey,
      baseUrl: process.env.LMNR_BASE_URL?.trim() || undefined,
      httpPort: optionalPort('LMNR_HTTP_PORT'),
      grpcPort: optionalPort('LMNR_GRPC_PORT'),
      metadata: {
        service: 'slackbot',
        environment:
          process.env.CENTAUR_ENVIRONMENT || process.env.DEPLOY_ENV || process.env.NODE_ENV || 'dev'
      },
      instrumentModules: {}
    })
    initialized = true
    return true
  } catch (error) {
    unavailable = true
    console.error('laminar_initialize_failed', error)
    return false
  }
}

function optionalPort(name: string): number | undefined {
  const value = process.env[name]?.trim()
  if (!value) return undefined
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined
}

function normalizeUuid(value: unknown): string | undefined {
  const raw = String(value ?? '').trim()
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(raw)
    ? raw.toLowerCase()
    : undefined
}
