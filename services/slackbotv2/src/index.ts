import { AsyncLocalStorage } from 'node:async_hooks'
import { randomUUID } from 'node:crypto'
import { Hono, type Context } from 'hono'
import {
  Chat,
  Message as ChatSdkMessage,
  parseMarkdown,
  type Adapter,
  type Attachment,
  type Logger,
  type Message as ChatMessage,
  type StateAdapter,
  type Thread
} from 'chat'
import { createSlackAdapter } from '@chat-adapter/slack'
import { fetchSlackThreadReplies } from '@chat-adapter/slack/api'
import { createPostgresState } from '@chat-adapter/state-pg'
import pg from 'pg'
import {
  codexAppServerToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type ChatSDKStreamChunk,
  type RendererEvent
} from '@centaur/rendering'
import { conflateChatSdkStream } from './conflate'
import { observeSeconds, slackbotMetrics } from './metrics'
import { renderSlackDisplayText, slackMessagePromptText } from './slack-display-text'
import { slackUserIdForMessage } from './slack-user'
import {
  collectInitialContext,
  forwardToSessionApi,
  harnessRestartPreamble,
  isRetryableSessionApiError,
  openSessionEventStream,
  serializeAttachment,
  serializeMessageLinks,
  serializeMessage,
  sessionStreamError,
  withSlackApiTimeout
} from './session-api'
import { extractMessageOverrides } from './overrides'
import { isAllowedSlackMessage, isAllowedSlackWebhookBody } from './slack-events'
import type {
  ForwardSessionInput,
  JsonObject,
  SlackbotV2,
  SlackbotV2ApiAttachment,
  SlackbotV2ApiMessage,
  SlackbotV2ExecuteSessionResponse,
  SlackbotV2MessageMode,
  SlackbotV2Options,
  SlackbotV2RenderObligation,
  SlackbotV2RendererSource,
  SlackbotV2ThreadState,
  SlackbotV2Trace
} from './types'
import {
  elapsedMs,
  errorMessage,
  isJsonObject,
  noopLogger,
  nowMs,
  startPendingOperationLog,
  stringValue,
  traceLog,
  traceWarn
} from './utils'

export type {
  SlackbotV2,
  SlackbotV2ApiAttachment,
  SlackbotV2ApiAuthor,
  SlackbotV2ApiMessage,
  SlackbotV2AppendMessagesRequest,
  SlackbotV2CreateSessionRequest,
  SlackbotV2ExecuteSessionRequest,
  SlackbotV2ExecuteSessionResponse,
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

const MAX_SLACK_MESSAGE_ATTACHMENTS = 20

type SlackbotV2RequestContext = {
  retryableErrors: unknown[]
  waitUntil(promise: Promise<unknown>): void
}

const requestContext = new AsyncLocalStorage<SlackbotV2RequestContext>()
const RENDER_OBLIGATION_INDEX_KEY = 'slackbotv2:render:index'
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000
const RENDER_INDEX_TTL_MS = 30 * 24 * 60 * 60 * 1000
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000
const RENDER_LEASE_REFRESH_INTERVAL_MS = 60 * 1000
const RENDER_RECOVERY_MAX_OBLIGATION_AGE_MS = 24 * 60 * 60 * 1000
const RENDER_RECOVERY_THREAD_TIMEOUT_MS = 2 * 60 * 1000
const RENDER_RECOVERY_MAX_THREAD_FAILURES = 5
const RENDER_RETRY_INITIAL_DELAY_MS = 250
const RENDER_RETRY_MAX_DELAY_MS = 5_000
const ASSISTANT_STATUS_MAX_CHARS = 50
const SLACK_TASK_DETAILS_MAX_CHARS = 500
const SLACK_FALLBACK_TEXT_MAX_CHARS = 35_000
const POSTGRES_CONNECT_INITIAL_DELAY_MS = 250
const POSTGRES_CONNECT_MAX_DELAY_MS = 10_000
const LATE_SLACK_FILE_MATCH_WINDOW_MS = 15_000
const LATE_SLACK_FILE_PENDING_TTL_MS = 60_000
const LATE_SLACK_FILE_CONSUMED_TTL_MS = 5 * 60_000
const LATE_SLACK_FILE_IDLE_WAIT_MS = 90_000
const LATE_SLACK_FILE_IDLE_POLL_MS = 500
const LATE_SLACK_FILE_MESSAGE_TEXT = 'Late Slack file attachment for the previous message.'

type PendingLateSlackFileMention = {
  channel: string
  message: ChatMessage
  mentionTs: string
  teamId: string
  thread: Thread<SlackbotV2ThreadState>
  user: string
}

type StickyThreadOverrides = Pick<SlackbotV2ThreadState, 'harnessType' | 'model' | 'provider'>

function stickyThreadOverrideUpdate(
  overrides: StickyThreadOverrides
): StickyThreadOverrides | undefined {
  const update: StickyThreadOverrides = {}
  if (overrides.harnessType) {
    update.harnessType = overrides.harnessType
    if (!overrides.model) update.model = null
    if (!overrides.provider) update.provider = null
  }
  if (overrides.model) update.model = overrides.model
  if (overrides.provider) {
    update.provider = overrides.provider
    if (!overrides.model) update.model = null
  }
  return Object.keys(update).length > 0 ? update : undefined
}

function resolveStickyThreadOverrides(
  state: SlackbotV2ThreadState,
  update: StickyThreadOverrides | undefined
): {
  harnessType?: string
  model?: string
  provider?: string
} {
  return {
    harnessType: stickyOverrideValue(state, update, 'harnessType'),
    model: stickyOverrideValue(state, update, 'model'),
    provider: stickyOverrideValue(state, update, 'provider')
  }
}

function stickyOverrideValue(
  state: SlackbotV2ThreadState,
  update: StickyThreadOverrides | undefined,
  key: keyof StickyThreadOverrides
): string | undefined {
  if (update && Object.prototype.hasOwnProperty.call(update, key)) return stringValue(update[key])
  return stringValue(state[key])
}

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
  const lateSlackFiles = createLateSlackFileRepair(options, state)

  chat.onNewMention(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    lateSlackFiles.rememberFilelessMention(thread, message)
    await handleSlackMessageHandoff(thread, message, {
      assistantStatusRequested: true,
      mode: 'execute',
      options,
      state,
      subscribe: true,
      trigger: 'new_mention'
    })
  })

  chat.onSubscribedMessage(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    lateSlackFiles.rememberFilelessMention(thread, message)
    await handleSlackMessageHandoff(thread, message, {
      assistantStatusRequested: message.isMention === true,
      mode: message.isMention === true ? 'execute' : 'append',
      options,
      state,
      trigger: 'subscribed_message'
    })
  })

  const app = new Hono()
  app.get('/health', c => c.json({ ok: true, service: 'slackbotv2' }))
  app.get('/metrics', c =>
    c.text(slackbotMetrics.expose(), 200, {
      'Content-Type': 'text/plain; version=0.0.4; charset=utf-8'
    })
  )
  const handleSlackWebhook = async (c: Context) => {
    const webhookStartedAtMs = nowMs()
    const route = c.req.path
    const rawBody = await c.req.raw.clone().text()
    const eventType = slackWebhookEventType(rawBody)
    let outcome = 'success'
    try {
      if (!isAllowedSlackWebhookBody(rawBody, options, logger)) {
        outcome = 'ignored'
        return new globalThis.Response('ok', { status: 200 })
      }
      const awaitHandoff = shouldAwaitSlackHandoff(rawBody)
      const webhookFields = slackWebhookLogFields(rawBody)
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
        const waitStartedAtMs = nowMs()
        const waitFields = {
          ...webhookFields,
          response_status: response.status,
          task_count: handoffTasks.length
        }
        traceLog(options, 'slackbotv2_webhook_handoff_wait_started', undefined, waitFields)
        const stopPendingLog = startPendingOperationLog(
          options,
          'slackbotv2_webhook_handoff_wait_pending',
          undefined,
          waitFields,
          waitStartedAtMs
        )
        let waitError: unknown
        try {
          await Promise.all(handoffTasks)
        } catch (error) {
          waitError = error
          if (isRetryableSessionApiError(error)) context.retryableErrors.push(error)
        } finally {
          stopPendingLog()
          traceLog(options, 'slackbotv2_webhook_handoff_wait_complete', undefined, {
            ...waitFields,
            error: waitError ? errorMessage(waitError) : undefined,
            phase_ms: elapsedMs(waitStartedAtMs),
            retryable_error_count: context.retryableErrors.length
          })
        }
        if (context.retryableErrors.length > 0) {
          outcome = 'retry_requested'
          slackbotMetrics.webhookRetryRequests.inc()
          traceLog(options, 'slackbotv2_webhook_retry_requested', undefined, {
            error: errorMessage(context.retryableErrors[0])
          })
          return new globalThis.Response('temporary upstream unavailable', { status: 503 })
        }
      }
      const lateFileTask = lateSlackFiles.repairFromWebhook(rawBody)
      if (lateFileTask) waitUntil(c, lateFileTask)
      outcome = response.ok ? 'success' : 'error'
      return new globalThis.Response(await response.text(), {
        headers: response.headers,
        status: response.status
      })
    } catch (error) {
      outcome = 'error'
      throw error
    } finally {
      slackbotMetrics.webhookRequests.inc({ event_type: eventType, outcome, route })
      slackbotMetrics.webhookDuration.observe(
        { event_type: eventType, outcome, route },
        observeSeconds(webhookStartedAtMs)
      )
    }
  }
  app.post('/api/webhooks/slack', handleSlackWebhook)
  app.post('/api/slack/events', handleSlackWebhook)

  if (options.recoverRenderObligationsOnStart !== false) {
    scheduleRenderObligationRecovery(chat, state, options)
  }

  return { app, chat }
}

async function handleSlackMessageHandoff(
  thread: Thread<SlackbotV2ThreadState>,
  message: ChatMessage,
  input: {
    assistantStatusRequested: boolean
    mode: SlackbotV2MessageMode
    options: SlackbotV2Options
    state: StateAdapter
    subscribe?: boolean
    trigger: string
  }
): Promise<void> {
  const trace = createHandoffTrace(thread, message, input.mode)
  traceLog(input.options, 'slackbotv2_handoff_started', trace, {
    assistant_status_requested: input.assistantStatusRequested,
    subscribe: input.subscribe === true,
    trigger: input.trigger
  })
  let initialAssistantStatusVisible = false
  const assistantStatus = input.assistantStatusRequested
    ? setInitialAssistantStatus(thread, input.options, trace)
        .then(visible => {
          initialAssistantStatusVisible = visible
          return visible
        })
    : Promise.resolve(false)
  if (input.assistantStatusRequested) {
    backgroundWaitUntil(assistantStatus.then(() => undefined).catch(() => undefined))
  }
  try {
    if (input.subscribe) {
      await subscribeSlackThreadForHandoff(thread, input.options, trace, input.trigger)
    }
    traceLog(input.options, 'slackbotv2_handoff_sync_starting', trace, {
      initial_assistant_status_deferred:
        input.assistantStatusRequested && !initialAssistantStatusVisible,
      initial_assistant_status_visible: initialAssistantStatusVisible,
      trigger: input.trigger
    })
    await syncThreadMessageToSession(thread, message, {
      initialAssistantStatusRequested: input.assistantStatusRequested,
      initialAssistantStatusVisible,
      mode: input.mode,
      options: input.options,
      state: input.state
    })
    traceLog(input.options, 'slackbotv2_handoff_complete', trace, {
      trigger: input.trigger
    })
  } catch (error) {
    traceWarn(input.options, 'slackbotv2_handoff_failed', trace, {
      error: errorMessage(error),
      trigger: input.trigger
    })
    backgroundWaitUntil(
      assistantStatus
        .then(visible =>
          visible ? setAssistantStatus(thread, '', input.options, trace) : undefined
        )
        .then(() => undefined)
        .catch(() => undefined)
    )
    throw error
  }
}

