import { AsyncLocalStorage } from 'node:async_hooks'
import { randomUUID } from 'node:crypto'
import { Hono, type Context } from 'hono'
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
import pg from 'pg'
import {
  codexAppServerToChatSdkStream,
  type CodexAppServerToChatStreamOptions,
  type ChatSDKStreamChunk,
  type RendererEvent
} from '@centaur/rendering'
import { conflateChatSdkStream } from './conflate'
import { feedbackEndBlocks, registerFeedbackHandlers } from './feedback'
import {
  collectInitialContext,
  forwardToSessionApi,
  isRetryableSessionApiError,
  openSessionEventStream,
  serializeMessage,
  sessionStreamError
} from './session-api'
import { extractMessageOverrides } from './overrides'
import { isAllowedSlackMessage, isAllowedSlackWebhookBody } from './slack-events'
import type {
  ForwardSessionInput,
  SlackbotV2,
  SlackbotV2ApiMessage,
  SlackbotV2ExecuteSessionResponse,
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

type SlackbotV2RequestContext = {
  retryableErrors: unknown[]
  waitUntil(promise: Promise<unknown>): void
}

const requestContext = new AsyncLocalStorage<SlackbotV2RequestContext>()
const RENDER_OBLIGATION_INDEX_KEY = 'slackbotv2:render:index'
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000
const RENDER_INDEX_TTL_MS = 30 * 24 * 60 * 60 * 1000
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000
const RENDER_RECOVERY_THREAD_TIMEOUT_MS = 2 * 60 * 1000
const RENDER_RECOVERY_MAX_THREAD_FAILURES = 5
const RENDER_RETRY_INITIAL_DELAY_MS = 250
const RENDER_RETRY_MAX_DELAY_MS = 5_000
const SLACK_TASK_DETAILS_MAX_CHARS = 500
const SLACK_FALLBACK_TEXT_MAX_CHARS = 35_000
const POSTGRES_CONNECT_INITIAL_DELAY_MS = 250
const POSTGRES_CONNECT_MAX_DELAY_MS = 10_000

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

  registerFeedbackHandlers(chat, {
    apiKey: options.apiKey,
    apiUrl: options.apiUrl,
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
  const handleSlackWebhook = async (c: Context) => {
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
  }
  app.post('/api/webhooks/slack', handleSlackWebhook)
  app.post('/api/slack/events', handleSlackWebhook)

  if (options.recoverRenderObligationsOnStart !== false) {
    scheduleRenderObligationRecovery(chat, state, options)
  }

  return { app, chat }
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
  const overrides = extractMessageOverrides(serializedMessage.text)
  serializedMessage.text = overrides.cleanedText
  if (overrides.harnessType || overrides.model) {
    traceLog(input.options, 'slackbotv2_forward_overrides_parsed', trace, {
      harness_type: overrides.harnessType,
      model: overrides.model
    })
  }
  traceLog(input.options, 'slackbotv2_forward_message_serialized', trace, {
    attachment_count: serializedMessage.attachments.length,
    phase_ms: elapsedMs(serializeStartedAtMs)
  })
  let context: SlackbotV2ApiMessage[] | undefined

  if (shouldIncludeContext && !state.historyForwarded) {
    const contextStartedAtMs = nowMs()
    context = await collectInitialContext(thread, message)
    // collectInitialContext re-serializes the current message; mirror the
    // flag-stripped text on that copy too.
    for (const item of context) {
      if (item.id === serializedMessage.id) item.text = serializedMessage.text
    }
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
    harnessType: overrides.harnessType,
    messages: messagesToAppend,
    model: overrides.model,
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

  const commitExecutionStarted = async (
    execution: SlackbotV2ExecuteSessionResponse
  ): Promise<void> => {
    const latest = (await thread.state) ?? {}
    const latestExecutedMessageIds = new Set(latest.executedMessageIds ?? [])
    latestExecutedMessageIds.add(serializedMessage.id)
    forwardInput.executionId = execution.execution_id
    await thread.setState({
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
        throw error
      }
    }
    try {
      await renderExecutionStream(thread, streamError(error), serializedMessage, input.options, trace)
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
    let attempt = 0
    while (true) {
      const result = await renderExecutionAttempt(
        thread,
        message,
        options,
        input,
        getLastEventId,
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
  })()
  backgroundWaitUntil(promise)
}

async function renderExecutionAttempt(
  thread: Thread<SlackbotV2ThreadState>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  getLastEventId: () => number,
  trace?: SlackbotV2Trace
): Promise<'complete' | 'retry'> {
  let rendered = false
  let retry = false
  let fallbackLastEventId = 0
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
    return 'complete'
  } catch (error) {
    // Check the Slack adapter's delivery annotation before retryability:
    // Slack network failures can surface as TypeError/AbortError, which would
    // otherwise be misclassified as retryable session API errors and re-render
    // the whole stream instead of posting the durable final answer.
    const answerLost = slackAnswerLost(error)
    if (answerLost === undefined && isRetryableSessionApiError(error)) {
      retry = true
      traceLog(options, 'slackbotv2_render_deferred', trace, {
        error: errorMessage(error),
        last_event_id: getLastEventId()
      })
      return 'retry'
    }
    if (answerLost === false) {
      // The Slack stream broke only after the final answer became visible
      // (for example a progress-card stop failed). Reposting would duplicate
      // the answer, so record the failure and finish.
      rendered = true
      traceLog(options, 'slackbotv2_render_failed_answer_visible', trace, {
        error: errorMessage(error)
      })
      return 'complete'
    }
    traceLog(options, 'slackbotv2_render_failed', trace, {
      error: errorMessage(error),
      slack_answer_lost: answerLost ?? 'unknown'
    })
    const fallback = await renderFallbackFinalAnswer(
      thread,
      options,
      {
        afterEventId: input.afterEventId,
        executionId: input.executionId,
        threadId: input.threadId
      },
      trace
    )
    if (fallback) {
      rendered = true
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
      retry_scheduled: retry,
      last_event_id: getLastEventId()
    })
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
  trace?: SlackbotV2Trace
): Promise<{ lastEventId: number } | null> {
  const startedAtMs = nowMs()
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
      traceLog(options, 'slackbotv2_render_fallback_empty', trace, {
        last_event_id: lastEventId,
        phase_ms: elapsedMs(startedAtMs)
      })
      return null
    }
    await thread.post(
      truncateSlackText(text, SLACK_FALLBACK_TEXT_MAX_CHARS, 'Slack final answer')
    )
    traceLog(options, 'slackbotv2_render_fallback_complete', trace, {
      chars: text.length,
      last_event_id: lastEventId,
      phase_ms: elapsedMs(startedAtMs)
    })
    return { lastEventId }
  } catch (error) {
    traceLog(options, 'slackbotv2_render_fallback_failed', trace, {
      error: errorMessage(error),
      phase_ms: elapsedMs(startedAtMs)
    })
    return null
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
      traceLog(options, 'slackbotv2_render_recovery_retry_scheduled', undefined, {
        deferred_count: deferredCount,
        retry_delay_ms: delayMs,
        retry_attempt: attempt
      })
      await sleep(delayMs)
    } catch (error) {
      traceLog(options, 'slackbotv2_render_recovery_failed', undefined, {
        error: errorMessage(error)
      })
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
  const timeoutMs = options.renderRecoveryThreadTimeoutMs ?? RENDER_RECOVERY_THREAD_TIMEOUT_MS
  let deferredCount = 0
  traceLog(options, 'slackbotv2_render_recovery_scan', undefined, {
    obligation_count: threadIds.length,
    phase_ms: elapsedMs(startedAtMs)
  })

  for (const threadId of threadIds) {
    try {
      const thread = chat.thread(threadId)
      const threadState = await thread.state
      const obligation = threadState?.renderObligation
      if (!obligation) continue

      // An obligation that keeps failing non-retryably (for example corrupt
      // state that can never address a Slack thread) must not poison the
      // retry loop forever: give up on it and unwedge the thread.
      if ((failureCounts.get(threadId) ?? 0) >= RENDER_RECOVERY_MAX_THREAD_FAILURES) {
        traceLog(options, 'slackbotv2_render_recovery_abandoned', undefined, {
          failure_count: failureCounts.get(threadId),
          thread_id: threadId
        })
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
        traceLog(options, 'slackbotv2_render_recovery_thread_timeout', undefined, {
          thread_id: threadId,
          timeout_ms: timeoutMs
        })
        continue
      }
      await releaseLease()
      if (outcome.deferred) deferredCount += 1
    } catch (error) {
      // One thread's corrupt state or failed render must not abort the scan:
      // log it, count it as deferred so a later pass retries it (up to the
      // failure budget above), and keep recovering the remaining threads.
      failureCounts.set(threadId, (failureCounts.get(threadId) ?? 0) + 1)
      deferredCount += 1
      traceLog(options, 'slackbotv2_render_recovery_thread_failed', undefined, {
        error: errorMessage(error),
        failure_count: failureCounts.get(threadId),
        thread_id: threadId
      })
    }
  }
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
    if (retryable) return true
    await renderRecoveredExecutionStream(thread, streamError(error), obligation.message, options, trace)
    await thread.setState({
      activeExecution: false,
      lastEventId,
      renderObligation: null
    })
    return false
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
    const answerLost = slackAnswerLost(error)
    if (answerLost === false) {
      // The recovered stream broke only after the final answer became
      // visible; reposting would duplicate it.
      rendered = true
      traceLog(options, 'slackbotv2_render_recovery_failed_answer_visible', trace, {
        error: errorMessage(error)
      })
    } else {
      traceLog(options, 'slackbotv2_render_recovery_render_failed', trace, {
        error: errorMessage(error),
        slack_answer_lost: answerLost ?? 'unknown'
      })
      const fallback = await renderFallbackFinalAnswer(
        thread,
        options,
        {
          afterEventId: obligation.afterEventId,
          executionId: obligation.executionId,
          threadId
        },
        trace
      )
      if (!fallback) throw error
      rendered = true
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

async function renderExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<void> {
  if (isPlainTextOnlyRequest(message.text)) {
    await renderPlainTextExecutionStream(thread, stream, message, options, trace)
    return
  }
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(message.text, options.userName))
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...')
  traceLog(options, 'slackbotv2_render_slack_metadata_set', trace, {
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  try {
    const visibleStream = await streamAfterFirstChunk(
      conflateChatSdkStream(
        slackSafeChatSdkStream(
          codexAppServerToChatSdkStream(
            stream,
            rendererOptions(thread, options)
          )
        )
      )
    )
    if (!visibleStream) return
    await thread.post(
      new StreamingPlan(
        visibleStream,
        {
          endWith: feedbackEndBlocks(),
          groupTasks: options.streamTaskDisplayMode ?? 'plan'
        }
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
  if (isPlainTextOnlyRequest(message.text)) {
    await renderPlainTextExecutionStream(thread, stream, message, options, trace)
    return
  }
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(message.text, options.userName))
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...')
  traceLog(options, 'slackbotv2_render_slack_metadata_set', trace, {
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  try {
    const visibleStream = await streamAfterFirstChunk(
      conflateChatSdkStream(
        slackSafeChatSdkStream(
          codexAppServerToChatSdkStream(
            stream,
            rendererOptions(thread, options)
          )
        )
      )
    )
    if (!visibleStream) return
    await thread.adapter.stream!(
      thread.id,
      visibleStream,
      {
        recipientTeamId: message.teamId,
        recipientUserId: message.author.userId,
        stopBlocks: feedbackEndBlocks(),
        taskDisplayMode: options.streamTaskDisplayMode ?? 'plan'
      }
    )
  } finally {
    await setAssistantStatus(thread, '')
  }
}

async function renderPlainTextExecutionStream(
  thread: Thread,
  stream: AsyncIterable<SlackbotV2RendererSource>,
  message: SlackbotV2ApiMessage,
  options: SlackbotV2Options,
  trace?: SlackbotV2Trace
): Promise<void> {
  const fallback = new SlackRenderFallback()
  const titleStartedAtMs = nowMs()
  await setAssistantTitle(thread, titleFromMessage(message.text, options.userName))
  await setAssistantStatus(thread, options.assistantStatus ?? 'Thinking...')
  traceLog(options, 'slackbotv2_render_plain_text_metadata_set', trace, {
    phase_ms: elapsedMs(titleStartedAtMs)
  })
  try {
    const chatStream = fallback.collectChatSdk(
      slackSafeChatSdkStream(
        codexAppServerToChatSdkStream(
          fallback.collectSource(stream),
          rendererOptions(thread, options)
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
    await setAssistantStatus(thread, '')
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

/**
 * Renderer options for the final-answer fallback drain: no Slack side effects
 * (no assistant title updates) and renderer hooks must not be able to fail
 * the delivery.
 */
function fallbackRendererOptions(options: SlackbotV2Options): CodexAppServerToChatStreamOptions {
  const mapper = options.mapper
  return {
    ...mapper,
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
