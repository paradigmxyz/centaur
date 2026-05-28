import { createHmac } from 'node:crypto'
import { afterEach, describe, expect, it, mock } from 'bun:test'

const originalEnv = { ...process.env }

afterEach(() => {
  for (const key of Object.keys(process.env)) {
    if (!(key in originalEnv)) delete process.env[key]
  }
  Object.assign(process.env, originalEnv)
})

describe('Slack event HTTP dedupe', () => {
  it('creates Linear issues from configured feedback slash commands', async () => {
    process.env.SLACK_SIGNING_SECRET = 'test-signing-secret'
    process.env.LINEAR_API_KEY = 'lin-test-key'
    process.env.SLACK_FEEDBACK_LINEAR_TEAM_ID = 'team-feedback'
    process.env.SLACK_FEEDBACK_LINEAR_PROJECT_ID = 'project-feedback'

    const originalFetch = globalThis.fetch
    const fetchMock = mock(async (_input: string | URL | Request, init?: RequestInit) => {
      const body = JSON.parse(init?.body as string) as {
        variables: { input: { title: string; teamId: string; projectId: string } }
      }
      expect(body.variables.input).toMatchObject({
        title: 'Button copy is confusing',
        teamId: 'team-feedback',
        projectId: 'project-feedback'
      })
      return new Response(
        JSON.stringify({
          data: {
            issueCreate: {
              issue: {
                identifier: 'DSGN-123',
                url: 'https://linear.app/paradigmxyz/issue/DSGN-123'
              }
            }
          }
        }),
        { status: 200 }
      )
    })
    globalThis.fetch = fetchMock as unknown as typeof fetch

    try {
      const { app } = await import('./index')
      const body = new URLSearchParams({
        command: '/website-feedback',
        text: 'Button copy is confusing\nThe submit button should mention Linear.',
        user_id: 'U123',
        channel_id: 'C123',
        channel_name: 'design-feedback'
      }).toString()

      const response = await app.request(
        '/api/slack/commands',
        signedFormRequest(body, process.env.SLACK_SIGNING_SECRET)
      )

      expect(response.status).toBe(200)
      expect(await response.json()).toEqual({
        response_type: 'ephemeral',
        text: 'Created DSGN-123: https://linear.app/paradigmxyz/issue/DSGN-123'
      })
      expect(fetchMock).toHaveBeenCalledTimes(1)
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('acks duplicate Slack envelopes without scheduling duplicate processing', async () => {
    process.env.SLACK_SIGNING_SECRET = 'test-signing-secret'
    process.env.SLACK_EVENT_DEDUP_TTL_MS = '600000'
    delete process.env.SLACK_BOT_TOKEN
    delete process.env.SLACKBOT_API_KEY
    delete process.env.CENTAUR_API_KEY

    const originalError = console.error
    const originalLog = console.log
    const originalWarn = console.warn
    console.error = mock(() => {}) as typeof console.error
    console.log = mock(() => {}) as typeof console.log
    console.warn = mock(() => {}) as typeof console.warn
    try {
      const { app } = await import('./index')
      const body = JSON.stringify({
        type: 'event_callback',
        event_id: 'Ev-duplicate',
        team_id: 'T123',
        event: {
          type: 'app_mention',
          user: 'U123',
          channel: 'C123',
          ts: '1778883099.579529',
          text: '<@UBOT> hello'
        }
      })
      const waits: Promise<unknown>[] = []
      const executionCtx = {
        waitUntil: (promise: Promise<unknown>) => {
          waits.push(promise)
        },
        passThroughOnException: () => {},
        props: {}
      }

      const first = await app.request(
        '/api/webhooks/slack',
        signedJsonRequest(body, process.env.SLACK_SIGNING_SECRET),
        {},
        executionCtx as any
      )
      const second = await app.request(
        '/api/webhooks/slack',
        signedJsonRequest(body, process.env.SLACK_SIGNING_SECRET),
        {},
        executionCtx as any
      )

      expect(first.status).toBe(200)
      expect(await first.json()).toEqual({ ok: true })
      expect(second.status).toBe(200)
      expect(await second.json()).toEqual({ ok: true, duplicate: true })
      expect(console.warn).toHaveBeenCalledWith(
        'slack_duplicate_event_skipped',
        expect.objectContaining({
          dedupe_key: 'event:Ev-duplicate',
          event_id: 'Ev-duplicate',
          team_id: 'T123',
          channel_id: 'C123',
          message_ts: '1778883099.579529',
          thread_ts: '1778883099.579529',
          event_type: 'app_mention',
          codex_thread_id: undefined,
          alert_channel_id: undefined,
          log_version_uuid: '013ca634-6a30-4047-8511-8e5483f313ea'
        })
      )
      expect(waits).toHaveLength(1)
      await Promise.allSettled(waits)
    } finally {
      console.error = originalError
      console.log = originalLog
      console.warn = originalWarn
    }
  })

  it('logs duplicate Slack messages when Slack event IDs are absent', async () => {
    process.env.SLACK_SIGNING_SECRET = 'test-signing-secret'
    process.env.SLACK_EVENT_DEDUP_TTL_MS = '600000'
    delete process.env.SLACK_BOT_TOKEN
    delete process.env.SLACKBOT_API_KEY
    delete process.env.CENTAUR_API_KEY

    const originalError = console.error
    const originalLog = console.log
    const originalWarn = console.warn
    console.error = mock(() => {}) as typeof console.error
    console.log = mock(() => {}) as typeof console.log
    console.warn = mock(() => {}) as typeof console.warn
    try {
      const { app } = await import('./index')
      const body = JSON.stringify({
        type: 'event_callback',
        team_id: 'T123',
        event: {
          type: 'message',
          user: 'U123',
          channel: 'C123',
          ts: '1778883099.579530',
          text: 'Duplicate report for Codex thread `T-019e28c1-08bb-777d-9a2e-74a393296b28`'
        }
      })
      const waits: Promise<unknown>[] = []
      const executionCtx = {
        waitUntil: (promise: Promise<unknown>) => {
          waits.push(promise)
        }
      }

      await app.request(
        '/api/webhooks/slack',
        signedJsonRequest(body, process.env.SLACK_SIGNING_SECRET),
        {},
        executionCtx as any
      )
      const second = await app.request(
        '/api/webhooks/slack',
        signedJsonRequest(body, process.env.SLACK_SIGNING_SECRET),
        {},
        executionCtx as any
      )

      expect(second.status).toBe(200)
      expect(await second.json()).toEqual({ ok: true, duplicate: true })
      expect(console.warn).toHaveBeenCalledWith(
        'slack_duplicate_message_skipped',
        expect.objectContaining({
          dedupe_key: 'message:T123:C123:1778883099.579530',
          event_id: undefined,
          team_id: 'T123',
          channel_id: 'C123',
          message_ts: '1778883099.579530',
          thread_ts: '1778883099.579530',
          event_type: 'message',
          codex_thread_id: 'T-019e28c1-08bb-777d-9a2e-74a393296b28',
          alert_channel_id: undefined,
          log_version_uuid: '013ca634-6a30-4047-8511-8e5483f313ea'
        })
      )
      expect(waits).toHaveLength(1)
      await Promise.allSettled(waits)
    } finally {
      console.error = originalError
      console.log = originalLog
      console.warn = originalWarn
    }
  })
})

function signedFormRequest(body: string, signingSecret: string): RequestInit {
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const signature = `v0=${createHmac('sha256', signingSecret)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')}`
  return {
    method: 'POST',
    headers: {
      'content-type': 'application/x-www-form-urlencoded',
      'x-slack-request-timestamp': timestamp,
      'x-slack-signature': signature
    },
    body
  }
}

function signedJsonRequest(body: string, signingSecret: string): RequestInit {
  const timestamp = Math.floor(Date.now() / 1000).toString()
  const signature = `v0=${createHmac('sha256', signingSecret)
    .update(`v0:${timestamp}:${body}`)
    .digest('hex')}`
  return {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-slack-request-timestamp': timestamp,
      'x-slack-signature': signature
    },
    body
  }
}
