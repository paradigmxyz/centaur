import { describe, expect, it } from 'bun:test'
import { healthResponse } from '../src/health.js'
import { FeishuMetrics } from '../src/metrics.js'

describe('health endpoints', () => {
  it('keeps liveness independent from the China Feishu connection', async () => {
    expect(healthResponse('/health', false)?.status).toBe(200)
    expect(healthResponse('/ready', false)?.status).toBe(503)
    expect(healthResponse('/ready', true)?.status).toBe(200)
  })

  it('exports bounded event, render, recovery, and conflict metrics', async () => {
    const metrics = new FeishuMetrics()
    metrics.recordEventAck('message', 2)
    metrics.recordEvent('message', 'accepted', 20)
    metrics.recordDeduplication('duplicate')
    metrics.recordRender('update_card', 'succeeded', 30)
    metrics.recordRecovery('attempted')
    metrics.recordRecovery('failed')
    metrics.recordStaleConflict('delivery')
    const response = healthResponse('/metrics', true, metrics)
    expect(response?.headers.get('content-type')).toContain('text/plain')
    const body = await response?.text()
    expect(body).toContain('centaur_feishubot_ready 1')
    expect(body).toContain('centaur_feishubot_events_total{kind="message",outcome="accepted"} 1')
    expect(body).toContain('centaur_feishubot_deduplications_total{outcome="duplicate"} 1')
    expect(body).toContain('centaur_feishubot_event_ack_duration_seconds_count{kind="message"} 1')
    expect(body).toContain('centaur_feishubot_render_operations_total{operation="update_card",outcome="succeeded"} 1')
    expect(body).toContain('centaur_feishubot_recovery_total{outcome="failed"} 1')
    expect(body).toContain('centaur_feishubot_stale_conflicts_total{kind="delivery"} 1')
    expect(body).not.toContain('tenant')
    expect(body).not.toContain('message_id')
  })
})
