import { randomUUID } from 'node:crypto'
import type { StateAdapter, Thread } from 'chat'
import { assertSlackOk, callSlackApi } from '@chat-adapter/slack/api'
import { fetchWithTimeout, interruptSessionExecution, slackApiTimeoutMs } from './session-api'
import type { SlackbotV2Options, SlackbotV2ThreadState } from './types'
import { isJsonObject, stringValue, traceLog, traceWarn, errorMessage } from './utils'
import { splitEnvList } from './slack-events'

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
    splitEnvList(process.env.SLACKBOT_EXTERNAL_ORG_ALLOWLIST)
  const allowed = teamId === homeTeamId || allowlist.includes(teamId)
  if (!allowed) traceLog(options, 'slackbotv2_agent_stop_external_user_ignored', undefined, {
    user_id: userId, team_id: teamId
  })
  return allowed
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

export type AgentStopIntent = {
  executionId?: string
  eventTs: string
  userId: string
  completed: boolean
}

export const agentStopIntentKey = (threadId: string) => `slackbotv2:agent-stop:intent:${threadId}`
const handoffKey = (threadId: string) => `slackbotv2:agent-stop:handoff:${threadId}`
const stoppedKey = (threadId: string, executionId: string) =>
  `slackbotv2:agent-stop:execution:${threadId}:${executionId}`
// Stop records cover the normal recovery window without growing thread state.
const STOP_TTL_MS = 30 * 24 * 60 * 60 * 1000
const STOP_POLL_MS = 250
const listeners = new WeakMap<StateAdapter, Map<string, Set<() => void>>>()

/** Track only a running handoff, not the delay before a scheduled retry. */
export async function beginAgentHandoff(
  state: StateAdapter, threadId: string, messageId: string
): Promise<() => Promise<void>> {
  const key = handoffKey(threadId)
  const token = randomUUID()
  await state.set(key, { token, messageId }, 300_000)
  return async () => {
    if ((await state.get<{ token: string }>(key))?.token === token) await state.delete(key)
  }
}

