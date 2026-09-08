import { randomUUID } from 'node:crypto'
import type { StateAdapter, Thread } from 'chat'
import { assertSlackOk, callSlackApi } from '@chat-adapter/slack/api'
import { fetchWithTimeout, interruptSessionExecution, slackApiTimeoutMs } from './session-api'
import type { SlackbotV2Options, SlackbotV2ThreadState } from './types'
import { isJsonObject, stringValue, traceLog } from './utils'

export type AgentStopEvent = {
  channel: string
  thread_ts: string
  event_ts: string
  user: string
}

export async function isAllowedAgentStopUser(
  userId: string,
  homeTeamId: string,
  options: SlackbotV2Options
): Promise<boolean> {
  // Native stop events omit the actor's team. Resolve it before applying the
  // same external-workspace policy as message events in shared channels.
  const result = await agentSlackApi('users.info', { user: userId }, options)
  const user = isJsonObject(result.user) ? result.user : undefined
  const teamId = stringValue(user?.team_id)
  if (!teamId || !homeTeamId) throw new Error('Could not resolve agent stop user workspace')
  if (user?.is_bot === true || user?.deleted === true) return false
  const allowlist = options.allowedExternalTeamIds ??
    (process.env.SLACKBOT_EXTERNAL_ORG_ALLOWLIST ?? '').split(',').map(value => value.trim())
  return teamId === homeTeamId || allowlist.includes(teamId)
}

async function agentSlackApi(
  method: string,
  args: Record<string, string>,
  options: SlackbotV2Options
) {
  const timeoutFetch = Object.assign(
    (input: RequestInfo | URL, init?: RequestInit) =>
      fetchWithTimeout(options.fetch ?? fetch, input, init ?? {}, slackApiTimeoutMs(options), method),
    { preconnect: fetch.preconnect }
  )
  const result = await callSlackApi(method, args, {
    token: options.botToken, apiUrl: options.slackApiUrl, fetch: timeoutFetch
  })
  assertSlackOk(method, result)
  return result
}

/**
 * Stop is a durable handoff, not a Chat SDK turn cancellation: rendering runs
 * after the SDK handler returns. Persist intent before interrupting so a crash
 * or a failed Slack stream cannot restart delivery of the stopped answer.
 */
export async function stopAgentSession(
  thread: Thread<SlackbotV2ThreadState>,
  event: AgentStopEvent,
  state: StateAdapter,
  options: SlackbotV2Options
): Promise<void> {
  const leaseKey = `slackbotv2:agent-stop:${thread.id}`
  const token = randomUUID()
  if (!(await state.setIfNotExists(leaseKey, token, 300_000))) {
    throw new Error('Agent stop is already being delivered')
  }
  try {
    const latest = (await thread.state) ?? {}
    const previous = latest.agentStop
    if (previous && Number(previous.eventTs) >= Number(event.event_ts) && previous.completed) return
    const obligation = latest.renderObligation
    // An old/redelivered stop must not cancel a follow-up execution. Slack
    // timestamps identify the initiating message even after a service restart.
    if (obligation && Number(obligation.message.id) > Number(event.event_ts)) return
    const stop = previous && !previous.completed &&
      (!obligation || Number(previous.eventTs) >= Number(obligation.message.id))
      ? { ...previous, executionId: previous.executionId ?? obligation?.executionId }
      : {
          executionId: obligation?.executionId,
          eventTs: event.event_ts,
          userId: event.user,
          completed: false
        }
    if (stop.executionId !== obligation?.executionId) return
    await thread.setState({
      agentStop: stop,
      ...(stop.executionId
        ? {
            stoppedExecutionIds: Array.from(new Set([
              ...(latest.stoppedExecutionIds ?? []), stop.executionId
            ])).slice(-1000)
          }
        : {})
    })
    // The loading indicator can appear before the execution handoff finishes.
    // Its commit callback will deliver this intent as soon as the ID is known.
    if (!stop.executionId) throw new Error('Agent execution handoff is still pending')
    await interruptSessionExecution(
      options,
      thread.id,
      `Interrupted from Slack by ${stop.userId}`
    )
    // Use the bounded API path. Slack's native stop also stops active streams,
    // but does not transition the session out of processing for the app.
    await agentSlackApi(
      'agents.sessions.setStatus',
      {
        channel_id: event.channel,
        thread_ts: event.thread_ts,
        status: 'active'
      },
      options
    )
    const current = (await thread.state) ?? {}
    await thread.setState({
      agentStop: { ...stop, completed: true },
      ...(current.renderObligation?.executionId === stop.executionId
        ? { activeExecution: false, renderObligation: null }
        : {})
    })
    traceLog(options, 'slackbotv2_agent_session_stopped', undefined, {
      execution_id: stop.executionId,
      thread_id: thread.id
    })
  } finally {
    if (await state.get<string>(leaseKey) === token) await state.delete(leaseKey)
  }
}

export async function isAgentExecutionStopped(
  thread: Thread<SlackbotV2ThreadState>,
  executionId: string | undefined
): Promise<boolean> {
  return Boolean(executionId && (await thread.state)?.stoppedExecutionIds?.includes(executionId))
}

export async function* suppressStoppedExecution<T>(
  thread: Thread<SlackbotV2ThreadState>,
  executionId: string | undefined,
  source: AsyncIterable<T>
): AsyncIterable<T> {
  for await (const event of source) {
    if (await isAgentExecutionStopped(thread, executionId)) throw new Error('Agent execution stopped')
    yield event
  }
}
