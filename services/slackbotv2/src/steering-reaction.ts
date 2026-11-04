import { callSlackApi } from '@chat-adapter/slack/api'
import type { Message as ChatMessage, Thread } from 'chat'
import { fetchWithTimeout, slackApiTimeoutMs } from './session-api'
import type { SlackbotV2Options, SlackbotV2Trace } from './types'
import { elapsedMs, errorMessage, nowMs, stringValue, traceLog, traceWarn } from './utils'

/** `ack` marks a message that starts a turn; `steering` marks a follow-up into one. */
export type ReactionKind = 'ack' | 'steering'

export type SteeringReactionAck = {
  added: Promise<boolean>
  channel: string
  name: string
  timestamp: string
  threadId: string
}

export type SteeringReactionController = {
  begin(
    thread: Thread,
    message: ChatMessage,
    kind: ReactionKind,
    trace?: SlackbotV2Trace
  ): SteeringReactionAck | undefined
  complete(ack: SteeringReactionAck, trace?: SlackbotV2Trace): Promise<void>
  completeThread(threadId: string, trace?: SlackbotV2Trace): Promise<void>
  hold(ack: SteeringReactionAck): void
}

export function createSteeringReactionController(
  options: SlackbotV2Options
): SteeringReactionController {
  // One controller owns both reactions so a missing reactions:write scope
  // disables both, and a thread's final render clears both.
  let scopeMissing = false
  const kinds: Record<ReactionKind, { enabled: boolean; name: string }> = {
    ack: {
      enabled: options.ackReactionEnabled === true,
      name: normalizeSlackReactionName(options.ackReactionName, 'eyes')
    },
    steering: {
      enabled: options.steeringReactionEnabled === true,
      name: normalizeSlackReactionName(options.steeringReactionName, 'hourglass_flowing_sand')
    }
  }
  // Deliberately process-local: reactions are best-effort UI state. A restart
  // may leave an old reaction behind instead of coupling render recovery to
  // Slack reaction bookkeeping.
  const pending = new Map<string, Map<string, SteeringReactionAck>>()

  const updateReaction = async (
    operation: 'add' | 'remove',
    channel: string,
    timestamp: string,
    name: string,
    trace?: SlackbotV2Trace
  ): Promise<boolean> => {
    // Once an add succeeds, still attempt its paired cleanup if another
    // concurrent acknowledgement has since disabled future adds.
    if (scopeMissing && operation === 'add') return false
    const method = operation === 'add' ? 'reactions.add' : 'reactions.remove'
    const startedAtMs = nowMs()
    try {
      const fetchFn = options.fetch ?? fetch
      const timeoutFetch = Object.assign(
        (input: RequestInfo | URL, init?: RequestInit) =>
          fetchWithTimeout(
            fetchFn,
            input,
            init ?? {},
            slackApiTimeoutMs(options),
            `Slack API ${method}`
          ),
        { preconnect: fetch.preconnect }
      )
      const payload = await callSlackApi(
        method,
        { channel, name, timestamp },
        {
          apiUrl: options.slackApiUrl,
          fetch: timeoutFetch,
          token: options.botToken
        }
      )
      const slackError = stringValue(payload.error)
      const idempotentSuccess =
        (operation === 'add' && slackError === 'already_reacted')
        || (operation === 'remove' && slackError === 'no_reaction')
      if (payload.ok === true || idempotentSuccess) {
        traceLog(options, 'slackbotv2_steering_reaction_complete', trace, {
          operation,
          phase_ms: elapsedMs(startedAtMs)
        })
        return true
      }
      if (slackError === 'missing_scope') {
        scopeMissing = true
        traceWarn(options, 'slackbotv2_steering_reaction_auto_disabled', trace, {
          error: slackError,
          needed: stringValue(payload.needed),
          operation
        })
        return false
      }
      traceWarn(options, 'slackbotv2_steering_reaction_failed', trace, {
        error: slackError ?? 'unknown Slack API error',
        operation,
        phase_ms: elapsedMs(startedAtMs)
      })
      return false
    } catch (error) {
      // Reaction acknowledgements are UI polish. Slack API failures must never
      // fail message persistence, steering, rendering, or webhook handling.
      traceWarn(options, 'slackbotv2_steering_reaction_failed', trace, {
        error: errorMessage(error),
        operation,
        phase_ms: elapsedMs(startedAtMs)
      })
      return false
    }
  }

  const completeAck = async (
    ack: SteeringReactionAck,
    trace?: SlackbotV2Trace
  ): Promise<void> => {
    if (!(await ack.added)) return
    await updateReaction('remove', ack.channel, ack.timestamp, ack.name, trace)
  }

  return {
    begin(thread, message, kind, trace) {
      const { enabled, name } = kinds[kind]
      if (!enabled || scopeMissing) return undefined
      const target = slackMessageReactionTarget(thread, message)
      if (!target) return undefined
      const added = updateReaction('add', target.channel, target.timestamp, name, trace)
      return {
        added,
        ...target,
        name,
        threadId: thread.id
      }
    },
    async complete(ack, trace) {
      const reactions = pending.get(ack.threadId)
      reactions?.delete(steeringReactionAckKey(ack))
      if (reactions?.size === 0) pending.delete(ack.threadId)
      await completeAck(ack, trace)
    },
    async completeThread(threadId, trace) {
      const reactions = Array.from(pending.get(threadId)?.values() ?? [])
      pending.delete(threadId)
      await Promise.all(reactions.map(ack => completeAck(ack, trace)))
    },
    hold(ack) {
      const reactions = pending.get(ack.threadId) ?? new Map<string, SteeringReactionAck>()
      reactions.set(steeringReactionAckKey(ack), ack)
      pending.set(ack.threadId, reactions)
    }
  }
}

function steeringReactionAckKey(ack: SteeringReactionAck): string {
  return `${ack.channel}:${ack.timestamp}`
}

function normalizeSlackReactionName(value: string | undefined, fallback: string): string {
  return value?.trim().replace(/^:+|:+$/g, '') || fallback
}

function slackMessageReactionTarget(
  thread: Thread,
  message: ChatMessage
): { channel: string; timestamp: string } | null {
  const parts = thread.id.split(':')
  if (parts[0] !== 'slack' || !parts[1] || !message.id) return null
  return { channel: parts[1], timestamp: message.id }
}