/** Stop intent has its own key: SDK thread state uses an unlocked read/merge/write. */
export async function stopAgentSession(
  thread: Thread<SlackbotV2ThreadState>,
  event: AgentStopEvent,
  state: StateAdapter,
  options: SlackbotV2Options
): Promise<boolean> {
  const leaseKey = `slackbotv2:agent-stop:${thread.id}`
  const token = randomUUID()
  // An early Stop and its execution commit can arrive together. Let the short
  // intent write finish before the commit delivers it.
  const deadline = Date.now() + 5_000
  while (!(await state.setIfNotExists(leaseKey, token, 300_000))) {
    if (Date.now() >= deadline) throw new Error('Agent stop is already being delivered')
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  try {
    const key = agentStopIntentKey(thread.id)
    const previous = await state.get<AgentStopIntent>(key)
    if (previous?.completed && Number(previous.eventTs) >= Number(event.event_ts)) return true
    const handoff = await state.get<{ messageId: string }>(handoffKey(thread.id))
    const latest = (await thread.state) ?? {}
    let obligation = latest.renderObligation
    if (previous?.executionId && !previous.completed && previous.executionId !== obligation?.executionId) {
      await state.delete(key)
      // This record belongs to an obsolete obligation. Recovery must proceed
      // with the current one, never deliver the obsolete interrupt against it.
      if (obligation && Number(event.event_ts) <= Number(previous.eventTs)) return false
    }
    if (obligation && Number(obligation.message.id) > Number(event.event_ts)) {
      if (previous && !previous.completed) await state.delete(key)
      return false
    }
    if (!obligation && (!handoff || Number(handoff.messageId) > Number(event.event_ts))) {
      await state.delete(key)
      if (!handoff) await resetIdleAgentStatus(thread.id, event, options)
      return true
    }
    const stop: AgentStopIntent = {
      executionId: obligation?.executionId,
      eventTs: event.event_ts,
      userId: event.user,
      completed: false
    }
    await state.set(key, stop, STOP_TTL_MS)
    // Close the intent/commit race: either the commit observes our intent or
    // this read observes its obligation. Neither writes over the other's key.
    obligation = (await thread.state)?.renderObligation
    if (!obligation) {
      if (!stop.executionId && await state.get(handoffKey(thread.id))) {
        throw new Error('Agent execution handoff is still pending')
      }
      await state.delete(key)
      await resetIdleAgentStatus(thread.id, event, options)
      return true
    }
    if (Number(obligation.message.id) > Number(stop.eventTs)) {
      await state.delete(key)
      return false
    }
    stop.executionId = obligation.executionId
    await state.set(key, stop, STOP_TTL_MS)
    const executionKey = stoppedKey(thread.id, stop.executionId)
    await state.set(executionKey, true, STOP_TTL_MS)
    for (const notify of listeners.get(state)?.get(executionKey) ?? []) notify()
    try {
      await interruptSessionExecution(options, thread.id, `Interrupted from Slack by ${stop.userId}`)
    } catch (error) {
      // A missing sandbox cannot be interrupted. Retire delivery and release
      // the thread anyway so the next mention can start a fresh execution.
      traceWarn(options, 'slackbotv2_agent_stop_interrupt_failed', undefined, {
        execution_id: stop.executionId, thread_id: thread.id, error: errorMessage(error)
      })
    }
    const current = (await thread.state) ?? {}
    if (current.renderObligation?.executionId === stop.executionId) {
      await thread.setState({
        activeExecution: false,
        lastEventId: current.lastEventId ?? obligation.afterEventId,
        renderObligation: null
      })
    }
    // Cleanup precedes Slack delivery: a Slack failure must not wedge execution.
    await resetAgentStatus(event, options)
    await state.set(key, { ...stop, completed: true }, STOP_TTL_MS)
    traceLog(options, 'slackbotv2_agent_session_stopped', undefined, {
      execution_id: stop.executionId, thread_id: thread.id
    })
    return true
  } finally {
    if (await state.get<string>(leaseKey) === token) await state.delete(leaseKey)
  }
}

async function resetIdleAgentStatus(
  threadId: string, event: AgentStopEvent, options: SlackbotV2Options
): Promise<void> {
  try {
    await resetAgentStatus(event, options)
  } catch (error) {
    traceWarn(options, 'slackbotv2_idle_agent_stop_status_failed', undefined, {
      thread_id: threadId, error: errorMessage(error)
    })
  }
}

async function resetAgentStatus(event: AgentStopEvent, options: SlackbotV2Options): Promise<void> {
  await agentSlackApi('agents.sessions.setStatus', {
    channel_id: event.channel, thread_ts: event.thread_ts, status: 'active'
  }, options)
}

export async function isAgentExecutionStopped(
  options: SlackbotV2Options, threadId: string, executionId: string | undefined
): Promise<boolean> {
  return Boolean(options.agentViewEnabled && executionId &&
    await options.state!.get(stoppedKey(threadId, executionId)))
}

/** Poll a small execution key at most four times a second, after conflation.
 * Local stops notify the stream immediately. No database read occurs per chunk.
 */
export async function* suppressStoppedExecution<T>(
  options: SlackbotV2Options,
  threadId: string,
  executionId: string | undefined,
  source: AsyncIterable<T>
): AsyncIterable<T> {
  if (!options.agentViewEnabled || !executionId) {
    yield* source
    return
  }
  const state = options.state!
  const key = stoppedKey(threadId, executionId)
  let stopped = false
  let nextPoll = 0
  const notify = () => { stopped = true }
  let byExecution = listeners.get(state)
  if (!byExecution) listeners.set(state, byExecution = new Map())
  let callbacks = byExecution.get(key)
  if (!callbacks) byExecution.set(key, callbacks = new Set())
  callbacks.add(notify)
  try {
    for await (const event of source) {
      if (!stopped && Date.now() >= nextPoll) {
        const persisted = Boolean(await state.get(key))
        stopped = stopped || persisted
        nextPoll = Date.now() + STOP_POLL_MS
      }
      if (stopped) throw new Error('Agent execution stopped')
      yield event
    }
  } finally {
    callbacks.delete(notify)
    if (callbacks.size === 0) byExecution.delete(key)
  }
}
