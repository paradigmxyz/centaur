import { describe, expect, it, mock } from 'bun:test'
import { CentaurHandoff } from './handoff'
import type { AppConfig } from '../config'
import type { NormalizedSlackEvent } from '../slack/types'

const config: AppConfig = {
  NODE_ENV: 'test',
  PORT: 3001,
  CENTAUR_API_URL: 'http://centaur-api.test',
  CENTAUR_SLACK_EVENTS_PATH: '/api/webhooks/slack',
  RUNTIME_ERROR_ALERT_CHANNEL: '',
  SLACK_EVENT_DEDUP_TTL_MS: 600000,
  SLACK_SIGNATURE_MAX_AGE_SECONDS: 300,
  SLACK_FEEDBACK_COMMANDS: ['/website-feedback'],
  SLACK_FEEDBACK_LINEAR_TEAM_ID: 'team-test',
  SLACK_FEEDBACK_LINEAR_PROJECT_ID: 'project-test',
  SLACK_FEEDBACK_ALLOWED_CHANNELS: [],
  SLACKBOT_EXTERNAL_ORG_ALLOWLIST: [],
  SLACKBOT_TRIGGER_BOT_ALLOWLIST: []
}

describe('CentaurHandoff', () => {
  it('omits envelope-specific Slack event metadata from idempotent workflow input', async () => {
    const originalFetch = globalThis.fetch
    let capturedInit: RequestInit | undefined
    const fetchMock = mock(async (_input: string | URL | Request, init?: RequestInit) => {
      capturedInit = init
      return new Response(JSON.stringify({ ok: true }), { status: 200 })
    })
    globalThis.fetch = fetchMock as any
    try {
      const handoff = new CentaurHandoff(config)
      const event: NormalizedSlackEvent = {
        thread_key: 'slack:T123:C123:1778883099.579529',
        message_id: 'slack:T123:C123:1778883099.579529',
        team_id: 'T123',
        user_id: 'U123',
        channel_id: 'C123',
        thread_ts: '1778883099.579529',
        is_mention: true,
        parts: [{ type: 'text', text: 'hello' }],
        slack: {
          event_id: 'Ev-envelope-one',
          event_ts: '1778883100.000000',
          message_ts: '1778883099.579529',
          enterprise_id: 'E123'
        }
      }

      await handoff.emit(event)

      expect(capturedInit).toBeDefined()
      const bodyText = capturedInit?.body
      expect(typeof bodyText).toBe('string')
      if (typeof bodyText !== 'string') throw new Error('expected JSON request body')
      const body = JSON.parse(bodyText) as {
        trigger_key: string
        input: { metadata: { slack: unknown } }
      }
      expect(body.trigger_key).toBe(event.message_id)
      expect(body.input.metadata.slack).toEqual({
        message_ts: '1778883099.579529',
        enterprise_id: 'E123'
      })
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('passes Slack attachment parts through workflow input', async () => {
    const originalFetch = globalThis.fetch
    let capturedInit: RequestInit | undefined
    const fetchMock = mock(async (_input: string | URL | Request, init?: RequestInit) => {
      capturedInit = init
      return new Response(JSON.stringify({ ok: true }), { status: 200 })
    })
    globalThis.fetch = fetchMock as any
    try {
      const handoff = new CentaurHandoff(config)
      const event: NormalizedSlackEvent = {
        thread_key: 'slack:T123:C123:1778883099.579529',
        message_id: 'slack:T123:C123:1778883099.579529',
        team_id: 'T123',
        user_id: 'U123',
        channel_id: 'C123',
        thread_ts: '1778883099.579529',
        is_mention: true,
        parts: [
          { type: 'text', text: 'review this' },
          {
            type: 'document',
            name: 'report.pdf',
            mime_type: 'application/pdf',
            size: 8,
            slack_file_id: 'F123',
            source: {
              type: 'base64',
              media_type: 'application/pdf',
              data: 'JVBERi0xLjQ='
            }
          }
        ],
        slack: {
          event_ts: '1778883100.000000',
          message_ts: '1778883099.579529'
        }
      }

      await handoff.emit(event)

      const bodyText = capturedInit?.body
      expect(typeof bodyText).toBe('string')
      if (typeof bodyText !== 'string') throw new Error('expected JSON request body')
      const body = JSON.parse(bodyText) as {
        input: { parts: NormalizedSlackEvent['parts'] }
      }
      expect(body.input.parts[1]).toMatchObject({
        type: 'document',
        name: 'report.pdf',
        mime_type: 'application/pdf',
        slack_file_id: 'F123',
        source: {
          type: 'base64',
          media_type: 'application/pdf',
          data: 'JVBERi0xLjQ='
        }
      })
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('uses recipient_team_id for Slack Connect delivery routing', async () => {
    const originalFetch = globalThis.fetch
    let capturedInit: RequestInit | undefined
    const fetchMock = mock(async (_input: string | URL | Request, init?: RequestInit) => {
      capturedInit = init
      return new Response(JSON.stringify({ ok: true }), { status: 200 })
    })
    globalThis.fetch = fetchMock as any
    try {
      const handoff = new CentaurHandoff(config)
      const event: NormalizedSlackEvent = {
        thread_key: 'slack:THOME:C123:1778883099.579529',
        message_id: 'slack:THOME:C123:1778883099.579529',
        team_id: 'THOME',
        recipient_team_id: 'TEXTERNAL',
        user_id: 'UEXTERNAL',
        channel_id: 'C123',
        thread_ts: '1778883099.579529',
        is_mention: true,
        parts: [{ type: 'text', text: 'hello' }],
        slack: {
          event_ts: '1778883100.000000',
          message_ts: '1778883099.579529',
          user_team: 'TEXTERNAL'
        }
      }

      await handoff.emit(event)

      const bodyText = capturedInit?.body
      expect(typeof bodyText).toBe('string')
      if (typeof bodyText !== 'string') throw new Error('expected JSON request body')
      const body = JSON.parse(bodyText) as {
        input: { delivery: { recipient_team_id: string; recipient_user_id: string } }
      }
      expect(body.input.delivery).toMatchObject({
        recipient_team_id: 'TEXTERNAL',
        recipient_user_id: 'UEXTERNAL'
      })
    } finally {
      globalThis.fetch = originalFetch
    }
  })
})