async function subscribeSlackThreadForHandoff(
  thread: Thread<SlackbotV2ThreadState>,
  options: SlackbotV2Options,
  trace: SlackbotV2Trace,
  trigger: string
): Promise<void> {
  const startedAtMs = nowMs()
  const fields = { trigger }
  traceLog(options, 'slackbotv2_handoff_subscribe_started', trace, fields)
  const stopPendingLog = startPendingOperationLog(
    options,
    'slackbotv2_handoff_subscribe_pending',
    trace,
    fields,
    startedAtMs
  )
  try {
    await thread.subscribe()
    traceLog(options, 'slackbotv2_handoff_subscribe_complete', trace, {
      ...fields,
      phase_ms: elapsedMs(startedAtMs)
    })
  } catch (error) {
    traceWarn(options, 'slackbotv2_handoff_subscribe_failed', trace, {
      ...fields,
      error: errorMessage(error),
      phase_ms: elapsedMs(startedAtMs)
    })
    throw error
  } finally {
    stopPendingLog()
  }
}

function createHandoffTrace(
  thread: Thread<SlackbotV2ThreadState>,
  message: ChatMessage,
  mode: SlackbotV2MessageMode
): SlackbotV2Trace {
  return {
    includeContext: mode === 'execute',
    messageId: message.id,
    mode,
    openStream: mode === 'execute',
    slackUserId: slackUserIdForMessage(message),
    startedAtMs: nowMs(),
    threadId: thread.id
  }
}

function slackWebhookEventType(rawBody: string): string {
  try {
    const payload = JSON.parse(rawBody)
    if (!isJsonObject(payload)) return 'unknown'
    const event = payload.event
    if (isJsonObject(event)) return stringValue(event.type) ?? 'unknown'
    return stringValue(payload.type) ?? 'unknown'
  } catch {
    return 'invalid_json'
  }
}

function recordForward(
  mode: SlackbotV2MessageMode,
  outcome: string,
  startedAtMs: number
): void {
  slackbotMetrics.forwardMessages.inc({ mode, outcome })
  slackbotMetrics.forwardDuration.observe({ mode, outcome }, observeSeconds(startedAtMs))
}

function recordRenderAttempt(source: string, outcome: string, startedAtMs: number): void {
  slackbotMetrics.renderAttempts.inc({ outcome, source })
  slackbotMetrics.renderAttemptDuration.observe({ outcome, source }, observeSeconds(startedAtMs))
  slackbotMetrics.sessionDelivery.inc({ delivery_status: deliveryStatusForRenderOutcome(outcome) })
  if (outcome === 'complete' || outcome === 'fallback' || outcome === 'answer_visible') {
    slackbotMetrics.lastSuccessfulRenderTimestamp.set(
      { source },
      Math.floor(Date.now() / 1000)
    )
  }
}

function deliveryStatusForRenderOutcome(outcome: string): string {
  switch (outcome) {
    case 'complete':
      return 'streamed'
    case 'fallback':
      return 'fallback_sent'
    case 'answer_visible':
      return 'answer_visible'
    case 'retry':
      return 'deferred'
    case 'stream_error_rendered':
      return 'error_visible'
    case 'size_limit_no_replacement':
      return 'failed_size_limit'
    default:
      return 'failed'
  }
}

function recordRecoveryScan(
  outcome: string,
  startedAtMs: number,
  counts: { deferred: number; indexedThreads: number; pending: number }
): void {
  slackbotMetrics.renderRecoveryScans.inc({ outcome })
  slackbotMetrics.renderRecoveryScanDuration.observe({ outcome }, observeSeconds(startedAtMs))
  slackbotMetrics.renderRecoveryObligations.set(
    { state: 'indexed_threads' },
    counts.indexedThreads
  )
  slackbotMetrics.renderRecoveryObligations.set({ state: 'pending' }, counts.pending)
  slackbotMetrics.renderRecoveryObligations.set({ state: 'deferred' }, counts.deferred)
}

function recordRecoveryThreadEvent(event: string): void {
  slackbotMetrics.renderRecoveryThreadEvents.inc({ event })
}

function recordFallback(outcome: string, startedAtMs: number): void {
  slackbotMetrics.renderFallbacks.inc({ outcome })
  slackbotMetrics.renderFallbackDuration.observe({ outcome }, observeSeconds(startedAtMs))
  if (outcome === 'complete') {
    slackbotMetrics.lastSuccessfulRenderTimestamp.set(
      { source: 'fallback' },
      Math.floor(Date.now() / 1000)
    )
  }
}

function createDefaultState(options: SlackbotV2Options, logger: Logger): StateAdapter {
  const stateLogger = logger.child('postgres-state')
  // Own the pool so we can attach an error handler. pg.Pool emits 'error' for
  // idle clients whose connection drops (Postgres restart, or a transient blip
  // while the pod's network is still being programmed at startup). With no
  // listener, node-postgres rethrows it as an uncaught exception and the process
  // crashes/spews. Logging and swallowing lets the pool reconnect on the next query.
  const pool = new pg.Pool({ connectionString: options.postgresUrl })
  pool.on('error', error => {
    stateLogger.warn('postgres pool error', { error: errorMessage(error) })
  })
  return createPostgresState({
    client: pool,
    keyPrefix: options.stateKeyPrefix ?? 'centaur-slackbotv2',
    logger: stateLogger
  })
}

/**
 * Blocks until the state backend accepts a connection, retrying with exponential
 * backoff. The first DB connection fires within milliseconds of process start and
 * can lose a race with the pod's network programming (a one-off ECONNREFUSED).
 * Retrying instead of throwing absorbs that race; the first successful connect
 * also flips the adapter's `connected` flag, so the message path comes alive too.
 */
async function ensureStateConnected(state: StateAdapter, options: SlackbotV2Options): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      await state.connect()
      if (attempt > 0) {
        traceLog(options, 'slackbotv2_postgres_connected', undefined, { attempts: attempt + 1 })
      }
      return
    } catch (error) {
      const delayMs = Math.min(
        POSTGRES_CONNECT_INITIAL_DELAY_MS * 2 ** attempt,
        POSTGRES_CONNECT_MAX_DELAY_MS
      )
      traceLog(options, 'slackbotv2_postgres_connect_retry', undefined, {
        attempt: attempt + 1,
        delay_ms: delayMs,
        error: errorMessage(error)
      })
      await sleep(delayMs)
    }
  }
}

/**
 * Persists a Slack thread update into the session API. In execute mode the create/append/execute
 * handoff completes before Slack is acknowledged; SSE rendering continues in background.
 */
