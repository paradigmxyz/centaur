import { AsyncLocalStorage } from 'node:async_hooks'
import { randomUUID } from 'node:crypto'
import { Hono } from 'hono'
import {
  Chat,
  StreamingPlan,
  type Adapter,
  type Logger,
  type Message as ChatMessage,
  type StateAdapter,
  type Thread
} from 'chat'
import { createSlackAdapter } from '@chat-adapter/slack'
import { createPostgresState } from '@chat-adapter/state-pg'
import {
  codexAppServerToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type RendererEvent
} from '@centaur/rendering'
import {
  collectInitialContext,
  forwardToSessionApi,
  isRetryableSessionApiError,
  openSessionEventStream,
  serializeMessage,
  sessionStreamError,
  startingStreamNotification
} from './session-api'
import { isAllowedSlackMessage, isAllowedSlackWebhookBody } from './slack-events'
import type {
  ForwardSessionInput,
  SlackbotV2,
  SlackbotV2ApiMessage,
  SlackbotV2MessageMode,
  SlackbotV2Options,
  SlackbotV2RenderObligation,
  SlackbotV2RendererSource,
  SlackbotV2ThreadState,
  SlackbotV2Trace
} from './types'
import { elapsedMs, errorMessage, noopLogger, nowMs, traceLog } from './utils'

export type {
  SlackbotV2,
  SlackbotV2ApiAttachment,
  SlackbotV2ApiAuthor,
  SlackbotV2ApiMessage,
  SlackbotV2AppendMessagesRequest,
  SlackbotV2CreateSessionRequest,
  SlackbotV2ExecuteSessionRequest,
  SlackbotV2Fetch,
  SlackbotV2Options,
  SlackbotV2SessionMessage,
  SlackbotV2SessionMessageRole
} from './types'

type WaitUntilContext = {
  waitUntil(promise: Promise<unknown>): void
}

type SlackAssistantAdapter = {
  setAssistantStatus?(
    channelId: string,
    threadTs: string,
    status: string,
    loadingMessages?: string[]
  ): Promise<void>
  setAssistantTitle?(channelId: string, threadTs: string, title: string): Promise<void>
}

type SlackbotV2RequestContext = {
  retryableErrors: unknown[]
  waitUntil(promise: Promise<unknown>): void
}

const requestContext = new AsyncLocalStorage<SlackbotV2RequestContext>()
const RENDER_OBLIGATION_INDEX_KEY = 'slackbotv2:render:index'
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000
const RENDER_INDEX_TTL_MS = 30 * 24 * 60 * 60 * 1000
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000

export function createSlackbotV2(options: SlackbotV2Options): SlackbotV2 {
  const userName = options.userName ?? 'centaur'
  const logger = options.logger ?? noopLogger
  const slack = createSlackAdapter({
    apiUrl: options.slackApiUrl,
    botToken: options.botToken,
    botUserId: options.botUserId,
    signingSecret: options.signingSecret,
    userName,
    logger
  })
  const state = options.state ?? createDefaultState(options, logger)
  const chat = new Chat<{ slack: typeof slack }, SlackbotV2ThreadState>({
    userName,
    adapters: { slack },
    state,
    onLockConflict: 'force',
    logger
  })

  chat.onNewMention(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    await thread.subscribe()
    await syncThreadMessageToSession(thread, message, {
      mode: 'execute',
      options,
      state
    })
  })

  chat.onSubscribedMessage(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    await syncThreadMessageToSession(thread, message, {
      mode: message.isMention === true ? 'execute' : 'append',
      options,
      state
    })
  })

  const app = new Hono()
  app.get('/health', c => c.json({ ok: true, service: 'slackbotv2' }))
  app.post('/api/webhooks/slack', async c => {
    const rawBody = await c.req.raw.clone().text()
    if (!isAllowedSlackWebhookBody(rawBody, options, logger)) {
      return new globalThis.Response('ok', { status: 200 })
    }
    const awaitHandoff = shouldAwaitSlackHandoff(rawBody)
    const handoffTasks: Promise<unknown>[] = []
    const context: SlackbotV2RequestContext = {
      retryableErrors: [],
      waitUntil: promise => waitUntil(c, promise)
    }
    const response = await requestContext.run(context, () => {
      return chat.webhooks.slack(c.req.raw, {
        waitUntil: promise => {
          if (awaitHandoff) {
            handoffTasks.push(promise)
          } else {
            waitUntil(c, promise)
          }
        }
      })
    })
    if (awaitHandoff && response.ok) {
      try {
        await Promise.all(handoffTasks)
      } catch (error) {
        if (isRetryableSessionApiError(error)) context.retryableErrors.push(error)
      }
      if (context.retryableErrors.length > 0) {
        traceLog(options, 'slackbotv2_webhook_retry_requested', undefined, {
          error: errorMessage(context.retryableErrors[0])
        })
        return new globalThis.Response('temporary upstream unavailable', { status: 503 })
      }
    }
    return new globalThis.Response(await response.text(), {
      headers: response.headers,
      status: response.status
    })
  })

  if (options.recoverRenderObligationsOnStart !== false) {
    scheduleRenderObligationRecovery(chat, state, options)
  }

  return { app, chat }
}

