import { Hono } from 'hono'
import {
  Chat,
  StreamingMarkdownRenderer,
  StreamingPlan,
  type Attachment,
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
import type { RustSessionStreamEvent } from '@centaur/harness-events'

export type SlackbotV2ApiAuthor = {
  fullName: string
  isBot: boolean | 'unknown'
  isMe: boolean
  userId: string
  userName: string
}

export type SlackbotV2ApiAttachment = {
  dataBase64?: string
  fetchError?: string
  fetchMetadata?: Record<string, string>
  height?: number
  mimeType?: string
  name?: string
  size?: number
  type: Attachment['type']
  url?: string
  width?: number
}

export type SlackbotV2ApiMessage = {
  attachments: SlackbotV2ApiAttachment[]
  author: SlackbotV2ApiAuthor
  id: string
  isMention: boolean
  raw: unknown
  text: string
  threadId: string
  timestamp: string
}

export type SlackbotV2SessionMessageRole = 'user' | 'assistant' | 'system' | 'tool'

export type SlackbotV2SessionMessage = {
  metadata: Record<string, unknown>
  parts: unknown[]
  role: SlackbotV2SessionMessageRole
}

export type SlackbotV2AppendMessagesRequest = {
  messages: SlackbotV2SessionMessage[]
}

export type SlackbotV2CreateSessionRequest = {
  harness_type: string
  metadata: Record<string, unknown>
}

export type SlackbotV2ExecuteSessionRequest = {
  idle_timeout_ms?: number
  input_lines: string[]
  max_duration_ms?: number
  metadata: Record<string, unknown>
}

export type SlackbotV2Fetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>

export type SlackbotV2Options = {
  allowedExternalTeamIds?: readonly string[]
  apiKey?: string
  apiUrl: string
  assistantStatus?: string
  botToken: string
  botUserId?: string
  fetch?: SlackbotV2Fetch
  idleTimeoutMs?: number
  logger?: Logger
  maxDurationMs?: number
  postgresUrl?: string
  signingSecret: string
  slackApiUrl?: string
  state?: StateAdapter
  stateKeyPrefix?: string
  streamTaskDisplayMode?: 'plan' | 'timeline'
  triggerBotAllowlist?: readonly string[]
  userName?: string
  mapper?: CodexAppServerToChatStreamOptions
}

export type SlackbotV2 = {
  app: Hono
  chat: Chat
}

type SlackbotV2ThreadState = {
  activeExecution?: boolean
  forwardedMessageIds?: string[]
  historyForwarded?: boolean
  lastEventId?: number
}

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

type SlackbotV2RendererSource = RustSessionStreamEvent | Record<string, unknown>

type SlackbotV2Trace = {
  execute: boolean
  includeContext: boolean
  messageId: string
  openStream: boolean
  startedAtMs: number
  threadId: string
}

type ForwardSessionInput = {
  afterEventId: number
  executeMessage?: SlackbotV2ApiMessage
  messages: SlackbotV2ApiMessage[]
  onEventId(eventId: number): void
  openStream: boolean
  threadId: string
  trace?: SlackbotV2Trace
}

const noopLogger: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
  child: () => noopLogger
}

function nowMs(): number {
  return globalThis.performance?.now?.() ?? Date.now()
}

function elapsedMs(startedAtMs: number): number {
  return Math.max(0, Math.round(nowMs() - startedAtMs))
}

function traceLog(
  options: SlackbotV2Options,
  event: string,
  trace?: SlackbotV2Trace,
  fields: Record<string, unknown> = {}
): void {
  const logger = options.logger ?? noopLogger
  logger.info(event, {
    ...(trace
      ? {
          elapsed_ms: elapsedMs(trace.startedAtMs),
          execute: trace.execute,
          include_context: trace.includeContext,
          message_id: trace.messageId,
          open_stream: trace.openStream,
          thread_id: trace.threadId
        }
      : {}),
    ...fields
  })
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}

