import {
  SpanKind,
  SpanStatusCode,
  context,
  propagation,
  trace,
  type Context as OtelContext,
  type Span,
  type SpanOptions
} from '@opentelemetry/api'
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto'
import { resourceFromAttributes } from '@opentelemetry/resources'
import { BatchSpanProcessor } from '@opentelemetry/sdk-trace-base'
import { NodeTracerProvider } from '@opentelemetry/sdk-trace-node'
import { ATTR_SERVICE_NAME } from '@opentelemetry/semantic-conventions'

type AttributeValue = string | number | boolean | string[] | number[] | boolean[]
type Attributes = Record<string, AttributeValue | null | undefined>

let configured = false
let provider: NodeTracerProvider | null = null

export function configureOtel(): void {
  if (configured) return
  configured = true

  if ((process.env.OTEL_TRACES_EXPORTER ?? '').trim().toLowerCase() === 'none') return

  const endpoint = traceEndpoint()
  if (!endpoint) return

  provider = new NodeTracerProvider({
    resource: resourceFromAttributes({
      [ATTR_SERVICE_NAME]: process.env.OTEL_SERVICE_NAME || 'centaur-slackbot',
      ...resourceAttributes()
    }),
    spanProcessors: [
      new BatchSpanProcessor(
        new OTLPTraceExporter({
          url: endpoint,
          headers: otlpHeaders()
        })
      )
    ]
  })
  provider.register()
}

export async function shutdownOtel(): Promise<void> {
  await provider?.shutdown()
  provider = null
  configured = false
}

export const tracer = trace.getTracer('centaur-slackbot')

export async function withSpan<T>(
  name: string,
  options: SpanOptions | undefined,
  fn: (span: Span) => Promise<T>
): Promise<T> {
  return tracer.startActiveSpan(name, options ?? {}, async span => {
    try {
      const result = await fn(span)
      return result
    } catch (error) {
      recordSpanError(span, error)
      throw error
    } finally {
      span.end()
    }
  })
}

export function spanAttributes(span: Span, attrs: Attributes): void {
  for (const [key, value] of Object.entries(attrs)) {
    if (value !== undefined && value !== null) span.setAttribute(key, value)
  }
}

export function activeSpanAttributes(attrs: Attributes): void {
  const span = trace.getActiveSpan()
  if (span) spanAttributes(span, attrs)
}

export function recordSpanError(span: Span, error: unknown): void {
  span.recordException(error instanceof Error ? error : String(error))
  span.setStatus({
    code: SpanStatusCode.ERROR,
    message: error instanceof Error ? error.message : String(error)
  })
}

export function injectTraceHeaders(
  carrier: Record<string, string> = {},
  ctx: OtelContext = context.active()
): Record<string, string> {
  propagation.inject(ctx, carrier, {
    set(target, key, value) {
      target[key] = value
    }
  })
  const spanContext = trace.getSpanContext(ctx)
  if (spanContext?.traceId && !carrier['X-Trace-Id']) carrier['X-Trace-Id'] = spanContext.traceId
  return carrier
}

export function extractTraceContext(headers: Record<string, string>): OtelContext {
  return propagation.extract(context.active(), headers, {
    get(carrier, key) {
      return carrier[key]
    },
    keys(carrier) {
      return Object.keys(carrier)
    }
  })
}

export function withTraceHeaders<T>(
  headers: Record<string, string>,
  fn: () => Promise<T>
): Promise<T> {
  return context.with(extractTraceContext(headers), fn)
}

export function clientSpanOptions(attrs?: Attributes): SpanOptions {
  return { kind: SpanKind.CLIENT, attributes: compactAttributes(attrs ?? {}) }
}

export function serverSpanOptions(attrs?: Attributes): SpanOptions {
  return { kind: SpanKind.SERVER, attributes: compactAttributes(attrs ?? {}) }
}

export function internalSpanOptions(attrs?: Attributes): SpanOptions {
  return { kind: SpanKind.INTERNAL, attributes: compactAttributes(attrs ?? {}) }
}

function compactAttributes(attrs: Attributes): Record<string, AttributeValue> {
  const compacted: Record<string, AttributeValue> = {}
  for (const [key, value] of Object.entries(attrs)) {
    if (value !== undefined && value !== null) compacted[key] = value
  }
  return compacted
}

function traceEndpoint(): string | null {
  const tracesEndpoint = process.env.OTEL_EXPORTER_OTLP_TRACES_ENDPOINT?.trim()
  if (tracesEndpoint) return tracesEndpoint

  const baseEndpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT?.trim()
  if (!baseEndpoint) return null
  return `${baseEndpoint.replace(/\/+$/, '')}/v1/traces`
}

function otlpHeaders(): Record<string, string> {
  const raw = process.env.OTEL_EXPORTER_OTLP_HEADERS?.trim()
  if (!raw) return {}
  const headers: Record<string, string> = {}
  for (const part of raw.split(',')) {
    const [rawKey, ...rawValue] = part.split('=')
    const key = rawKey?.trim()
    if (!key) continue
    headers[key] = decodeURIComponent(rawValue.join('=').trim())
  }
  return headers
}

function resourceAttributes(): Record<string, string> {
  const raw = process.env.OTEL_RESOURCE_ATTRIBUTES?.trim()
  if (!raw) return {}
  const attrs: Record<string, string> = {}
  for (const part of raw.split(',')) {
    const [rawKey, ...rawValue] = part.split('=')
    const key = rawKey?.trim()
    if (!key || key === ATTR_SERVICE_NAME) continue
    attrs[key] = decodeURIComponent(rawValue.join('=').trim())
  }
  return attrs
}
