import { describe, expect, test } from 'bun:test'
import { rawSlackUserId, slackUserIdForMessage } from '../src/slack-user'

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
})
