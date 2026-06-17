type LabelValue = boolean | number | string | undefined
type Labels = Record<string, LabelValue>

type MetricType = 'counter' | 'gauge' | 'histogram'

type Sample = {
  labels: Record<string, string>
  value: number
}

type HistogramSample = {
  bucketCounts: number[]
  count: number
  labels: Record<string, string>
  sum: number
}

const DEFAULT_HISTOGRAM_BUCKETS = [
  0.005,
  0.01,
  0.025,
  0.05,
  0.1,
  0.25,
  0.5,
  1,
  2.5,
  5,
  10,
  30,
  60,
  120,
  300,
  600,
  900,
  1800
]

class Registry {
  private readonly metrics: Metric[] = []

  register(metric: Metric): void {
    this.metrics.push(metric)
  }

  render(): string {
    const lines: string[] = []
    for (const metric of this.metrics) {
      lines.push(`# HELP ${metric.name} ${escapeHelp(metric.help)}`)
      lines.push(`# TYPE ${metric.name} ${metric.type}`)
      lines.push(...metric.renderSamples())
    }
    lines.push('')
    return lines.join('\n')
  }

  reset(): void {
    for (const metric of this.metrics) metric.reset()
  }
}

abstract class Metric {
  readonly help: string
  readonly labelNames: readonly string[]
  readonly name: string
  readonly type: MetricType

  protected constructor(input: {
    help: string
    labelNames?: readonly string[]
    name: string
    type: MetricType
  }) {
    this.help = input.help
    this.labelNames = input.labelNames ?? []
    this.name = input.name
    this.type = input.type
  }

  abstract renderSamples(): string[]
  abstract reset(): void

  protected labelKey(labels: Labels = {}): { key: string; normalized: Record<string, string> } {
    const normalized: Record<string, string> = {}
    for (const name of this.labelNames) {
      normalized[name] = String(labels[name] ?? '')
    }
    return { key: JSON.stringify(normalized), normalized }
  }

  protected sampleLine(suffix: string, labels: Record<string, string>, value: number): string {
    return `${this.name}${suffix}${formatLabels(labels)} ${formatNumber(value)}`
  }
}

class Counter extends Metric {
  private readonly samples = new Map<string, Sample>()

  constructor(input: { help: string; labelNames?: readonly string[]; name: string }) {
    super({ ...input, type: 'counter' })
  }

  inc(labels: Labels = {}, value = 1): void {
    if (value < 0) throw new Error(`counter ${this.name} cannot be decreased`)
    const { key, normalized } = this.labelKey(labels)
    const sample = this.samples.get(key) ?? { labels: normalized, value: 0 }
    sample.value += value
    this.samples.set(key, sample)
  }

  renderSamples(): string[] {
    return Array.from(this.samples.values(), sample => this.sampleLine('', sample.labels, sample.value))
  }

  reset(): void {
    this.samples.clear()
  }
}

class Gauge extends Metric {
  private readonly samples = new Map<string, Sample>()

  constructor(input: { help: string; labelNames?: readonly string[]; name: string }) {
    super({ ...input, type: 'gauge' })
  }

  dec(labels: Labels = {}, value = 1): void {
    this.inc(labels, -value)
  }

  inc(labels: Labels = {}, value = 1): void {
    const current = this.get(labels)
    this.set(labels, current + value)
  }

  set(labels: Labels = {}, value: number): void {
    const { key, normalized } = this.labelKey(labels)
    this.samples.set(key, { labels: normalized, value })
  }

  renderSamples(): string[] {
    return Array.from(this.samples.values(), sample => this.sampleLine('', sample.labels, sample.value))
  }

  reset(): void {
    this.samples.clear()
  }

  private get(labels: Labels): number {
    const { key } = this.labelKey(labels)
    return this.samples.get(key)?.value ?? 0
  }
}

class Histogram extends Metric {
  private readonly buckets: readonly number[]
  private readonly samples = new Map<string, HistogramSample>()

  constructor(input: {
    buckets?: readonly number[]
    help: string
    labelNames?: readonly string[]
    name: string
  }) {
    super({ ...input, type: 'histogram' })
    this.buckets = input.buckets ?? DEFAULT_HISTOGRAM_BUCKETS
  }

  observe(labels: Labels = {}, value: number): void {
    if (!Number.isFinite(value)) return
    const { key, normalized } = this.labelKey(labels)
    const sample =
      this.samples.get(key) ??
      {
        bucketCounts: Array.from({ length: this.buckets.length }, () => 0),
        count: 0,
        labels: normalized,
        sum: 0
      }
    for (const [index, bucket] of this.buckets.entries()) {
      if (value <= bucket) sample.bucketCounts[index] = (sample.bucketCounts[index] ?? 0) + 1
    }
    sample.count += 1
    sample.sum += value
    this.samples.set(key, sample)
  }

  renderSamples(): string[] {
    const lines: string[] = []
    for (const sample of this.samples.values()) {
      for (const [index, bucket] of this.buckets.entries()) {
        lines.push(
          this.sampleLine('_bucket', { ...sample.labels, le: String(bucket) }, sample.bucketCounts[index] ?? 0)
        )
      }
      lines.push(this.sampleLine('_bucket', { ...sample.labels, le: '+Inf' }, sample.count))
      lines.push(this.sampleLine('_sum', sample.labels, sample.sum))
      lines.push(this.sampleLine('_count', sample.labels, sample.count))
    }
    return lines
  }