function createDefaultState(options: SlackbotV2Options, logger: Logger): StateAdapter {
  return createPostgresState({
    url: options.postgresUrl,
    keyPrefix: options.stateKeyPrefix ?? 'centaur-slackbotv2',
    logger: logger.child('postgres-state')
  })
}

/**
 * Persists a Slack thread update into the session API. In execute mode the create/append/execute
 * handoff completes before Slack is acknowledged; SSE rendering continues in background.
 */
async function syncThreadMessageToSession(
  thread: Thread<SlackbotV2ThreadState>,
  message: ChatMessage,
  input: {
    mode: SlackbotV2MessageMode
    options: SlackbotV2Options
    state: StateAdapter
  }
): Promise<void> {
  const traceStartedAtMs = nowMs()
  const state = (await thread.state) ?? {}
  const messageIds = new Set(state.forwardedMessageIds ?? [])
  const executedMessageIds = new Set(state.executedMessageIds ?? [])
  const shouldStartExecution =
    input.mode === 'execute' && state.activeExecution !== true && !executedMessageIds.has(message.id)
  const shouldIncludeContext = shouldStartExecution && state.historyForwarded !== true
  const isDuplicateIncrementalMessage =
    messageIds.has(message.id) && !shouldStartExecution && !shouldIncludeContext
  const trace: SlackbotV2Trace = {
    includeContext: shouldIncludeContext,
    messageId: message.id,
    mode: input.mode,
    openStream: shouldStartExecution,
    startedAtMs: traceStartedAtMs,
    threadId: thread.id
  }
  if (isDuplicateIncrementalMessage) {
    traceLog(input.options, 'slackbotv2_forward_duplicate_skipped', trace)
    return
  }
  traceLog(input.options, 'slackbotv2_forward_started', trace, {
    active_execution: state.activeExecution === true,
    history_forwarded: state.historyForwarded === true
  })

  const serializeStartedAtMs = nowMs()
  const serializedMessage = await serializeMessage(message)
  traceLog(input.options, 'slackbotv2_forward_message_serialized', trace, {
    attachment_count: serializedMessage.attachments.length,
    phase_ms: elapsedMs(serializeStartedAtMs)
  })
  let context: SlackbotV2ApiMessage[] | undefined

  if (shouldIncludeContext && !state.historyForwarded) {
    const contextStartedAtMs = nowMs()
    context = await collectInitialContext(thread, message)
    traceLog(input.options, 'slackbotv2_forward_context_collected', trace, {
      message_count: context.length,
      phase_ms: elapsedMs(contextStartedAtMs)
    })
  } else {
    traceLog(input.options, 'slackbotv2_forward_context_skipped', trace, {
      message_count: 1
    })
  }

  let lastEventId = state.lastEventId ?? 0
  const candidateMessages = context ?? [serializedMessage]
  const messagesToAppend = candidateMessages.filter(item => !messageIds.has(item.id))

  const forwardInput: ForwardSessionInput = {
    afterEventId: lastEventId,
    executeMessage: shouldStartExecution ? serializedMessage : undefined,
    messages: messagesToAppend,
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: false,
    threadId: thread.id,
    trace
  }

  const commitMessagesAppended = async (): Promise<void> => {
    const latest = (await thread.state) ?? {}
    const latestMessageIds = new Set(latest.forwardedMessageIds ?? [])
    for (const item of messagesToAppend) latestMessageIds.add(item.id)
    await thread.setState({
      forwardedMessageIds: Array.from(latestMessageIds).slice(-1000),
      historyForwarded: latest.historyForwarded || shouldIncludeContext,
      lastEventId
    })
    traceLog(input.options, 'slackbotv2_forward_messages_committed', trace, {
      appended_message_count: messagesToAppend.length,
      forwarded_message_count: Math.min(latestMessageIds.size, 1000)
    })
  }

  const commitExecutionStarted = async (): Promise<void> => {
    const latest = (await thread.state) ?? {}
    const latestExecutedMessageIds = new Set(latest.executedMessageIds ?? [])
    latestExecutedMessageIds.add(serializedMessage.id)
    await thread.setState({
      activeExecution: true,
      executedMessageIds: Array.from(latestExecutedMessageIds).slice(-1000),
      lastEventId,
      renderObligation: {
        afterEventId: lastEventId,
        message: serializedMessage
      }
    })
    await indexRenderObligation(input.state, {
      options: input.options,
      threadId: thread.id,
      trace
    })
    traceLog(input.options, 'slackbotv2_forward_execution_committed', trace, {
      executed_message_count: Math.min(latestExecutedMessageIds.size, 1000)
    })
  }

  if (!shouldStartExecution) {
    try {
      if (messagesToAppend.length > 0) {
        await forwardToSessionApi(input.options, forwardInput, {
          onMessagesAppended: commitMessagesAppended
        })
      }
    } catch (error) {
      if (await requestSlackRetry(input.state, message, error, input.options, trace)) {
        throw error
      }
      throw error
    }
    traceLog(input.options, 'slackbotv2_forward_complete', trace)
    return
  }

  try {
    await thread.setState({ activeExecution: true })
    traceLog(input.options, 'slackbotv2_forward_active_execution_marked', trace)
    await forwardToSessionApi(input.options, forwardInput, {
      onExecutionStarted: commitExecutionStarted,
      onMessagesAppended: commitMessagesAppended
    })
    scheduleExecutionRender(
      thread,
      serializedMessage,
      input.options,
      forwardInput,
      () => lastEventId,
      trace
    )
    traceLog(input.options, 'slackbotv2_forward_complete', trace, {
      last_event_id: lastEventId
    })
  } catch (error) {
    const latest = (await thread.state) ?? {}
    await thread.setState({
      activeExecution: false,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId)
    })
    if (await requestSlackRetry(input.state, message, error, input.options, trace)) {
      throw error
    }
    await renderExecutionStream(thread, streamError(error), serializedMessage, input.options, trace)
    traceLog(input.options, 'slackbotv2_forward_complete', trace, {
      latest_active_execution: latest.activeExecution === true,
      last_event_id: lastEventId
    })
  }
}

