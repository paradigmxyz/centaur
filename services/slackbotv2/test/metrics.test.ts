import { describe, expect, test } from 'bun:test'
import { createMemoryState } from '@chat-adapter/state-memory'
import { createSlackbotV2 } from '../src/index'
import { resetSlackbotMetricsForTests, slackbotMetrics } from '../src/metrics'

describe('slackbotv2 metrics', () => {
  test('withholds health until state connects', async () => {
    const state = createMemoryState()
    const originalConnect = state.connect.bind(state)
    let releaseConnect!: () => void
    const connectGate = new Promise<void>(resolve => {
      releaseConnect = resolve
    })
    state.connect = async () => {
      await connectGate
      await originalConnect()
    }
    const bot = createSlackbotV2({
      apiUrl: 'http://api.test',
      botToken: 'xoxb-test',
      recoverRenderObligationsOnStart: false,
      signingSecret: 'secret',
      state
    })

    const notReadyResponse = await bot.app.request('/health')
    expect(notReadyResponse.status).toBe(503)
    await expect(notReadyResponse.json()).resolves.toMatchObject({
      ok: false,
      service: 'slackbotv2',
      database_connected: false,
      database_status: 'connecting'
    })

    releaseConnect()
    const readyResponse = await waitForHealthy(bot)
    expect(readyResponse.status).toBe(200)
    await expect(readyResponse.json()).resolves.toMatchObject({
      ok: true,
      service: 'slackbotv2',
      database_connected: true
    })
  })

  test('serves Prometheus text metrics', async () => {
    resetSlackbotMetricsForTests()
    slackbotMetrics.webhookRequests.inc({
      event_type: 'app_mention',
      outcome: 'success',
      route: '/api/webhooks/slack'
    })
    slackbotMetrics.sessionDelivery.inc({
      delivery_status: 'streamed'
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
    expect(body).toContain(
      'centaur_session_delivery_total{delivery_status="streamed"} 1'
    )
  })
})

async function waitForHealthy(bot: ReturnType<typeof createSlackbotV2>): Promise<Response> {
  for (let attempt = 0; attempt < 20; attempt++) {
    const response = await bot.app.request('/health')
    if (response.status === 200) return response
    await sleep(5)
  }
  return bot.app.request('/health')
}

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}