type RawSlackEvent = Record<string, unknown> & {
  app_id?: unknown
  bot_id?: unknown
  bot_profile?: {
    app_id?: unknown
    id?: unknown
    user_id?: unknown
  }
  source_team?: unknown
  subtype?: unknown
  team?: unknown
  team_id?: unknown
  user?: unknown
  user_team?: unknown
}

type RawSlackEnvelope = Record<string, unknown> & {
  event?: unknown
  event_id?: unknown
  team_id?: unknown
  type?: unknown
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
    await forwardAndMaybeRender(thread, message, {
      execute: true,
      includeContext: true,
      options
    })
  })

  chat.onSubscribedMessage(async (thread, message) => {
    if (!isAllowedSlackMessage(message, options, logger)) return
    await forwardAndMaybeRender(thread, message, {
      execute: message.isMention === true,
      includeContext: message.isMention === true,
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

function isAllowedSlackWebhookBody(
  rawBody: string,
  options: SlackbotV2Options,
  logger: Logger
): boolean {
  let payload: unknown
  try {
    payload = JSON.parse(rawBody)
  } catch {
    return true
  }
  if (!isRawSlackEnvelope(payload) || payload.type !== 'event_callback') return true
  const event = isRawSlackEvent(payload.event) ? payload.event : undefined
  if (!event) return true

  const allowedExternalTeamIds =
    options.allowedExternalTeamIds ?? splitEnvList(process.env.SLACKBOT_EXTERNAL_ORG_ALLOWLIST)
  const externalTeamId = externalSlackTeamIdForHome(stringValue(payload.team_id), event)
  if (externalTeamId && !new Set(allowedExternalTeamIds).has(externalTeamId)) {
    logger.warn('slackbotv2_event_ignored_external_org_not_allowlisted', {
      event_id: stringValue(payload.event_id),
      external_team_id: externalTeamId,
      team_id: stringValue(payload.team_id)
    })
    return false
  }
  return true
}

function isAllowedSlackMessage(
  message: Message,
  options: SlackbotV2Options,
  logger: Logger
): boolean {
  const raw = isRawSlackEvent(message.raw) ? message.raw : undefined
  const allowedExternalTeamIds =
    options.allowedExternalTeamIds ?? splitEnvList(process.env.SLACKBOT_EXTERNAL_ORG_ALLOWLIST)
  const externalTeamId = raw ? externalSlackTeamId(raw) : undefined
  if (externalTeamId && !new Set(allowedExternalTeamIds).has(externalTeamId)) {
    logger.warn('slackbotv2_event_ignored_external_org_not_allowlisted', {
      external_team_id: externalTeamId,
      message_id: message.id,
      thread_id: message.threadId
    })
    return false
  }

  const triggerBotAllowlist =
    options.triggerBotAllowlist ?? splitEnvList(process.env.SLACKBOT_TRIGGER_BOT_ALLOWLIST)
  const botAuthored = message.author.isBot === true || (raw ? isBotAuthoredSlackEvent(raw) : false)
  if (botAuthored && !(raw && isAllowedTriggerBotMessage(raw, triggerBotAllowlist))) {
    logger.warn('slackbotv2_event_ignored_bot_not_allowlisted', {
      message_id: message.id,
      thread_id: message.threadId
    })
    return false
  }

  return true
}

function externalSlackTeamId(event: RawSlackEvent): string | undefined {
  return externalSlackTeamIdForHome(stringValue(event.team_id), event)
}

function externalSlackTeamIdForHome(
  homeTeamId: string | undefined,
  event: RawSlackEvent
): string | undefined {
  if (!homeTeamId) return undefined
  for (const candidate of [event.user_team, event.source_team, event.team]) {
    const teamId = stringValue(candidate)
    if (teamId && teamId !== homeTeamId) return teamId
  }
  return undefined
}

function isBotAuthoredSlackEvent(event: RawSlackEvent): boolean {
  return Boolean(event.bot_id || event.bot_profile || event.subtype === 'bot_message')
}

function isAllowedTriggerBotMessage(
  event: RawSlackEvent,
  allowlist: readonly string[] | undefined
): boolean {
  if (!allowlist?.length) return false
  const appIds = normalizedIdentifierSet(stringValue(event.app_id), stringValue(event.bot_profile?.app_id))
  const botIds = normalizedIdentifierSet(stringValue(event.bot_id), stringValue(event.bot_profile?.id))
  const botUserIds = normalizedIdentifierSet(
    stringValue(event.user),
    stringValue(event.bot_profile?.user_id)
  )
  const anyIds = new Set([...appIds, ...botIds, ...botUserIds])

  for (const entry of allowlist) {
    const parsed = parseTriggerBotAllowlistEntry(entry)
    if (!parsed) continue
    if (parsed.kind === 'app' && appIds.has(parsed.value)) return true
    if (parsed.kind === 'bot' && botIds.has(parsed.value)) return true
    if (parsed.kind === 'user' && botUserIds.has(parsed.value)) return true
    if (parsed.kind === 'any' && anyIds.has(parsed.value)) return true
  }
  return false
}

function normalizedIdentifierSet(...values: Array<string | undefined>): Set<string> {
  return new Set(values.map(value => value?.trim()).filter((value): value is string => Boolean(value)))
}

function parseTriggerBotAllowlistEntry(
  entry: string
): { kind: 'app' | 'bot' | 'user' | 'any'; value: string } | null {
  const trimmed = entry.trim()
  if (!trimmed) return null
  const prefixed = /^(app|bot|user):(.+)$/i.exec(trimmed)
  if (!prefixed) return { kind: 'any', value: trimmed }
  const kind = prefixed[1]
  const value = prefixed[2]?.trim()
  if (!kind || !value) return null
  return { kind: kind.toLowerCase() as 'app' | 'bot' | 'user', value }
}

function splitEnvList(value: string | undefined): string[] {
  return (value ?? '')
    .split(/[\s,]+/)
    .map(part => part.trim())
    .filter(Boolean)
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

function isRawSlackEvent(value: unknown): value is RawSlackEvent {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function isRawSlackEnvelope(value: unknown): value is RawSlackEnvelope {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

async function forwardAndMaybeRender(
  thread: Thread<SlackbotV2ThreadState>,
  message: Message,
  input: {
    execute: boolean
    includeContext: boolean
    options: SlackbotV2Options
  }
): Promise<void> {
  const traceStartedAtMs = nowMs()
  const state = (await thread.state) ?? {}
  const messageIds = new Set(state.forwardedMessageIds ?? [])
  const isDuplicateIncrementalMessage =
    messageIds.has(message.id) && (!input.includeContext || state.historyForwarded)
  const shouldOpenStream = input.execute && state.activeExecution !== true
  const trace: SlackbotV2Trace = {
    execute: input.execute,
    includeContext: input.includeContext,
    messageId: message.id,
    openStream: shouldOpenStream,
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

  if (input.includeContext && !state.historyForwarded) {
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
    executeMessage: input.execute ? serializedMessage : undefined,
    messages: context ?? [serializedMessage],
    onEventId: eventId => {
      lastEventId = Math.max(lastEventId, eventId)
    },
    openStream: shouldOpenStream,
    threadId: thread.id,
    trace
  }

  const commitForwardedState = async (): Promise<void> => {
    await thread.setState({
      activeExecution: state.activeExecution || shouldOpenStream,
      forwardedMessageIds: Array.from(messageIds).slice(-1000),
      historyForwarded: state.historyForwarded || input.includeContext,
      lastEventId: state.lastEventId
    })
    traceLog(input.options, 'slackbotv2_forward_state_committed', trace, {
      forwarded_message_count: Math.min(messageIds.size, 1000)
    })
  }

  if (!shouldOpenStream) {
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

function startingStreamNotification(threadId: string): Record<string, unknown> {
  return {
    method: 'item/started',
    params: {
      threadId,
      turnId: 'slackbotv2-starting-turn',
      startedAtMs: Date.now(),
      item: {
        id: 'slackbotv2-starting',
        memoryCitation: null,
        phase: 'commentary',
        text: '',
        type: 'agentMessage'
      }
    }
  }
}

function sessionStreamError(error: unknown): RustSessionStreamEvent {
  return {
    data: { error: errorMessage(error) },
    event: 'session.stream_error',
    eventKind: 'session.stream_error'
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

type SlackStreamingClient = {
  chatStream(input: Record<string, unknown>): SlackStreamer
}

type SlackStreamer = {
  append(input: Record<string, unknown>): Promise<unknown>
  stop(input?: Record<string, unknown>): Promise<{ message?: { ts?: string }; ts?: string }>
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
    await flushMarkdownDelta(finalCommittable.slice(lastAppended.length))
    const result = await streamer.stop(options.stopBlocks ? { blocks: options.stopBlocks } : undefined)
    const messageTs = result.message?.ts ?? result.ts
    if (!messageTs) throw new Error('Slack stream completed without a message timestamp')
    logger.debug('Slack: token-bound stream complete', { messageId: messageTs })
    return { id: messageTs, threadId, raw: result }
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
  return `${oneLine.slice(0, Math.max(0, max - 1)).trimEnd()}…`
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

async function collectInitialContext(
  thread: Thread,
  currentMessage: Message
): Promise<SlackbotV2ApiMessage[]> {
  const messages: Message[] = []
  for await (const message of thread.allMessages) {
    messages.push(message)
  }

  const currentIndex = messages.findIndex(message => message.id === currentMessage.id)
  if (currentIndex >= 0) {
    messages[currentIndex] = currentMessage
  } else {
    messages.push(currentMessage)
  }

  const serialized: SlackbotV2ApiMessage[] = []
  for (const message of messages) {
    serialized.push(await serializeMessage(message))
  }
  return serialized
}

async function serializeMessage(message: Message): Promise<SlackbotV2ApiMessage> {
  const attachments: SlackbotV2ApiAttachment[] = []
  for (const attachment of message.attachments) {
    attachments.push(await serializeAttachment(attachment))
  }

  return {
    attachments,
    author: {
      fullName: message.author.fullName,
      isBot: message.author.isBot,
      isMe: message.author.isMe,
      userId: message.author.userId,
      userName: message.author.userName
    },
    id: message.id,
    isMention: message.isMention === true,
    raw: message.raw,
    text: message.text,
    threadId: message.threadId,
    timestamp: message.metadata.dateSent.toISOString()
  }
}

async function serializeAttachment(attachment: Attachment): Promise<SlackbotV2ApiAttachment> {
  const serialized: SlackbotV2ApiAttachment = {
    fetchMetadata: attachment.fetchMetadata,
    height: attachment.height,
    mimeType: attachment.mimeType,
    name: attachment.name,
    size: attachment.size,
    type: attachment.type,
    url: attachment.url,
    width: attachment.width
  }

  try {
    const data = attachment.data ?? (await attachment.fetchData?.())
    if (data) {
      serialized.dataBase64 = await bytesToBase64(data)
    }
  } catch (error) {
    serialized.fetchError = error instanceof Error ? error.message : String(error)
  }

  return serialized
}

async function bytesToBase64(data: Buffer | Blob): Promise<string> {
  if (Buffer.isBuffer(data)) return data.toString('base64')
  const bytes = await data.arrayBuffer()
  return Buffer.from(bytes).toString('base64')
}

async function forwardToSessionApi(
  options: SlackbotV2Options,
  input: ForwardSessionInput
): Promise<AsyncIterable<SlackbotV2RendererSource> | null> {
  const createStartedAtMs = nowMs()
  await createSession(options, input.threadId)
  traceLog(options, 'slackbotv2_session_create_complete', input.trace, {
    phase_ms: elapsedMs(createStartedAtMs)
  })
  const appendStartedAtMs = nowMs()
  await appendSessionMessages(options, input.threadId, input.messages)
  traceLog(options, 'slackbotv2_session_append_complete', input.trace, {
    message_count: input.messages.length,
    phase_ms: elapsedMs(appendStartedAtMs)
  })
  if (!input.executeMessage) return null

  const executeStartedAtMs = nowMs()
  await executeSession(options, input.threadId, input.executeMessage)
  traceLog(options, 'slackbotv2_session_execute_complete', input.trace, {
    phase_ms: elapsedMs(executeStartedAtMs)
  })
  if (!input.openStream) return null

  const streamStartedAtMs = nowMs()
  const stream = await streamSessionNotifications(
    options,
    input.threadId,
    input.afterEventId,
    input.onEventId
  )
  traceLog(options, 'slackbotv2_session_events_opened', input.trace, {
    after_event_id: input.afterEventId,
    phase_ms: elapsedMs(streamStartedAtMs)
  })
  return stream
}

async function createSession(options: SlackbotV2Options, threadId: string): Promise<void> {
  const fetchFn = options.fetch ?? fetch
  const body: SlackbotV2CreateSessionRequest = {
    harness_type: 'codex',
    metadata: {
      source: 'slackbotv2',
      platform: 'slack',
      thread_id: threadId
    }
  }
  const response = await fetchFn(apiSessionUrl(options.apiUrl, threadId), {
    method: 'POST',
    headers: apiHeaders(options),
    body: JSON.stringify(body)
  })
  await ensureApiOk(response, 'create session')
}

async function appendSessionMessages(
  options: SlackbotV2Options,
  threadId: string,
  messages: SlackbotV2ApiMessage[]
): Promise<void> {
  const fetchFn = options.fetch ?? fetch
  const body: SlackbotV2AppendMessagesRequest = {
    messages: messages.map(toSessionMessage)
  }
  const response = await fetchFn(apiSessionUrl(options.apiUrl, threadId, 'messages'), {
    method: 'POST',
    headers: apiHeaders(options),
    body: JSON.stringify(body)
  })
  await ensureApiOk(response, 'append session messages')
}

async function executeSession(
  options: SlackbotV2Options,
  threadId: string,
  message: SlackbotV2ApiMessage
): Promise<void> {
  const fetchFn = options.fetch ?? fetch
  const body: SlackbotV2ExecuteSessionRequest = {
    metadata: sessionMetadata(message, { action: 'execute' }),
    input_lines: [toCodexInputLine(message, threadId)],
    ...(options.idleTimeoutMs === undefined ? {} : { idle_timeout_ms: options.idleTimeoutMs }),
    ...(options.maxDurationMs === undefined ? {} : { max_duration_ms: options.maxDurationMs })
  }
  const response = await fetchFn(apiSessionUrl(options.apiUrl, threadId, 'execute'), {
    method: 'POST',
    headers: apiHeaders(options),
    body: JSON.stringify(body)
  })
  await ensureApiOk(response, 'execute session')
}

async function ensureApiOk(response: Response, action: string): Promise<void> {
  if (response.ok) return
  let body = ''
  try {
    body = await response.text()
  } catch {
    body = ''
  }
  const suffix = body ? `: ${body}` : ''
  throw new Error(`Centaur session ${action} failed: ${response.status} ${response.statusText}${suffix}`)
}

async function streamSessionNotifications(
  options: SlackbotV2Options,
  threadId: string,
  afterEventId: number,
  onEventId: (eventId: number) => void
): Promise<AsyncIterable<SlackbotV2RendererSource>> {
  const fetchFn = options.fetch ?? fetch
  const response = await fetchFn(
    `${apiSessionUrl(options.apiUrl, threadId, 'events')}?after_event_id=${afterEventId}`,
    {
      method: 'GET',
      headers: apiHeaders(options, false)
    }
  )
  await ensureApiOk(response, 'stream events')
  if (!response.body) return toAsyncIterable([])
  return parseSessionEventStream(response.body, onEventId)
}

function apiSessionUrl(
  apiUrl: string,
  threadId: string,
  suffix?: 'messages' | 'execute' | 'events'
): string {
  const path = `/api/session/${encodeURIComponent(threadId)}${suffix ? `/${suffix}` : ''}`
  return new URL(path, ensureTrailingSlash(apiUrl)).toString()
}

function ensureTrailingSlash(value: string): string {
  return value.endsWith('/') ? value : `${value}/`
}

function apiHeaders(options: SlackbotV2Options, jsonBody = true): HeadersInit {
  const apiKey = options.apiKey ?? process.env.SLACKBOT_API_KEY ?? process.env.CENTAUR_API_KEY
  return {
    ...(jsonBody ? { 'content-type': 'application/json' } : {}),
    ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {})
  }
}

function toSessionMessage(message: SlackbotV2ApiMessage): SlackbotV2SessionMessage {
  return {
    role: message.author.isMe ? 'assistant' : 'user',
    parts: sessionMessageParts(message),
    metadata: sessionMetadata(message)
  }
}

function sessionMessageParts(message: SlackbotV2ApiMessage): unknown[] {
  const parts: unknown[] = []
  if (message.text.trim()) {
    parts.push({ type: 'text', text: message.text })
  }
  for (const attachment of message.attachments) {
    parts.push({ ...attachment, attachment_type: attachment.type, type: 'attachment' })
  }
  return parts.length > 0 ? parts : [{ type: 'text', text: '' }]
}

function sessionMetadata(
  message: SlackbotV2ApiMessage,
  extra: Record<string, unknown> = {}
): Record<string, unknown> {
  return {
    source: 'slackbotv2',
    platform: 'slack',
    message_id: message.id,
    thread_id: message.threadId,
    is_mention: message.isMention,
    timestamp: message.timestamp,
    user_id: message.author.userId,
    user_name: message.author.userName,
    ...extra
  }
}

function toCodexInputLine(message: SlackbotV2ApiMessage, threadId: string): string {
  return JSON.stringify({
    type: 'user',
    thread_key: threadId,
    trace_metadata: sessionMetadata(message, { action: 'execute' }),
    message: {
      role: 'user',
      content: codexInputContent(message)
    }
  })
}

function codexInputContent(message: SlackbotV2ApiMessage): unknown[] {
  const content: unknown[] = []
  if (message.text.trim()) {
    content.push({ type: 'text', text: message.text })
  }
  for (const attachment of message.attachments) {
    content.push(codexAttachmentInput(attachment))
  }
  return content.length > 0 ? content : [{ type: 'text', text: 'continue' }]
}

function codexAttachmentInput(attachment: SlackbotV2ApiAttachment): unknown {
  const dataUrl =
    attachment.dataBase64 && attachment.mimeType
      ? `data:${attachment.mimeType};base64,${attachment.dataBase64}`
      : undefined
  if (attachment.type === 'image' && (dataUrl || attachment.url)) {
    return {
      type: 'image',
      url: dataUrl ?? attachment.url,
      detail: 'auto',
      name: attachment.name
    }
  }
  return {
    type: 'text',
    text: attachmentDescription(attachment)
  }
}

function attachmentDescription(attachment: SlackbotV2ApiAttachment): string {
  const fields = [
    `name=${attachment.name ?? 'attachment'}`,
    `type=${attachment.type}`,
    attachment.mimeType ? `mime=${attachment.mimeType}` : undefined,
    attachment.url ? `url=${attachment.url}` : undefined,
    attachment.dataBase64 ? `base64=${attachment.dataBase64}` : undefined,
    attachment.fetchError ? `fetch_error=${attachment.fetchError}` : undefined
  ].filter(Boolean)
  return `[Slack attachment: ${fields.join(' ')}]`
}

type ParsedSessionEvent = {
  data: string
  event?: string
  id?: number
}

async function* parseSessionEventStream(
  stream: ReadableStream<Uint8Array>,
  onEventId: (eventId: number) => void
): AsyncIterable<SlackbotV2RendererSource> {
  for await (const event of parseSseEvents(stream)) {
    if (typeof event.id === 'number') onEventId(event.id)
    if (event.event === 'session.output.line') {
      yield {
        data: event.data,
        event: event.event,
        eventId: event.id,
        eventKind: event.event
      } satisfies RustSessionStreamEvent
      if (isTerminalCodexOutputLine(event.data)) return
      continue
    }
    if (event.event === 'session.execution_failed' || event.event === 'session.stream_error') {
      yield {
        data: { error: sessionErrorMessage(event) },
        event: event.event,
        eventId: event.id,
        eventKind: event.event
      } satisfies RustSessionStreamEvent
      return
    }
  }
}

async function* parseSseEvents(stream: ReadableStream<Uint8Array>): AsyncIterable<ParsedSessionEvent> {
  const reader = stream.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  let eventName: string | undefined
  let eventId: number | undefined
  let data: string[] = []

  while (true) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split(/\r?\n/)
    buffer = lines.pop() ?? ''

    for (const line of lines) {
      const emitted = parseSseLine(line, { data, eventId, eventName })
      data = emitted.state.data
      eventId = emitted.state.eventId
      eventName = emitted.state.eventName
      if (emitted.event) yield emitted.event
    }
  }

  buffer += decoder.decode()
  if (buffer) {
    const emitted = parseSseLine(buffer, { data, eventId, eventName })
    data = emitted.state.data
    eventId = emitted.state.eventId
    eventName = emitted.state.eventName
    if (emitted.event) yield emitted.event
  }
  if (data.length > 0) {
    yield { data: data.join('\n'), event: eventName, id: eventId }
  }
}

function parseSseLine(
  line: string,
  state: {
    data: string[]
    eventId?: number
    eventName?: string
  }
): {
  event?: ParsedSessionEvent
  state: { data: string[]; eventId?: number; eventName?: string }
} {
  if (!line.trim()) {
    const event =
      state.data.length > 0
        ? { data: state.data.join('\n'), event: state.eventName, id: state.eventId }
        : undefined
    return { event, state: { data: [] } }
  }
  if (line.startsWith(':')) return { state }

  const separator = line.indexOf(':')
  const field = separator >= 0 ? line.slice(0, separator) : line
  const value = separator >= 0 ? line.slice(separator + 1).replace(/^ /, '') : ''
  if (field === 'event') return { state: { ...state, eventName: value } }
  if (field === 'id') {
    const id = Number.parseInt(value, 10)
    return { state: { ...state, eventId: Number.isFinite(id) ? id : undefined } }
  }
  if (field === 'data' && value !== '[DONE]') {
    return { state: { ...state, data: [...state.data, value] } }
  }

  return { state }
}

function isTerminalCodexOutputLine(line: string): boolean {
  let payload: unknown
  try {
    payload = JSON.parse(line)
  } catch {
    return true
  }
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) return false

  const object = payload as Record<string, unknown>
  return (
    object.type === 'turn.completed' ||
    object.type === 'turn.failed' ||
    object.type === 'turn.done' ||
    object.method === 'error' ||
    object.method === 'turn/completed'
  )
}

function sessionErrorMessage(event: ParsedSessionEvent): string {
  let message = `${event.event ?? 'session error'}`
  try {
    const payload = JSON.parse(event.data) as Record<string, unknown>
    message = stringValue(payload.error) ?? stringValue(payload.message) ?? message
  } catch {
    if (event.data.trim()) message = event.data.trim()
  }
  return message
}

async function* toAsyncIterable<T>(source: Iterable<T>): AsyncIterable<T> {
  for await (const item of source) {
    yield item
  }
}

function waitUntil(c: { executionCtx: WaitUntilContext }, promise: Promise<unknown>): void {
  try {
    c.executionCtx.waitUntil(promise)
  } catch {
    void promise.catch(() => undefined)
  }
}