function scheduleExecutionRender(
  thread: Thread<SlackbotV2ThreadState>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  trace?: SlackbotV2Trace
): void {
  const promise = (async () => {
    let rendered = false
    try {
      await renderExecutionStream(
        thread,
        streamSessionAfterHandoff(options, input),
        message,
        options,
        trace
      )
      rendered = true
      traceLog(options, 'slackbotv2_render_complete', trace)
    } catch (error) {
      traceLog(options, 'slackbotv2_render_failed', trace, {
        error: errorMessage(error)
      })
      throw error
    } finally {
      const latest = (await thread.state) ?? {}
      await thread.setState({
        activeExecution: false,
        lastEventId: Math.max(latest.lastEventId ?? 0, getLastEventId()),
        ...(rendered ? { renderObligation: null } : {})
      })
      traceLog(options, 'slackbotv2_render_finalized', trace, {
        obligation_cleared: rendered,
        last_event_id: getLastEventId()
      })
    }
  })()
  backgroundWaitUntil(promise)
}

function scheduleRenderObligationRecovery(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options
): void {
  backgroundWaitUntil(
    recoverRenderObligations(chat, state, options).catch(error => {
      traceLog(options, 'slackbotv2_render_recovery_failed', undefined, {
        error: errorMessage(error)
      })
    })
  )
}