async function syncThreadMessageToSession(
  thread: Thread<SlackbotV2ThreadState>,
  message: ChatMessage,
  input: {
    initialAssistantStatusRequested?: boolean
    initialAssistantStatusVisible?: boolean
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
  const shouldRefreshThreadContext = shouldStartExecution && isSlackThreadReply(message)
  const shouldIncludeContext =
    shouldStartExecution && (state.historyForwarded !== true || shouldRefreshThreadContext)
  const isDuplicateIncrementalMessage =
    messageIds.has(message.id) && !shouldStartExecution && !shouldIncludeContext
  const trace: SlackbotV2Trace = {
    includeContext: shouldIncludeContext,
    messageId: message.id,
    mode: input.mode,
    openStream: shouldStartExecution,
    slackUserId: slackUserIdForMessage(message),
    startedAtMs: traceStartedAtMs,
    threadId: thread.id
  }
  if (isDuplicateIncrementalMessage) {
    traceLog(input.options, 'slackbotv2_forward_duplicate_skipped', trace)
    if (input.initialAssistantStatusVisible) {
      await setAssistantStatus(thread, '', input.options, trace)
    }
    recordForward(input.mode, 'duplicate_skipped', traceStartedAtMs)
    return
  }
  traceLog(input.options, 'slackbotv2_forward_started', trace, {
    active_execution: state.activeExecution === true,
    history_forwarded: state.historyForwarded === true
  })
  const assistantStatusVisible = shouldStartExecution
    ? input.initialAssistantStatusVisible === true ||
      input.initialAssistantStatusRequested === true
    : false
  if (shouldStartExecution && input.initialAssistantStatusVisible === undefined) {
    backgroundWaitUntil(
      setInitialAssistantStatus(thread, input.options, trace)
        .then(() => undefined)
        .catch(() => undefined)
    )
  }
  if (!shouldStartExecution && input.initialAssistantStatusVisible) {
    await setAssistantStatus(thread, '', input.options, trace)
  }

  const serializeStartedAtMs = nowMs()
  const serializedMessage = await serializeMessage(message, input.options)
  const overrides = extractMessageOverrides(serializedMessage.text)
  setMessageText(serializedMessage, overrides.cleanedText)
  const stickyOverridesUpdate = stickyThreadOverrideUpdate(overrides)
  const effectiveOverrides = resolveStickyThreadOverrides(state, stickyOverridesUpdate)
  if (overrides.harnessType || overrides.model || overrides.provider || overrides.reasoning) {
    traceLog(input.options, 'slackbotv2_forward_overrides_parsed', trace, {
      harness_type: overrides.harnessType,
      model: overrides.model,
      provider: overrides.provider,
      reasoning: overrides.reasoning
    })
  }
  traceLog(input.options, 'slackbotv2_forward_message_serialized', trace, {
    attachment_count: serializedMessage.attachments.length,
    raw_slack_attachment_count: serializedMessage.rawSlackAttachmentCount,
    raw_slack_block_count: serializedMessage.rawSlackBlockCount,
    slack_display_text_chars: slackMessagePromptText(serializedMessage).length,
    slack_text_source: serializedMessage.displayTextSource,
    phase_ms: elapsedMs(serializeStartedAtMs)
  })
  let context: SlackbotV2ApiMessage[] | undefined
  let contextDegraded = false

  if (shouldIncludeContext) {
    const contextStartedAtMs = nowMs()
    try {
      context = shouldRefreshThreadContext
        ? await withSlackApiTimeout(input.options, 'collect Slack thread context', () =>
            collectSlackThreadContext(input.options, message)
          )
        : await withSlackApiTimeout(input.options, 'collect initial thread context', () =>
            collectInitialContext(thread, message, input.options)
          )
    } catch (error) {
      contextDegraded = true
      context = [serializedMessage]
      traceWarn(input.options, 'slackbotv2_forward_context_degraded', trace, {
        error: errorMessage(error),
        phase_ms: elapsedMs(contextStartedAtMs)
      })
    }
    // collectInitialContext re-serializes the current message; mirror the
    // flag-stripped text on that copy too.
    for (const item of context) {
      if (item.id === serializedMessage.id) copyMessageTextFields(item, serializedMessage)
    }
    traceLog(input.options, 'slackbotv2_forward_context_collected', trace, {
      degraded: contextDegraded,
      message_count: context.length,
      phase_ms: elapsedMs(contextStartedAtMs)
    })
  } else {
    traceLog(input.options, 'slackbotv2_forward_context_skipped', trace, {
      message_count: 1
    })
  }

  let lastEventId = state.lastEventId ?? 0
  const renderLease: { release: (() => Promise<void>) | null } = { release: null }
  const candidateMessages = context ?? [serializedMessage]
  const messagesToAppend = candidateMessages.filter(item => !messageIds.has(item.id))

  const forwardInput: ForwardSessionInput = {
    afterEventId: lastEventId,
    executeContextMessages:
      shouldStartExecution && shouldIncludeContext ? candidateMessages : undefined,
    executeMessage: shouldStartExecution ? serializedMessage : undefined,
    // Sticky harness changes only apply when a message starts an execution;
    // restarting the thread out from under an active execution would kill it.
    harnessType: shouldStartExecution ? effectiveOverrides.harnessType : undefined,
    messages: messagesToAppend,
    model: shouldStartExecution ? effectiveOverrides.model : undefined,
    provider: shouldStartExecution ? effectiveOverrides.provider : undefined,
    reasoning: overrides.reasoning,
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: false,
    threadId: thread.id,
    trace
  }

  // The previous harness's conversation state dies with its sandbox on a
  // restart, so re-feed the Slack thread transcript with this turn.
  const handleSessionRestarted = async (): Promise<void> => {
    let history = context
    let restartContextDegraded = contextDegraded
    if (!history) {
      const restartContextStartedAtMs = nowMs()
      try {
        history = await withSlackApiTimeout(
          input.options,
          'collect restart thread context',
          () => collectInitialContext(thread, message, input.options)
        )
      } catch (error) {
        restartContextDegraded = true
        history = [serializedMessage]
        traceWarn(input.options, 'slackbotv2_forward_restart_context_degraded', trace, {
          error: errorMessage(error),
          phase_ms: elapsedMs(restartContextStartedAtMs)
        })
      }
    }
    forwardInput.contextPreamble = harnessRestartPreamble(history, serializedMessage.id)
    traceLog(input.options, 'slackbotv2_forward_restart_context_built', trace, {
      degraded: restartContextDegraded,
      history_message_count: history.length,
      preamble_chars: forwardInput.contextPreamble?.length ?? 0
    })
  }

  const commitMessagesAppended = async (): Promise<void> => {
    const latest = (await thread.state) ?? {}
    const latestMessageIds = new Set(latest.forwardedMessageIds ?? [])
    for (const item of messagesToAppend) latestMessageIds.add(item.id)
    await thread.setState({
      ...(stickyOverridesUpdate ?? {}),
      forwardedMessageIds: Array.from(latestMessageIds).slice(-1000),
      historyForwarded: latest.historyForwarded || (shouldIncludeContext && !contextDegraded),
      lastEventId
    })
    traceLog(input.options, 'slackbotv2_forward_messages_committed', trace, {
      appended_message_count: messagesToAppend.length,
      forwarded_message_count: Math.min(latestMessageIds.size, 1000)
    })
  }

  const commitExecutionStarted = async (
    execution: SlackbotV2ExecuteSessionResponse
  ): Promise<void> => {
    const latest = (await thread.state) ?? {}
    const latestExecutedMessageIds = new Set(latest.executedMessageIds ?? [])
    latestExecutedMessageIds.add(serializedMessage.id)
    forwardInput.executionId = execution.execution_id
    // Take the render lease before the obligation becomes visible so a
    // concurrent recovery sweep never claims it while this process is about
    // to render it live.
    try {
      renderLease.release = await acquireRenderLease(input.state, thread.id)
    } catch (error) {
      traceLog(input.options, 'slackbotv2_render_lease_acquire_failed', trace, {
        error: errorMessage(error)
      })
    }
    await thread.setState({
      ...(stickyOverridesUpdate ?? {}),
      activeExecution: true,
      executedMessageIds: Array.from(latestExecutedMessageIds).slice(-1000),
      lastEventId,
      renderObligation: {
        afterEventId: lastEventId,
        executionId: execution.execution_id,
        message: serializedMessage
      }
    })
    await indexRenderObligation(input.state, {
      options: input.options,
      threadId: thread.id,
      trace
    })
    traceLog(input.options, 'slackbotv2_forward_execution_committed', trace, {
      execution_id: execution.execution_id,
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
      if (isRetryableSessionApiError(error)) {
        const context = requestContext.getStore()
        if (context) {
          context.retryableErrors.push(error)
          try {
            await input.state.delete(`dedupe:slack:${message.id}`)
          } catch (deleteError) {
            traceLog(input.options, 'slackbotv2_webhook_retry_dedupe_clear_failed', trace, {
              error: errorMessage(deleteError)
            })
          }
          traceLog(input.options, 'slackbotv2_webhook_retry_marked', trace, {
            error: errorMessage(error)
          })
        }
      }
      recordForward(
        input.mode,
        isRetryableSessionApiError(error) ? 'retry_requested' : 'error',
        traceStartedAtMs
      )
      throw error
    }
    traceLog(input.options, 'slackbotv2_forward_complete', trace)
    recordForward(input.mode, 'complete', traceStartedAtMs)
    return
  }

  try {
    await thread.setState({ activeExecution: true })
    traceLog(input.options, 'slackbotv2_forward_active_execution_marked', trace)
    await forwardToSessionApi(input.options, forwardInput, {
      onExecutionStarted: commitExecutionStarted,
      onMessagesAppended: commitMessagesAppended,
      onSessionRestarted: handleSessionRestarted
    })
    scheduleExecutionRender(
      thread,
      serializedMessage,
      input.options,
      forwardInput,
      () => lastEventId,
      renderLease,
      assistantStatusVisible,
      trace
    )
    traceLog(input.options, 'slackbotv2_forward_complete', trace, {
      last_event_id: lastEventId
    })
    recordForward(input.mode, 'complete', traceStartedAtMs)
  } catch (error) {
    // The live render is not happening; let the recovery sweep claim the
    // obligation (if one was committed) as soon as it scans.
    await renderLease.release?.()
    const latest = (await thread.state) ?? {}
    await thread.setState({
      activeExecution: false,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId)
    })
    if (isRetryableSessionApiError(error)) {
      const context = requestContext.getStore()
      if (context) {
        context.retryableErrors.push(error)
        try {
          await input.state.delete(`dedupe:slack:${message.id}`)
        } catch (deleteError) {
          traceLog(input.options, 'slackbotv2_webhook_retry_dedupe_clear_failed', trace, {
            error: errorMessage(deleteError)
          })
        }
        traceLog(input.options, 'slackbotv2_webhook_retry_marked', trace, {
          error: errorMessage(error)
        })
        if (assistantStatusVisible) await setAssistantStatus(thread, '', input.options, trace)
        recordForward(input.mode, 'retry_requested', traceStartedAtMs)
        throw error
      }
    }
    try {
      await renderExecutionStream(
        thread,
        streamError(error),
        serializedMessage,
        input.options,
        trace,
        assistantStatusVisible
      )
    } catch (renderError) {
      // The error notice is best-effort; a Slack render failure here must not
      // mask the original forward failure.
      traceLog(input.options, 'slackbotv2_forward_error_notice_render_failed', trace, {
        error: errorMessage(renderError)
      })
    }
    traceLog(input.options, 'slackbotv2_forward_complete', trace, {
      latest_active_execution: latest.activeExecution === true,
      last_event_id: lastEventId
    })
    recordForward(input.mode, 'error_notice_rendered', traceStartedAtMs)
  }
}

function scheduleExecutionRender(
  thread: Thread<SlackbotV2ThreadState>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  renderLease: { release: (() => Promise<void>) | null },
  assistantStatusVisible: boolean,
  trace?: SlackbotV2Trace
): void {
  const promise = (async () => {
    slackbotMetrics.activeLiveRenders.inc()
    try {
      let attempt = 0
      while (true) {
        const result = await renderExecutionAttempt(
          thread,
          message,
          options,
          input,
          getLastEventId,
          assistantStatusVisible,
          trace
        )
        if (result === 'complete') return
        const delayMs = renderRetryDelayMs(attempt)
        attempt += 1
        traceLog(options, 'slackbotv2_render_retry_scheduled', trace, {
          retry_delay_ms: delayMs,
          retry_attempt: attempt
        })
        await sleep(delayMs)
      }
    } finally {
      slackbotMetrics.activeLiveRenders.dec()
      await renderLease.release?.()
    }
  })()
  backgroundWaitUntil(promise)
}

function setMessageText(message: SlackbotV2ApiMessage, text: string): void {
  const displayText = renderSlackDisplayText({ raw: message.raw, text })
  message.text = text
  message.displayText = displayText.text
  message.displayTextSource = displayText.source
  message.rawSlackAttachmentCount = displayText.rawAttachmentCount
  message.rawSlackBlockCount = displayText.rawBlockCount
}

function copyMessageTextFields(target: SlackbotV2ApiMessage, source: SlackbotV2ApiMessage): void {
  target.text = source.text
  target.displayText = source.displayText
  target.displayTextSource = source.displayTextSource
  target.rawSlackAttachmentCount = source.rawSlackAttachmentCount
  target.rawSlackBlockCount = source.rawSlackBlockCount
}

async function renderExecutionAttempt(
  thread: Thread<SlackbotV2ThreadState>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  assistantStatusVisible: boolean,
  trace?: SlackbotV2Trace
): Promise<'complete' | 'retry'> {
  const renderStartedAtMs = nowMs()
  let outcome = 'failure'
  let rendered = false
  let retry = false
  let fallbackLastEventId = 0
  try {
    const streamResult = await renderExecutionStream(
      thread,
      streamSessionAfterHandoff(options, input),
      message,
      options,
      trace,
      assistantStatusVisible
    )
    rendered = true
    outcome = 'complete'
    let divergenceReconciled = false
    if (streamResult.diverged && streamResult.messageId) {
      // The live answer stream diverged from the recomposed answer, so the delta
      // stream was frozen at the last clean prefix to avoid interleaving. Swap
      // the (possibly truncated) streamed message for the durable, de-duplicated
      // final answer so the user sees the complete response instead of a message
      // that looks cut off. Reuses the final-answer fallback, which derives the
      // answer from the terminal result rather than the doubled live buffer.
      const reconciled = await renderFallbackFinalAnswer(
        thread,
        options,
        {
          afterEventId: input.afterEventId,
          executionId: input.executionId,
          threadId: input.threadId
        },
        trace,
        { replaceMessageId: streamResult.messageId }
      )
      if (reconciled) {
        divergenceReconciled = true
        fallbackLastEventId = reconciled.lastEventId
      }
    }
    traceLog(options, 'slackbotv2_render_complete', trace, {
      answer_diverged: streamResult.diverged,
      divergence_reconciled: divergenceReconciled
    })
    return 'complete'
  } catch (error) {
    // Check the Slack adapter's delivery annotation before retryability:
    // Slack network failures can surface as TypeError/AbortError, which would
    // otherwise be misclassified as retryable session API errors and re-render
    // the whole stream instead of posting the durable final answer.
    const answerLost = slackAnswerLost(error)
    if (answerLost === undefined && isRetryableSessionApiError(error)) {
      retry = true
      outcome = 'retry'
      traceLog(
        options,
        'slackbotv2_render_deferred',
        trace,
        {
          error: errorMessage(error),
          last_event_id: getLastEventId()
        },
        'warn'
      )
      return 'retry'
    }
    if (answerLost === false) {
      // The Slack stream broke only after the final answer became visible
      // (for example a progress-card stop failed). Reposting would duplicate
      // the answer, so record the failure and finish.
      rendered = true
      outcome = 'answer_visible'
      traceLog(
        options,
        'slackbotv2_render_failed_answer_visible',
        trace,
        {
          error: errorMessage(error)
        },
        'warn'
      )
      return 'complete'
    }
    traceLog(
      options,
      'slackbotv2_render_failed',
      trace,
      {
        error: errorMessage(error),
        slack_answer_lost: answerLost ?? 'unknown'
      },
      'warn'
    )
    const replaceMessageId = isSlackStreamSizeLimitError(error)
      ? slackStreamMessageId(error)
      : undefined
    if (isSlackStreamSizeLimitError(error) && !replaceMessageId) {
      // Size-limit failures should be prevented by stream segmentation. If
      // Slack still rejects a stream as too large but does not expose the
      // failed stream message id, do not post a separate duplicate fallback.
      rendered = true
      outcome = 'size_limit_no_replacement'
      traceLog(
        options,
        'slackbotv2_render_failed_size_limit_no_replacement',
        trace,
        {
          error: errorMessage(error),
          slack_answer_lost: answerLost ?? 'unknown'
        },
        'warn'
      )
      return 'complete'
    }
    const fallback = await renderFallbackFinalAnswer(
      thread,
      options,
      {
        afterEventId: input.afterEventId,
        executionId: input.executionId,
        threadId: input.threadId
      },
      trace,
      replaceMessageId ? { replaceMessageId } : undefined
    )
    if (fallback) {
      rendered = true
      outcome = 'fallback'
      fallbackLastEventId = fallback.lastEventId
      return 'complete'
    }
    throw error
  } finally {
    const latest = (await thread.state) ?? {}
    await thread.setState({
      activeExecution: retry,
      lastEventId: Math.max(latest.lastEventId ?? 0, getLastEventId(), fallbackLastEventId),
      ...(rendered ? { renderObligation: null } : {})
    })
    traceLog(options, 'slackbotv2_render_finalized', trace, {
      obligation_cleared: rendered,
      render_duration_ms: elapsedMs(renderStartedAtMs),
      retry_scheduled: retry,
      last_event_id: getLastEventId()
    })
    recordRenderAttempt('live', outcome, renderStartedAtMs)
  }
}

/**
 * Reads the delivery annotation the Slack chat adapter attaches to streaming
 * errors. `false` means the stream's final answer was confirmed visible before
 * the failure; `true` means it was definitely not; `undefined` means the error
 * did not come through the adapter's streaming path.
 */
function slackAnswerLost(error: unknown): boolean | undefined {
  if (!error || typeof error !== 'object') return undefined
  const value = (error as { slackAnswerLost?: unknown }).slackAnswerLost
  return typeof value === 'boolean' ? value : undefined
}

function isSlackStreamSizeLimitError(error: unknown): boolean {
  const code = slackStreamErrorCode(error)
  return code.includes('msg_too_long') || code.includes('msg_blocks_too_long')
}

function slackStreamMessageId(error: unknown): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  const value = (error as { slackStreamMessageId?: unknown }).slackStreamMessageId
  return typeof value === 'string' && value.length > 0 ? value : undefined
}