  reset(): void {
    this.samples.clear()
  }
}

const registry = new Registry()

function counter(input: { help: string; labelNames?: readonly string[]; name: string }): Counter {
  const metric = new Counter(input)
  registry.register(metric)
  return metric
}

function gauge(input: { help: string; labelNames?: readonly string[]; name: string }): Gauge {
  const metric = new Gauge(input)
  registry.register(metric)
  return metric
}

function histogram(input: {
  buckets?: readonly number[]
  help: string
  labelNames?: readonly string[]
  name: string
}): Histogram {
  const metric = new Histogram(input)
  registry.register(metric)
  return metric
}

export const slackbotMetrics = {
  activeLiveRenders: gauge({
    help: 'Number of live Slack render tasks currently running.',
    name: 'slackbotv2_active_live_renders'
  }),
  expose(): string {
    this.info.set({}, 1)
    return registry.render()
  },
  forwardDuration: histogram({
    help: 'Duration of Slack message forwarding into the session API, in seconds.',
    labelNames: ['mode', 'outcome'],
    name: 'slackbotv2_forward_duration_seconds'
  }),
  forwardMessages: counter({
    help: 'Slack messages processed by the forwarder.',
    labelNames: ['mode', 'outcome'],
    name: 'slackbotv2_forward_messages_total'
  }),
  info: gauge({
    help: 'Static Slackbot v2 service info.',
    name: 'slackbotv2_info'
  }),
  lastSuccessfulRenderTimestamp: gauge({
    help: 'Unix timestamp of the last successful Slack render completion.',
    labelNames: ['source'],
    name: 'slackbotv2_last_successful_render_timestamp_seconds'
  }),
  renderAttempts: counter({
    help: 'Slack render attempts by source and outcome.',
    labelNames: ['source', 'outcome'],
    name: 'slackbotv2_render_attempts_total'
  }),
  renderAttemptDuration: histogram({
    help: 'Slack render attempt duration, in seconds.',
    labelNames: ['source', 'outcome'],
    name: 'slackbotv2_render_attempt_duration_seconds'
  }),
  renderFallbacks: counter({
    help: 'Durable final-answer fallback render attempts by outcome.',
    labelNames: ['outcome'],
    name: 'slackbotv2_render_fallbacks_total'
  }),
  renderFallbackDuration: histogram({
    help: 'Durable final-answer fallback render duration, in seconds.',
    labelNames: ['outcome'],
    name: 'slackbotv2_render_fallback_duration_seconds'
  }),
  renderObligationsIndexed: counter({
    help: 'Render obligations written to the recovery index.',
    name: 'slackbotv2_render_obligations_indexed_total'
  }),
  renderRecoveryObligations: gauge({
    help: 'Render obligations observed during the latest recovery scan.',
    labelNames: ['state'],
    name: 'slackbotv2_render_recovery_obligations'
  }),
  renderRecoveryScans: counter({
    help: 'Render recovery scans by outcome.',
    labelNames: ['outcome'],
    name: 'slackbotv2_render_recovery_scans_total'
  }),
  renderRecoveryScanDuration: histogram({
    help: 'Render recovery scan duration, in seconds.',
    labelNames: ['outcome'],
    name: 'slackbotv2_render_recovery_scan_duration_seconds'
  }),
  renderRecoveryThreadEvents: counter({
    help: 'Per-thread render recovery events.',
    labelNames: ['event'],
    name: 'slackbotv2_render_recovery_thread_events_total'
  }),
  sessionApiOperationDuration: histogram({
    help: 'Session API operation duration from Slackbot, in seconds.',
    labelNames: ['operation', 'outcome'],
    name: 'slackbotv2_session_api_operation_duration_seconds'
  }),
  sessionApiOperations: counter({
    help: 'Session API operations initiated by Slackbot.',
    labelNames: ['operation', 'outcome'],
    name: 'slackbotv2_session_api_operations_total'
  }),
  webhookDuration: histogram({
    help: 'Slack webhook request handling duration, in seconds.',
    labelNames: ['route', 'event_type', 'outcome'],
    name: 'slackbotv2_slack_webhook_duration_seconds'
  }),
  webhookRequests: counter({
    help: 'Slack webhook requests handled by Slackbot.',
    labelNames: ['route', 'event_type', 'outcome'],
    name: 'slackbotv2_slack_webhook_requests_total'
  }),
  webhookRetryRequests: counter({
    help: 'Slack webhook requests answered with a retryable error so Slack retries delivery.',
    name: 'slackbotv2_webhook_retry_requests_total'
  })
}

export function resetSlackbotMetricsForTests(): void {
  registry.reset()
}

export function observeSeconds(startedAtMs: number): number {
  return Math.max(0, (globalThis.performance.now() - startedAtMs) / 1000)
}

function escapeHelp(value: string): string {
  return value.replaceAll('\\', '\\\\').replaceAll('\n', '\\n')
}

function escapeLabelValue(value: string): string {
  return value.replaceAll('\\', '\\\\').replaceAll('\n', '\\n').replaceAll('"', '\\"')
}

function formatLabels(labels: Record<string, string>): string {
  const entries = Object.entries(labels)
  if (entries.length === 0) return ''
  return `{${entries.map(([key, value]) => `${key}="${escapeLabelValue(value)}"`).join(',')}}`
}

function formatNumber(value: number): string {
  return Number.isInteger(value) ? String(value) : String(value)
}
