type EventKind = 'message' | 'card'
type EventOutcome = 'accepted' | 'ignored' | 'failed'
type RenderOperation = 'reply_card' | 'reply_text' | 'update_card'
type OperationOutcome = 'succeeded' | 'failed'
type RecoveryOutcome = 'attempted' | 'succeeded' | 'failed'

type Labels = Readonly<Record<string, string>>

export class FeishuMetrics {
  readonly #counters = new Map<string, number>()
  readonly #sums = new Map<string, number>()
  readonly #counts = new Map<string, number>()

  recordEventAck(kind: EventKind, elapsedMs: number): void {
    this.#observe('event_ack', { kind }, elapsedMs)
  }

  recordEvent(kind: EventKind, outcome: EventOutcome, elapsedMs: number): void {
    this.#increment('events', { kind, outcome })
    this.#observe('event_processing', { kind, outcome }, elapsedMs)
  }

  recordDeduplication(outcome: 'new' | 'duplicate'): void {
    this.#increment('deduplications', { outcome })
  }

  recordRender(operation: RenderOperation, outcome: OperationOutcome, elapsedMs: number): void {
    this.#increment('render_operations', { operation, outcome })
    this.#observe('render', { operation, outcome }, elapsedMs)
  }

  recordRecovery(outcome: RecoveryOutcome): void {
    this.#increment('recovery', { outcome })
  }

  recordStaleConflict(kind: 'selection' | 'delivery'): void {
    this.#increment('stale_conflicts', { kind })
  }

  prometheus(ready: boolean): string {
    const lines = [
      '# HELP centaur_feishubot_ready Whether the China Feishu long connection is ready.',
      '# TYPE centaur_feishubot_ready gauge',
      `centaur_feishubot_ready ${ready ? 1 : 0}`,
      '# HELP centaur_feishubot_events_total Normalized China Feishu events by bounded processing outcome.',
      '# TYPE centaur_feishubot_events_total counter',
      ...this.#counterLines('events'),
      '# HELP centaur_feishubot_deduplications_total Durable message idempotency results.',
      '# TYPE centaur_feishubot_deduplications_total counter',
      ...this.#counterLines('deduplications'),
      '# HELP centaur_feishubot_event_ack_duration_seconds Time to hand an event off from the SDK callback.',
      '# TYPE centaur_feishubot_event_ack_duration_seconds summary',
      ...this.#summaryLines('event_ack'),
      '# HELP centaur_feishubot_event_processing_duration_seconds End-to-end ingress processing time.',
      '# TYPE centaur_feishubot_event_processing_duration_seconds summary',
      ...this.#summaryLines('event_processing'),
      '# HELP centaur_feishubot_render_operations_total China Feishu reply and update results.',
      '# TYPE centaur_feishubot_render_operations_total counter',
      ...this.#counterLines('render_operations'),
      '# HELP centaur_feishubot_render_duration_seconds China Feishu reply and update duration.',
      '# TYPE centaur_feishubot_render_duration_seconds summary',
      ...this.#summaryLines('render'),
      '# HELP centaur_feishubot_recovery_total Durable delivery recovery attempts and results.',
      '# TYPE centaur_feishubot_recovery_total counter',
      ...this.#counterLines('recovery'),
      '# HELP centaur_feishubot_stale_conflicts_total Optimistic concurrency conflicts by bounded kind.',
      '# TYPE centaur_feishubot_stale_conflicts_total counter',
      ...this.#counterLines('stale_conflicts')
    ]
    return `${lines.join('\n')}\n`
  }

  #increment(metric: string, labels: Labels): void {
    const key = metricKey(metric, labels)
    this.#counters.set(key, (this.#counters.get(key) ?? 0) + 1)
  }

  #observe(metric: string, labels: Labels, elapsedMs: number): void {
    const key = metricKey(metric, labels)
    this.#sums.set(key, (this.#sums.get(key) ?? 0) + Math.max(0, elapsedMs) / 1_000)
    this.#counts.set(key, (this.#counts.get(key) ?? 0) + 1)
  }

  #counterLines(metric: string): string[] {
    return [...this.#counters.entries()]
      .filter(([key]) => key.startsWith(`${metric}|`))
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, value]) => `centaur_feishubot_${metric}_total${metricLabels(key)} ${value}`)
  }

  #summaryLines(metric: string): string[] {
    return [...this.#counts.entries()]
      .filter(([key]) => key.startsWith(`${metric}|`))
      .sort(([left], [right]) => left.localeCompare(right))
      .flatMap(([key, count]) => {
        const labels = metricLabels(key)
        const sum = this.#sums.get(key) ?? 0
        return [
          `centaur_feishubot_${metric}_duration_seconds_sum${labels} ${sum.toFixed(6)}`,
          `centaur_feishubot_${metric}_duration_seconds_count${labels} ${count}`
        ]
      })
  }
}

function metricKey(metric: string, labels: Labels): string {
  const encoded = Object.entries(labels)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, value]) => `${key}=${value}`)
    .join(',')
  return `${metric}|${encoded}`
}

function metricLabels(key: string): string {
  const encoded = key.slice(key.indexOf('|') + 1)
  if (!encoded) return ''
  return `{${encoded.split(',').map(label => {
    const separator = label.indexOf('=')
    return `${label.slice(0, separator)}="${label.slice(separator + 1)}"`
  }).join(',')}}`
}
