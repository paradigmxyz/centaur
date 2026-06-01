import { describe, expect, it, mock } from 'bun:test'
import { resolveSlackTrigger } from './triggers'
import type { NormalizedSlackEvent } from './types'

describe('resolveSlackTrigger', () => {
  it('dispatches mentions regardless of thread follow flag', async () => {
    const hasActiveThread = mock(async () => false)
    await expect(
      resolveSlackTrigger({
        event: event({ is_mention: true }),
        threadFollowEnabled: false,
        hasActiveThread
      })
    ).resolves.toEqual({ action: 'dispatch', trigger_reason: 'mention' })
    expect(hasActiveThread).not.toHaveBeenCalled()
  })

  it('ignores non-mentions when thread follow is off', async () => {
    await expect(
      resolveSlackTrigger({
        event: event({ is_mention: false, channel_type: 'im' }),
        threadFollowEnabled: false,
        hasActiveThread: async () => true
      })
    ).resolves.toEqual({ action: 'ignore', ignore_reason: 'not_mention' })
  })

  it('dispatches direct messages without an active thread lookup', async () => {
    const hasActiveThread = mock(async () => false)
    await expect(
      resolveSlackTrigger({
        event: event({ is_mention: false, channel_type: 'im' }),
        threadFollowEnabled: true,
        hasActiveThread
      })
    ).resolves.toEqual({ action: 'dispatch', trigger_reason: 'direct_message' })
    expect(hasActiveThread).not.toHaveBeenCalled()
  })

  it('ignores channel root messages without a mention', async () => {
    const hasActiveThread = mock(async () => true)
    await expect(
      resolveSlackTrigger({
        event: event({ is_mention: false, channel_type: 'channel' }),
        threadFollowEnabled: true,
        hasActiveThread
      })
    ).resolves.toEqual({ action: 'ignore', ignore_reason: 'not_mention' })
    expect(hasActiveThread).not.toHaveBeenCalled()
  })

  it('does not treat multiparty DMs as thread-follow channels', async () => {
    const hasActiveThread = mock(async () => true)
    await expect(
      resolveSlackTrigger({
        event: event({
          is_mention: false,
          channel_type: 'mpim',
          thread_ts: '1778883099.000100',
          message_ts: '1778883100.000100'
        }),
        threadFollowEnabled: true,
        hasActiveThread
      })
    ).resolves.toEqual({ action: 'ignore', ignore_reason: 'not_mention' })
    expect(hasActiveThread).not.toHaveBeenCalled()
  })

  it('dispatches only active channel thread replies', async () => {
    const inactive = await resolveSlackTrigger({
      event: event({
        is_mention: false,
        channel_type: 'channel',
        thread_ts: '1778883099.000100',
        message_ts: '1778883100.000100'
      }),
      threadFollowEnabled: true,
      hasActiveThread: async () => false
    })
    expect(inactive).toEqual({ action: 'ignore', ignore_reason: 'inactive_thread' })

    const active = await resolveSlackTrigger({
      event: event({
        is_mention: false,
        channel_type: 'channel',
        thread_ts: '1778883099.000100',
        message_ts: '1778883100.000100'
      }),
      threadFollowEnabled: true,
      hasActiveThread: async () => true
    })
    expect(active).toEqual({ action: 'dispatch', trigger_reason: 'active_thread_reply' })
  })
})

function event(
  overrides: Partial<NormalizedSlackEvent> & { message_ts?: string } = {}
): NormalizedSlackEvent {
  const messageTs = overrides.message_ts ?? overrides.thread_ts ?? '1778883099.000100'
  return {
    thread_key: `slack:T123:C123:${overrides.thread_ts ?? messageTs}`,
    message_id: `slack:T123:C123:${messageTs}`,
    team_id: 'T123',
    user_id: 'U123',
    channel_id: 'C123',
    channel_type: 'channel',
    thread_ts: overrides.thread_ts ?? messageTs,
    is_mention: false,
    parts: [{ type: 'text', text: 'hello' }],
    slack: {
      message_ts: messageTs
    },
    ...overrides
  }
}
