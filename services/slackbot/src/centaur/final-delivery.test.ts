import { afterEach, describe, expect, it, mock } from 'bun:test'
import { pollFinalDeliveriesOnce } from './final-delivery'
import type { AppConfig } from '../config'

const config: AppConfig = {
  NODE_ENV: 'test',
  PORT: 3001,
  CENTAUR_API_URL: 'http://centaur-api.test',
  CENTAUR_API_KEY: 'centaur-test-key',
  CENTAUR_SLACK_EVENTS_PATH: '/api/webhooks/slack',
  RUNTIME_ERROR_ALERT_CHANNEL: '',
  SLACK_EVENT_DEDUP_TTL_MS: 600000,
  SLACK_SIGNATURE_MAX_AGE_SECONDS: 300,
  SLACK_FEEDBACK_COMMANDS: ['/website-feedback'],
  SLACK_FEEDBACK_LINEAR_TEAM_ID: 'team-test',
  SLACK_FEEDBACK_LINEAR_PROJECT_ID: 'project-test',
  SLACK_FEEDBACK_ALLOWED_CHANNELS: [],
  SLACKBOT_EXTERNAL_ORG_ALLOWLIST: []
}

afterEach(() => {
  mock.restore()
})

describe('final delivery polling', () => {
  it('posts a claimed delivery once and marks it delivered before the next poll', async () => {
    const originalFetch = globalThis.fetch
    const fetchCalls: Array<{ path: string; body: unknown }> = []
    let claimCount = 0
    const fetchMock = mock(async (input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(input instanceof Request ? input.url : input)
      const body = init?.body ? JSON.parse(init.body as string) : undefined
      fetchCalls.push({ path: url.pathname, body })

      if (url.pathname === '/agent/final-deliveries/claim') {
        claimCount += 1
        return jsonResponse({
          deliveries:
            claimCount === 1
              ? [
                  {
                    execution_id: 'exe-duplicate-guard',
                    thread_key: 'slack:T123:C123:1778883099.579529',
                    delivery: {
                      platform: 'slack',
                      channel: 'C123',
                      thread_ts: '1778883099.579529',
                      recipient_team_id: 'T123',
                      recipient_user_id: 'U123'
                    },
                    final_payload: {
                      session_title: 'Centaur · codex',
                      result_text: 'done [once](https://example.com) with **bold** text'
                    }
                  }
                ]
              : []
        })
      }

      if (url.pathname === '/agent/final-deliveries/exe-duplicate-guard/delivered') {
        return jsonResponse({ ok: true })
      }

      throw new Error(`unexpected request: ${url.pathname}`)
    })
    globalThis.fetch = fetchMock as unknown as typeof fetch

    const slackCalls: Array<{ method: string; params: unknown }> = []
    const client = {
      assistant: {
        threads: {
          setStatus: async (params: unknown) => {
            slackCalls.push({ method: 'assistant.threads.setStatus', params })
            return { ok: true }
          }
        }
      },
      chat: {
        startStream: async (params: any) => {
          slackCalls.push({ method: 'chat.startStream', params })
          return { ok: true, channel: params.channel, ts: '1778883100.000000' }
        },
        appendStream: async (params: unknown) => {
          slackCalls.push({ method: 'chat.appendStream', params })
          return { ok: true }
        },
        postMessage: async (params: unknown) => {
          slackCalls.push({ method: 'chat.postMessage', params })
          return { ok: true }
        },
        stopStream: async (params: unknown) => {
          slackCalls.push({ method: 'chat.stopStream', params })
          return { ok: true }
        },
        update: async (params: unknown) => {
          slackCalls.push({ method: 'chat.update', params })
          return { ok: true }
        }
      }
    }

    try {
      await pollFinalDeliveriesOnce(config, client as any)
      await pollFinalDeliveriesOnce(config, client as any)

      expect(fetchCalls.filter(call => call.path === '/agent/final-deliveries/claim')).toHaveLength(
        2
      )
      expect(
        fetchCalls.filter(
          call => call.path === '/agent/final-deliveries/exe-duplicate-guard/delivered'
        )
      ).toHaveLength(1)
      expect(slackCalls.filter(call => call.method === 'chat.startStream')).toHaveLength(0)
      expect(slackCalls.filter(call => call.method === 'chat.stopStream')).toHaveLength(0)
      const postMessage = slackCalls.find(call => call.method === 'chat.postMessage')
      expect((postMessage?.params as any)?.text).toBe(
        'done [once](https://example.com) with **bold** text'
      )
      expect((postMessage?.params as any)?.blocks).toEqual([
        {
          type: 'markdown',
          text: 'done [once](https://example.com) with **bold** text'
        }
      ])
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' }
  })
}
