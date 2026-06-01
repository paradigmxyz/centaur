import { Hono } from 'hono'
import {
  Chat,
  StreamingMarkdownRenderer,
  StreamingPlan,
  type Logger,
  type Message,
  type StateAdapter,
  type StreamChunk,
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
  patchSlackAdapterStreaming(slack, options.botToken, logger)
  const chat = new Chat({
    userName,
    adapters: { slack },
    state: options.state ?? createDefaultState(options, logger),
    onLockConflict: 'force',
    logger
  })

  chat.onNewMention(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    await thread.subscribe()
    await syncThreadMessageToSession(thread, message, {
      mode: 'execute',
      options
    })
  })

  chat.onSubscribedMessage(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    await syncThreadMessageToSession(thread, message, {
      mode: message.isMention === true ? 'execute' : 'append',
      options
    })
  })

  const app = new Hono()
  app.get('/health', c => c.json({ ok: true, service: 'slackbotv2' }))
  app.post('/api/webhooks/slack', async c => {
    const rawBody = await c.req.raw.clone().text()
    if (!isAllowedSlackWebhookBody(rawBody, options, logger)) {
      return new globalThis.Response('ok', { status: 200 })
    }
    const response = await chat.webhooks.slack(c.req.raw, {
      waitUntil: promise => waitUntil(c, promise)
    })
    return new globalThis.Response(await response.text(), {
      headers: response.headers,
      status: response.status
    })
  })

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
 * Persists a Slack thread update into the session API. In execute mode it also starts and
 * renders a session stream unless another execution is already active for the same thread.
 */
async function syncThreadMessageToSession(
  thread: Thread<SlackbotV2ThreadState>,
  message: Message,
  input: {
    mode: SlackbotV2MessageMode
    options: SlackbotV2Options
  }
): Promise<void> {
  const traceStartedAtMs = nowMs()
  const state = (await thread.state) ?? {}
  const messageIds = new Set(state.forwardedMessageIds ?? [])
  const shouldStartExecution = input.mode === 'execute' && state.activeExecution !== true
  const shouldIncludeContext = shouldStartExecution
  const isDuplicateIncrementalMessage =
    messageIds.has(message.id) && (!shouldIncludeContext || state.historyForwarded)
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
    for (const item of context) {
      messageIds.add(item.id)
    }
    traceLog(input.options, 'slackbotv2_forward_context_collected', trace, {
      message_count: context.length,
      phase_ms: elapsedMs(contextStartedAtMs)
    })
  } else {
    messageIds.add(serializedMessage.id)
    traceLog(input.options, 'slackbotv2_forward_context_skipped', trace, {
      message_count: 1
    })
  }

  let lastEventId = state.lastEventId ?? 0

  const forwardInput: ForwardSessionInput = {
    afterEventId: lastEventId,
    executeMessage: shouldStartExecution ? serializedMessage : undefined,
    messages: context ?? [serializedMessage],
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: shouldStartExecution,
    threadId: thread.id,
    trace
  }

  const commitForwardedState = async (): Promise<void> => {
    await thread.setState({
      activeExecution: state.activeExecution || shouldStartExecution,
      forwardedMessageIds: Array.from(messageIds).slice(-1000),
      historyForwarded: state.historyForwarded || shouldIncludeContext,
      lastEventId
    })
    traceLog(input.options, 'slackbotv2_forward_state_committed', trace, {
      forwarded_message_count: Math.min(messageIds.size, 1000)
    })
  }

  if (!shouldStartExecution) {
    await forwardToSessionApi(input.options, forwardInput)
    await commitForwardedState()
    traceLog(input.options, 'slackbotv2_forward_complete', trace)
    return
  }

  try {
    await thread.setState({ ...state, activeExecution: true })
    traceLog(input.options, 'slackbotv2_forward_active_execution_marked', trace)
    await renderExecutionStream(
      thread,
      executeAndStreamSession(input.options, forwardInput, commitForwardedState),
      serializedMessage,
      input.options,
      trace
    )
    traceLog(input.options, 'slackbotv2_render_complete', trace)
  } finally {
    const latest = (await thread.state) ?? {}
    await thread.setState({
      ...latest,
      activeExecution: false,
      lastEventId: Math.max(latest.lastEventId ?? 0, lastEventId)
    })
    traceLog(input.options, 'slackbotv2_forward_complete', trace, {
      last_event_id: lastEventId
    })
  }
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

