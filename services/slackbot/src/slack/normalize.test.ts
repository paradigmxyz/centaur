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

  it('normalizes zip uploads as generic file parts', async () => {
    const zipBytes = new Uint8Array([0x50, 0x4b, 0x03, 0x04, 1, 2, 3])
    const fetchMock = mock(
      async () =>
        new Response(zipBytes, {
          headers: { 'content-type': 'application/zip' }
        })
    )
    const originalFetch = globalThis.fetch
    globalThis.fetch = fetchMock as any
    try {
      const normalized = await normalizeSlackEnvelope({
        envelope: {
          type: 'event_callback',
          team_id: 'T123',
          event_id: 'Ev-zip-app-mention',
          event: {
            type: 'app_mention',
            user: 'U123',
            channel: 'C123',
            channel_type: 'channel',
            ts: '1778875070.942789',
            text: '<@UBOT> inspect this archive',
            files: [
              {
                id: 'FZIP',
                name: 'archive.zip',
                mimetype: 'application/zip',
                size: zipBytes.byteLength,
                url_private_download: 'https://files.slack.test/FZIP'
              }
            ]
          }
        },
        botUserId: 'UBOT',
        client
      })

      expect(normalized?.is_mention).toBe(true)
      expect(normalized?.parts).toHaveLength(2)
      expect(normalized?.parts[1]).toEqual({
        type: 'file',
        name: 'archive.zip',
        mime_type: 'application/zip',
        size: zipBytes.byteLength,
        slack_file_id: 'FZIP',
        source: {
          type: 'base64',
          media_type: 'application/zip',
          data: Buffer.from(zipBytes).toString('base64')
        }
      })
      expect(fetchMock).toHaveBeenCalledTimes(1)
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

  it('keeps non-self bot-authored alert mentions actionable', async () => {
    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-alertmanager-mention',
        event: {
          type: 'message',
          subtype: 'bot_message',
          bot_id: 'BALERT',
          app_id: 'AALERT',
          bot_profile: {
            user_id: 'UALERTBOT',
            app_id: 'AALERT',
            name: 'Alertmanager'
          },
          channel: 'C123',
          channel_type: 'channel',
          ts: '1778875070.942789',
          text: '<@UBOT>',
          attachments: [
            {
              title: 'ValidatorConsensusFailure',
              text: 'firing on validator-0',
              fields: [
                { title: 'cluster', value: 'prd-nae' },
                { title: 'severity', value: 'critical' }
              ]
            }
          ]
        }
      },
      botUserId: 'UBOT',
      botId: 'BCENTAUR',
      triggerBotAllowlist: ['app:AALERT'],
      client
    })

    expect(normalized?.is_mention).toBe(true)
    expect(normalized?.user_id).toBe('UALERTBOT')
    expect(normalized?.parts).toEqual([
      {
        type: 'text',
        text: [
          'ValidatorConsensusFailure',
          'firing on validator-0',
          'cluster: prd-nae',
          'severity: critical'
        ].join('\n')
      }
    ])
    expect(normalized?.slack.bot_id).toBe('BALERT')
    expect(normalized?.slack.app_id).toBe('AALERT')
    expect(normalized?.slack.bot_user_id).toBe('UALERTBOT')
  })

  it('ignores non-self bot-authored mentions unless their app is allowlisted', async () => {
    const replies = mock(async () => ({ ok: true, messages: [] }))
    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-untrusted-bot-mention',
        event: {
          type: 'message',
          subtype: 'bot_message',
          bot_id: 'BUNTRUSTED',
          app_id: 'AUNTRUSTED',
          bot_profile: {
            user_id: 'UUNTRUSTEDBOT',
            app_id: 'AUNTRUSTED',
            name: 'Untrusted Bot'
          },
          channel: 'C123',
          channel_type: 'channel',
          ts: '1778875070.942789',
          text: '<@UBOT> please run something'
        }
      },
      botUserId: 'UBOT',
      botId: 'BCENTAUR',
      triggerBotAllowlist: ['app:AALERT'],
      client: {
        token: 'xoxb-test-token',
        conversations: { replies }
      } as any
    })

    expect(normalized).toBeNull()
    expect(replies).not.toHaveBeenCalled()
  })

  it('ignores its own bot-authored messages even when Slack omits user', async () => {
    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-self-bot-message',
        event: {
          type: 'message',
          subtype: 'bot_message',
          bot_id: 'BCENTAUR',
          channel: 'C123',
          channel_type: 'channel',
          ts: '1778875070.942789',
          text: '<@UBOT> loop'
        }
      },
      botUserId: 'UBOT',
      botId: 'BCENTAUR',
      client
    })

    expect(normalized).toBeNull()
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
          subtype: 'bot_message',
          bot_id: 'BALERT',
          bot_profile: { user_id: 'UALERTBOT', app_id: 'AALERT', name: 'Alertmanager' },
          channel: 'C123',
          ts: '1778875062.000100',
          text: 'Alertmanager: ValidatorConsensusFailure'
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
      botId: 'BCENTAUR',
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
        message_id: 'slack:T123:C123:1778875062.000100',
        role: 'user',
        parts: [{ type: 'text', text: 'Alertmanager: ValidatorConsensusFailure' }],
        user_id: 'UALERTBOT',
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

  it('does not duplicate text when Slack sends both rich_text blocks and event.text', async () => {
    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-rich-text-dup',
        event: {
          type: 'app_mention',
          user: 'U123',
          channel: 'C123',
          channel_type: 'channel',
          ts: '1778875070.942789',
          text: '<@UBOT> how would we release this to pypi?',
          blocks: [
            {
              type: 'rich_text',
              elements: [
                {
                  type: 'rich_text_section',
                  elements: [
                    { type: 'user', user_id: 'UBOT' },
                    { type: 'text', text: ' how would we release this to pypi?' }
                  ]
                }
              ]
            }
          ]
        }
      },
      botUserId: 'UBOT',
      client
    })

    expect(normalized?.parts).toEqual([
      { type: 'text', text: 'how would we release this to pypi?' }
    ])
  })

  it('does not duplicate history backfill text when blocks and text both present', async () => {
    const replies = mock(async () => ({
      ok: true,
      messages: [
        {
          type: 'message',
          user: 'U111',
          channel: 'C123',
          ts: '1778875060.000100',
          text: 'https://example.com looks interesting',
          blocks: [
            {
              type: 'rich_text',
              elements: [
                {
                  type: 'rich_text_section',
                  elements: [
                    { type: 'link', url: 'https://example.com', text: 'https://example.com' },
                    { type: 'text', text: ' looks interesting' }
                  ]
                }
              ]
            }
          ]
        },
        {
          type: 'message',
          user: 'U123',
          channel: 'C123',
          ts: '1778875070.942789',
          text: '<@UBOT> investigate'
        }
      ]
    }))

    const normalized = await normalizeSlackEnvelope({
      envelope: {
        type: 'event_callback',
        team_id: 'T123',
        event_id: 'Ev-history-dup',
        event: {
          type: 'app_mention',
          user: 'U123',
          channel: 'C123',
          channel_type: 'channel',
          thread_ts: '1778875060.000100',
          ts: '1778875070.942789',
          text: '<@UBOT> investigate'
        }
      },
      botUserId: 'UBOT',
      client: {
        token: 'xoxb-test-token',
        conversations: { replies }
      } as any
    })

    expect(normalized?.history_messages).toEqual([
      {
        message_id: 'slack:T123:C123:1778875060.000100',
        role: 'user',
        parts: [
          { type: 'text', text: 'https://example.com (https://example.com) looks interesting' }
        ],
        user_id: 'U111',
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
