import { describe, expect, test } from 'bun:test'
import { createMemoryState } from '@chat-adapter/state-memory'
import { createSlackbotV2 } from '../src/index'
import { resetSlackbotMetricsForTests, slackbotMetrics } from '../src/metrics'

describe('slackbotv2 metrics', () => {
  test('serves Prometheus text metrics', async () => {
    resetSlackbotMetricsForTests()
    slackbotMetrics.webhookRequests.inc({
      event_type: 'app_mention',
      outcome: 'success',
      route: '/api/webhooks/slack'
    })

    const bot = createSlackbotV2({
      apiUrl: 'http://api.test',
      botToken: 'xoxb-test',
      recoverRenderObligationsOnStart: false,
      signingSecret: 'secret',
      state: createMemoryState()
    })

    const response = await bot.app.request('/metrics')
    const body = await response.text()

    expect(response.status).toBe(200)
    expect(response.headers.get('content-type')).toContain('text/plain')
    expect(body).toContain('# HELP slackbotv2_info Static Slackbot v2 service info.')
    expect(body).toContain('slackbotv2_info 1')
    expect(body).toContain(
      'slackbotv2_slack_webhook_requests_total{route="/api/webhooks/slack",event_type="app_mention",outcome="success"} 1'
    )
  })
})
