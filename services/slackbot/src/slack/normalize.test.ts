import { describe, expect, it, mock } from 'bun:test'
import { normalizeSlackEnvelope } from './normalize'

const client = {
  token: 'xoxb-test-token',
  conversations: {
    replies: mock(async () => ({ ok: true, messages: [] }))
  }
} as any

describe('normalizeSlackEnvelope', () => {
  it('ignores message file_share carrier events', async () => {
    const fetchMock = mock(async () => new Response('unused'))
    const originalFetch = globalThis.fetch
    globalThis.fetch = fetchMock as any
    try {
      const normalized = await normalizeSlackEnvelope({
        envelope: {
          type: 'event_callback',
          team_id: 'T123',
          event_id: 'Ev-file-share',
          event: {
            type: 'message',
            subtype: 'file_share',
            user: 'U123',
            channel: 'C123',
            channel_type: 'channel',
            ts: '1778875070.942789',
            text: '<@UBOT> what are these?',
            files: [
              {
                id: 'F123',
                name: 'image.png',
                mimetype: 'image/png',
                url_private_download: 'https://files.slack.test/F123'
              }
            ]
          }
        },
        botUserId: 'UBOT',
        client
      })

      expect(normalized).toBeNull()
      expect(fetchMock).not.toHaveBeenCalled()
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('keeps app_mention events with files actionable', async () => {
    let capturedInput: string | URL | Request | undefined
    let capturedInit: RequestInit | undefined
    const fetchMock = mock(async (input: string | URL | Request, init?: RequestInit) => {
      capturedInput = input
      capturedInit = init
      return new Response(new Uint8Array([1, 2, 3]), {
        headers: { 'content-type': 'image/png' }
      })
    })
    const originalFetch = globalThis.fetch
    globalThis.fetch = fetchMock as any
    try {
      const normalized = await normalizeSlackEnvelope({
        envelope: {
          type: 'event_callback',
          team_id: 'T123',
          event_id: 'Ev-app-mention',
          event: {
            type: 'app_mention',
            user: 'U123',
            channel: 'C123',
            channel_type: 'channel',
            ts: '1778875070.942789',
            text: '<@UBOT> what are these?',
            files: [
              {
                id: 'F123',
                name: 'image.png',
                mimetype: 'image/png',
                url_private_download: 'https://files.slack.test/F123'
              }
            ]
          }
        },
        botUserId: 'UBOT',
        client
      })

      expect(normalized?.is_mention).toBe(true)
      expect(normalized?.parts).toHaveLength(2)
      expect(normalized?.parts[1]).toMatchObject({
        type: 'image',
        name: 'image.png',
        mime_type: 'image/png',
        slack_file_id: 'F123'
      })
      expect(fetchMock).toHaveBeenCalledTimes(1)
      expect(capturedInput).toBe('https://files.slack.test/F123')
      expect(capturedInit?.headers).toEqual({ Authorization: 'Bearer xoxb-test-token' })
      const filePart = normalized?.parts[1]
      if (!filePart || filePart.type === 'text') throw new Error('expected binary part')
      expect(filePart.source.data).toBe(Buffer.from(new Uint8Array([1, 2, 3])).toString('base64'))
    } finally {
      globalThis.fetch = originalFetch
    }
  })

  it('preserves Slack Connect user_team as recipient_team_id without changing thread key', async () => {
    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'THOME',
        event_id: 'Ev-slack-connect',
        event: {
          type: 'app_mention',
          user: 'UEXTERNAL',
          user_team: 'TEXTERNAL',
          source_team: 'TEXTERNAL',
          team: 'THOME',
          channel: 'C123',
          channel_type: 'channel',
          thread_ts: '1778875060.000100',
          ts: '1778875070.942789',
          text: '<@UBOT> hello'
        }
      },
      botUserId: 'UBOT',
      client
    })

    expect(normalized?.thread_key).toBe('slack:THOME:C123:1778875060.000100')
    expect(normalized?.team_id).toBe('THOME')
    expect(normalized?.recipient_team_id).toBe('TEXTERNAL')
    expect(normalized?.slack.user_team).toBe('TEXTERNAL')
  })

  it('backfills prior Slack thread messages for mid-thread mentions', async () => {
    const replies = mock(async () => ({
      ok: true,
      messages: [
        {
          type: 'message',
          user: 'U111',
          channel: 'C123',
          ts: '1778875060.000100',
          text: 'Earlier market context'
        },
        {
          type: 'message',
          user: 'UBOT',
          channel: 'C123',
          bot_id: 'B123',
          ts: '1778875065.000100',
          text: 'Prior Centaur answer'
        },
        {
          type: 'message',
          user: 'U123',
          channel: 'C123',
          ts: '1778875070.942789',
          text: '<@UBOT> --invest pick this up'
        }
      ]
    }))

    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-thread-mention',
        event: {
          type: 'app_mention',
          user: 'U123',
          channel: 'C123',
          channel_type: 'channel',
          thread_ts: '1778875060.000100',
          ts: '1778875070.942789',
          text: '<@UBOT> --invest pick this up'
        }
      },
      botUserId: 'UBOT',
      client: {
        token: 'xoxb-test-token',
        conversations: { replies }
      } as any
    })

    expect(replies).toHaveBeenCalledWith({
      channel: 'C123',
      ts: '1778875060.000100',
      limit: 200,
      cursor: undefined
    })
    expect(normalized?.history_messages).toEqual([
      {
        message_id: 'slack:T123:C123:1778875060.000100',
        role: 'user',
        parts: [{ type: 'text', text: 'Earlier market context' }],
        user_id: 'U111',
        metadata: { platform: 'slack', history_backfill: true }
      },
      {
        message_id: 'slack:T123:C123:1778875065.000100',
        role: 'assistant',
        parts: [{ type: 'text', text: 'Prior Centaur answer' }],
        user_id: 'UBOT',
        metadata: { platform: 'slack', history_backfill: true }
      }
    ])
  })

  it('keeps mention handoff actionable when Slack thread history fetch fails', async () => {
    const replies = mock(async () => {
      throw new Error('ratelimited')
    })

    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-thread-mention-no-history',
        event: {
          type: 'app_mention',
          user: 'U123',
          channel: 'C123',
          channel_type: 'channel',
          thread_ts: '1778875060.000100',
          ts: '1778875070.942789',
          text: '<@UBOT> --invest pick this up'
        }
      },
      botUserId: 'UBOT',
      client: {
        token: 'xoxb-test-token',
        conversations: { replies }
      } as any
    })

    expect(normalized?.is_mention).toBe(true)
    expect(normalized?.parts).toEqual([{ type: 'text', text: '--invest pick this up' }])
    expect(normalized?.history_messages).toBeUndefined()
  })
})