async function recoverRenderObligations(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options
): Promise<void> {
  const startedAtMs = nowMs()
  await chat.initialize()
  const indexedThreadIds = await state.getList<unknown>(RENDER_OBLIGATION_INDEX_KEY)
  const threadIds = Array.from(
    new Set(indexedThreadIds.filter((threadId): threadId is string => typeof threadId === 'string'))
  )
  traceLog(options, 'slackbotv2_render_recovery_scan', undefined, {
    obligation_count: threadIds.length,
    phase_ms: elapsedMs(startedAtMs)
  })

  for (const threadId of threadIds) {
    const thread = chat.thread(threadId)
    const threadState = await thread.state
    const obligation = threadState?.renderObligation
    if (!obligation) continue
    if (!canRecoverRenderObligation(obligation)) {
      traceLog(options, 'slackbotv2_render_recovery_invalid_obligation', undefined, {
        thread_id: threadId
      })
      continue
    }

    const leaseToken = randomUUID()
    const leaseAcquired = await state.setIfNotExists(
      renderRecoveryLeaseKey(threadId),
      leaseToken,
      RENDER_RECOVERY_LEASE_TTL_MS
    )
    if (!leaseAcquired) {
      traceLog(options, 'slackbotv2_render_recovery_lease_skipped', undefined, {
        thread_id: threadId
      })
      continue
    }

    try {
      await recoverRenderObligation(chat, state, options, threadId, obligation)
    } finally {
      const activeLeaseToken = await state.get<string>(renderRecoveryLeaseKey(threadId))
      if (activeLeaseToken === leaseToken) await state.delete(renderRecoveryLeaseKey(threadId))
    }
  }
}

async function recoverRenderObligation(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options,
  threadId: string,
  obligation: SlackbotV2RenderObligation
): Promise<void> {
  const trace: SlackbotV2Trace = {
    includeContext: false,
    messageId: obligation.message.id,
    mode: 'execute',
    openStream: true,
    startedAtMs: nowMs(),
    threadId
  }
  const thread = chat.thread(threadId)
  const threadState = (await thread.state) ?? {}
  let lastEventId = Math.max(threadState.lastEventId ?? 0, obligation.afterEventId)
  const input: ForwardSessionInput = {
    afterEventId: lastEventId,
    messages: [],
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: false,
    threadId,
    trace
  }

  let openedStream: AsyncIterable<SlackbotV2RendererSource>
  try {
    openedStream = await openSessionEventStream(options, input)
  } catch (error) {
    traceLog(options, 'slackbotv2_render_recovery_deferred', trace, {
      error: errorMessage(error),
      last_event_id: lastEventId
    })
    return
  }

  let rendered = false
  try {
    await thread.setState({
      activeExecution: true,
      lastEventId
    })
    await renderRecoveredExecutionStream(
      thread,
      streamOpenedSession(input, openedStream),
      obligation.message,
      options,
      trace
    )
    rendered = true
    traceLog(options, 'slackbotv2_render_recovery_complete', trace)
  } catch (error) {
    traceLog(options, 'slackbotv2_render_recovery_render_failed', trace, {
      error: errorMessage(error)
    })
    throw error
  } finally {
    const latest = (await thread.state) ?? {}
    await thread.setState({
      activeExecution: false,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId),
      ...(rendered ? { renderObligation: null } : {})
    })
    traceLog(options, 'slackbotv2_render_recovery_finalized', trace, {
      obligation_cleared: rendered,
      last_event_id: lastEventId
    })
  }
}

