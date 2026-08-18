import { describe, expect, test } from 'bun:test'
import {
  rawSlackUserId,
  resolveSlackBotUserId,
  slackUserIdForMessage
} from '../src/slack-user'

describe('Slack user ID extraction', () => {
  test('prefers the Chat SDK author user ID', () => {
    expect(
      slackUserIdForMessage({
        author: { userId: 'UAUTHOR' },
        raw: { user: 'URAW' }
      })
    ).toBe('UAUTHOR')
  })

  test('falls back to a raw Slack user string', () => {
    expect(slackUserIdForMessage({ raw: { user: 'URAW' } })).toBe('URAW')
    expect(rawSlackUserId({ user: 'URAW' })).toBe('URAW')
  })

  test('falls back to raw Slack user object ids', () => {
    expect(rawSlackUserId({ user: { id: 'UOBJECT' } })).toBe('UOBJECT')
    expect(rawSlackUserId({ user: { user_id: 'UOBJECT_ALT' } })).toBe('UOBJECT_ALT')
  })

  test('falls back to bot profile user ID', () => {
    expect(rawSlackUserId({ bot_profile: { user_id: 'UBOT' } })).toBe('UBOT')
  })

  test('returns undefined when no Slack user ID is present', () => {
    expect(slackUserIdForMessage({ raw: { user: {} } })).toBeUndefined()
    expect(rawSlackUserId({ bot_profile: {} })).toBeUndefined()
  })

  test('uses the configured bot user ID without calling Slack', async () => {
    let calls = 0
    const userId = await resolveSlackBotUserId({
      botToken: 'xoxb-test',
      configuredBotUserId: 'UBOT',
      fetchFn: async () => {
        calls += 1
        return Response.json({ ok: true, user_id: 'UOTHER' })
      }
    })

    expect(userId).toBe('UBOT')
    expect(calls).toBe(0)
  })

  test('resolves the bot user ID with auth.test when it is not configured', async () => {
    let request: Request | undefined
    const userId = await resolveSlackBotUserId({
      botToken: 'xoxb-test',
      slackApiUrl: 'https://slack.test/api/',
      fetchFn: async (input, init) => {
        request = new Request(input, init)
        return Response.json({ ok: true, user_id: 'URESOLVED' })
      }
    })

    expect(userId).toBe('URESOLVED')
    expect(request?.url).toBe('https://slack.test/api/auth.test')
    expect(request?.method).toBe('POST')
    expect(request?.headers.get('authorization')).toBe('Bearer xoxb-test')
  })

  test('rejects an auth.test response without a bot user ID', async () => {
    await expect(
      resolveSlackBotUserId({
        botToken: 'xoxb-test',
        fetchFn: async () => Response.json({ ok: false, error: 'invalid_auth' })
      })
    ).rejects.toThrow('Slack auth.test failed: invalid_auth')
  })
})
