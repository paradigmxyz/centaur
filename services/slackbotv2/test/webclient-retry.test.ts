import { describe, expect, it } from 'bun:test'
import type { SlackAdapter } from '@chat-adapter/slack'
import { createMemoryState } from '@chat-adapter/state-memory'
import { createSlackbotV2, type SlackbotV2 } from '../src/index'

// The adapter's WebClient defaults to retrying 429s with
// tenRetriesInAboutThirtyMinutes, which can hold a live render for ~30
// minutes. This offline fixture proves the service configures a bounded
// budget instead: a rate-limited chat.postMessage must exhaust exactly
// three attempts (1 + 2 retries) within seconds, then fail into the
// durable render/recovery paths.
describe('webClient retry policy', () => {
  // Bun's default per-test timeout (5s) is shorter than the bounded budget
  // itself, so pin an explicit one; a regression to the SDK default fails at
  // the 15s race bound instead of hanging for ~30 minutes.
  it('exhausts a bounded retry budget when chat.postMessage is rate limited', async () => {
    let calls = 0
    const server = Bun.serve({ port: 0, fetch: async () => {
      calls++
      return Response.json({ ok: false, error: 'rate_limited' }, {
        status: 429,
        headers: { 'retry-after': '1' }
      })
    } })
    let instance: SlackbotV2 | undefined
    try {
      instance = createSlackbotV2({
        apiKey: 'slackbotv2-webclient-retry-test',
        apiUrl: 'http://127.0.0.1:9/unreachable',
        botToken: 'xoxb-fixture',
        signingSecret: 'fixture',
        slackApiUrl: `${server.url}api/`,
        state: createMemoryState(),
        recoverRenderObligationsOnStart: false
      })
      const startedAtMs = Date.now()
      const webClient = (instance.chat.getAdapter('slack') as SlackAdapter).webClient
      const outcome = await Promise.race([
        webClient.chat.postMessage({ channel: 'CRETRY', text: 'hi' })
          .then(() => 'ok' as const, () => 'failed' as const),
        Bun.sleep(15_000).then(() => 'timeout' as const)
      ])
      const elapsedMs = Date.now() - startedAtMs
      expect(outcome).toBe('failed')
      expect(calls).toBe(3)
      expect(elapsedMs).toBeLessThan(15_000)
    } finally {
      server.stop(true)
    }
  }, 30_000)
})