function slackStreamErrorCode(error: unknown): string {
  if (!error || typeof error !== 'object') return typeof error === 'string' ? error : ''
  const record = error as Record<string, unknown>
  if (typeof record.error === 'string') return record.error
  const data = record.data
  if (data && typeof data === 'object' && !Array.isArray(data)) {
    const dataError = (data as Record<string, unknown>).error
    if (typeof dataError === 'string') return dataError
  }
  return typeof record.message === 'string' ? record.message : ''
}

const FALLBACK_OPEN_MAX_ATTEMPTS = 4

/**
 * Delivers the durable final answer as a plain thread post after the live
 * Slack streaming render failed. Replays the session event stream from the
 * execution's starting position (the control plane keeps the events durably,
 * so the terminal result is replayable even when the failed render already
 * consumed it), drains it without making Slack calls, and posts the terminal
 * result text once. Slack streaming is best-effort; this is the delivery
 * guarantee. Returns null when nothing could be delivered.
 */
async function renderFallbackFinalAnswer(
  thread: Thread,
  options: SlackbotV2Options,
  source: { afterEventId: number; executionId?: string; threadId: string },
  trace?: SlackbotV2Trace,
  replacement?: { replaceMessageId: string }
): Promise<{ lastEventId: number } | null> {
  const startedAtMs = nowMs()
  let outcome = 'error'
  let lastEventId = source.afterEventId
  try {
    let stream: AsyncIterable<SlackbotV2RendererSource> | undefined
    for (let attempt = 0; ; attempt++) {
      try {
        stream = await openSessionEventStream(options, {
          afterEventId: source.afterEventId,
          executionId: source.executionId,
          onEventId: eventId => {
            lastEventId = Math.max(lastEventId, eventId)
          },
          threadId: source.threadId,
          trace
        })
        break
      } catch (error) {
        if (!isRetryableSessionApiError(error) || attempt + 1 >= FALLBACK_OPEN_MAX_ATTEMPTS) {
          throw error
        }
        await sleep(renderRetryDelayMs(attempt))
      }
    }
    const fallback = new SlackRenderFallback()
    const chatStream = fallback.collectChatSdk(
      slackSafeChatSdkStream(
        codexAppServerToChatSdkStream(
          fallback.collectSource(stream),
          fallbackRendererOptions(options)
        )
      )
    )
    for await (const _chunk of chatStream) {
      void _chunk
    }
    const text = fallback.text()
    if (!text) {
      outcome = 'empty'
      traceLog(options, 'slackbotv2_render_fallback_empty', trace, {
        last_event_id: lastEventId,
        phase_ms: elapsedMs(startedAtMs)
      })
      return null
    }
    const fallbackText = truncateSlackText(text, SLACK_FALLBACK_TEXT_MAX_CHARS, 'Slack final answer')
    if (replacement) {
      await thread.adapter.editMessage(thread.id, replacement.replaceMessageId, fallbackText)
    } else {
      await thread.post(fallbackText)
    }
    traceLog(options, 'slackbotv2_render_fallback_complete', trace, {
      chars: text.length,
      last_event_id: lastEventId,
      replacement_message_id: replacement?.replaceMessageId,
      phase_ms: elapsedMs(startedAtMs)
    })
    outcome = 'complete'
    return { lastEventId }
  } catch (error) {
    outcome = 'error'
    traceLog(
      options,
      'slackbotv2_render_fallback_failed',
      trace,
      {
        error: errorMessage(error),
        phase_ms: elapsedMs(startedAtMs)
      },
      'error'
    )
    return null
  } finally {
    recordFallback(outcome, startedAtMs)
  }
}

function scheduleRenderObligationRecovery(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options
): void {
  backgroundWaitUntil(
    recoverRenderObligationsWithRetry(chat, state, options)
  )
}

async function recoverRenderObligationsWithRetry(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options
): Promise<void> {
  // Wait for Postgres before scanning for obligations. This is also what warms the
  // shared pool at startup, so transient connect failures don't wedge the bot.
  await ensureStateConnected(state, options)
  const failureCounts = new Map<string, number>()
  let attempt = 0
  while (true) {
    try {
      const deferredCount = await recoverRenderObligations(chat, state, options, failureCounts)
      if (deferredCount === 0) return
      const delayMs = renderRetryDelayMs(attempt)
      attempt += 1
      recordRenderRecoveryRetry(options, { attempt, deferredCount, delayMs })
      await sleep(delayMs)
    } catch (error) {
      recordRecoveryScan('error', nowMs(), {
        deferred: 0,
        indexedThreads: 0,
        pending: 0
      })
      traceLog(
        options,
        'slackbotv2_render_recovery_failed',
        undefined,
        {
          error: errorMessage(error)
        },
        'error'
      )
      return
    }
  }
}

