import { describe, expect, it } from 'bun:test'
import { isTrivialAck, shouldAckWithReaction } from './trivial-ack'
import type { NormalizedSlackEvent } from './types'

describe('isTrivialAck', () => {
  it('matches whitelisted ack phrases regardless of casing or trailing punctuation', () => {
    expect(isTrivialAck('ok')).toBe(true)
    expect(isTrivialAck('OK!')).toBe(true)
    expect(isTrivialAck('Thanks!!')).toBe(true)
    expect(isTrivialAck('ty.')).toBe(true)
    expect(isTrivialAck('thank you')).toBe(true)
    expect(isTrivialAck('thank you so much')).toBe(true)
    expect(isTrivialAck('Got it.')).toBe(true)
    expect(isTrivialAck('sgtm')).toBe(true)
  })

  it('matches pure ack-emoji messages', () => {
    expect(isTrivialAck('👍')).toBe(true)
    expect(isTrivialAck('🙏')).toBe(true)
    expect(isTrivialAck('👌👍')).toBe(true)
    expect(isTrivialAck(' ✨ ')).toBe(true)
  })

  it('does not treat bare single words like "you", "done", or "great" as acks', () => {
    // These previously slipped through a permissive token set; without the
    // surrounding phrase they're real asks ("@bot you?", "@bot done?").
    expect(isTrivialAck('you')).toBe(false)
    expect(isTrivialAck('it')).toBe(false)
    expect(isTrivialAck('done')).toBe(false)
    expect(isTrivialAck('great')).toBe(false)
    expect(isTrivialAck('nice')).toBe(false)
    expect(isTrivialAck('cool')).toBe(false)
    expect(isTrivialAck('perfect')).toBe(false)
  })

  it('rejects messages that contain non-ack words even when short', () => {
    expect(isTrivialAck('ok do the next one')).toBe(false)
    expect(isTrivialAck('thanks, also can you')).toBe(false)
    expect(isTrivialAck('this is a request')).toBe(false)
    expect(isTrivialAck('cool, thanks!')).toBe(false)
  })

  it('rejects long messages even if they start with thanks', () => {
    const longish = 'thanks for that, can you also pull last quarter and compare?'
    expect(isTrivialAck(longish)).toBe(false)
  })

  it('returns false for empty or whitespace input', () => {
    expect(isTrivialAck('')).toBe(false)
    expect(isTrivialAck('   ')).toBe(false)
  })
})

function baseEvent(overrides: Partial<NormalizedSlackEvent> = {}): NormalizedSlackEvent {
  return {
    thread_key: 'slack:T:C:1.0',
    message_id: 'slack:T:C:2.0',
    team_id: 'T',
    recipient_team_id: 'T',
    user_id: 'U1',
    channel_id: 'C',
    thread_ts: '1.0',
    is_mention: true,
    parts: [{ type: 'text', text: 'thanks!' }],
    history_messages: [
      {
        message_id: 'slack:T:C:1.0',
        role: 'assistant',
        parts: [{ type: 'text', text: 'Done.' }]
      }
    ],
    slack: {
      event_id: 'Ev1',
      event_ts: '2.0',
      message_ts: '2.0'
    },
    ...overrides
  }
}

describe('shouldAckWithReaction', () => {
  it('acks a trivial thanks reply inside a thread with prior assistant work', () => {
    expect(shouldAckWithReaction(baseEvent())).toBe(true)
  })

  it('does not ack the first message in a brand-new thread', () => {
    expect(
      shouldAckWithReaction(
        baseEvent({
          slack: { event_id: 'Ev1', event_ts: '1.0', message_ts: '1.0' },
          history_messages: []
        })
      )
    ).toBe(false)
  })

  it('does not ack when the thread has no prior assistant reply', () => {
    expect(
      shouldAckWithReaction(
        baseEvent({
          history_messages: [
            {
              message_id: 'slack:T:C:1.0',
              role: 'user',
              parts: [{ type: 'text', text: 'hi' }]
            }
          ]
        })
      )
    ).toBe(false)
  })

  it('does not ack when the message carries attachments', () => {
    expect(
      shouldAckWithReaction(
        baseEvent({
          parts: [
            { type: 'text', text: 'thanks' },
            {
              type: 'image',
              name: 'pic.png',
              mime_type: 'image/png',
              size: 100,
              source: { type: 'base64', media_type: 'image/png', data: 'AA==' }
            }
          ]
        })
      )
    ).toBe(false)
  })

  it('does not ack a non-mention message', () => {
    expect(shouldAckWithReaction(baseEvent({ is_mention: false }))).toBe(false)
  })
})