async function indexRenderObligation(
  state: StateAdapter,
  input: {
    options: SlackbotV2Options
    threadId: string
    trace?: SlackbotV2Trace
  }
): Promise<void> {
  await state.appendToList(RENDER_OBLIGATION_INDEX_KEY, input.threadId, {
    maxLength: RENDER_OBLIGATION_INDEX_MAX_LENGTH,
    ttlMs: RENDER_INDEX_TTL_MS
  })
  traceLog(input.options, 'slackbotv2_render_obligation_indexed', input.trace)
}

async function* streamOpenedSession(
  input: Pick<ForwardSessionInput, 'threadId' | 'trace'>,
  stream: AsyncIterable<SlackbotV2RendererSource>
): AsyncIterable<SlackbotV2RendererSource> {
  yield startingStreamNotification(input.threadId)
  for await (const event of stream) yield event
}

function renderRecoveryLeaseKey(threadId: string): string {
  return `slackbotv2:render:lease:${threadId}`
}

function canRecoverRenderObligation(value: SlackbotV2RenderObligation): boolean {
  const message = value.message
  return (
    typeof value.afterEventId === 'number' &&
    Boolean(message) &&
    typeof message.id === 'string' &&
    typeof message.teamId === 'string' &&
    typeof message.text === 'string' &&
    typeof message.threadId === 'string'
  )
}

