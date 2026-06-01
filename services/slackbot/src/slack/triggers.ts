import type { NormalizedSlackEvent, SlackTriggerReason } from './types'

export type SlackTriggerDecision =
  | { action: 'dispatch'; trigger_reason: SlackTriggerReason }
  | { action: 'ignore'; ignore_reason: string }

export async function resolveSlackTrigger(opts: {
  event: NormalizedSlackEvent
  threadFollowEnabled: boolean
  hasActiveThread: (threadKey: string) => Promise<boolean>
}): Promise<SlackTriggerDecision> {
  const { event } = opts
  if (event.is_mention) return { action: 'dispatch', trigger_reason: 'mention' }
  if (!opts.threadFollowEnabled) return { action: 'ignore', ignore_reason: 'not_mention' }
  if (event.channel_type === 'im') return { action: 'dispatch', trigger_reason: 'direct_message' }
  if (!isThreadFollowChannel(event.channel_type)) {
    return { action: 'ignore', ignore_reason: 'not_mention' }
  }
  if (event.slack.message_ts === event.thread_ts) {
    return { action: 'ignore', ignore_reason: 'not_mention' }
  }
  if (await opts.hasActiveThread(event.thread_key)) {
    return { action: 'dispatch', trigger_reason: 'active_thread_reply' }
  }
  return { action: 'ignore', ignore_reason: 'inactive_thread' }
}

function isThreadFollowChannel(channelType: string | undefined): boolean {
  return channelType === undefined || channelType === 'channel' || channelType === 'group'
}