async function* executeAndStreamSession(
  options: SlackbotV2Options,
  input: ForwardSessionInput,
  onSessionReady: () => Promise<void>
): AsyncIterable<SlackbotV2RendererSource> {
  yield startingStreamNotification(input.threadId)
  traceLog(options, 'slackbotv2_stream_heartbeat_emitted', input.trace)

  try {
    const stream = await forwardToSessionApi(options, input)
    await onSessionReady()
    if (!stream) return
    for await (const event of stream) yield event
  } catch (error) {
    traceLog(options, 'slackbotv2_forward_failed', input.trace, {
      error: errorMessage(error)
    })
    yield sessionStreamError(error)
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

type SlackStreamingAdapter = {
  decodeThreadId(threadId: string): { channel: string; threadTs?: string }
  getClientForToken?(token: string): SlackStreamingClient
  stream?: (
    threadId: string,
    textStream: AsyncIterable<string | StreamChunk>,
    options?: SlackStreamOptions
  ) => Promise<{ id: string; raw: unknown; threadId: string }>
}

type SlackStreamPayload = { [key: string]: unknown }
const SLACK_MAX_BLOCKS = 50
const SLACK_SECTION_TEXT_MAX_CHARS = 3000

type SlackStreamingClient = {
  chatStream(input: SlackStreamPayload): SlackStreamer
}

type SlackStreamer = {
  append(input: SlackStreamPayload): Promise<unknown>
  stop(input?: SlackStreamPayload): Promise<{ message?: { ts?: string }; ts?: string }>
}

type SlackStreamOptions = {
  recipientTeamId?: string
  recipientUserId?: string
  stopBlocks?: unknown[]
  taskDisplayMode?: 'plan' | 'timeline' | 'dense'
}

function patchSlackAdapterStreaming(adapter: unknown, botToken: string, logger: Logger): void {
  const slack = adapter as SlackStreamingAdapter
  if (!slack.getClientForToken || !slack.stream) return

  const originalStream = slack.stream.bind(slack)
  slack.stream = async (threadId, textStream, options) => {
    if (!(options?.recipientUserId && options?.recipientTeamId)) {
      return originalStream(threadId, textStream, options)
    }

    const { channel, threadTs: rawThreadTs } = slack.decodeThreadId(threadId)
    const threadTs = rawThreadTs || undefined
    if (!threadTs) {
      return originalStream(threadId, textStream, options)
    }

    logger.debug('Slack: starting token-bound stream', { channel, threadTs })
    const client = slack.getClientForToken!(botToken)
    const streamer = client.chatStream({
      channel,
      thread_ts: threadTs,
      recipient_user_id: options.recipientUserId,
      recipient_team_id: options.recipientTeamId,
      ...(options.taskDisplayMode ? { task_display_mode: options.taskDisplayMode } : {})
    })

    let lastAppended = ''
    let structuredChunksSupported = true
    const renderer = new StreamingMarkdownRenderer({ wrapTablesForAppend: false })

    const flushMarkdownDelta = async (delta: string): Promise<void> => {
      if (!delta) return
      await streamer.append({ markdown_text: delta })
    }

    const pushTextAndFlush = async (text: string): Promise<void> => {
      renderer.push(text)
      const committable = renderer.getCommittableText()
      const delta = committable.slice(lastAppended.length)
      await flushMarkdownDelta(delta)
      lastAppended = committable
    }

    const sendStructuredChunk = async (chunk: StreamChunk): Promise<void> => {
      if (!structuredChunksSupported) return
      const committable = renderer.getCommittableText()
      const delta = committable.slice(lastAppended.length)
      await flushMarkdownDelta(delta)
      lastAppended = committable
      try {
        await streamer.append({ chunks: [chunk] })
      } catch (error) {
        structuredChunksSupported = false
        logger.warn('Slack structured streaming chunk failed; falling back to text-only stream', {
          chunkType: chunk.type,
          error
        })
      }
    }

    for await (const chunk of textStream) {
      if (typeof chunk === 'string') {
        await pushTextAndFlush(chunk)
      } else if (chunk.type === 'markdown_text') {
        await pushTextAndFlush(chunk.text)
      } else {
        await sendStructuredChunk(chunk)
      }
    }

    renderer.finish()
    const finalCommittable = renderer.getCommittableText()
    const finalDelta = finalCommittable.slice(lastAppended.length)
    await flushMarkdownDelta(finalDelta)
    lastAppended = finalCommittable
    const stopPayload: SlackStreamPayload = {}
    const stopBlocks =
      options.stopBlocks && finalCommittable.trim()
        ? appendFinalMarkdownBlocks(options.stopBlocks, finalCommittable)
        : options.stopBlocks
    if (stopBlocks) {
      stopPayload.blocks = stopBlocks
    }
    const result = await streamer.stop(Object.keys(stopPayload).length ? stopPayload : undefined)
    const messageTs = result.message?.ts ?? result.ts
    if (!messageTs) throw new Error('Slack stream completed without a message timestamp')
    logger.debug('Slack: token-bound stream complete', { messageId: messageTs })
    return { id: messageTs, threadId, raw: result }
  }
}

function appendFinalMarkdownBlocks(blocks: unknown[], markdown: string): unknown[] {
  const finalBlocks = markdownSectionBlocks(markdown)
  const available = Math.max(0, SLACK_MAX_BLOCKS - finalBlocks.length)
  return [...blocks.slice(0, available), ...finalBlocks]
}

function markdownSectionBlocks(markdown: string): unknown[] {
  const text = markdown.trim()
  if (!text) return []

  const blocks: unknown[] = []
  let remaining = text
  while (remaining && blocks.length < SLACK_MAX_BLOCKS) {
    const chunk = remaining.slice(0, SLACK_SECTION_TEXT_MAX_CHARS)
    blocks.push({
      type: 'section',
      text: {
        type: 'mrkdwn',
        text: chunk
      }
    })
    remaining = remaining.slice(chunk.length)
  }
  return blocks
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
