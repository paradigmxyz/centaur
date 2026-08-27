import { callSlackApi } from '@chat-adapter/slack/api'
import type { Message as ChatMessage, Thread } from 'chat'
import { fetchWithTimeout, slackApiTimeoutMs } from './session-api'
import type { SlackbotV2Options, SlackbotV2Trace } from './types'
import { elapsedMs, errorMessage, nowMs, stringValue, traceLog, traceWarn } from './utils'

export type SteeringReactionTarget = {
  channel: string
  timestamp: string
}

export type SteeringReactionAck = SteeringReactionTarget & {
  added: Promise<boolean>
}

export type SteeringReactionController = {
  begin(
    thread: Thread,
    message: ChatMessage,
    trace?: SlackbotV2Trace
  ): SteeringReactionAck | undefined
  complete(ack: SteeringReactionAck, trace?: SlackbotV2Trace): Promise<void>
  completeTargets(targets: readonly SteeringReactionTarget[], trace?: SlackbotV2Trace): Promise<void>
}

export function createSteeringReactionController(
  options: SlackbotV2Options
): SteeringReactionController {
  let enabled = options.steeringReactionEnabled === true
  const name = normalizeSlackReactionName(
    options.steeringReactionName ?? 'hourglass_flowing_sand'
  )
  const pendingAdds = new Map<string, Promise<boolean>>()

  const updateReaction = async (
    operation: 'add' | 'remove',
    channel: string,
    timestamp: string,
    trace?: SlackbotV2Trace
  ): Promise<boolean> => {
    // Once an add succeeds, still attempt its paired cleanup if another
    // concurrent acknowledgement has since disabled future adds.
    if (!enabled && operation === 'add') return false
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
        enabled = false
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

  const completeTarget = async (
    target: SteeringReactionTarget,
    trace?: SlackbotV2Trace
  ): Promise<void> => {
    const key = steeringReactionTargetKey(target)
    const added = pendingAdds.get(key)
    try {
      // In the live process, wait for a concurrent add before removing it. On
      // recovery after a restart there is no add promise, so remove the
      // persisted target directly.
      if (added && !(await added)) return
      await updateReaction('remove', target.channel, target.timestamp, trace)
    } finally {
      pendingAdds.delete(key)
    }
  }

  return {
    begin(thread, message, trace) {
      if (!enabled) return undefined
      const target = slackMessageReactionTarget(thread, message)
      if (!target) return undefined
      const added = updateReaction('add', target.channel, target.timestamp, trace)
      pendingAdds.set(steeringReactionTargetKey(target), added)
      return {
        added,
        ...target
      }
    },
    async complete(ack, trace) {
      await completeTarget(ack, trace)
    },
    async completeTargets(targets, trace) {
      await Promise.all(targets.map(target => completeTarget(target, trace)))
    }
  }
}

function steeringReactionTargetKey(target: SteeringReactionTarget): string {
  return `${target.channel}:${target.timestamp}`
}

function normalizeSlackReactionName(value: string): string {
  return value.trim().replace(/^:+|:+$/g, '') || 'hourglass_flowing_sand'
}

function slackMessageReactionTarget(
  thread: Thread,
  message: ChatMessage
): { channel: string; timestamp: string } | null {
  const parts = thread.id.split(':')
  if (parts[0] !== 'slack' || !parts[1] || !message.id) return null
  return { channel: parts[1], timestamp: message.id }
}