async function renderExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<void> {
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(message.text, options.userName))
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...')
  traceLog(options, 'slackbotv2_render_slack_metadata_set', trace, {
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  try {
    await thread.post(
      new StreamingPlan(
        codexAppServerToChatSdkStream(stream, rendererOptions(thread, options)),
        { groupTasks: options.streamTaskDisplayMode ?? 'plan' }
      )
    )
  } finally {
    await setAssistantStatus(thread, '')
  }
}

async function renderRecoveredExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<void> {
  const recipient = slackStreamRecipient(message)
  if (!recipient) {
    throw new Error('Cannot recover Slack stream without recipient user/team context')
  }
  if (!thread.adapter.stream) {
    throw new Error('Cannot recover Slack stream because adapter streaming is unavailable')
  }

  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(message.text, options.userName))
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...')
  traceLog(options, 'slackbotv2_render_slack_metadata_set', trace, {
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  try {
    await thread.adapter.stream(
      thread.id,
      codexAppServerToChatSdkStream(stream, rendererOptions(thread, options)),
      {
        recipientTeamId: recipient.teamId,
        recipientUserId: recipient.userId,
        taskDisplayMode: options.streamTaskDisplayMode ?? 'plan'
      }
    )
  } finally {
    await setAssistantStatus(thread, '')
  }
}

function slackStreamRecipient(
  message: SlackbotV2ApiMessage
): { teamId: string; userId: string } | null {
  const teamId = message.teamId
  const userId = message.author.userId
  if (!teamId || !userId) return null
  return { teamId, userId }
}

async function* streamSessionAfterHandoff(
  options: SlackbotV2Options,
  input: ForwardSessionInput
): AsyncIterable<SlackbotV2RendererSource> {
  yield startingStreamNotification(input.threadId)
  traceLog(options, 'slackbotv2_stream_heartbeat_emitted', input.trace)

  try {
    const stream = await openSessionEventStream(options, input)
    for await (const event of stream) yield event
  } catch (error) {
    traceLog(options, 'slackbotv2_forward_failed', input.trace, {
      error: errorMessage(error)
    })
    yield sessionStreamError(error)
  }
}

async function* streamError(error: unknown): AsyncIterable<SlackbotV2RendererSource> {
  yield sessionStreamError(error)
}

async function requestSlackRetry(
  state: StateAdapter,
  message: ChatMessage,
  error: unknown,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<boolean> {
  if (!isRetryableSessionApiError(error)) return false
  const context = requestContext.getStore()
  if (!context) return false
  context.retryableErrors.push(error)
  try {
    await state.delete(`dedupe:slack:${message.id}`)
  } catch (deleteError) {
    traceLog(options, 'slackbotv2_webhook_retry_dedupe_clear_failed', trace, {
      error: errorMessage(deleteError)
    })
  }
  traceLog(options, 'slackbotv2_webhook_retry_marked', trace, {
    error: errorMessage(error)
  })
  return true
}

function backgroundWaitUntil(promise: Promise<unknown>): void {
  const context = requestContext.getStore()
  if (context) {
    context.waitUntil(promise)
    return
  }
  void promise.catch(() => undefined)
}

function shouldAwaitSlackHandoff(rawBody: string): boolean {
  try {
    const payload = JSON.parse(rawBody) as { event?: { type?: unknown }; type?: unknown }
    const eventType = payload.event?.type
    return payload.type === 'event_callback' && (eventType === 'message' || eventType === 'app_mention')
  } catch {
    return false
  }
}

function rendererOptions(thread: Thread, options: SlackbotV2Options): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper
  return {
    ...mapper,
    async onRendererEvent(event: RendererEvent) {
      await mapper?.onRendererEvent?.(event)
      if (event.type === 'renderer.title.update') {
        await setAssistantTitle(thread, event.title)
      }
    }
  }
}

async function setAssistantStatus(thread: Thread, status: string): Promise<void> {
  const target = slackAssistantTarget(thread)
  const adapter = thread.adapter as SlackAssistantAdapter
  if (!target || !adapter.setAssistantStatus) return
  await ignoreAssistantError(() =>
    adapter.setAssistantStatus!(
      target.channel,
      target.threadTs,
      status,
      status ? [status] : undefined
    )
  )
}

async function setAssistantTitle(thread: Thread, title: string | undefined): Promise<void> {
  const normalized = title?.trim()
  if (!normalized) return
  const target = slackAssistantTarget(thread)
  const adapter = thread.adapter as SlackAssistantAdapter
  if (!target || !adapter.setAssistantTitle) return
  await ignoreAssistantError(() =>
    adapter.setAssistantTitle!(target.channel, target.threadTs, clipOneLine(normalized, 80))
  )
}

async function ignoreAssistantError(fn: () => Promise<void>): Promise<void> {
  try {
    await fn()
  } catch {
    // Assistant status/title are Slack UI polish. Rendering should continue if unsupported.
  }
}

function slackAssistantTarget(thread: Thread): { channel: string; threadTs: string } | null {
  const parts = thread.id.split(':')
  if (parts[0] !== 'slack' || !parts[1] || !parts[2]) return null
  return { channel: parts[1], threadTs: parts[2] }
}

function titleFromMessage(text: string, userName = 'centaur'): string {
  const mentionless = text
    .replace(/<@[A-Z0-9]+(?:\|[^>]+)?>/g, '')
    .replace(new RegExp(`^\\s*@?${escapeRegExp(userName)}\\b[:,]?\\s*`, 'i'), '')
    .replace(/^@\S+\s+/, '')
    .trim()
  return clipOneLine(mentionless || 'Centaur task', 80)
}

function clipOneLine(value: string, max: number): string {
  const oneLine = value.replace(/\s+/g, ' ').trim()
  if (oneLine.length <= max) return oneLine
  return `${oneLine.slice(0, Math.max(0, max - 1)).trimEnd()}...`
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function waitUntil(c: { executionCtx: WaitUntilContext }, promise: Promise<unknown>): void {
  try {
    c.executionCtx.waitUntil(promise)
  } catch {
    void promise.catch(() => undefined)
  }
}