async function recoverRenderObligations(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options,
  failureCounts: Map<string, number>
): Promise<number> {
  const startedAtMs = nowMs()
  await chat.initialize()
  const indexedThreadIds = await state.getList<string>(RENDER_OBLIGATION_INDEX_KEY)
  const threadIds = Array.from(new Set(indexedThreadIds))
  const maxObligationAgeMs =
    options.renderRecoveryMaxObligationAgeMs ?? RENDER_RECOVERY_MAX_OBLIGATION_AGE_MS
  const timeoutMs = options.renderRecoveryThreadTimeoutMs ?? RENDER_RECOVERY_THREAD_TIMEOUT_MS
  let abandonedCount = 0
  let activeObligationCount = 0
  let deferredCount = 0
  let failedCount = 0
  let leaseSkippedCount = 0
  let resolvedCount = 0
  let retryableDeferredCount = 0
  let staleSkippedCount = 0
  let timedOutCount = 0
  traceLog(options, 'slackbotv2_render_recovery_scan', undefined, {
    indexed_thread_count: threadIds.length,
    obligation_count: threadIds.length,
    phase_ms: elapsedMs(startedAtMs)
  })

  for (const threadId of threadIds) {
    try {
      const thread = chat.thread(threadId)
      const threadState = await thread.state
      const obligation = threadState?.renderObligation
      if (!obligation) continue
      activeObligationCount += 1

      const obligationAgeMs = renderObligationAgeMs(obligation)
      if (obligationAgeMs !== undefined && obligationAgeMs > maxObligationAgeMs) {
        staleSkippedCount += 1
        recordRecoveryThreadEvent('stale_skipped')
        traceLog(options, 'slackbotv2_render_recovery_stale_obligation_skipped', undefined, {
          ...renderObligationFields(obligation),
          max_obligation_age_ms: maxObligationAgeMs,
          thread_id: threadId
        })
        await thread.setState({
          activeExecution: false,
          lastEventId: threadState?.lastEventId ?? 0,
          renderObligation: null
        })
        continue
      }

      // An obligation that keeps failing non-retryably (for example corrupt
      // state that can never address a Slack thread) must not poison the
      // retry loop forever: give up on it and unwedge the thread.
      if ((failureCounts.get(threadId) ?? 0) >= RENDER_RECOVERY_MAX_THREAD_FAILURES) {
        abandonedCount += 1
        recordRecoveryThreadEvent('abandoned')
        traceLog(
          options,
          'slackbotv2_render_recovery_abandoned',
          undefined,
          {
            ...renderObligationFields(obligation),
            failure_count: failureCounts.get(threadId),
            thread_id: threadId
          },
          'error'
        )
        await thread.setState({
          activeExecution: false,
          lastEventId: threadState?.lastEventId ?? 0,
          renderObligation: null
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
        // Another holder (or a lease from a crashed pass, pending TTL expiry)
        // owns this thread. Count it as deferred so the retry loop keeps
        // running until the obligation is actually resolved.
        deferredCount += 1
        leaseSkippedCount += 1
        recordRecoveryThreadEvent('lease_skipped')
        traceLog(options, 'slackbotv2_render_recovery_lease_skipped', undefined, {
          thread_id: threadId
        })
        continue
      }
      const releaseLease = async (): Promise<void> => {
        const activeLeaseToken = await state.get<string>(renderRecoveryLeaseKey(threadId))
        if (activeLeaseToken === leaseToken) await state.delete(renderRecoveryLeaseKey(threadId))
      }

      // A single hung recovery (for example an event stream that never
      // produces a chunk) must not block every obligation queued behind it.
      // Race a deadline; on timeout move on and leave the attempt running
      // detached - it may still finish and clear the obligation, which is why
      // the lease is kept so a later pass does not start a duplicate render.
      const recovery = recoverRenderObligation(chat, state, options, threadId, obligation)
      let outcome: { timedOut: true } | { timedOut: false; deferred: boolean }
      try {
        outcome = await Promise.race([
          recovery.then(deferred => ({ timedOut: false as const, deferred })),
          sleep(timeoutMs).then(() => ({ timedOut: true as const }))
        ])
      } catch (error) {
        await releaseLease()
        throw error
      }
      if (outcome.timedOut) {
        void recovery.catch(() => undefined)
        deferredCount += 1
        timedOutCount += 1
        // Count timeouts toward the abandonment budget: an obligation whose
        // recovery hangs on every claim (for example an event stream that
        // never yields) would otherwise keep the sweep loop spinning forever,
        // racing every live render in the process.
        failureCounts.set(threadId, (failureCounts.get(threadId) ?? 0) + 1)
        recordRecoveryThreadEvent('timeout')
        traceLog(
          options,
          'slackbotv2_render_recovery_thread_timeout',
          undefined,
          {
            ...renderObligationFields(obligation),
            failure_count: failureCounts.get(threadId),
            thread_id: threadId,
            timeout_ms: timeoutMs
          },
          'warn'
        )
        continue
      }
      await releaseLease()
      if (outcome.deferred) {
        deferredCount += 1
        retryableDeferredCount += 1
        recordRecoveryThreadEvent('deferred')
      } else {
        resolvedCount += 1
        recordRecoveryThreadEvent('complete')
      }
    } catch (error) {
      // One thread's corrupt state or failed render must not abort the scan:
      // log it, count it as deferred so a later pass retries it (up to the
      // failure budget above), and keep recovering the remaining threads.
      failureCounts.set(threadId, (failureCounts.get(threadId) ?? 0) + 1)
      deferredCount += 1
      failedCount += 1
      recordRecoveryThreadEvent('failed')
      traceLog(
        options,
        'slackbotv2_render_recovery_thread_failed',
        undefined,
        {
          error: errorMessage(error),
          failure_count: failureCounts.get(threadId),
          thread_id: threadId
        },
        'warn'
      )
    }
  }
  recordRenderRecoveryScan(options, {
    abandonedCount,
    activeObligationCount,
    deferredCount,
    failedCount,
    indexedThreadCount: threadIds.length,
    leaseSkippedCount,
    phaseMs: elapsedMs(startedAtMs),
    resolvedCount,
    retryableDeferredCount,
    staleSkippedCount,
    timedOutCount
  })
  recordRecoveryScan(deferredCount > 0 ? 'deferred' : 'complete', startedAtMs, {
    deferred: deferredCount,
    indexedThreads: threadIds.length,
    pending: activeObligationCount
  })
  return deferredCount
}

async function recoverRenderObligation(
  chat: Chat<Record<string, Adapter>, SlackbotV2ThreadState>,
  state: StateAdapter,
  options: SlackbotV2Options,
  threadId: string,
  obligation: SlackbotV2RenderObligation
): Promise<boolean> {
  const trace: SlackbotV2Trace = {
    includeContext: false,
    messageId: obligation.message.id,
    mode: 'execute',
    openStream: true,
    startedAtMs: nowMs(),
    threadId
  }
  const thread = chat.thread(threadId)
  // Replay from the obligation's starting position, not the thread's
  // lastEventId: the failed render may have consumed events (including the
  // terminal result) past which a resumed stream would never see the final
  // answer again. Session events are durable, so a full replay is safe.
  let lastEventId = obligation.afterEventId
  const input: ForwardSessionInput = {
    afterEventId: obligation.afterEventId,
    executionId: obligation.executionId,
    messages: [],
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: false,
    threadId,
    trace
  }
  const renderStartedAtMs = nowMs()
  let renderOutcome = 'failure'

  let openedStream: AsyncIterable<SlackbotV2RendererSource>
  try {
    openedStream = await openSessionEventStream(options, input)
  } catch (error) {
    const retryable = isRetryableSessionApiError(error)
    traceLog(options, 'slackbotv2_render_recovery_deferred', trace, {
      error: errorMessage(error),
      last_event_id: lastEventId,
      retryable
    })
    if (retryable) {
      renderOutcome = 'deferred'
      recordRenderAttempt('recovery', renderOutcome, renderStartedAtMs)
      return true
    }
    await renderRecoveredExecutionStream(thread, streamError(error), obligation.message, options, trace)
    await thread.setState({
      activeExecution: false,
      lastEventId,
      renderObligation: null
    })
    renderOutcome = 'stream_error_rendered'
    recordRenderAttempt('recovery', renderOutcome, renderStartedAtMs)
    return false
  }

  let rendered = false
  try {
    await thread.setState({
      activeExecution: true,
      lastEventId
    })
    const streamResult = await renderRecoveredExecutionStream(
      thread,
      streamOpenedSession(input, openedStream),
      obligation.message,
      options,
      trace
    )
    rendered = true
    renderOutcome = 'complete'
    let divergenceReconciled = false
    if (streamResult.diverged && streamResult.messageId) {
      // Same divergence reconcile as the live path: the answer stream was
      // frozen at the last clean prefix, so swap the streamed message for the
      // durable, de-duplicated final answer instead of leaving it truncated.
      const reconciled = await renderFallbackFinalAnswer(
        thread,
        options,
        {
          afterEventId: obligation.afterEventId,
          executionId: obligation.executionId,
          threadId
        },
        trace,
        { replaceMessageId: streamResult.messageId }
      )
      if (reconciled) {
        divergenceReconciled = true
        lastEventId = Math.max(lastEventId, reconciled.lastEventId)
      }
    }
    traceLog(options, 'slackbotv2_render_recovery_complete', trace, {
      answer_diverged: streamResult.diverged,
      divergence_reconciled: divergenceReconciled
    })
  } catch (error) {
    const answerLost = slackAnswerLost(error)
    if (answerLost === false) {
      // The recovered stream broke only after the final answer became
      // visible; reposting would duplicate it.
      rendered = true
      renderOutcome = 'answer_visible'
      traceLog(options, 'slackbotv2_render_recovery_failed_answer_visible', trace, {
        error: errorMessage(error)
      })
    } else {
      traceLog(
        options,
        'slackbotv2_render_recovery_render_failed',
        trace,
        {
          error: errorMessage(error),
          slack_answer_lost: answerLost ?? 'unknown'
        },
        'warn'
      )
      const replaceMessageId = isSlackStreamSizeLimitError(error)
        ? slackStreamMessageId(error)
        : undefined
      if (isSlackStreamSizeLimitError(error) && !replaceMessageId) {
        // Size-limit failures should be prevented by stream segmentation. If
        // Slack still rejects a stream as too large but does not expose the
        // failed stream message id, do not post a separate duplicate fallback.
        rendered = true
        renderOutcome = 'size_limit_no_replacement'
        traceLog(
          options,
          'slackbotv2_render_recovery_failed_size_limit_no_replacement',
          trace,
          {
            error: errorMessage(error),
            slack_answer_lost: answerLost ?? 'unknown'
          },
          'warn'
        )
        return false
      }
      const fallback = await renderFallbackFinalAnswer(
        thread,
        options,
        {
          afterEventId: obligation.afterEventId,
          executionId: obligation.executionId,
          threadId
        },
        trace,
        replaceMessageId ? { replaceMessageId } : undefined
      )
      if (!fallback) throw error
      rendered = true
      renderOutcome = 'fallback'
      lastEventId = Math.max(lastEventId, fallback.lastEventId)
    }
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
    recordRenderAttempt('recovery', renderOutcome, renderStartedAtMs)
  }
  return false
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
  slackbotMetrics.renderObligationsIndexed.inc()
  traceLog(input.options, 'slackbotv2_render_obligation_indexed', input.trace)
}

async function* streamOpenedSession(
  _input: Pick<ForwardSessionInput, 'threadId' | 'trace'>,
  stream: AsyncIterable<SlackbotV2RendererSource>
): AsyncIterable<SlackbotV2RendererSource> {
  for await (const event of stream) yield event
}

function renderRecoveryLeaseKey(threadId: string): string {
  return `slackbotv2:render:lease:${threadId}`
}

function recordRenderRecoveryRetry(
  options: SlackbotV2Options,
  observation: { attempt: number; deferredCount: number; delayMs: number }
): void {
  const fields = {
    deferred_count: observation.deferredCount,
    retry_delay_ms: observation.delayMs,
    retry_attempt: observation.attempt
  }
  traceLog(options, 'slackbotv2_render_recovery_retry_scheduled', undefined, fields)
}

function recordRenderRecoveryScan(
  options: SlackbotV2Options,
  observation: {
    abandonedCount: number
    activeObligationCount: number
    deferredCount: number
    failedCount: number
    indexedThreadCount: number
    leaseSkippedCount: number
    phaseMs: number
    resolvedCount: number
    retryableDeferredCount: number
    staleSkippedCount: number
    timedOutCount: number
  }
): void {
  const fields = {
    abandoned_count: observation.abandonedCount,
    active_obligation_count: observation.activeObligationCount,
    deferred_count: observation.deferredCount,
    failed_count: observation.failedCount,
    indexed_thread_count: observation.indexedThreadCount,
    lease_skipped_count: observation.leaseSkippedCount,
    phase_ms: observation.phaseMs,
    resolved_count: observation.resolvedCount,
    retryable_deferred_count: observation.retryableDeferredCount,
    stale_skipped_count: observation.staleSkippedCount,
    timed_out_count: observation.timedOutCount
  }
  traceLog(options, 'slackbotv2_render_recovery_scan_complete', undefined, fields)
}

function renderObligationAgeMs(obligation: SlackbotV2RenderObligation): number | undefined {
  const messageTimestampMs = Date.parse(obligation.message.timestamp)
  return Number.isFinite(messageTimestampMs)
    ? Math.max(0, Date.now() - messageTimestampMs)
    : undefined
}

function renderObligationFields(obligation: SlackbotV2RenderObligation): JsonObject {
  const obligationAgeMs = renderObligationAgeMs(obligation)
  return {
    after_event_id: obligation.afterEventId,
    execution_id: obligation.executionId,
    message_id: obligation.message.id,
    message_timestamp: obligation.message.timestamp,
    ...(obligationAgeMs !== undefined ? { obligation_age_ms: obligationAgeMs } : {})
  }
}

/**
 * Holds the per-thread render lease for the duration of a live render so the
 * recovery sweep cannot claim the just-indexed obligation and post a
 * duplicate answer (it lease-skips instead). The TTL keeps this crash-safe:
 * if the pod dies mid-render the lease expires and recovery takes over. The
 * lease is refreshed while the render runs because agent turns routinely
 * outlive a single TTL window.
 */
async function acquireRenderLease(
  state: StateAdapter,
  threadId: string
): Promise<() => Promise<void>> {
  const key = renderRecoveryLeaseKey(threadId)
  const token = randomUUID()
  await state.set(key, token, RENDER_RECOVERY_LEASE_TTL_MS)
  const refresh = setInterval(() => {
    void state
      .get<string>(key)
      .then(current =>
        current === token ? state.set(key, token, RENDER_RECOVERY_LEASE_TTL_MS) : undefined
      )
      .catch(() => undefined)
  }, RENDER_LEASE_REFRESH_INTERVAL_MS)
  return async () => {
    clearInterval(refresh)
    try {
      const current = await state.get<string>(key)
      if (current === token) await state.delete(key)
    } catch {
      // Best effort: TTL expiry is the backstop.
    }
  }
}

async function renderExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace,
  assistantStatusVisible = false
): Promise<{ diverged: boolean; messageId?: string }> {
  const promptText = slackMessagePromptText(message)
  if (isPlainTextOnlyRequest(promptText)) {
    await renderPlainTextExecutionStream(
      thread,
      stream,
      message,
      options,
      trace,
      assistantStatusVisible
    )
    return { diverged: false }
  }
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(promptText, options.userName), options, trace)
  if (!assistantStatusVisible) {
    await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...', options, trace)
  }
  traceLog(options, 'slackbotv2_render_slack_metadata_set', trace, {
    assistant_status_already_visible: assistantStatusVisible,
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  const capture = { diverged: false }
  try {
    const taskDisplayMode = slackStreamTaskDisplayMode(options)
    const visibleStream = await streamAfterFirstChunk(
      conflateChatSdkStream(
        slackSafeChatSdkStream(
          slackVisibleChatSdkStream(
            codexAppServerToChatSdkStream(
              stream,
              rendererOptions(thread, options, capture, trace)
            ),
            taskDisplayMode
          )
        )
      )
    )
    if (!visibleStream) return { diverged: false }
    // Stream via the adapter (as renderRecoveredExecutionStream does) so the
    // posted message id is available for divergence reconciliation. For Slack
    // this matches thread.post(StreamingPlan): updateIntervalMs is a no-op
    // (Slack streams server-side) and the recipient context is the message
    // author.
    const sent = await thread.adapter.stream!(thread.id, visibleStream, {
      recipientTeamId: message.teamId,
      recipientUserId: message.author.userId,
      ...(taskDisplayMode === 'none' ? {} : { taskDisplayMode })
    })
    return { diverged: capture.diverged, messageId: sent?.id }
  } finally {
    await setAssistantStatus(thread, '', options, trace)
  }
}

async function renderRecoveredExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<{ diverged: boolean; messageId?: string }> {
  const promptText = slackMessagePromptText(message)
  if (isPlainTextOnlyRequest(promptText)) {
    await renderPlainTextExecutionStream(thread, stream, message, options, trace)
    return { diverged: false }
  }
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(promptText, options.userName), options, trace)
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...', options, trace)
  traceLog(options, 'slackbotv2_render_slack_metadata_set', trace, {
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  const capture = { diverged: false }
  try {
    const taskDisplayMode = slackStreamTaskDisplayMode(options)
    const visibleStream = await streamAfterFirstChunk(
      conflateChatSdkStream(
        slackSafeChatSdkStream(
          slackVisibleChatSdkStream(
            codexAppServerToChatSdkStream(
              stream,
              rendererOptions(thread, options, capture, trace)
            ),
            taskDisplayMode
          )
        )
      )
    )
    if (!visibleStream) return { diverged: false }
    const sent = await thread.adapter.stream!(
      thread.id,
      visibleStream,
      {
        recipientTeamId: message.teamId,
        recipientUserId: message.author.userId,
        ...(taskDisplayMode === 'none' ? {} : { taskDisplayMode })
      }
    )
    return { diverged: capture.diverged, messageId: sent?.id }
  } finally {
    await setAssistantStatus(thread, '', options, trace)
  }
}

async function renderPlainTextExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace,
  assistantStatusVisible = false
): Promise<void> {
  const fallback = new SlackRenderFallback()
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(
    thread,
    titleFromMessage(slackMessagePromptText(message), options.userName),
    options,
    trace
  )
  if (!assistantStatusVisible) {
    await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...', options, trace)
  }
  traceLog(options, 'slackbotv2_render_plain_text_metadata_set', trace, {
    assistant_status_already_visible: assistantStatusVisible,
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  try {
    const chatStream = fallback.collectChatSdk(
      slackSafeChatSdkStream(
        codexAppServerToChatSdkStream(
          fallback.collectSource(stream),
          rendererOptions(thread, options, undefined, trace)
        )
      )
    )
    for await (const _chunk of chatStream) {
      void _chunk
    }
    const text = truncateSlackText(
      fallback.text() || 'Execution completed, but no final text was captured.',
      SLACK_FALLBACK_TEXT_MAX_CHARS,
      'Slack final answer'
    )
    traceLog(options, 'slackbotv2_render_plain_text_final', trace, {
      chars: text.length
    })
    await thread.post(text)
  } finally {
    await setAssistantStatus(thread, '', options, trace)
  }
}

class SlackRenderFallback {
  private markdownText = ''
  private terminalText = ''

  async *collectSource(
    stream: AsyncIterable<SlackbotV2RendererSource>
  ): AsyncIterable<SlackbotV2RendererSource> {
    for await (const event of stream) {
      this.captureTerminalText(event)
      yield event
    }
  }

  async *collectChatSdk(
    stream: AsyncIterable<ChatSDKStreamChunk>
  ): AsyncIterable<ChatSDKStreamChunk> {
    for await (const chunk of stream) {
      if (chunk.type === 'markdown_text') this.markdownText += chunk.text
      yield chunk
    }
  }

  text(): string {
    return (this.terminalText || this.markdownText).trim()
  }

  private captureTerminalText(event: SlackbotV2RendererSource): void {
    if (!event || typeof event !== 'object') return
    const eventKind = String(
      'eventKind' in event ? event.eventKind : 'event' in event ? event.event : ''
    )
    if (
      eventKind !== 'session.execution_completed' &&
      eventKind !== 'session.execution_cancelled' &&
      !isTerminalCodexAppServerEvent(event)
    ) {
      return
    }
    const data = 'data' in event && event.data && typeof event.data === 'object'
      ? event.data
      : event
    const text = terminalResultText(data)
    if (text) this.terminalText = text
  }
}

async function* slackSafeChatSdkStream(
  stream: AsyncIterable<ChatSDKStreamChunk>
): AsyncIterable<ChatSDKStreamChunk> {
  for await (const chunk of stream) {
    yield slackSafeChatSdkChunk(chunk)
  }
}

type SlackStreamTaskDisplayMode = NonNullable<SlackbotV2Options['streamTaskDisplayMode']>

function slackStreamTaskDisplayMode(options: SlackbotV2Options): SlackStreamTaskDisplayMode {
  return options.streamTaskDisplayMode ?? (options.activitySummaryStatusEnabled ? 'none' : 'plan')
}

async function* slackVisibleChatSdkStream(
  stream: AsyncIterable<ChatSDKStreamChunk>,
  taskDisplayMode: SlackStreamTaskDisplayMode
): AsyncIterable<ChatSDKStreamChunk> {
  for await (const chunk of stream) {
    if (taskDisplayMode === 'none' && chunk.type !== 'markdown_text') continue
    yield chunk
  }
}

function slackSafeChatSdkChunk(chunk: ChatSDKStreamChunk): ChatSDKStreamChunk {
  if (chunk.type !== 'task_update') return chunk
  const { output: _output, details, ...safeChunk } = chunk
  void _output
  return {
    ...safeChunk,
    ...(details ? { details: truncateSlackTaskField(details) } : {})
  }
}

function isPlainTextOnlyRequest(text: string): boolean {
  const normalized = text.toLowerCase()
  return (
    /\bplain\s+text\s+only\b/.test(normalized)
    || /\bno\s+interactive\s+blocks?\b/.test(normalized)
    || /\bno\s+dashboards?\b/.test(normalized)
  )
}

function truncateSlackTaskField(value: string): string {
  return truncateSlackText(value, SLACK_TASK_DETAILS_MAX_CHARS, 'Slack task details')
}

function truncateSlackText(value: string, maxChars: number, label: string): string {
  if (value.length <= maxChars) return value
  let omitted = value.length - maxChars
  while (true) {
    const suffix = `\n[truncated ${omitted} chars from ${label}]`
    const keep = Math.max(0, maxChars - suffix.length)
    const actualOmitted = value.length - keep
    if (actualOmitted === omitted) return `${value.slice(0, keep).trimEnd()}${suffix}`
    omitted = actualOmitted
  }
}

async function streamAfterFirstChunk(
  stream: AsyncIterable<ChatSDKStreamChunk>
): Promise<AsyncIterable<ChatSDKStreamChunk> | null> {
  const iterator = stream[Symbol.asyncIterator]()
  const first = await iterator.next()
  if (first.done) return null

  return {
    async *[Symbol.asyncIterator](): AsyncIterator<ChatSDKStreamChunk> {
      yield first.value
      for (;;) {
        const next = await iterator.next()
        if (next.done) return
        yield next.value
      }
    }
  }
}

function isTerminalCodexAppServerEvent(event: unknown): boolean {
  if (!event || typeof event !== 'object') return false
  const type = (event as { type?: unknown }).type
  return type === 'result' || type === 'turn.done' || type === 'turn.completed'
}

function terminalResultText(event: unknown): string {
  if (!event || typeof event !== 'object') return ''
  for (const key of ['result', 'result_text', 'text', 'final_text']) {
    const value = (event as Record<string, unknown>)[key]
    if (typeof value !== 'string') continue
    const resultText = value.trim()
    if (resultText) return resultText
  }
  return ''
}

async function* streamSessionAfterHandoff(
  options: SlackbotV2Options,
  input: ForwardSessionInput
): AsyncIterable<SlackbotV2RendererSource> {
  let stream: AsyncIterable<SlackbotV2RendererSource>
  try {
    stream = await openSessionEventStream(options, input)
  } catch (error) {
    traceLog(options, 'slackbotv2_forward_failed', input.trace, {
      error: errorMessage(error)
    })
    if (isRetryableSessionApiError(error)) throw error
    yield sessionStreamError(error)
    return
  }

  for await (const event of stream) yield event
}

async function* streamError(error: unknown): AsyncIterable<SlackbotV2RendererSource> {
  yield sessionStreamError(error)
}

function backgroundWaitUntil(promise: Promise<unknown>): void {
  const context = requestContext.getStore()
  if (context) {
    context.waitUntil(promise)
    return
  }
  void promise.catch(() => undefined)
}

function createLateSlackFileRepair(options: SlackbotV2Options, state: StateAdapter) {
  const pending = new Map<string, PendingLateSlackFileMention[]>()
  const consumed = new Map<string, number>()

  const cleanup = () => {
    const cutoff = Date.now() - LATE_SLACK_FILE_PENDING_TTL_MS
    for (const [key, entries] of pending) {
      const fresh = entries.filter(entry => slackTsToMs(entry.mentionTs) >= cutoff)
      if (fresh.length > 0) pending.set(key, fresh)
      else pending.delete(key)
    }
    const consumedCutoff = Date.now() - LATE_SLACK_FILE_CONSUMED_TTL_MS
    for (const [key, timestamp] of consumed) {
      if (timestamp < consumedCutoff) consumed.delete(key)
    }
  }

  return {
    rememberFilelessMention(thread: Thread<SlackbotV2ThreadState>, message: ChatMessage): void {
      if (message.isMention !== true) return
      const raw = slackRawRecord(message)
      if (slackFiles(raw).length > 0 || message.attachments.length > 0) return
      const teamId = stringField(raw.team) || stringField(raw.team_id)
      const channel = stringField(raw.channel)
      const user = stringField(raw.user)
      const mentionTs = stringField(raw.ts) || message.id
      if (!teamId || !channel || !user || !mentionTs) return

      cleanup()
      const key = lateSlackFilePendingKey(teamId, channel, user)
      const entry: PendingLateSlackFileMention = {
        channel,
        message,
        mentionTs,
        teamId,
        thread,
        user
      }
      const entries = [entry, ...(pending.get(key) ?? [])]
        .filter(item => slackTsToMs(item.mentionTs) >= Date.now() - LATE_SLACK_FILE_PENDING_TTL_MS)
        .slice(0, 20)
      pending.set(key, entries)
      traceLog(options, 'slackbotv2_late_file_pending_mention_recorded', undefined, {
        slack_channel: channel,
        slack_message_ts: mentionTs,
        slack_team_id: teamId,
        slack_user_id: user,
        thread_id: thread.id
      })
    },

    repairFromWebhook(rawBody: string): Promise<void> | null {
      const payload = slackWebhookPayload(rawBody)
      if (!payload) return null
      const event = slackWebhookEvent(payload)
      if (!event || !isLateSlackFileEvent(event, options)) return null
      cleanup()

      const dedupeKey = lateSlackFileDedupeKey(payload, event)
      if (consumed.has(dedupeKey)) {
        traceLog(options, 'slackbotv2_late_file_duplicate_skipped', undefined, {
          dedupe_key: dedupeKey
        })
        return null
      }

      const match = matchLateSlackFileMention(pending, payload, event)
      if (!match) {
        traceLog(options, 'slackbotv2_late_file_no_match', undefined, {
          slack_channel: stringField(event.channel),
          slack_event_id: stringField(payload.event_id),
          slack_message_ts: stringField(event.ts),
          slack_team_id: slackEventTeamId(payload, event),
          slack_thread_ts: stringField(event.thread_ts),
          slack_user_id: stringField(event.user)
        })
        return null
      }

      consumed.set(dedupeKey, Date.now())
      return repairLateSlackFileMessage(options, state, match, event).catch(error => {
        traceWarn(options, 'slackbotv2_late_file_repair_failed', undefined, {
          dedupe_key: dedupeKey,
          error: errorMessage(error),
          slack_channel: stringField(event.channel),
          slack_message_ts: stringField(event.ts),
          thread_id: match.thread.id
        })
      })
    }
  }
}

async function repairLateSlackFileMessage(
  options: SlackbotV2Options,
  state: StateAdapter,
  pending: PendingLateSlackFileMention,
  event: Record<string, unknown>
): Promise<void> {
  const startedAtMs = nowMs()
  const eventTs = stringField(event.ts)
  const hydratedEvent = await hydrateLateSlackFileEvent(options, event)
  const ready = await waitForThreadIdle(pending.thread, options, {
    includeContext: true,
    messageId: eventTs,
    mode: 'execute',
    openStream: true,
    startedAtMs: nowMs(),
    threadId: pending.thread.id
  })
  if (!ready) {
    traceWarn(options, 'slackbotv2_late_file_repair_idle_timeout', undefined, {
      slack_channel: pending.channel,
      slack_message_ts: eventTs,
      thread_id: pending.thread.id
    })
    return
  }

  const message = lateSlackFileSyntheticMessage(pending, hydratedEvent)
  await handleSlackMessageHandoff(pending.thread, message, {
    assistantStatusRequested: true,
    mode: 'execute',
    options,
    state,
    trigger: 'late_file_message'
  })
  traceLog(options, 'slackbotv2_late_file_repair_complete', undefined, {
    phase_ms: elapsedMs(startedAtMs),
    slack_channel: pending.channel,
    slack_message_ts: eventTs,
    thread_id: pending.thread.id
  })
}

async function waitForThreadIdle(
  thread: Thread<SlackbotV2ThreadState>,
  options: SlackbotV2Options,
  trace: SlackbotV2Trace
): Promise<boolean> {
  const startedAtMs = nowMs()
  while (elapsedMs(startedAtMs) < LATE_SLACK_FILE_IDLE_WAIT_MS) {
    const latest = (await thread.state) ?? {}
    if (latest.activeExecution !== true) return true
    traceLog(options, 'slackbotv2_late_file_repair_waiting_for_idle', trace, {
      waited_ms: elapsedMs(startedAtMs)
    })
    await sleep(LATE_SLACK_FILE_IDLE_POLL_MS)
  }
  return false
}

function lateSlackFileSyntheticMessage(
  pending: PendingLateSlackFileMention,
  event: Record<string, unknown>
): ChatMessage {
  const eventTs = stringField(event.ts) || randomUUID()
  const raw: Record<string, unknown> = {
    ...event,
    channel: pending.channel,
    team: stringField(event.team) || pending.teamId,
    team_id: stringField(event.team_id) || pending.teamId,
    text: stringField(event.text) || LATE_SLACK_FILE_MESSAGE_TEXT,
    thread_ts: stringField(event.thread_ts) || pending.mentionTs,
    ts: eventTs
  }
  return new ChatSdkMessage({
    attachments: [],
    author: {
      ...pending.message.author,
      userId: stringField(event.user) || pending.user,
      userName: stringField(event.user) || pending.user,
      fullName: stringField(event.user) || pending.user,
      isBot: Boolean(event.bot_id),
      isMe: false
    },
    formatted: parseMarkdown(LATE_SLACK_FILE_MESSAGE_TEXT),
    id: eventTs,
    isMention: true,
    links: [],
    metadata: {
      dateSent: new Date(slackTsToMs(eventTs)),
      edited: false
    },
    raw,
    text: LATE_SLACK_FILE_MESSAGE_TEXT,
    threadId: pending.thread.id
  })
}

async function hydrateLateSlackFileEvent(
  options: SlackbotV2Options,
  event: Record<string, unknown>
): Promise<Record<string, unknown>> {
  const files = slackFiles(event)
  if (!files.some(file => stringField(file.file_access) === 'check_file_info')) return event
  const hydratedFiles: Record<string, unknown>[] = []
  for (const file of files) {
    if (stringField(file.file_access) !== 'check_file_info') {
      hydratedFiles.push(file)
      continue
    }
    const id = stringField(file.id)
    if (!id) {
      hydratedFiles.push(file)
      continue
    }
    const hydrated = await slackFilesInfo(options, id)
    hydratedFiles.push(hydrated ?? file)
  }
  return { ...event, files: hydratedFiles }
}

async function slackFilesInfo(
  options: SlackbotV2Options,
  fileId: string
): Promise<Record<string, unknown> | null> {
  const fetchFn = options.fetch ?? fetch
  const url = new URL('files.info', options.slackApiUrl ?? 'https://slack.com/api/')
  url.searchParams.set('file', fileId)
  const response = await withSlackApiTimeout(options, 'Slack files.info', () =>
    fetchFn(url, {
      headers: { authorization: `Bearer ${options.botToken}` }
    })
  )
  if (!response.ok) {
    throw new Error(`Slack files.info failed: ${response.status} ${response.statusText}`)
  }
  const payload = (await response.json()) as unknown
  if (!isJsonObject(payload) || payload.ok !== true || !isJsonObject(payload.file)) return null
  traceLog(options, 'slackbotv2_late_file_hydrated_file_info', undefined, {
    slack_file_id: fileId
  })
  return payload.file as Record<string, unknown>
}

function matchLateSlackFileMention(
  pending: Map<string, PendingLateSlackFileMention[]>,
  payload: Record<string, unknown>,
  event: Record<string, unknown>
): PendingLateSlackFileMention | null {
  const teamId = slackEventTeamId(payload, event)
  const channel = stringField(event.channel)
  const user = stringField(event.user)
  const eventTs = stringField(event.ts)
  if (!teamId || !channel || !user || !eventTs) return null

  const entries = pending.get(lateSlackFilePendingKey(teamId, channel, user)) ?? []
  const threadTs = stringField(event.thread_ts)
  const eventMs = slackTsToMs(eventTs)
  return (
    entries.find(entry => {
      if (threadTs && threadTs !== slackThreadTsForPendingMention(entry)) return false
      const mentionMs = slackTsToMs(entry.mentionTs)
      return eventMs > mentionMs && eventMs - mentionMs <= LATE_SLACK_FILE_MATCH_WINDOW_MS
    }) ?? null
  )
}

function isLateSlackFileEvent(
  event: Record<string, unknown>,
  options: SlackbotV2Options
): boolean {
  if (stringField(event.type) !== 'message') return false
  if (stringField(event.subtype) && stringField(event.subtype) !== 'file_share') return false
  if (slackFiles(event).length === 0) return false
  if (stringField(event.user) === options.botUserId) return false
  const text = stringField(event.text)
  if (options.botUserId && text.includes(`<@${options.botUserId}>`)) return false
  return true
}

function slackWebhookPayload(rawBody: string): Record<string, unknown> | null {
  try {
    const payload = JSON.parse(rawBody)
    return isJsonObject(payload) ? (payload as Record<string, unknown>) : null
  } catch {
    return null
  }
}

function slackWebhookEvent(payload: Record<string, unknown>): Record<string, unknown> | null {
  return isJsonObject(payload.event) ? (payload.event as Record<string, unknown>) : null
}

function lateSlackFilePendingKey(teamId: string, channel: string, user: string): string {
  return `${teamId}:${channel}:${user}`
}

function lateSlackFileDedupeKey(
  payload: Record<string, unknown>,
  event: Record<string, unknown>
): string {
  const eventId = stringField(payload.event_id)
  if (eventId) return `event:${eventId}`
  const fileIds = slackFiles(event).map(file => stringField(file.id)).filter(Boolean).join(',')
  return [
    'file',
    slackEventTeamId(payload, event),
    stringField(event.channel),
    stringField(event.ts),
    fileIds
  ].join(':')
}

function slackEventTeamId(
  payload: Record<string, unknown>,
  event: Record<string, unknown>
): string {
  return stringField(event.team) || stringField(event.team_id) || stringField(payload.team_id)
}

function slackThreadTsForPendingMention(entry: PendingLateSlackFileMention): string {
  const raw = slackRawRecord(entry.message)
  return stringField(raw.thread_ts) || entry.mentionTs
}

function slackTsToMs(ts: string): number {
  const seconds = Number(ts)
  return Number.isFinite(seconds) ? seconds * 1000 : 0
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

function slackWebhookLogFields(rawBody: string): JsonObject {
  try {
    const payload = JSON.parse(rawBody) as Record<string, unknown>
    const rawEvent = payload.event
    const event =
      rawEvent && typeof rawEvent === 'object' && !Array.isArray(rawEvent)
        ? (rawEvent as Record<string, unknown>)
        : {}
    const fields: JsonObject = {}
    setStringField(fields, 'slack_event_id', payload.event_id)
    setStringField(fields, 'slack_event_type', event.type)
    setStringField(fields, 'slack_channel', event.channel)
    setStringField(fields, 'slack_message_ts', event.ts)
    setStringField(fields, 'slack_thread_ts', event.thread_ts)
    setStringField(fields, 'slack_team_id', payload.team_id || event.team)
    return fields
  } catch {
    return { slack_payload_parse_error: true }
  }
}

function setStringField(fields: JsonObject, key: string, value: unknown): void {
  const text = stringField(value)
  if (text) fields[key] = text
}

function isSlackThreadReply(message: ChatMessage): boolean {
  const raw = message.raw
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return false
  const item = raw as Record<string, unknown>
  const threadTs = typeof item.thread_ts === 'string' ? item.thread_ts : ''
  const ts = typeof item.ts === 'string' ? item.ts : message.id
  return Boolean(threadTs && ts && threadTs !== ts)
}

async function collectSlackThreadContext(
  options: SlackbotV2Options,
  currentMessage: ChatMessage
): Promise<SlackbotV2ApiMessage[]> {
  const raw = slackRawRecord(currentMessage)
  const channel = stringField(raw.channel)
  const threadTs = stringField(raw.thread_ts)
  const currentTs = stringField(raw.ts) || currentMessage.id
  if (!channel || !threadTs) return [await serializeMessage(currentMessage, options)]

  const messages: SlackbotV2ApiMessage[] = []
  let cursor: string | undefined
  do {
    const response = await withSlackApiTimeout(options, 'fetch Slack thread replies', () =>
      fetchSlackThreadReplies({
        apiUrl: options.slackApiUrl,
        channel,
        cursor,
        limit: 200,
        token: options.botToken,
        ts: threadTs
      })
    )
    const slackMessages = Array.isArray(response.messages) ? response.messages : []
    for (const rawMessage of slackMessages) {
      const message = rawMessage as Record<string, unknown>
      const messageTs = stringField(message.ts)
      if (!messageTs || compareSlackTs(messageTs, currentTs) > 0) continue
      if (isSelfSlackBotMessage(options, message)) continue
      messages.push(await slackApiMessageFromSlack(options, message, currentMessage))
    }
    cursor = response.nextCursor
  } while (cursor)

  const currentIndex = messages.findIndex(message => message.id === currentMessage.id)
  const serializedCurrent = await serializeMessage(currentMessage, options)
  if (currentIndex >= 0) {
    messages[currentIndex] = serializedCurrent
  } else {
    messages.push(serializedCurrent)
  }
  return messages
}

async function slackApiMessageFromSlack(
  options: SlackbotV2Options,
  message: Record<string, unknown>,
  currentMessage: ChatMessage
): Promise<SlackbotV2ApiMessage> {
  const rawCurrent = slackRawRecord(currentMessage)
  const id = stringField(message.ts) || randomUUID()
  const actorId = slackActorId(message)
  const isBot = Boolean(message.bot_id || message.bot_profile)
  const text = normalizeSlackText(stringField(message.text))
  const displayText = renderSlackDisplayText({ raw: message, text })
  return {
    attachments: await slackApiAttachmentsFromFiles(options, message, rawCurrent),
    author: {
      fullName: actorId,
      isBot,
      isMe: Boolean(actorId && actorId === currentMessage.author.userId),
      userId: actorId,
      userName: actorId
    },
    displayText: displayText.text,
    displayTextSource: displayText.source,
    id,
    isMention: id === currentMessage.id ? currentMessage.isMention === true : false,
    links: serializeMessageLinks(undefined, message),
    raw: message,
    rawSlackAttachmentCount: displayText.rawAttachmentCount,
    rawSlackBlockCount: displayText.rawBlockCount,
    teamId:
      stringField(message.team)
      || stringField(message.team_id)
      || stringField(rawCurrent.team)
      || stringField(rawCurrent.team_id),
    text,
    threadId: currentMessage.threadId,
    timestamp: slackTimestampToIso(id)
  }
}

async function slackApiAttachmentsFromFiles(
  options: SlackbotV2Options,
  message: Record<string, unknown>,
  rawCurrent: Record<string, unknown>
): Promise<SlackbotV2ApiAttachment[]> {
  const files = slackFiles(message)
  if (files.length === 0) return []
  const teamId =
    stringField(message.team)
    || stringField(message.team_id)
    || stringField(rawCurrent.team)
    || stringField(rawCurrent.team_id)
  const attachments: SlackbotV2ApiAttachment[] = []
  for (const file of files.slice(0, MAX_SLACK_MESSAGE_ATTACHMENTS)) {
    attachments.push(await serializeAttachment(slackFileAttachment(options, file, teamId), options))
  }
  if (files.length > MAX_SLACK_MESSAGE_ATTACHMENTS) {
    attachments.push({
      fetchError:
        `only the first ${MAX_SLACK_MESSAGE_ATTACHMENTS} Slack message attachments were fetched`,
      name: 'additional Slack thread attachments',
      type: 'file'
    })
  }
  return attachments
}

function slackFiles(message: Record<string, unknown>): Record<string, unknown>[] {
  return Array.isArray(message.files)
    ? (message.files.filter(file =>
        file && typeof file === 'object' && !Array.isArray(file)
      ) as Record<string, unknown>[])
    : []
}

function slackFileAttachment(
  options: SlackbotV2Options,
  file: Record<string, unknown>,
  teamId: string
): Attachment {
  const url = stringField(file.url_private_download) || stringField(file.url_private)
  const mimeType = stringField(file.mimetype)
  const fetchMetadata: Record<string, string> = {}
  if (url) fetchMetadata.url = url
  if (teamId) fetchMetadata.teamId = teamId
  return {
    fetchData: url ? () => fetchSlackFile(options, url) : undefined,
    fetchMetadata: Object.keys(fetchMetadata).length > 0 ? fetchMetadata : undefined,
    height: numberField(file.original_h),
    mimeType,
    name: stringField(file.name) || stringField(file.title) || stringField(file.id),
    size: numberField(file.size),
    type: slackFileAttachmentType(mimeType),
    url,
    width: numberField(file.original_w)
  }
}

async function fetchSlackFile(options: SlackbotV2Options, url: string): Promise<Buffer> {
  const fetchFn = options.fetch ?? fetch
  const controller = new AbortController()
  try {
    const response = await withSlackApiTimeout(options, 'fetch Slack file', () =>
      fetchFn(url, {
        headers: { authorization: `Bearer ${options.botToken}` },
        signal: controller.signal
      })
    )
    if (!response.ok) {
      throw new Error(`failed to fetch Slack file: ${response.status} ${response.statusText}`)
    }
    const body = await withSlackApiTimeout(options, 'read Slack file', () =>
      response.arrayBuffer()
    )
    return Buffer.from(body)
  } catch (error) {
    controller.abort()
    throw error
  }
}

function slackFileAttachmentType(mimeType: string): Attachment['type'] {
  if (mimeType.startsWith('image/')) return 'image'
  if (mimeType.startsWith('video/')) return 'video'
  if (mimeType.startsWith('audio/')) return 'audio'
  return 'file'
}

function slackRawRecord(message: ChatMessage): Record<string, unknown> {
  return message.raw && typeof message.raw === 'object' && !Array.isArray(message.raw)
    ? (message.raw as Record<string, unknown>)
    : {}
}

function slackActorId(message: Record<string, unknown>): string {
  const profile = message.bot_profile
  if (profile && typeof profile === 'object' && !Array.isArray(profile)) {
    const userId = stringField((profile as Record<string, unknown>).user_id)
    if (userId) return userId
  }
  return stringField(message.user) || stringField(message.bot_id)
}

function isSelfSlackBotMessage(
  options: SlackbotV2Options,
  message: Record<string, unknown>
): boolean {
  const botUserId = options.botUserId
  if (!botUserId) return false
  if (stringField(message.user) === botUserId) return true
  const profile = message.bot_profile
  if (profile && typeof profile === 'object' && !Array.isArray(profile)) {
    return stringField((profile as Record<string, unknown>).user_id) === botUserId
  }
  return false
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function numberField(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

function compareSlackTs(a: string, b: string): number {
  const left = Number(a)
  const right = Number(b)
  if (Number.isFinite(left) && Number.isFinite(right)) return left - right
  return a.localeCompare(b)
}

function slackTimestampToIso(ts: string): string {
  const seconds = Number(ts)
  return Number.isFinite(seconds)
    ? new Date(seconds * 1000).toISOString()
    : new Date().toISOString()
}

export function normalizeSlackText(input: string): string {
  return input
    .replace(/<([a-z]+:\/\/[^>|]+)\|([^>]+)>/gi, '$2 ($1)')
    .replace(/<([a-z]+:\/\/[^>]+)>/gi, '$1')
    .replace(/<#([A-Z0-9]+)\|([^>]+)>/g, '#$2 ($1)')
    .replace(/<#([A-Z0-9]+)>/g, '#$1')
    .replace(/<@([A-Z0-9]+)>/g, '@$1')
    .replace(/<!subteam\^([A-Z0-9]+)\|([^>]+)>/g, '@$2')
    .replace(/<!(channel|here|everyone)>/g, '@$1')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .trim()
}

// Surfaces the renderer's structured diagnostics (otherwise a no-op: nothing
// wires logInfo), turns the answer-divergence guard into a Prometheus signal so
// the real rate is measurable, and flips the per-render `capture.diverged` flag
// so the caller can reconcile the message with the durable final answer.
function rendererLogInfo(
  options: SlackbotV2Options,
  capture?: { diverged: boolean }
): (event: string, fields: Record<string, unknown>) => void {
  return (event, fields) => {
    options.mapper?.logInfo?.(event, fields)
    options.logger?.info(event, fields)
    if (event === 'codex_renderer_stream_divergence_suppressed') {
      slackbotMetrics.renderAnswerDivergence.inc()
      if (capture) capture.diverged = true
    }
  }
}

function rendererOptions(
  thread: Thread,
  options: SlackbotV2Options,
  capture?: { diverged: boolean },
  trace?: SlackbotV2Trace
): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper
  return {
    ...mapper,
    logInfo: rendererLogInfo(options, capture),
    async onRendererEvent(event: RendererEvent) {
      await mapper?.onRendererEvent?.(event)
      if (event.type === 'renderer.title.update') {
        await setAssistantTitle(thread, event.title, options)
      }
      if (event.type === 'renderer.status' && options.activitySummaryStatusEnabled) {
        await setAssistantStatus(thread, event.status, options, trace)
      }
    }
  }
}

/**
 * Renderer options for the final-answer fallback drain: no Slack side effects
 * (no assistant title updates) and renderer hooks must not be able to fail
 * the delivery.
 */
function fallbackRendererOptions(options: SlackbotV2Options): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper
  return {
    ...mapper,
    logInfo: rendererLogInfo(options),
    async onRendererEvent(event: RendererEvent) {
      try {
        await mapper?.onRendererEvent?.(event)
      } catch {
        // Fallback delivery must not depend on renderer side-effect hooks.
      }
    }
  }
}

function renderRetryDelayMs(attempt: number): number {
  return Math.min(RENDER_RETRY_INITIAL_DELAY_MS * 2 ** attempt, RENDER_RETRY_MAX_DELAY_MS)
}

async function sleep(ms: number): Promise<void> {
  await new Promise(resolve => setTimeout(resolve, ms))
}

async function setInitialAssistantStatus(
  thread: Thread,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<boolean> {
  const startedAtMs = nowMs()
  const visible = await setAssistantStatus(
    thread,
    options.assistantStatus ?? 'Thinking...',
    options,
    trace
  )
  traceLog(options, 'slackbotv2_forward_initial_status_set', trace, {
    phase_ms: elapsedMs(startedAtMs),
    visible
  })
  return visible
}

async function setAssistantStatus(
  thread: Thread,
  status: string,
  options?: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<boolean> {
  const startedAtMs = nowMs()
  const normalizedStatus = normalizeAssistantStatus(status)
  const target = slackAssistantTarget(thread)
  const adapter = thread.adapter as SlackAssistantAdapter
  const fields = {
    has_adapter: Boolean(adapter.setAssistantStatus),
    has_target: Boolean(target),
    operation: normalizedStatus ? 'set' : 'clear',
    status_empty: !normalizedStatus
  }
  if (options) traceLog(options, 'slackbotv2_assistant_status_started', trace, fields)
  if (!target || !adapter.setAssistantStatus) {
    if (options) {
      traceLog(options, 'slackbotv2_assistant_status_complete', trace, {
        ...fields,
        phase_ms: elapsedMs(startedAtMs),
        visible: false
      })
    }
    return false
  }
  const stopPendingLog = options
    ? startPendingOperationLog(
        options,
        'slackbotv2_assistant_status_pending',
        trace,
        fields,
        startedAtMs
      )
    : () => undefined
  try {
    const visible = await withSlackApiTimeout(options, 'set assistant status', () =>
      ignoreAssistantError(() =>
        adapter.setAssistantStatus!(
          target.channel,
          target.threadTs,
          normalizedStatus,
          normalizedStatus ? [normalizedStatus] : undefined
        )
      )
    )
    if (options) {
      traceLog(options, 'slackbotv2_assistant_status_complete', trace, {
        ...fields,
        phase_ms: elapsedMs(startedAtMs),
        visible
      })
    }
    return visible
  } catch (error) {
    if (options) {
      traceWarn(options, 'slackbotv2_assistant_status_failed', trace, {
        ...fields,
        error: errorMessage(error),
        phase_ms: elapsedMs(startedAtMs)
      })
    }
    return false
  } finally {
    stopPendingLog()
  }
}

function normalizeAssistantStatus(status: string): string {
  const oneLine = status.replace(/\s+/g, ' ').trim()
  const chars = Array.from(oneLine)
  if (chars.length <= ASSISTANT_STATUS_MAX_CHARS) return oneLine
  return `${chars.slice(0, ASSISTANT_STATUS_MAX_CHARS - 3).join('').trimEnd()}...`
}

async function setAssistantTitle(
  thread: Thread,
  title: string | undefined,
  options?: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<void> {
  const normalized = title?.trim()
  if (!normalized) return
  const startedAtMs = nowMs()
  const target = slackAssistantTarget(thread)
  const adapter = thread.adapter as SlackAssistantAdapter
  if (!target || !adapter.setAssistantTitle) return
  try {
    await withSlackApiTimeout(options, 'set assistant title', () =>
      ignoreAssistantError(() =>
        adapter.setAssistantTitle!(target.channel, target.threadTs, clipOneLine(normalized, 80))
      )
    )
  } catch (error) {
    if (options) {
      traceWarn(options, 'slackbotv2_assistant_title_failed', trace, {
        error: errorMessage(error),
        phase_ms: elapsedMs(startedAtMs)
      })
    }
  }
}

async function ignoreAssistantError(fn: () => Promise<void>): Promise<boolean> {
  try {
    await fn()
    return true
  } catch {
    // Assistant status/title are Slack UI polish. Rendering should continue if unsupported.
    return false
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
